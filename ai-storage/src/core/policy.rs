//! Layer 3 Guardrails: URI Normalization, Sandbox Traversal Check, and Policy Guard.

use anyhow::{anyhow, Result};
use std::path::Path;

use crate::profile::{AccessMode, PolicyRule, ProjectProfile};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchetypeKind {
    Workspace,
    Memory,
    Knowledge,
    Datasets,
    Models,
    Artifacts,
    Traces,
    State,
    SharedDrive,
}

impl ArchetypeKind {
    pub fn scheme(&self) -> &'static str {
        match self {
            Self::Workspace => "ws",
            Self::Memory => "mem",
            Self::Knowledge => "kb",
            Self::Datasets => "data",
            Self::Models => "models",
            Self::Artifacts => "artifacts",
            Self::Traces => "traces",
            Self::State => "state",
            Self::SharedDrive => "shared",
        }
    }

    pub fn from_scheme(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ws" | "workspace" => Ok(Self::Workspace),
            "mem" | "memory" => Ok(Self::Memory),
            "kb" | "knowledge" => Ok(Self::Knowledge),
            "data" | "dataset" | "datasets" => Ok(Self::Datasets),
            "model" | "models" => Ok(Self::Models),
            "artifact" | "artifacts" => Ok(Self::Artifacts),
            "trace" | "traces" => Ok(Self::Traces),
            "state" => Ok(Self::State),
            "shared" | "share" => Ok(Self::SharedDrive),
            other => Err(anyhow!("unsupported AI storage URI scheme '{}://'", other)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParsedUri {
    pub kind: ArchetypeKind,
    pub scheme: String,
    pub rel_path: String,
    pub canonical: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Read,
    List,
    Create,
    Overwrite,
    Delete,
}

/// Step 1: Normalize URI and block path traversal / null bytes / illegal characters.
pub fn normalize_uri(raw: &str) -> Result<ParsedUri> {
    let trimmed = raw.trim();
    if trimmed.contains('\0') {
        return Err(anyhow!("URI contains null byte"));
    }
    let idx = trimmed
        .find("://")
        .ok_or_else(|| anyhow!("invalid AI storage URI '{}': missing scheme (expected e.g. ws://, mem://, kb://)", trimmed))?;
    let raw_scheme = &trimmed[..idx];
    let kind = ArchetypeKind::from_scheme(raw_scheme)?;
    let scheme = kind.scheme().to_string();

    let rest = &trimmed[idx + 3..];
    let normalized_slashes = rest.replace('\\', "/");
    let mut clean_segments: Vec<&str> = Vec::new();
    for seg in normalized_slashes.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return Err(anyhow!(
                "path traversal '..' rejected by VFS sandbox in URI '{}'",
                trimmed
            ));
        }
        if seg.contains(':') {
            return Err(anyhow!("illegal ':' segment '{}' in URI '{}'", seg, trimmed));
        }
        clean_segments.push(seg);
    }

    let rel_path = clean_segments.join("/");
    let canonical = format!("{}://{}", scheme, rel_path);
    Ok(ParsedUri {
        kind,
        scheme,
        rel_path,
        canonical,
    })
}

/// Step 2: Policy Guard checking access mode, size bounds, extensions, and scope isolation.
pub fn check_policy(
    profile: &ProjectProfile,
    uri: &ParsedUri,
    op: OperationKind,
    payload_len: Option<usize>,
    bypass_readonly_ingest: bool,
) -> Result<PolicyRule> {
    let rule = profile.policy_for(&uri.scheme);

    match op {
        OperationKind::Read | OperationKind::List => {}
        OperationKind::Create => {
            if rule.mode == AccessMode::ReadOnly && !bypass_readonly_ingest {
                return Err(anyhow!(
                    "Policy Guard denied write to '{}': scheme '{}://' is read-only",
                    uri.canonical,
                    uri.scheme
                ));
            }
        }
        OperationKind::Overwrite => match rule.mode {
            AccessMode::ReadOnly if !bypass_readonly_ingest => {
                return Err(anyhow!(
                    "Policy Guard denied overwrite on '{}': scheme '{}://' is read-only",
                    uri.canonical,
                    uri.scheme
                ));
            }
            AccessMode::AppendOnly => {
                return Err(anyhow!(
                    "Policy Guard denied overwrite on '{}': scheme '{}://' is append-only (immutable objects)",
                    uri.canonical,
                    uri.scheme
                ));
            }
            _ => {}
        },
        OperationKind::Delete => match rule.mode {
            AccessMode::ReadOnly | AccessMode::AppendOnly => {
                return Err(anyhow!(
                    "Policy Guard denied delete on '{}': scheme '{}://' is {:?}",
                    uri.canonical,
                    uri.scheme,
                    rule.mode
                ));
            }
            AccessMode::ReadWrite => {}
        },
    }

    if let (Some(max_mb), Some(len)) = (rule.max_file_size_mb, payload_len) {
        let max_bytes = (max_mb as usize).saturating_mul(1024 * 1024);
        if len > max_bytes {
            return Err(anyhow!(
                "Policy Guard denied write to '{}': payload {} bytes exceeds maxFileSizeMB ({} MB)",
                uri.canonical,
                len,
                max_mb
            ));
        }
    }

    if matches!(op, OperationKind::Create | OperationKind::Overwrite) && !rule.allowed_extensions.is_empty() {
        let ext = Path::new(&uri.rel_path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_ascii_lowercase()))
            .unwrap_or_default();
        let allowed = rule
            .allowed_extensions
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&ext) || a.trim_start_matches('.').eq_ignore_ascii_case(ext.trim_start_matches('.')));
        if !allowed {
            return Err(anyhow!(
                "Policy Guard denied extension '{}' for '{}'; allowed: {:?}",
                ext,
                uri.canonical,
                rule.allowed_extensions
            ));
        }
    }

    if rule.require_scope.unwrap_or(false) && matches!(op, OperationKind::Create | OperationKind::Overwrite) {
        // Require at least `<scope>/<name>` (e.g., `project/session/note.md`)
        if !uri.rel_path.contains('/') {
            return Err(anyhow!(
                "Policy Guard requires scoped path (<scope>/<item>) for '{}://', got '{}'",
                uri.scheme,
                uri.canonical
            ));
        }
    }

    Ok(rule)
}
