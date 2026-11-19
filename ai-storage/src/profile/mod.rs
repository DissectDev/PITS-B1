//! Layer 4: Declarative Project Profile (`ProjectProfile` / `.ai-store.yaml`)
//!
//! Defines physical OpenDAL backends, the 8 AI Storage Archetype bindings,
//! per-scheme Policy Guard rules, and MCP transport exposure settings.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_PROFILE_API_VERSION: &str = "storage.ai.pits.io/v1alpha1";
pub const DEFAULT_PROFILE_KIND: &str = "ProjectProfile";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectProfile {
    #[serde(default = "default_api_version")]
    pub api_version: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub metadata: ProfileMetadata,
    pub spec: ProfileSpec,
}

fn default_api_version() -> String {
    DEFAULT_PROFILE_API_VERSION.to_string()
}

fn default_kind() -> String {
    DEFAULT_PROFILE_KIND.to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileMetadata {
    #[serde(default = "default_profile_name")]
    pub name: String,
    #[serde(default = "default_namespace")]
    pub namespace: String,
}

fn default_profile_name() -> String {
    "local-ai-space".to_string()
}

fn default_namespace() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSpec {
    #[serde(default)]
    pub backends: BTreeMap<String, BackendConfig>,
    #[serde(default)]
    pub archetypes: ArchetypesConfig,
    #[serde(default)]
    pub policies: BTreeMap<String, PolicyRule>,
    #[serde(default)]
    pub exposure: ExposureConfig,
    #[serde(default)]
    pub layers: LayerTuningConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendConfig {
    /// OpenDAL scheme: `fs`, `memory`, `s3`, `http`, `hf` / `huggingface`
    pub scheme: String,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub bucket: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub repo_type: Option<String>,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub access_key_id: Option<String>,
    #[serde(default)]
    pub secret_access_key: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub credential_ref: Option<String>,
    #[serde(default)]
    pub enable_virtual_host_style: Option<bool>,
    #[serde(default)]
    pub extra: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchetypesConfig {
    #[serde(default)]
    pub workspace: Option<WorkspaceArchetypeConfig>,
    #[serde(default)]
    pub memory: Option<MemoryArchetypeConfig>,
    #[serde(default)]
    pub knowledge: Option<KnowledgeArchetypeConfig>,
    #[serde(default)]
    pub datasets: Option<DatasetsArchetypeConfig>,
    #[serde(default)]
    pub models: Option<DatasetsArchetypeConfig>,
    #[serde(default)]
    pub artifacts: Option<ArtifactsArchetypeConfig>,
    #[serde(default)]
    pub traces: Option<TracesArchetypeConfig>,
    #[serde(default)]
    pub state: Option<StateArchetypeConfig>,
    #[serde(default)]
    pub shared_drive: Option<SharedDriveArchetypeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceArchetypeConfig {
    #[serde(default = "default_ws_uri")]
    pub uri: String,
    pub backend: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub index: Option<String>,
    #[serde(default)]
    pub archive_backend: Option<String>,
    #[serde(default)]
    pub archive_prefix: Option<String>,
}

fn default_ws_uri() -> String {
    "ws://".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryArchetypeConfig {
    #[serde(default = "default_mem_uri")]
    pub uri: String,
    pub backend: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub index: Option<String>,
    #[serde(default = "default_ttl_days")]
    pub ttl_days: u64,
}

fn default_mem_uri() -> String {
    "mem://".to_string()
}

fn default_ttl_days() -> u64 {
    90
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeArchetypeConfig {
    #[serde(default = "default_kb_uri")]
    pub uri: String,
    pub backend: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub index: Option<String>,
    #[serde(default = "default_true")]
    pub enable_trigram: bool,
    #[serde(default = "default_true")]
    pub enable_vector: bool,
}

fn default_kb_uri() -> String {
    "kb://".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatasetsArchetypeConfig {
    #[serde(default = "default_data_uri")]
    pub uri: String,
    pub backend: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub cache_dir: Option<String>,
    #[serde(default = "default_cache_max_bytes")]
    pub cache_max_bytes: u64,
}

fn default_data_uri() -> String {
    "data://".to_string()
}

fn default_cache_max_bytes() -> u64 {
    10 * 1024 * 1024 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactsArchetypeConfig {
    #[serde(default = "default_artifacts_uri")]
    pub uri: String,
    pub backend: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub index: Option<String>,
    #[serde(default = "default_true")]
    pub content_addressed: bool,
}

fn default_artifacts_uri() -> String {
    "artifacts://".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TracesArchetypeConfig {
    #[serde(default = "default_traces_uri")]
    pub uri: String,
    pub backend: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default = "default_flush_interval_sec")]
    pub flush_interval_sec: u64,
    #[serde(default = "default_traces_format")]
    pub format: String,
}

fn default_traces_uri() -> String {
    "traces://".to_string()
}

fn default_flush_interval_sec() -> u64 {
    15
}

fn default_traces_format() -> String {
    "jsonl".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateArchetypeConfig {
    #[serde(default = "default_state_uri")]
    pub uri: String,
    pub backend: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub coordinator: Option<String>,
}

fn default_state_uri() -> String {
    "state://".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedDriveArchetypeConfig {
    #[serde(default = "default_shared_uri")]
    pub uri: String,
    pub backend: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub lock_backend: Option<String>,
}

fn default_shared_uri() -> String {
    "shared://".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccessMode {
    ReadOnly,
    AppendOnly,
    ReadWrite,
}

impl Default for AccessMode {
    fn default() -> Self {
        Self::ReadWrite
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRule {
    #[serde(default)]
    pub mode: AccessMode,
    #[serde(default, alias = "maxFileSizeMB")]
    pub max_file_size_mb: Option<u64>,
    #[serde(default)]
    pub allowed_extensions: Vec<String>,
    #[serde(default)]
    pub require_scope: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExposureConfig {
    #[serde(default = "default_transport")]
    pub transport: String,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_mcp_path")]
    pub path: String,
    #[serde(default = "default_tools_preset")]
    pub tools_preset: String,
    #[serde(default)]
    pub bearer_token_ref: Option<String>,
}

impl Default for ExposureConfig {
    fn default() -> Self {
        Self {
            transport: default_transport(),
            host: default_host(),
            port: default_port(),
            path: default_mcp_path(),
            tools_preset: default_tools_preset(),
            bearer_token_ref: None,
        }
    }
}

fn default_transport() -> String {
    "streamable-http".to_string()
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8787
}

fn default_mcp_path() -> String {
    "/mcp".to_string()
}

fn default_tools_preset() -> String {
    "full".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerTuningConfig {
    #[serde(default = "default_retry_max_times")]
    pub retry_max_times: usize,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_io_timeout_secs")]
    pub io_timeout_secs: u64,
    #[serde(default = "default_concurrent_limit")]
    pub concurrent_limit: usize,
    #[serde(default = "default_true")]
    pub enable_logging: bool,
    #[serde(default = "default_true")]
    pub enable_metrics: bool,
}

impl Default for LayerTuningConfig {
    fn default() -> Self {
        Self {
            retry_max_times: default_retry_max_times(),
            timeout_secs: default_timeout_secs(),
            io_timeout_secs: default_io_timeout_secs(),
            concurrent_limit: default_concurrent_limit(),
            enable_logging: true,
            enable_metrics: true,
        }
    }
}

fn default_retry_max_times() -> usize {
    4
}
fn default_timeout_secs() -> u64 {
    30
}
fn default_io_timeout_secs() -> u64 {
    15
}
fn default_concurrent_limit() -> usize {
    64
}

impl ProjectProfile {
    pub fn load_or_default(path: Option<&Path>) -> Result<(Self, PathBuf, String)> {
        let resolved = match path {
            Some(p) => p.to_path_buf(),
            None => discover_profile_path(),
        };

        if resolved.exists() {
            let raw = fs::read_to_string(&resolved)
                .with_context(|| format!("failed to read ProjectProfile at {}", resolved.display()))?;
            let profile: ProjectProfile = serde_yaml::from_str(&raw)
                .with_context(|| format!("failed to parse YAML ProjectProfile at {}", resolved.display()))?;
            profile.validate()?;
            let digest = sha256_hex(raw.as_bytes());
            Ok((profile, resolved, digest))
        } else {
            let root = resolved
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(default_state_root);
            let profile = Self::standalone_default(&root);
            profile.validate()?;
            let serialized = serde_yaml::to_string(&profile)?;
            let digest = sha256_hex(serialized.as_bytes());
            Ok((profile, resolved, digest))
        }
    }

    pub fn standalone_default(base_dir: &Path) -> Self {
        let store_root = base_dir.join(".ai-store");
        let fs_root = store_root.join("objects");
        let sqlite_file = store_root.join("ai-store-meta.sqlite3");
        let cache_dir = store_root.join("nvme-cache");

        let mut backends = BTreeMap::new();
        backends.insert(
            "local_fs".to_string(),
            BackendConfig {
                scheme: "fs".to_string(),
                root: Some(fs_root.to_string_lossy().replace('\\', "/")),
                endpoint: None,
                bucket: None,
                region: None,
                repo: None,
                repo_type: None,
                revision: None,
                access_key_id: None,
                secret_access_key: None,
                token: None,
                credential_ref: None,
                enable_virtual_host_style: None,
                extra: BTreeMap::new(),
            },
        );
        backends.insert(
            "mem_scratch".to_string(),
            BackendConfig {
                scheme: "memory".to_string(),
                root: Some("/".to_string()),
                endpoint: None,
                bucket: None,
                region: None,
                repo: None,
                repo_type: None,
                revision: None,
                access_key_id: None,
                secret_access_key: None,
                token: None,
                credential_ref: None,
                enable_virtual_host_style: None,
                extra: BTreeMap::new(),
            },
        );

        let sqlite_dsn = format!("sqlite://{}", sqlite_file.to_string_lossy().replace('\\', "/"));

        let mut policies = BTreeMap::new();
        policies.insert(
            "kb://".to_string(),
            PolicyRule {
                mode: AccessMode::ReadOnly,
                max_file_size_mb: Some(128),
                allowed_extensions: vec![],
                require_scope: None,
            },
        );
        policies.insert(
            "data://".to_string(),
            PolicyRule {
                mode: AccessMode::ReadOnly,
                max_file_size_mb: Some(4096),
                allowed_extensions: vec![],
                require_scope: None,
            },
        );
        policies.insert(
            "models://".to_string(),
            PolicyRule {
                mode: AccessMode::ReadOnly,
                max_file_size_mb: Some(16384),
                allowed_extensions: vec![],
                require_scope: None,
            },
        );
        policies.insert(
            "artifacts://".to_string(),
            PolicyRule {
                mode: AccessMode::AppendOnly,
                max_file_size_mb: Some(512),
                allowed_extensions: vec![],
                require_scope: None,
            },
        );
        policies.insert(
            "traces://".to_string(),
            PolicyRule {
                mode: AccessMode::AppendOnly,
                max_file_size_mb: Some(256),
                allowed_extensions: vec![],
                require_scope: None,
            },
        );
        policies.insert(
            "ws://".to_string(),
            PolicyRule {
                mode: AccessMode::ReadWrite,
                max_file_size_mb: Some(64),
                allowed_extensions: vec![],
                require_scope: None,
            },
        );
        policies.insert(
            "mem://".to_string(),
            PolicyRule {
                mode: AccessMode::ReadWrite,
                max_file_size_mb: Some(16),
                allowed_extensions: vec![],
                require_scope: Some(true),
            },
        );
        policies.insert(
            "state://".to_string(),
            PolicyRule {
                mode: AccessMode::ReadWrite,
                max_file_size_mb: Some(32),
                allowed_extensions: vec![],
                require_scope: None,
            },
        );
        policies.insert(
            "shared://".to_string(),
            PolicyRule {
                mode: AccessMode::ReadWrite,
                max_file_size_mb: Some(1024),
                allowed_extensions: vec![],
                require_scope: None,
            },
        );

        ProjectProfile {
            api_version: default_api_version(),
            kind: default_kind(),
            metadata: ProfileMetadata::default(),
            spec: ProfileSpec {
                backends,
                archetypes: ArchetypesConfig {
                    workspace: Some(WorkspaceArchetypeConfig {
                        uri: "ws://".to_string(),
                        backend: "local_fs".to_string(),
                        prefix: Some("workspace".to_string()),
                        index: Some(sqlite_dsn.clone()),
                        archive_backend: Some("local_fs".to_string()),
                        archive_prefix: Some("snapshots".to_string()),
                    }),
                    memory: Some(MemoryArchetypeConfig {
                        uri: "mem://".to_string(),
                        backend: "local_fs".to_string(),
                        prefix: Some("memory".to_string()),
                        index: Some(sqlite_dsn.clone()),
                        ttl_days: 90,
                    }),
                    knowledge: Some(KnowledgeArchetypeConfig {
                        uri: "kb://".to_string(),
                        backend: "local_fs".to_string(),
                        prefix: Some("knowledge".to_string()),
                        index: Some(sqlite_dsn.clone()),
                        enable_trigram: true,
                        enable_vector: true,
                    }),
                    datasets: Some(DatasetsArchetypeConfig {
                        uri: "data://".to_string(),
                        backend: "local_fs".to_string(),
                        prefix: Some("datasets".to_string()),
                        cache_dir: Some(cache_dir.to_string_lossy().replace('\\', "/")),
                        cache_max_bytes: 2 * 1024 * 1024 * 1024,
                    }),
                    models: Some(DatasetsArchetypeConfig {
                        uri: "models://".to_string(),
                        backend: "local_fs".to_string(),
                        prefix: Some("models".to_string()),
                        cache_dir: Some(cache_dir.to_string_lossy().replace('\\', "/")),
                        cache_max_bytes: 4 * 1024 * 1024 * 1024,
                    }),
                    artifacts: Some(ArtifactsArchetypeConfig {
                        uri: "artifacts://".to_string(),
                        backend: "local_fs".to_string(),
                        prefix: Some("artifacts".to_string()),
                        index: Some(sqlite_dsn.clone()),
                        content_addressed: true,
                    }),
                    traces: Some(TracesArchetypeConfig {
                        uri: "traces://".to_string(),
                        backend: "local_fs".to_string(),
                        prefix: Some("traces".to_string()),
                        flush_interval_sec: 15,
                        format: "jsonl".to_string(),
                    }),
                    state: Some(StateArchetypeConfig {
                        uri: "state://".to_string(),
                        backend: "local_fs".to_string(),
                        prefix: Some("state".to_string()),
                        coordinator: Some(sqlite_dsn.clone()),
                    }),
                    shared_drive: Some(SharedDriveArchetypeConfig {
                        uri: "shared://".to_string(),
                        backend: "local_fs".to_string(),
                        prefix: Some("shared".to_string()),
                        lock_backend: Some(sqlite_dsn),
                    }),
                },
                policies,
                exposure: ExposureConfig::default(),
                layers: LayerTuningConfig::default(),
            },
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.spec.backends.is_empty() {
            return Err(anyhow!("ProjectProfile must declare at least one backend in spec.backends"));
        }
        let check_backend = |label: &str, name: &str| -> Result<()> {
            if !self.spec.backends.contains_key(name) {
                return Err(anyhow!("archetype '{}' references unknown backend '{}'", label, name));
            }
            Ok(())
        };
        if let Some(ws) = &self.spec.archetypes.workspace {
            check_backend("workspace", &ws.backend)?;
            if let Some(arch) = &ws.archive_backend {
                check_backend("workspace.archiveBackend", arch)?;
            }
        }
        if let Some(m) = &self.spec.archetypes.memory {
            check_backend("memory", &m.backend)?;
        }
        if let Some(k) = &self.spec.archetypes.knowledge {
            check_backend("knowledge", &k.backend)?;
        }
        if let Some(d) = &self.spec.archetypes.datasets {
            check_backend("datasets", &d.backend)?;
        }
        if let Some(m) = &self.spec.archetypes.models {
            check_backend("models", &m.backend)?;
        }
        if let Some(a) = &self.spec.archetypes.artifacts {
            check_backend("artifacts", &a.backend)?;
        }
        if let Some(t) = &self.spec.archetypes.traces {
            check_backend("traces", &t.backend)?;
        }
        if let Some(s) = &self.spec.archetypes.state {
            check_backend("state", &s.backend)?;
        }
        if let Some(sh) = &self.spec.archetypes.shared_drive {
            check_backend("sharedDrive", &sh.backend)?;
        }
        Ok(())
    }

    /// Return a sanitized copy with any inline credentials or raw secret paths masked.
    pub fn redacted(&self) -> Self {
        let mut copy = self.clone();
        for cfg in copy.spec.backends.values_mut() {
            if cfg.access_key_id.is_some() {
                cfg.access_key_id = Some("***REDACTED***".to_string());
            }
            if cfg.secret_access_key.is_some() {
                cfg.secret_access_key = Some("***REDACTED***".to_string());
            }
            if cfg.token.is_some() {
                cfg.token = Some("***REDACTED***".to_string());
            }
            for (k, v) in cfg.extra.iter_mut() {
                let kl = k.to_ascii_lowercase();
                if kl.contains("secret") || kl.contains("token") || kl.contains("password") || kl.contains("key") {
                    *v = "***REDACTED***".to_string();
                }
            }
        }
        if let Some(m) = copy.spec.archetypes.memory.as_mut() {
            m.index = m.index.as_deref().map(redact_dsn);
        }
        if let Some(k) = copy.spec.archetypes.knowledge.as_mut() {
            k.index = k.index.as_deref().map(redact_dsn);
        }
        if let Some(a) = copy.spec.archetypes.artifacts.as_mut() {
            a.index = a.index.as_deref().map(redact_dsn);
        }
        if let Some(s) = copy.spec.archetypes.state.as_mut() {
            s.coordinator = s.coordinator.as_deref().map(redact_dsn);
        }
        if let Some(sh) = copy.spec.archetypes.shared_drive.as_mut() {
            sh.lock_backend = sh.lock_backend.as_deref().map(redact_dsn);
        }
        copy
    }

    pub fn policy_for(&self, scheme: &str) -> PolicyRule {
        let key = format!("{}://", scheme);
        if let Some(rule) = self.spec.policies.get(&key) {
            return rule.clone();
        }
        match scheme {
            "kb" | "data" | "models" => PolicyRule {
                mode: AccessMode::ReadOnly,
                max_file_size_mb: Some(1024),
                allowed_extensions: vec![],
                require_scope: None,
            },
            "artifacts" | "traces" => PolicyRule {
                mode: AccessMode::AppendOnly,
                max_file_size_mb: Some(512),
                allowed_extensions: vec![],
                require_scope: None,
            },
            "mem" => PolicyRule {
                mode: AccessMode::ReadWrite,
                max_file_size_mb: Some(16),
                allowed_extensions: vec![],
                require_scope: Some(true),
            },
            _ => PolicyRule {
                mode: AccessMode::ReadWrite,
                max_file_size_mb: Some(256),
                allowed_extensions: vec![],
                require_scope: None,
            },
        }
    }
}

pub fn redact_dsn(dsn: &str) -> String {
    if let Some(idx) = dsn.find("://") {
        let scheme = &dsn[..idx];
        let rest = &dsn[idx + 3..];
        if let Some(at) = rest.find('@') {
            return format!("{}://***@{}", scheme, &rest[at + 1..]);
        }
    }
    dsn.to_string()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

pub fn discover_profile_path() -> PathBuf {
    if let Ok(p) = env::var("AI_STORE_PROFILE") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(cwd) = env::current_dir() {
        let local = cwd.join(".ai-store.yaml");
        if local.exists() {
            return local;
        }
    }
    let k8s_profile = PathBuf::from("/etc/ai-store/project-profile.yaml");
    if k8s_profile.exists() {
        return k8s_profile;
    }
    default_state_root().join(".ai-store.yaml")
}

pub fn default_state_root() -> PathBuf {
    if let Ok(dir) = env::var("AI_STORE_HOME") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(cwd) = env::current_dir() {
        return cwd;
    }
    PathBuf::from(".")
}

pub fn default_daemon_dir() -> PathBuf {
    if let Ok(dir) = env::var("AI_STORE_HOME") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(home) = env::var("HOME").or_else(|_| env::var("USERPROFILE")) {
        return PathBuf::from(home).join(".ai-store");
    }
    PathBuf::from(".ai-store")
}

/// Resolve a `secret://<group>/<key>` or `env://<VAR>` reference into a concrete string.
/// Also handles `postgres://secret://<group>/<key>?...` or `redis://secret://<group>/<key>` patterns.
pub fn resolve_secret_reference(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(env_name) = trimmed.strip_prefix("env://") {
        return env::var(env_name).ok().filter(|v| !v.is_empty());
    }
    if let Some(secret_spec) = trimmed.strip_prefix("secret://") {
        return read_secret_spec(secret_spec);
    }
    // Handle `postgres://secret://pits-opendal-creds/pg-dsn?schema=ai_mem` or `redis://secret://...`
    for prefix in ["postgres://secret://", "postgresql://secret://", "redis://secret://"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let (spec, suffix) = match rest.find('?') {
                Some(q) => (&rest[..q], Some(&rest[q..])),
                None => (rest, None),
            };
            if let Some(mut val) = read_secret_spec(spec) {
                if let Some(qs) = suffix {
                    if !val.contains('?') {
                        val.push_str(qs);
                    } else {
                        val.push('&');
                        val.push_str(&qs[1..]);
                    }
                }
                return Some(val);
            }
            return None;
        }
    }
    Some(trimmed.to_string())
}

fn read_secret_spec(spec: &str) -> Option<String> {
    let parts: Vec<&str> = spec.split('/').filter(|s| !s.is_empty()).collect();
    let secret_dirs = [
        env::var("AI_STORE_SECRETS_DIR").ok(),
        Some("/var/run/ai-store/secrets".to_string()),
        Some("/etc/ai-store/secrets".to_string()),
    ];

    if parts.len() >= 2 {
        let group = parts[0];
        let key = parts[1..].join("/");
        for base in secret_dirs.iter().flatten() {
            let candidate = Path::new(base).join(group).join(&key);
            if let Ok(content) = fs::read_to_string(&candidate) {
                let v = content.trim().to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
            let flat = Path::new(base).join(&key);
            if let Ok(content) = fs::read_to_string(&flat) {
                let v = content.trim().to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
        let env_key = format!(
            "AI_STORE_SECRET_{}_{}",
            sanitize_env_part(group),
            sanitize_env_part(&key)
        );
        if let Ok(v) = env::var(&env_key) {
            if !v.is_empty() {
                return Some(v);
            }
        }
        let short_env_key = format!("AI_STORE_{}", sanitize_env_part(&key));
        if let Ok(v) = env::var(&short_env_key) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    } else if parts.len() == 1 {
        let key = parts[0];
        for base in secret_dirs.iter().flatten() {
            let flat = Path::new(base).join(key);
            if let Ok(content) = fs::read_to_string(&flat) {
                let v = content.trim().to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
        let env_key = format!("AI_STORE_{}", sanitize_env_part(key));
        if let Ok(v) = env::var(&env_key) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

fn sanitize_env_part(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
        .collect()
}
