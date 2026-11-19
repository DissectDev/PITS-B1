//! Client profile, kubeconfig/talosconfig parsers, Ed25519 mTLS PKI generator,
//! and edge-router workspace contract importer.
//!
//! Runtime credentials and generated `.rdp` files are stored exclusively under
//! `%LOCALAPPDATA%\PITS\Client` (outside Syncthing `commonIgnore` rules).

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;
use x509_cert::builder::{Builder, CertificateBuilder, Profile};
use x509_cert::der::asn1::{BitString, OctetString};
use x509_cert::der::EncodePem;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::SubjectAltName;
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::time::Validity;

use crate::san::{SANDeviceSpec, SANNamespaceSpec};

/// Additional external/virtual Kubernetes cluster registered in operator profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExtraClusterConfig {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    pub kubeconfig_path: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub via_agent_bridge: bool,
}

/// Local operator profile stored in `%LOCALAPPDATA%\PITS\Client\profile.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientProfile {
    pub active_workspace_source: Option<String>,
    pub agent_endpoint: String,
    pub agent_bearer_token: Option<String>,
    pub agent_ca_pem: Option<String>,
    pub agent_client_cert_pem: Option<String>,
    pub agent_client_key_pem: Option<String>,
    pub core_kubeconfig_path: Option<String>,
    pub core_context: Option<String>,
    pub lab_kubeconfig_path: Option<String>,
    pub lab_context: Option<String>,
    pub lab_via_agent_bridge: bool,
    pub talosconfig_path: Option<String>,
    pub talos_context: Option<String>,
    pub talos_via_agent_bridge: bool,
    /// Optional additional Kubernetes / vCluster endpoints discovered alongside core & lab
    #[serde(default)]
    pub extra_clusters: Vec<ExtraClusterConfig>,
    /// Optional PostgreSQL sync URL (`postgres://user:pass@host:5432/db?sslmode=prefer`)
    #[serde(default)]
    pub sourcing_db_url: Option<String>,
}

impl Default for ClientProfile {
    fn default() -> Self {
        #[cfg(windows)]
        let (workspace, agent_ep) = (
            Some(r"c:\Users\pitsadmin\edge-router".to_string()),
            "https://100.95.95.88:18443".to_string(),
        );
        #[cfg(not(windows))]
        let (workspace, agent_ep) = (None, "https://127.0.0.1:18443".to_string());

        Self {
            active_workspace_source: workspace,
            agent_endpoint: agent_ep,
            agent_bearer_token: None,
            agent_ca_pem: None,
            agent_client_cert_pem: None,
            agent_client_key_pem: None,
            core_kubeconfig_path: None,
            core_context: None,
            lab_kubeconfig_path: None,
            lab_context: None,
            lab_via_agent_bridge: true,
            talosconfig_path: None,
            talos_context: None,
            talos_via_agent_bridge: true,
            extra_clusters: Vec::new(),
            sourcing_db_url: None,
        }
    }
}

/// Resolves the active PostgreSQL sync URL from CLI flag -> `PITS_SOURCING_DB_URL` -> profile.
pub fn effective_sourcing_db_url(
    cli_override: Option<&str>,
    profile: &ClientProfile,
) -> Option<String> {
    if let Some(v) = cli_override.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(v.to_string());
    }
    if let Ok(env_v) = std::env::var("PITS_SOURCING_DB_URL") {
        let trimmed = env_v.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    profile
        .sourcing_db_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Returns the machine-local state directory (`%LOCALAPPDATA%\PITS\Client` on Windows).
pub fn local_state_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("PITS_STATE_DIR") {
        if !custom.trim().is_empty() {
            return PathBuf::from(custom);
        }
    }
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        return PathBuf::from(local_app_data).join("PITS").join("Client");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".local").join("share").join("pits-client");
    }
    std::env::temp_dir().join("pits-client")
}

pub fn default_profile_path() -> PathBuf {
    local_state_dir().join("profile.yaml")
}

impl ClientProfile {
    pub fn load_or_default(path: Option<&Path>) -> Result<Self> {
        let target = path
            .map(PathBuf::from)
            .unwrap_or_else(default_profile_path);
        if !target.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&target)
            .with_context(|| format!("read profile {}", target.display()))?;
        let prof: Self = serde_yaml::from_str(&raw)
            .with_context(|| format!("parse YAML profile {}", target.display()))?;
        Ok(prof)
    }

    pub fn save(&self, path: Option<&Path>) -> Result<PathBuf> {
        let target = path
            .map(PathBuf::from)
            .unwrap_or_else(default_profile_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create profile dir {}", parent.display()))?;
        }
        let yaml = serde_yaml::to_string(self)?;
        fs::write(&target, yaml)
            .with_context(|| format!("write profile {}", target.display()))?;
        Ok(target)
    }
}

/// Parsed connection credentials for a single Kubernetes cluster context.
#[derive(Debug, Clone)]
pub struct ResolvedKubeEndpoint {
    pub context_name: String,
    pub server_url: String,
    pub tls_server_name: Option<String>,
    pub ca_pem: Option<Vec<u8>>,
    pub client_cert_pem: Option<Vec<u8>>,
    pub client_key_pem: Option<Vec<u8>>,
    pub bearer_token: Option<String>,
    pub insecure_skip_tls_verify: bool,
}

#[derive(Debug, Deserialize)]
struct RawKubeConfig {
    #[serde(rename = "current-context", default)]
    current_context: String,
    #[serde(default)]
    clusters: Vec<RawNamedCluster>,
    #[serde(default)]
    contexts: Vec<RawNamedContext>,
    #[serde(default)]
    users: Vec<RawNamedUser>,
}

#[derive(Debug, Deserialize)]
struct RawNamedCluster {
    name: String,
    cluster: RawCluster,
}

#[derive(Debug, Deserialize)]
struct RawCluster {
    server: String,
    #[serde(rename = "tls-server-name")]
    tls_server_name: Option<String>,
    #[serde(rename = "certificate-authority-data")]
    ca_data: Option<String>,
    #[serde(rename = "certificate-authority")]
    ca_file: Option<String>,
    #[serde(rename = "insecure-skip-tls-verify", default)]
    insecure_skip_tls_verify: bool,
}

#[derive(Debug, Deserialize)]
struct RawNamedContext {
    name: String,
    context: RawContext,
}

#[derive(Debug, Deserialize)]
struct RawContext {
    cluster: String,
    user: String,
}

#[derive(Debug, Deserialize)]
struct RawNamedUser {
    name: String,
    user: RawUser,
}

#[derive(Debug, Deserialize)]
struct RawUser {
    token: Option<String>,
    #[serde(rename = "tokenFile")]
    token_file: Option<String>,
    #[serde(rename = "client-certificate-data")]
    client_cert_data: Option<String>,
    #[serde(rename = "client-certificate")]
    client_cert_file: Option<String>,
    #[serde(rename = "client-key-data")]
    client_key_data: Option<String>,
    #[serde(rename = "client-key")]
    client_key_file: Option<String>,
}

pub fn parse_kubeconfig_str(
    yaml_text: &str,
    base_dir: Option<&Path>,
    preferred_context: Option<&str>,
) -> Result<ResolvedKubeEndpoint> {
    let raw: RawKubeConfig =
        serde_yaml::from_str(yaml_text).context("invalid kubeconfig YAML")?;
    if raw.clusters.is_empty() {
        bail!("kubeconfig contains zero clusters");
    }
    let ctx_name = preferred_context
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            if !raw.current_context.is_empty() {
                Some(raw.current_context.clone())
            } else {
                raw.contexts.first().map(|c| c.name.clone())
            }
        })
        .unwrap_or_else(|| "default".to_string());

    let (cluster_name, user_name) = if let Some(nc) = raw.contexts.iter().find(|c| c.name == ctx_name)
    {
        (nc.context.cluster.clone(), nc.context.user.clone())
    } else {
        (
            raw.clusters[0].name.clone(),
            raw.users.first().map(|u| u.name.clone()).unwrap_or_default(),
        )
    };

    let cluster = raw
        .clusters
        .iter()
        .find(|c| c.name == cluster_name)
        .ok_or_else(|| anyhow!("cluster {cluster_name:?} not found in kubeconfig"))?;
    let user = raw.users.iter().find(|u| u.name == user_name);

    let ca_pem = decode_inline_or_file(
        cluster.cluster.ca_data.as_deref(),
        cluster.cluster.ca_file.as_deref(),
        base_dir,
    )?;
    let client_cert_pem = match user {
        Some(u) => decode_inline_or_file(
            u.user.client_cert_data.as_deref(),
            u.user.client_cert_file.as_deref(),
            base_dir,
        )?,
        None => None,
    };
    let client_key_pem = match user {
        Some(u) => decode_inline_or_file(
            u.user.client_key_data.as_deref(),
            u.user.client_key_file.as_deref(),
            base_dir,
        )?,
        None => None,
    };
    let bearer_token = match user {
        Some(u) => {
            if let Some(t) = &u.user.token {
                Some(t.trim().to_string())
            } else if let Some(tf) = &u.user.token_file {
                let p = resolve_rel(base_dir, tf);
                Some(fs::read_to_string(p)?.trim().to_string())
            } else {
                None
            }
        }
        None => None,
    };

    Ok(ResolvedKubeEndpoint {
        context_name: ctx_name,
        server_url: cluster.cluster.server.trim_end_matches('/').to_string(),
        tls_server_name: cluster
            .cluster
            .tls_server_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        ca_pem,
        client_cert_pem,
        client_key_pem,
        bearer_token,
        insecure_skip_tls_verify: cluster.cluster.insecure_skip_tls_verify,
    })
}

pub fn load_kubeconfig_file(
    path: &Path,
    preferred_context: Option<&str>,
) -> Result<ResolvedKubeEndpoint> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("read kubeconfig {}", path.display()))?;
    parse_kubeconfig_str(&text, path.parent(), preferred_context)
}

/// Parsed Sidero Talos client configuration (`talosconfig`).
#[derive(Debug, Clone)]
pub struct ResolvedTalosContext {
    pub context_name: String,
    pub endpoints: Vec<String>,
    pub nodes: Vec<String>,
    pub ca_pem: Vec<u8>,
    pub crt_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct RawTalosConfig {
    context: Option<String>,
    contexts: BTreeMap<String, RawTalosContextEntry>,
}

#[derive(Debug, Deserialize)]
struct RawTalosContextEntry {
    #[serde(default)]
    endpoints: Vec<String>,
    #[serde(default)]
    nodes: Vec<String>,
    ca: String,
    crt: String,
    key: String,
}

pub fn parse_talosconfig_str(
    yaml_text: &str,
    preferred_context: Option<&str>,
) -> Result<ResolvedTalosContext> {
    let raw: RawTalosConfig =
        serde_yaml::from_str(yaml_text).context("invalid talosconfig YAML")?;
    if raw.contexts.is_empty() {
        bail!("talosconfig contains zero contexts");
    }
    let ctx_name = preferred_context
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or(raw.context)
        .unwrap_or_else(|| raw.contexts.keys().next().cloned().unwrap());

    let entry = raw
        .contexts
        .get(&ctx_name)
        .ok_or_else(|| anyhow!("talosconfig context {ctx_name:?} not found"))?;

    Ok(ResolvedTalosContext {
        context_name: ctx_name,
        endpoints: entry.endpoints.clone(),
        nodes: entry.nodes.clone(),
        ca_pem: decode_pem_or_b64(&entry.ca)?,
        crt_pem: decode_pem_or_b64(&entry.crt)?,
        key_pem: normalize_talos_ed25519_key_pem(&decode_pem_or_b64(&entry.key)?)?,
    })
}

pub fn load_talosconfig_file(
    path: &Path,
    preferred_context: Option<&str>,
) -> Result<ResolvedTalosContext> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("read talosconfig {}", path.display()))?;
    parse_talosconfig_str(&text, preferred_context)
}

/// Converts Sidero's custom `-----BEGIN ED25519 PRIVATE KEY-----` (48-byte PKCS#8 DER,
/// raw 32-byte seed, or 64-byte keypair) into standard PKCS#8 `-----BEGIN PRIVATE KEY-----` PEM
/// so rustls loads it natively without OpenSSL or C dependencies.
pub fn normalize_talos_ed25519_key_pem(raw_pem: &[u8]) -> Result<Vec<u8>> {
    use ed25519_dalek::pkcs8::DecodePrivateKey;

    let s = std::str::from_utf8(raw_pem).context("private key PEM is not valid UTF-8")?;
    let begin_marker = "-----BEGIN ED25519 PRIVATE KEY-----";
    let end_marker = "-----END ED25519 PRIVATE KEY-----";
    if let (Some(start), Some(end)) = (s.find(begin_marker), s.find(end_marker)) {
        let body: String = s[start + begin_marker.len()..end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(body)
            .context("decode Talos ED25519 PRIVATE KEY base64")?;
        let signing_key = if let Ok(sk) = SigningKey::from_pkcs8_der(&bytes) {
            sk
        } else {
            let seed: [u8; 32] = match bytes.len() {
                32 => bytes.as_slice().try_into().unwrap(),
                64 => bytes[..32].try_into().unwrap(),
                other => bail!(
                    "unexpected Talos ED25519 PRIVATE KEY length {} (expected PKCS#8 DER, 32, or 64 bytes)",
                    other
                ),
            };
            SigningKey::from_bytes(&seed)
        };
        let pkcs8_pem = signing_key
            .to_pkcs8_pem(x509_cert::der::pem::LineEnding::LF)
            .map_err(|e| anyhow!("encode Ed25519 PKCS#8 PEM: {e}"))?;
        return Ok(pkcs8_pem.as_bytes().to_vec());
    }
    Ok(raw_pem.to_vec())
}

/// Generated mTLS Ed25519 PKI bundle for zero-trust `pitsctl` <-> `pits-agent` communication.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedAgentPki {
    pub ca_cert_pem: String,
    pub server_cert_pem: String,
    pub server_key_pem: String,
    pub client_cert_pem: String,
    pub client_key_pem: String,
}

pub fn generate_agent_mtls_pki(server_ip_or_dns: &str) -> Result<GeneratedAgentPki> {
    let mut rng = OsRng;

    // 1. Root CA (Ed25519)
    let ca_key = SigningKey::generate(&mut rng);
    let ca_subject = Name::from_str("CN=PITS Root CA,O=PITS Infrastructure")
        .map_err(|e| anyhow!("CA subject: {e}"))?;
    let ca_builder = CertificateBuilder::new(
        Profile::Root,
        SerialNumber::from(1u64),
        Validity::from_now(Duration::from_secs(86400 * 3650))
            .map_err(|e| anyhow!("CA validity: {e}"))?,
        ca_subject.clone(),
        x509_cert::spki::SubjectPublicKeyInfoOwned::from_key(ca_key.verifying_key())
            .map_err(|e| anyhow!("CA SPKI: {e}"))?,
        &ca_key,
    )
    .map_err(|e| anyhow!("CA builder: {e}"))?;
    let ca_cert = sign_x509_ed25519(ca_builder, &ca_key)?;
    let ca_cert_pem = ca_cert
        .to_pem(x509_cert::der::pem::LineEnding::LF)
        .map_err(|e| anyhow!("CA PEM: {e}"))?;

    // 2. Server Leaf Certificate (with IP or DNS SAN)
    let srv_key = SigningKey::generate(&mut rng);
    let srv_subject = Name::from_str("CN=pits-agent,O=PITS Core")
        .map_err(|e| anyhow!("Server subject: {e}"))?;
    let mut srv_builder = CertificateBuilder::new(
        Profile::Leaf {
            issuer: ca_subject.clone(),
            enable_key_agreement: false,
            enable_key_encipherment: false,
        },
        SerialNumber::from(2u64),
        Validity::from_now(Duration::from_secs(86400 * 825))
            .map_err(|e| anyhow!("Server validity: {e}"))?,
        srv_subject,
        x509_cert::spki::SubjectPublicKeyInfoOwned::from_key(srv_key.verifying_key())
            .map_err(|e| anyhow!("Server SPKI: {e}"))?,
        &ca_key,
    )
    .map_err(|e| anyhow!("Server builder: {e}"))?;

    let mut dns_entries = vec![
        "pits-core".to_string(),
        "pits-core.baiji-company.ts.net".to_string(),
    ];
    let mut ip_entries: Vec<std::net::IpAddr> = vec![
        "100.95.95.88".parse().unwrap(),
        "192.168.71.35".parse().unwrap(),
        "127.0.0.1".parse().unwrap(),
    ];

    for raw_item in server_ip_or_dns.split(',') {
        let item = raw_item.trim();
        if item.is_empty() {
            continue;
        }
        if let Ok(ip) = item.parse::<std::net::IpAddr>() {
            if !ip_entries.contains(&ip) {
                ip_entries.push(ip);
            }
        } else if !dns_entries.iter().any(|d| d.eq_ignore_ascii_case(item)) {
            dns_entries.push(item.to_string());
        }
    }

    let mut sans = Vec::new();
    for d in dns_entries {
        sans.push(GeneralName::DnsName(
            d.try_into().map_err(|e| anyhow!("SAN DNS: {e}"))?,
        ));
    }
    for ip in ip_entries {
        let octets = match ip {
            std::net::IpAddr::V4(v4) => v4.octets().to_vec(),
            std::net::IpAddr::V6(v6) => v6.octets().to_vec(),
        };
        sans.push(GeneralName::IpAddress(
            OctetString::new(octets).map_err(|e| anyhow!("SAN IP: {e}"))?,
        ));
    }
    srv_builder
        .add_extension(&SubjectAltName(sans))
        .map_err(|e| anyhow!("add Server SAN: {e}"))?;
    let srv_cert = sign_x509_ed25519(srv_builder, &ca_key)?;
    let server_cert_pem = srv_cert
        .to_pem(x509_cert::der::pem::LineEnding::LF)
        .map_err(|e| anyhow!("Server PEM: {e}"))?;
    let server_key_pem = srv_key
        .to_pkcs8_pem(x509_cert::der::pem::LineEnding::LF)
        .map_err(|e| anyhow!("Server key PEM: {e}"))?
        .to_string();

    // 3. Client Leaf Certificate (pitsctl operator)
    let cli_key = SigningKey::generate(&mut rng);
    let cli_subject = Name::from_str("CN=pitsctl-operator,O=PITS Operators")
        .map_err(|e| anyhow!("Client subject: {e}"))?;
    let cli_builder = CertificateBuilder::new(
        Profile::Leaf {
            issuer: ca_subject,
            enable_key_agreement: false,
            enable_key_encipherment: false,
        },
        SerialNumber::from(3u64),
        Validity::from_now(Duration::from_secs(86400 * 825))
            .map_err(|e| anyhow!("Client validity: {e}"))?,
        cli_subject,
        x509_cert::spki::SubjectPublicKeyInfoOwned::from_key(cli_key.verifying_key())
            .map_err(|e| anyhow!("Client SPKI: {e}"))?,
        &ca_key,
    )
    .map_err(|e| anyhow!("Client builder: {e}"))?;
    let cli_cert = sign_x509_ed25519(cli_builder, &ca_key)?;
    let client_cert_pem = cli_cert
        .to_pem(x509_cert::der::pem::LineEnding::LF)
        .map_err(|e| anyhow!("Client PEM: {e}"))?;
    let client_key_pem = cli_key
        .to_pkcs8_pem(x509_cert::der::pem::LineEnding::LF)
        .map_err(|e| anyhow!("Client key PEM: {e}"))?
        .to_string();

    Ok(GeneratedAgentPki {
        ca_cert_pem,
        server_cert_pem,
        server_key_pem,
        client_cert_pem,
        client_key_pem,
    })
}

fn sign_x509_ed25519(
    mut builder: CertificateBuilder<'_, SigningKey>,
    ca_key: &SigningKey,
) -> Result<x509_cert::Certificate> {
    let tbs = builder
        .finalize()
        .map_err(|e| anyhow!("finalize X.509 TBS: {e}"))?;
    let sig = ca_key.sign(&tbs);
    let bit_sig = BitString::from_bytes(&sig.to_bytes())
        .map_err(|e| anyhow!("encode Ed25519 BIT STRING: {e}"))?;
    builder
        .assemble(bit_sig)
        .map_err(|e| anyhow!("assemble X.509 certificate: {e}"))
}

/// Optional non-NPU discrete accelerator (e.g. NVIDIA GeForce RTX 5090 GPU via DRA v1) in a node contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeAcceleratorContract {
    pub model: String,
    #[serde(default)]
    pub architecture: String,
    pub pci_address: String,
    pub pci_id: String,
    pub kernel_driver: String,
    pub dra_device_class: String,
    #[serde(default)]
    pub dra_driver: Option<String>,
    #[serde(default)]
    pub dra_validated: bool,
    #[serde(default)]
    pub vram_mib: Option<u64>,
}

/// Enrolled PITS node contract (compiled baseline + live `edge-router` workspace import).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrolledNodeContract {
    pub device_id: String,
    pub hostname: String,
    pub node_ip: String,
    pub target_ip: String,
    pub pxe_mac: String,
    pub product_uuid: String,
    pub model: String,
    pub cpu_vendor: String,
    pub cpu_model: String,
    pub has_intel_npu: bool,
    pub npu_vendor: Option<String>,
    pub npu_pci_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accelerator: Option<NodeAcceleratorContract>,
    pub approved_vfio_pci: Vec<String>,
    pub san: SANDeviceSpec,
}

/// Imported snapshot of contracts and manifests from an `edge-router` repository or built-in PITS defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceBundle {
    pub source_dir: Option<String>,
    pub imported_files: Vec<String>,
    pub edge_router_yaml: Option<String>,
    pub nodes: Vec<EnrolledNodeContract>,
}

impl WorkspaceBundle {
    /// Built-in verified PITS infrastructure baseline (`netboot-001..004` + `pits-core`).
    pub fn builtin_pits_baseline() -> Self {
        let nodes = vec![
            make_baseline_node(
                "netboot-001",
                "10.89.20.21",
                "10.89.20.11",
                4420,
                "88:ae:dd:64:ac:db",
                "57669fee-8ef2-4b68-6cc3-88aedd64acdb",
                "ASUS NUC 14 Pro (NUC14RVH-B)",
                "intel",
                "Intel(R) Core(TM) Ultra 7 155H",
                Some(("intel", "8086:7d1d")),
                vec!["0000:01:00.0".to_string()],
                false,
            ),
            make_baseline_node(
                "netboot-002",
                "10.89.20.22",
                "10.89.20.15",
                4421,
                "88:ae:dd:64:8f:b3",
                "cf1f1a22-087d-7374-fcc8-88aedd648fb3",
                "ASUS NUC 14 Pro (NUC14RVH-B)",
                "intel",
                "Intel(R) Core(TM) Ultra 7 155H",
                Some(("intel", "8086:7d1d")),
                vec!["0000:01:00.0".to_string()],
                false,
            ),
            make_baseline_node(
                "netboot-003",
                "10.89.20.23",
                "10.89.20.16",
                4422,
                "58:47:ca:74:3b:22",
                "ed059400-7d9c-11ee-a72c-de277c485e00",
                "Minisforum UM780 XTX (Venus series / F7BSD)",
                "amd",
                "AMD Ryzen 7 7840HS w/ Radeon 780M Graphics",
                Some(("amd", "1022:1502")),
                vec!["0000:01:00.0".to_string()],
                false,
            ),
            make_baseline_node(
                "netboot-004",
                "10.89.20.24",
                "10.89.20.17",
                4423,
                "58:47:ca:75:f6:9a",
                "c39be600-2d37-11ef-9418-3174c88d7400",
                "Minisforum AtomMan X7 Ti (AtomMan X Series / KTBAC)",
                "intel",
                "Intel(R) Core(TM) Ultra 9 185H",
                Some(("intel", "8086:7d1d")),
                vec!["0000:01:00.0".to_string()],
                true,
            ),
        ];
        Self {
            source_dir: None,
            imported_files: vec![],
            edge_router_yaml: None,
            nodes,
        }
    }

    /// Imports live contracts (`*.contract.json`, `*-san.json`, `examples/platform/pits-core.yaml`)
    /// from an `edge-router` directory when readable, overlaying onto the verified PITS baseline.
    pub fn load_from_edge_router(repo_dir: &Path) -> Result<Self> {
        let mut bundle = Self::builtin_pits_baseline();
        if !repo_dir.exists() {
            return Ok(bundle);
        }
        bundle.source_dir = Some(repo_dir.display().to_string());

        let router_path = repo_dir.join("examples/platform/pits-core.yaml");
        if let Ok(yaml) = fs::read_to_string(&router_path) {
            bundle.edge_router_yaml = Some(yaml);
            bundle
                .imported_files
                .push("examples/platform/pits-core.yaml".to_string());
        }

        let devices_dir = repo_dir.join("deploy/tinkerbell/devices");
        for node in &mut bundle.nodes {
            let contract_path = devices_dir.join(format!("{}.contract.json", node.device_id));
            if let Ok(raw) = fs::read_to_string(&contract_path) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
                    apply_contract_overlay(node, &val);
                    bundle.imported_files.push(format!(
                        "deploy/tinkerbell/devices/{}.contract.json",
                        node.device_id
                    ));
                }
            }

            let san_path = devices_dir.join(format!("{}-san.json", node.device_id));
            if let Ok(raw) = fs::read_to_string(&san_path) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
                    apply_san_overlay(node, &val);
                    bundle.imported_files.push(format!(
                        "deploy/tinkerbell/devices/{}-san.json",
                        node.device_id
                    ));
                }
            }
        }

        // Dynamically discover any additional `*.contract.json` nodes (e.g. `netboot-005`)
        if let Ok(entries) = fs::read_dir(&devices_dir) {
            let mut contract_paths: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.ends_with(".contract.json"))
                        .unwrap_or(false)
                })
                .collect();
            contract_paths.sort();

            for contract_path in contract_paths {
                let Ok(raw) = fs::read_to_string(&contract_path) else {
                    continue;
                };
                let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
                    continue;
                };
                let stem_id = contract_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_suffix(".contract.json"))
                    .unwrap_or("")
                    .to_string();
                let dev_id = val
                    .get("deviceID")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&stem_id)
                    .to_string();
                if dev_id.is_empty()
                    || bundle
                        .nodes
                        .iter()
                        .any(|n| n.device_id.eq_ignore_ascii_case(&dev_id))
                {
                    continue;
                }

                let next_port = 4420 + (bundle.nodes.len() as i32);
                let mut node = make_baseline_node(
                    &dev_id,
                    "10.89.20.25",
                    "10.89.20.18",
                    next_port,
                    "00:00:00:00:00:00",
                    "",
                    "Enrolled Baremetal Server",
                    "intel",
                    "x86_64 Server Processor",
                    None,
                    Vec::new(),
                    false,
                );
                apply_contract_overlay(&mut node, &val);
                bundle.imported_files.push(format!(
                    "deploy/tinkerbell/devices/{dev_id}.contract.json"
                ));

                let san_path = devices_dir.join(format!("{dev_id}-san.json"));
                if let Ok(san_raw) = fs::read_to_string(&san_path) {
                    if let Ok(san_val) = serde_json::from_str::<serde_json::Value>(&san_raw) {
                        apply_san_overlay(&mut node, &san_val);
                        bundle.imported_files.push(format!(
                            "deploy/tinkerbell/devices/{dev_id}-san.json"
                        ));
                    }
                }
                bundle.nodes.push(node);
            }
        }

        Ok(bundle)
    }

    pub fn find_node(&self, device_id: &str) -> Option<&EnrolledNodeContract> {
        self.nodes
            .iter()
            .find(|n| n.device_id == device_id || n.hostname == device_id)
    }
}

#[allow(clippy::too_many_arguments)]
fn make_baseline_node(
    device_id: &str,
    node_ip: &str,
    target_ip: &str,
    configfs_port: i32,
    pxe_mac: &str,
    product_uuid: &str,
    model: &str,
    cpu_vendor: &str,
    cpu_model: &str,
    npu: Option<(&str, &str)>,
    approved_vfio_pci: Vec<String>,
    with_vmstore: bool,
) -> EnrolledNodeContract {
    let mut namespaces = vec![
        SANNamespaceSpec {
            id: 1,
            name: "system".to_string(),
            bytes: 70_866_960_384, // 66 GiB validated PITS system extent
            init_system_gpt: true,
        },
        SANNamespaceSpec {
            id: 2,
            name: "userdata".to_string(),
            bytes: 20_000_002_048, // 4KiB-aligned 20 GB userdata extent
            init_system_gpt: false,
        },
    ];
    if with_vmstore {
        namespaces.push(SANNamespaceSpec {
            id: 3,
            name: "vmstore".to_string(),
            bytes: 171_798_691_840, // 160 GiB Windows 11 SAN backing store
            init_system_gpt: false,
        });
    }

    let has_intel_npu = matches!(npu, Some(("intel", _)));

    EnrolledNodeContract {
        device_id: device_id.to_string(),
        hostname: device_id.to_string(),
        node_ip: node_ip.to_string(),
        target_ip: target_ip.to_string(),
        pxe_mac: pxe_mac.to_string(),
        product_uuid: product_uuid.to_string(),
        model: model.to_string(),
        cpu_vendor: cpu_vendor.to_string(),
        cpu_model: cpu_model.to_string(),
        has_intel_npu,
        npu_vendor: npu.map(|(v, _)| v.to_string()),
        npu_pci_id: npu.map(|(_, id)| id.to_string()),
        accelerator: None,
        approved_vfio_pci,
        san: SANDeviceSpec {
            device_id: device_id.to_string(),
            pxe_mac: pxe_mac.to_string(),
            product_uuid: product_uuid.to_string(),
            subsystem_nqn: format!("nqn.2026-11.arpa.home.pits:{device_id}"),
            host_nqn: format!("nqn.2026-11.arpa.home.pits:host-{device_id}"),
            address: target_ip.to_string(),
            port: 4420,
            configfs_port,
            vm_store: with_vmstore,
            namespaces,
        },
    }
}

fn apply_contract_overlay(node: &mut EnrolledNodeContract, val: &serde_json::Value) {
    if let Some(mac) = val
        .pointer("/pxeMACs/0")
        .or_else(|| val.pointer("/network/pxeMac"))
        .or_else(|| val.get("pxeMac"))
        .and_then(|v| v.as_str())
    {
        node.pxe_mac = mac.to_lowercase();
        node.san.pxe_mac = mac.to_lowercase();
    }
    if let Some(uuid) = val
        .get("productUUID")
        .or_else(|| val.pointer("/identity/productUUID"))
        .and_then(|v| v.as_str())
    {
        node.product_uuid = uuid.to_lowercase();
        node.san.product_uuid = uuid.to_lowercase();
    }
    if let Some(ip) = val
        .pointer("/network/address")
        .or_else(|| val.pointer("/network/nodeIP"))
        .or_else(|| val.get("nodeIP"))
        .and_then(|v| v.as_str())
    {
        node.node_ip = ip.to_string();
    }
    if let Some(tip) = val
        .pointer("/san/targetAddress")
        .or_else(|| val.pointer("/san/address"))
        .or_else(|| val.get("sanAddress"))
        .and_then(|v| v.as_str())
    {
        node.target_ip = tip.to_string();
        node.san.address = tip.to_string();
    }
    if let Some(sub_nqn) = val.pointer("/san/subsystemNQN").and_then(|v| v.as_str()) {
        node.san.subsystem_nqn = sub_nqn.to_string();
    }
    if let Some(host_nqn) = val.pointer("/san/hostNQN").and_then(|v| v.as_str()) {
        node.san.host_nqn = host_nqn.to_string();
    }
    if let Some(sys_bytes) = val
        .pointer("/san/systemVolumeBytes")
        .and_then(|v| v.as_i64())
    {
        if let Some(ns) = node.san.namespaces.iter_mut().find(|n| n.name == "system") {
            ns.bytes = sys_bytes;
        }
    }
    if let Some(user_bytes) = val
        .pointer("/san/userBackingAlignedBytes")
        .or_else(|| val.pointer("/san/userVolumeBytes"))
        .and_then(|v| v.as_i64())
    {
        if let Some(ns) = node.san.namespaces.iter_mut().find(|n| n.name == "userdata") {
            ns.bytes = user_bytes;
        }
    }
    if let Some(vm_bytes) = val
        .pointer("/san/vmStoreVolumeBytes")
        .and_then(|v| v.as_i64())
    {
        if let Some(ns) = node.san.namespaces.iter_mut().find(|n| n.name == "vmstore") {
            ns.bytes = vm_bytes;
        }
    }
    if let Some(pcis) = val
        .pointer("/localStorage/pciAddresses")
        .and_then(|v| v.as_array())
    {
        let list: Vec<String> = pcis
            .iter()
            .filter_map(|p| p.as_str().map(str::to_string))
            .collect();
        if !list.is_empty() {
            node.approved_vfio_pci = list;
        }
    }
    if let Some(exts) = val.pointer("/talos/extensions").and_then(|v| v.as_array()) {
        let ext_strs: Vec<&str> = exts.iter().filter_map(|e| e.as_str()).collect();
        if ext_strs.iter().any(|e| e.contains("amd-ucode")) {
            node.cpu_vendor = "amd".to_string();
        } else if ext_strs.iter().any(|e| e.contains("intel-ucode")) {
            node.cpu_vendor = "intel".to_string();
        }
    }
    if let Some(intel_npu) = val.pointer("/accelerators/intelNPU") {
        node.has_intel_npu = true;
        node.npu_vendor = Some("intel".to_string());
        if let Some(pid) = intel_npu.get("pciID").and_then(|v| v.as_str()) {
            node.npu_pci_id = Some(pid.to_lowercase());
        }
    }
    if let Some(acc) = val.get("accelerator").and_then(|v| v.as_object()) {
        let model = acc
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("Discrete Accelerator")
            .to_string();
        let arch = acc
            .get("architecture")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let pci_addr = acc
            .get("pciAddress")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let pci_id = acc
            .get("pciID")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let kernel_driver = acc
            .get("kernelDriver")
            .and_then(|v| v.as_str())
            .unwrap_or("nvidia")
            .to_string();
        let dra_dc = acc
            .get("draDeviceClass")
            .and_then(|v| v.as_str())
            .unwrap_or("gpu.nvidia.com")
            .to_string();
        let dra_driver = acc
            .get("draDriver")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let dra_validated = acc
            .get("draValidated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let vram_mib = acc.get("vramMiB").and_then(|v| v.as_u64());
        node.npu_vendor = Some(kernel_driver.clone());
        if !pci_id.is_empty() {
            node.npu_pci_id = Some(pci_id.clone());
        }
        node.accelerator = Some(NodeAcceleratorContract {
            model,
            architecture: arch,
            pci_address: pci_addr,
            pci_id,
            kernel_driver,
            dra_device_class: dra_dc,
            dra_driver,
            dra_validated,
            vram_mib,
        });
    }
}

fn apply_san_overlay(node: &mut EnrolledNodeContract, val: &serde_json::Value) {
    if let Some(addr) = val.get("address").and_then(|v| v.as_str()) {
        node.target_ip = addr.to_string();
        node.san.address = addr.to_string();
    }
    if let Some(port) = val.get("port").and_then(|v| v.as_i64()) {
        node.san.port = port as i32;
    }
    if let Some(cf_port) = val.get("configfsPort").and_then(|v| v.as_i64()) {
        node.san.configfs_port = cf_port as i32;
    }
    if let Some(sub) = val.get("subsystemNQN").and_then(|v| v.as_str()) {
        node.san.subsystem_nqn = sub.to_string();
    }
    if let Some(host) = val.get("hostNQN").and_then(|v| v.as_str()) {
        node.san.host_nqn = host.to_string();
    }
    if let Some(arr) = val.get("namespaces").and_then(|v| v.as_array()) {
        let mut parsed_ns = Vec::new();
        for item in arr {
            let id = item
                .get("id")
                .or_else(|| item.get("nsid"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32;
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let bytes = item
                .get("bytes")
                .or_else(|| item.get("sizeBytes"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            if id > 0 && !name.is_empty() && bytes > 0 {
                parsed_ns.push(SANNamespaceSpec {
                    id,
                    name: name.clone(),
                    bytes,
                    init_system_gpt: id == 1 && name == "system",
                });
            }
        }
        if parsed_ns.len() >= 2 {
            node.san.vm_store = parsed_ns.iter().any(|n| n.name == "vmstore");
            node.san.namespaces = parsed_ns;
        }
    }
}

fn decode_inline_or_file(
    inline_b64: Option<&str>,
    file_path: Option<&str>,
    base_dir: Option<&Path>,
) -> Result<Option<Vec<u8>>> {
    if let Some(b64) = inline_b64 {
        let trimmed = b64.trim();
        if !trimmed.is_empty() {
            return Ok(Some(decode_pem_or_b64(trimmed)?));
        }
    }
    if let Some(fp) = file_path {
        let p = resolve_rel(base_dir, fp);
        return Ok(Some(
            fs::read(&p).with_context(|| format!("read cert/key {}", p.display()))?,
        ));
    }
    Ok(None)
}

fn decode_pem_or_b64(input: &str) -> Result<Vec<u8>> {
    let trimmed = input.trim();
    if trimmed.starts_with("-----BEGIN ") {
        return Ok(trimmed.as_bytes().to_vec());
    }
    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(compact)
        .context("base64 decode PEM block")
}

fn resolve_rel(base_dir: Option<&Path>, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else if let Some(base) = base_dir {
        base.join(p)
    } else {
        p.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ed25519_mtls_pki_and_talos_key_normalization() {
        let pki = generate_agent_mtls_pki("192.168.71.50").expect("generate Ed25519 PKI");
        assert!(pki.ca_cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(pki.server_cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(pki.client_key_pem.contains("BEGIN PRIVATE KEY"));

        // Verify custom Sidero Talos 32-byte raw ED25519 PRIVATE KEY conversion.
        let seed = [7u8; 32];
        let b64 = base64::engine::general_purpose::STANDARD.encode(seed);
        let talos_raw = format!(
            "-----BEGIN ED25519 PRIVATE KEY-----\n{b64}\n-----END ED25519 PRIVATE KEY-----\n"
        );
        let pkcs8 = normalize_talos_ed25519_key_pem(talos_raw.as_bytes()).unwrap();
        let pkcs8_str = String::from_utf8(pkcs8).unwrap();
        assert!(pkcs8_str.contains("-----BEGIN PRIVATE KEY-----"));

        // Verify live Sidero Talos v1.14 (Go x509.MarshalPKCS8PrivateKey) 48-byte PKCS#8 v1 DER
        // wrapped in BEGIN ED25519 PRIVATE KEY.
        let mut go_pkcs8_v1 = vec![
            0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06,
            0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
        ];
        go_pkcs8_v1.extend_from_slice(&seed);
        assert_eq!(go_pkcs8_v1.len(), 48);
        let b64_48 = base64::engine::general_purpose::STANDARD.encode(&go_pkcs8_v1);
        let talos_48 = format!(
            "-----BEGIN ED25519 PRIVATE KEY-----\n{b64_48}\n-----END ED25519 PRIVATE KEY-----\n"
        );
        let pkcs8_48 = normalize_talos_ed25519_key_pem(talos_48.as_bytes()).unwrap();
        assert_eq!(String::from_utf8(pkcs8_48).unwrap(), pkcs8_str);
    }
}
