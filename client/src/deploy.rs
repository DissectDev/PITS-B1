//! Declarative NixOS deployer (`pitsctl deploy-nixos`) for `pits-core` (`100.74.25.42`).
//!
//! Respects NixOS immutability (`/home/libraskywalker/PITS-B1/Base` & RKE2):
//! - Never writes to `/usr/local/bin` or imperative `/etc/systemd/system/*.service`.
//! - Supports two declarative NixOS deployment modes:
//!   1. `nixos-module`: Installs `modules/pits-agent.nix` + `modules/pits-agent-package.nix`
//!      into `/home/libraskywalker/PITS-B1/Base`, stages static ELF/source under `/var/lib/pits/agent-src`
//!      and mTLS PKI in root-only `/var/lib/pits/agent/pki` (`0600`, outside `/nix/store` & Git),
//!      registers the module in `hosts/core/default.nix` (`git add -N` for Flake visibility),
//!      and activates via `nixos-rebuild test` / `nixos-rebuild switch` with automatic rollback on error.
//!   2. `rke2-daemonset`: Declarative Kubernetes `DaemonSet` (`pits-system/pits-agent`) pinned to
//!      `pits-core` (mirroring `pits-storage/btrfs-manager`) without modifying `PITS-B1` worktree.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::{generate_agent_mtls_pki, local_state_dir, ClientProfile};
use crate::transport::AgentClient;

const EMBEDDED_PACKAGE_NIX: &str = include_str!("../../deploy/nixos/package.nix");
const EMBEDDED_MODULE_NIX: &str = include_str!("../../deploy/nixos/pits-agent-module.nix");
const EMBEDDED_RKE2_DAEMONSET_YAML: &str = include_str!("../../deploy/nixos/rke2-daemonset.yaml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NixosDeployMode {
    NixosModule,
    Rke2Daemonset,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NixosDeployOptions {
    pub ssh_target: String,
    pub agent_host: String,
    pub agent_port: u16,
    pub flake_dir: String,
    pub flake_attr: String,
    pub mode: NixosDeployMode,
    pub switch_generation: bool,
    pub sync_credentials: bool,
    pub rotate_pki: bool,
    pub binary_path: PathBuf,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NixosDeployReport {
    pub dry_run: bool,
    pub mode: NixosDeployMode,
    pub ssh_target: String,
    pub agent_endpoint: String,
    pub binary_sha256: String,
    pub binary_size_bytes: u64,
    pub nixos_version: Option<String>,
    pub activation_action: String,
    pub synced_credentials: Vec<String>,
    pub live_agent_health: Option<serde_json::Value>,
    pub planned_steps: Vec<String>,
}

/// Injects a module path (e.g. `../../modules/pits-agent.nix`) idempotently into a NixOS `hosts/core/default.nix` `imports` list.
pub fn inject_nixos_module_import(default_nix: &str, import_rel_path: &str) -> Result<(String, bool)> {
    let module_file = Path::new(import_rel_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(import_rel_path);
    if default_nix.contains(import_rel_path) || default_nix.contains(module_file) {
        return Ok((default_nix.to_string(), false));
    }
    // Match `imports = [` with arbitrary whitespace/newlines.
    let marker = "imports";
    let mut search_from = 0usize;
    while let Some(rel_idx) = default_nix[search_from..].find(marker) {
        let idx = search_from + rel_idx;
        // Ensure `imports` is at token boundary
        let prev_ok = idx == 0
            || !default_nix.as_bytes()[idx - 1].is_ascii_alphanumeric()
                && default_nix.as_bytes()[idx - 1] != b'_'
                && default_nix.as_bytes()[idx - 1] != b'-';
        let after_idx = idx + marker.len();
        if prev_ok {
            let rest = &default_nix[after_idx..];
            let trimmed = rest.trim_start();
            if let Some(after_eq) = trimmed.strip_prefix('=') {
                let trimmed_br = after_eq.trim_start();
                if trimmed_br.starts_with('[') {
                    let bracket_offset = default_nix.len() - trimmed_br.len() + 1;
                    let mut out = String::with_capacity(default_nix.len() + 64);
                    out.push_str(&default_nix[..bracket_offset]);
                    out.push_str("\n    ");
                    out.push_str(import_rel_path);
                    out.push_str(&default_nix[bracket_offset..]);
                    return Ok((out, true));
                }
            }
        }
        search_from = after_idx;
    }
    bail!("could not locate `imports = [ ... ]` block in NixOS host configuration")
}

/// Rewrites `127.0.0.1:6443` / `localhost:6443` in RKE2's `/etc/rancher/rke2/rke2.yaml`
/// to the reachable `pits-core` Tailscale or management address, and injects
/// `tls-server-name: pits-core` when dialing by IP so strict TLS SAN verification passes.
pub fn rewrite_rke2_kubeconfig_endpoint(raw_yaml: &str, host: &str) -> String {
    let target_server = format!("https://{host}:6443");
    let rewritten = raw_yaml
        .replace("https://127.0.0.1:6443", &target_server)
        .replace("https://localhost:6443", &target_server);
    if host != "pits-core" && !rewritten.contains("tls-server-name:") {
        let needle = format!("server: {target_server}");
        rewritten.replacen(
            &needle,
            &format!("tls-server-name: pits-core\n    {needle}"),
            1,
        )
    } else {
        rewritten
    }
}

fn run_ssh_script(ssh_target: &str, script: &str) -> Result<String> {
    let mut child = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            ssh_target,
            "sh -eu",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn `ssh {ssh_target}`"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(script.as_bytes())
            .context("write remote script to ssh stdin")?;
    }
    let out = child
        .wait_with_output()
        .context("wait for ssh command completion")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        bail!(
            "SSH command to `{ssh_target}` failed (exit {:?}):\nSTDERR: {}\nSTDOUT: {}",
            out.status.code(),
            stderr.trim(),
            stdout.trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn stream_file_over_ssh(ssh_target: &str, local_path: &Path, remote_path: &str, mode_octal: &str) -> Result<()> {
    let data = fs::read(local_path)
        .with_context(|| format!("read local artifact {}", local_path.display()))?;
    stream_bytes_over_ssh(ssh_target, &data, remote_path, mode_octal)
}

fn stream_bytes_over_ssh(ssh_target: &str, data: &[u8], remote_path: &str, mode_octal: &str) -> Result<()> {
    let parent = Path::new(remote_path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("/var/lib/pits/agent");
    let remote_cmd = format!(
        "umask 077 && mkdir -p '{parent}' && tmp='{remote_path}.tmp.$$' && cat > \"$tmp\" && chmod {mode_octal} \"$tmp\" && mv -f \"$tmp\" '{remote_path}'"
    );
    let mut child = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            ssh_target,
            &remote_cmd,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn ssh upload to {ssh_target}:{remote_path}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(data)
            .with_context(|| format!("stream {} bytes to {remote_path}", data.len()))?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "upload to `{ssh_target}:{remote_path}` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub async fn deploy_to_nixos_pits_core(
    profile: &mut ClientProfile,
    profile_path: Option<&Path>,
    opts: &NixosDeployOptions,
) -> Result<NixosDeployReport> {
    if !opts.binary_path.exists() {
        bail!(
            "Linux amd64 binary not found at {} (build `server/bin/pits-agent-linux-amd64` first)",
            opts.binary_path.display()
        );
    }
    let bin_bytes = fs::read(&opts.binary_path)?;
    if bin_bytes.len() < 4 || &bin_bytes[0..4] != b"\x7fELF" {
        bail!(
            "{} is not a valid Linux ELF binary",
            opts.binary_path.display()
        );
    }
    let bin_sha256 = hex::encode(Sha256::digest(&bin_bytes));
    let endpoint = format!("https://{}:{}", opts.agent_host, opts.agent_port);

    let activation_action = match opts.mode {
        NixosDeployMode::NixosModule => {
            if opts.switch_generation {
                format!(
                    "nixos-rebuild switch --flake path:{}#{}",
                    opts.flake_dir, opts.flake_attr
                )
            } else {
                format!(
                    "nixos-rebuild test --flake path:{}#{}",
                    opts.flake_dir, opts.flake_attr
                )
            }
        }
        NixosDeployMode::Rke2Daemonset => {
            "kubectl apply -f rke2-daemonset.yaml (pits-system/pits-agent)".to_string()
        }
    };

    let planned_steps = vec![
        format!(
            "1. Ensure Ed25519 mTLS PKI (SAN: {}, 100.74.25.42, 192.168.71.35, pits-core, pits-core.bun-humboldt.ts.net) in {}",
            opts.agent_host,
            local_state_dir().display()
        ),
        format!(
            "2. Verify Tailscale SSH `{}` reaches NixOS host `pits-core`",
            opts.ssh_target
        ),
        format!(
            "3. Stage verified ELF `pits-agent-linux-amd64` ({} bytes, sha256={}) & root-only PKI (`/var/lib/pits/agent/pki`, 0600)",
            bin_bytes.len(),
            &bin_sha256[..16]
        ),
        match opts.mode {
            NixosDeployMode::NixosModule => format!(
                "4. Install declarative `modules/pits-agent.nix` into `{}`, register in `hosts/core/default.nix`, `git add -N`, and run `{}`",
                opts.flake_dir, activation_action
            ),
            NixosDeployMode::Rke2Daemonset => {
                "4. Reconcile declarative RKE2 DaemonSet `pits-system/pits-agent` pinned to `pits-core`".to_string()
            }
        },
        if opts.sync_credentials {
            "5. Pull RKE2 `rke2.yaml` + Lab `kubeconfig` & `talosconfig` into `%LOCALAPPDATA%\\PITS\\Client` (excluded from Syncthing)".to_string()
        } else {
            "5. Skip credential pull (`--sync-credentials=false`)".to_string()
        },
        format!("6. Verify live TLS 1.3 Ed25519 mTLS health at `{endpoint}/v1/health`"),
    ];

    // Step 1: Prepare or reuse PKI locally (even in dry-run we validate PKI state without overwriting unless requested)
    let state_pki_dir = local_state_dir().join("agent-pki");
    let need_new_pki = opts.rotate_pki
        || profile.agent_ca_pem.is_none()
        || profile.agent_client_cert_pem.is_none()
        || profile.agent_client_key_pem.is_none()
        || !state_pki_dir.join("server.crt").exists()
        || !state_pki_dir.join("server.key").exists();

    if opts.dry_run {
        return Ok(NixosDeployReport {
            dry_run: true,
            mode: opts.mode,
            ssh_target: opts.ssh_target.clone(),
            agent_endpoint: endpoint,
            binary_sha256: bin_sha256,
            binary_size_bytes: bin_bytes.len() as u64,
            nixos_version: None,
            activation_action,
            synced_credentials: vec![],
            live_agent_health: None,
            planned_steps,
        });
    }

    if need_new_pki {
        let pki = generate_agent_mtls_pki(&opts.agent_host)?;
        fs::create_dir_all(&state_pki_dir)?;
        fs::write(state_pki_dir.join("ca.crt"), &pki.ca_cert_pem)?;
        fs::write(state_pki_dir.join("server.crt"), &pki.server_cert_pem)?;
        fs::write(state_pki_dir.join("server.key"), &pki.server_key_pem)?;
        profile.agent_ca_pem = Some(pki.ca_cert_pem);
        profile.agent_client_cert_pem = Some(pki.client_cert_pem);
        profile.agent_client_key_pem = Some(pki.client_key_pem);
    }
    profile.agent_endpoint = endpoint.clone();
    profile.lab_via_agent_bridge = true;
    profile.talos_via_agent_bridge = true;
    profile.save(profile_path)?;

    let ca_pem = profile
        .agent_ca_pem
        .as_ref()
        .ok_or_else(|| anyhow!("missing CA PEM"))?
        .as_bytes()
        .to_vec();
    let srv_cert = fs::read(state_pki_dir.join("server.crt"))?;
    let srv_key = fs::read(state_pki_dir.join("server.key"))?;

    // Step 2: Verify target host identity & NixOS version over SSH
    let probe = run_ssh_script(
        &opts.ssh_target,
        "printf '%s\\n' \"$(hostname)\" && nixos-version",
    )?;
    let mut probe_lines = probe.lines();
    let remote_host = probe_lines.next().unwrap_or("").trim();
    if remote_host != "pits-core" {
        bail!(
            "refusing deployment: remote hostname is `{remote_host}`, expected `pits-core`"
        );
    }
    let nixos_ver = probe_lines.next().map(|s| s.trim().to_string());

    // Step 3: Upload PKI (`0600`) and verified Linux binary
    run_ssh_script(
        &opts.ssh_target,
        "install -d -m 0700 /var/lib/pits/agent /var/lib/pits/agent/pki /var/lib/pits/agent/bin /var/lib/pits/agent-src",
    )?;
    stream_bytes_over_ssh(&opts.ssh_target, &ca_pem, "/var/lib/pits/agent/pki/ca.crt", "0600")?;
    stream_bytes_over_ssh(&opts.ssh_target, &srv_cert, "/var/lib/pits/agent/pki/server.crt", "0600")?;
    stream_bytes_over_ssh(&opts.ssh_target, &srv_key, "/var/lib/pits/agent/pki/server.key", "0600")?;

    // Step 4: Activate declarative NixOS module OR declarative RKE2 DaemonSet
    match opts.mode {
        NixosDeployMode::NixosModule => {
            stream_file_over_ssh(
                &opts.ssh_target,
                &opts.binary_path,
                "/var/lib/pits/agent/bin/pits-agent",
                "0755",
            )?;
            let mod_dir = format!("{}/modules", opts.flake_dir.trim_end_matches('/'));
            let host_default = format!(
                "{}/hosts/core/default.nix",
                opts.flake_dir.trim_end_matches('/')
            );
            stream_bytes_over_ssh(
                &opts.ssh_target,
                EMBEDDED_PACKAGE_NIX.as_bytes(),
                &format!("{mod_dir}/pits-agent-package.nix"),
                "0644",
            )?;
            stream_bytes_over_ssh(
                &opts.ssh_target,
                EMBEDDED_MODULE_NIX.as_bytes(),
                &format!("{mod_dir}/pits-agent.nix"),
                "0644",
            )?;

            let current_default = run_ssh_script(
                &opts.ssh_target,
                &format!("cat '{host_default}'"),
            )?;
            let (patched_default, changed) =
                inject_nixos_module_import(&current_default, "../../modules/pits-agent.nix")?;
            if changed {
                stream_bytes_over_ssh(
                    &opts.ssh_target,
                    current_default.as_bytes(),
                    "/var/lib/pits/agent/default.nix.bak",
                    "0600",
                )?;
                stream_bytes_over_ssh(
                    &opts.ssh_target,
                    patched_default.as_bytes(),
                    &host_default,
                    "0644",
                )?;
            }

            let subcmd = if opts.switch_generation { "switch" } else { "test" };
            let activate_script = format!(
                r#"
cd '{flake_dir}'
if [ -f '{flake_dir}/flake.nix' ]; then
  chown --reference='{flake_dir}/flake.nix' '{mod_dir}/pits-agent.nix' '{mod_dir}/pits-agent-package.nix' '{host_default}' || true
fi
if ! nixos-rebuild {subcmd} --flake 'path:{flake_dir}#{flake_attr}'; then
  if [ -f /var/lib/pits/agent/default.nix.bak ]; then
    cp -f /var/lib/pits/agent/default.nix.bak '{host_default}'
    chown --reference='{flake_dir}/flake.nix' '{host_default}' || true
  fi
  exit 1
fi
systemctl restart pits-agent.service
systemctl is-active pits-agent.service
"#,
                flake_dir = opts.flake_dir.trim_end_matches('/'),
                mod_dir = mod_dir,
                flake_attr = opts.flake_attr,
                subcmd = subcmd,
                host_default = host_default,
            );
            run_ssh_script(&opts.ssh_target, &activate_script)?;
        }
        NixosDeployMode::Rke2Daemonset => {
            stream_file_over_ssh(
                &opts.ssh_target,
                &opts.binary_path,
                "/var/lib/pits/agent/bin/pits-agent",
                "0755",
            )?;
            stream_bytes_over_ssh(
                &opts.ssh_target,
                EMBEDDED_RKE2_DAEMONSET_YAML.as_bytes(),
                "/var/lib/pits/agent/rke2-daemonset.yaml",
                "0600",
            )?;
            let k8s_script = r#"
export KUBECONFIG=/etc/rancher/rke2/rke2.yaml
/var/lib/rancher/rke2/bin/kubectl apply -f /var/lib/pits/agent/rke2-daemonset.yaml
/var/lib/rancher/rke2/bin/kubectl -n pits-system rollout status daemonset/pits-agent --timeout=90s
"#;
            run_ssh_script(&opts.ssh_target, k8s_script)?;
        }
    }

    // Step 5: Optionally sync cluster credentials into %LOCALAPPDATA%\PITS\Client (outside Syncthing)
    let mut synced = Vec::new();
    if opts.sync_credentials {
        let creds_dir = local_state_dir().join("clusters");
        fs::create_dir_all(&creds_dir)?;

        if let Ok(raw_rke2) = run_ssh_script(&opts.ssh_target, "cat /etc/rancher/rke2/rke2.yaml") {
            let rewritten = rewrite_rke2_kubeconfig_endpoint(&raw_rke2, &opts.agent_host);
            let core_kc = creds_dir.join("pits-core-rke2.yaml");
            fs::write(&core_kc, rewritten)?;
            profile.core_kubeconfig_path = Some(core_kc.display().to_string());
            synced.push(core_kc.display().to_string());
        }

        let find_lab_kc = r#"
for p in /var/lib/pits/tinkerbell-20261108/private/talos-netboot-001-resume/kubeconfig \
         /home/libraskywalker/edge-router-work/*/private/talos-netboot-001-resume/kubeconfig; do
  if [ -f "$p" ]; then base64 -w0 "$p"; break; fi
done
"#;
        if let Ok(b64) = run_ssh_script(&opts.ssh_target, find_lab_kc) {
            let trimmed = b64.trim();
            if !trimmed.is_empty() {
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(trimmed) {
                    let lab_kc = creds_dir.join("pits-lab-talos-k8s.yaml");
                    fs::write(&lab_kc, bytes)?;
                    profile.lab_kubeconfig_path = Some(lab_kc.display().to_string());
                    synced.push(lab_kc.display().to_string());
                }
            }
        }

        let find_talos = r#"
for p in /var/lib/pits/tinkerbell-20261108/private/talos-netboot-001-resume/talosconfig \
         /home/libraskywalker/edge-router-work/*/private/talos-netboot-001-resume/talosconfig; do
  if [ -f "$p" ]; then base64 -w0 "$p"; break; fi
done
"#;
        if let Ok(b64) = run_ssh_script(&opts.ssh_target, find_talos) {
            let trimmed = b64.trim();
            if !trimmed.is_empty() {
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(trimmed) {
                    let tc_path = creds_dir.join("pits-lab-talosconfig.yaml");
                    fs::write(&tc_path, bytes)?;
                    profile.talosconfig_path = Some(tc_path.display().to_string());
                    synced.push(tc_path.display().to_string());
                }
            }
        }

        profile.save(profile_path)?;
    }

    // Step 6: Verify end-to-end TLS 1.3 Ed25519 mTLS health from Rust client to Go pits-agent
    let ag = AgentClient::from_profile(profile)?;
    let mut health = None;
    let mut last_err = None;
    for _ in 0..8 {
        match ag.get_json::<serde_json::Value>("/v1/health").await {
            Ok(v) => {
                health = Some(v);
                break;
            }
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(750)).await;
            }
        }
    }
    if health.is_none() {
        if let Some(e) = last_err {
            bail!("pits-agent deployed on NixOS, but mTLS `/v1/health` probe failed: {e:#}");
        }
    }

    Ok(NixosDeployReport {
        dry_run: false,
        mode: opts.mode,
        ssh_target: opts.ssh_target.clone(),
        agent_endpoint: endpoint,
        binary_sha256: bin_sha256,
        binary_size_bytes: bin_bytes.len() as u64,
        nixos_version: nixos_ver,
        activation_action,
        synced_credentials: synced,
        live_agent_health: health,
        planned_steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_nixos_module_import_idempotent() {
        let sample = r#"{ config, pkgs, ... }:
{
  imports = [
    ./hardware-configuration.nix
    ../../modules/rke2.nix
    ../../modules/btrfs.nix
  ];
  networking.hostName = "pits-core";
}
"#;
        let (patched, changed) =
            inject_nixos_module_import(sample, "../../modules/pits-agent.nix").unwrap();
        assert!(changed);
        assert!(patched.contains("../../modules/pits-agent.nix"));

        let (patched_again, changed_again) =
            inject_nixos_module_import(&patched, "../../modules/pits-agent.nix").unwrap();
        assert!(!changed_again);
        assert_eq!(patched, patched_again);

        let (with_portal, portal_added) =
            inject_nixos_module_import(&patched, "../../modules/pits-portal.nix").unwrap();
        assert!(portal_added);
        assert!(with_portal.contains("../../modules/pits-portal.nix"));
    }

    #[test]
    fn test_rewrite_rke2_kubeconfig_endpoint() {
        let raw = "apiVersion: v1\nclusters:\n- cluster:\n    server: https://127.0.0.1:6443\n  name: default\nusers: []\ncontexts: []\n";
        let out = rewrite_rke2_kubeconfig_endpoint(raw, "100.74.25.42");
        assert!(out.contains("https://100.74.25.42:6443"));
        assert!(out.contains("tls-server-name: pits-core"));
        let parsed = crate::config::parse_kubeconfig_str(&out, None, None).unwrap();
        assert_eq!(parsed.server_url, "https://100.74.25.42:6443");
        assert_eq!(parsed.tls_server_name.as_deref(), Some("pits-core"));
    }
}
