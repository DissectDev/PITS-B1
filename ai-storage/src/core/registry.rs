//! Layer 2: Apache OpenDAL Pure Data Access Plane (`BackendRegistry` + Middleware `Layer` Stack).
//!
//! Instantiates an independent `opendal::Operator` for every backend declared in
//! `ProjectProfile.spec.backends`, wrapping it with a uniform OpenDAL `Layer` chain:
//! `RetryLayer -> TimeoutLayer -> ConcurrentLimitLayer -> LoggingLayer -> MetricsLayer`.

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use opendal::layers::{ConcurrentLimitLayer, LoggingLayer, MetricsLayer, RetryLayer, TimeoutLayer};
use opendal::{EntryMode, Operator};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::profile::{resolve_secret_reference, BackendConfig, LayerTuningConfig, ProjectProfile};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectStat {
    pub path: String,
    pub is_dir: bool,
    pub content_length: u64,
    pub etag: Option<String>,
    pub content_type: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectEntry {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub content_length: u64,
    pub etag: Option<String>,
}

#[derive(Clone)]
pub struct BackendHandle {
    pub name: String,
    pub scheme: String,
    pub operator: Operator,
}

#[derive(Clone)]
pub struct BackendRegistry {
    backends: BTreeMap<String, BackendHandle>,
}

impl BackendRegistry {
    pub fn from_profile(profile: &ProjectProfile) -> Result<Self> {
        opendal::install_default();
        let mut backends = BTreeMap::new();
        for (name, cfg) in &profile.spec.backends {
            let op = build_operator(name, cfg, &profile.spec.layers)
                .with_context(|| format!("failed to initialize OpenDAL backend '{}'", name))?;
            backends.insert(
                name.clone(),
                BackendHandle {
                    name: name.clone(),
                    scheme: cfg.scheme.clone(),
                    operator: op,
                },
            );
        }
        Ok(Self { backends })
    }

    pub fn get(&self, name: &str) -> Result<&BackendHandle> {
        self.backends
            .get(name)
            .ok_or_else(|| anyhow!("unknown OpenDAL backend '{}'", name))
    }

    pub fn list_names(&self) -> Vec<(&str, &str)> {
        self.backends
            .values()
            .map(|b| (b.name.as_str(), b.scheme.as_str()))
            .collect()
    }
}

fn build_operator(name: &str, cfg: &BackendConfig, tuning: &LayerTuningConfig) -> Result<Operator> {
    let mut map: BTreeMap<String, String> = cfg.extra.clone();
    let scheme = normalize_scheme(&cfg.scheme);

    if scheme == "fs" {
        let root = cfg
            .root
            .clone()
            .unwrap_or_else(|| format!(".ai-store/{}", name));
        fs::create_dir_all(Path::new(&root))
            .with_context(|| format!("failed to create fs backend root '{}'", root))?;
        map.insert("root".to_string(), root);
    } else if let Some(root) = &cfg.root {
        map.insert("root".to_string(), root.clone());
    }

    if let Some(ep) = &cfg.endpoint {
        map.insert("endpoint".to_string(), ep.clone());
    }
    if let Some(bucket) = &cfg.bucket {
        map.insert("bucket".to_string(), bucket.clone());
    }
    if let Some(region) = &cfg.region {
        map.insert("region".to_string(), region.clone());
    }
    if let Some(repo) = &cfg.repo {
        map.insert("repo_id".to_string(), repo.clone());
    }
    if let Some(repo_type) = &cfg.repo_type {
        map.insert("repo_type".to_string(), repo_type.clone());
    }
    if let Some(rev) = &cfg.revision {
        map.insert("revision".to_string(), rev.clone());
    }
    if let Some(vhost) = cfg.enable_virtual_host_style {
        map.insert("enable_virtual_host_style".to_string(), vhost.to_string());
    }

    if scheme == "s3" {
        map.entry("disable_config_load".to_string())
            .or_insert_with(|| "true".to_string());
        map.entry("disable_ec2_metadata".to_string())
            .or_insert_with(|| "true".to_string());
        if let Some(ak) = cfg.access_key_id.as_deref().and_then(resolve_secret_reference) {
            map.insert("access_key_id".to_string(), ak);
        } else if let Some(cred_ref) = &cfg.credential_ref {
            if let Some(ak) = resolve_secret_reference(&format!("{}/access_key_id", cred_ref.trim_end_matches('/'))) {
                map.insert("access_key_id".to_string(), ak);
            }
        }
        if let Some(sk) = cfg.secret_access_key.as_deref().and_then(resolve_secret_reference) {
            map.insert("secret_access_key".to_string(), sk);
        } else if let Some(cred_ref) = &cfg.credential_ref {
            if let Some(sk) = resolve_secret_reference(&format!("{}/secret_access_key", cred_ref.trim_end_matches('/'))) {
                map.insert("secret_access_key".to_string(), sk);
            }
        }
    } else if scheme == "hf" {
        if let Some(tok) = cfg.token.as_deref().and_then(resolve_secret_reference) {
            map.insert("token".to_string(), tok);
        } else if let Some(cred_ref) = &cfg.credential_ref {
            if let Some(tok) = resolve_secret_reference(&format!("{}/token", cred_ref.trim_end_matches('/'))) {
                map.insert("token".to_string(), tok);
            }
        }
    }

    let mut op = Operator::via_iter(scheme, map)?;
    op = op.layer(
        RetryLayer::new()
            .with_jitter()
            .with_max_times(tuning.retry_max_times.max(1)),
    );
    op = op.layer(
        TimeoutLayer::new()
            .with_timeout(Duration::from_secs(tuning.timeout_secs.max(1)))
            .with_io_timeout(Duration::from_secs(tuning.io_timeout_secs.max(1))),
    );
    if tuning.concurrent_limit > 0 {
        op = op.layer(ConcurrentLimitLayer::new(tuning.concurrent_limit));
    }
    if tuning.enable_logging {
        op = op.layer(LoggingLayer::default());
    }
    if tuning.enable_metrics {
        op = op.layer(MetricsLayer::new());
    }
    Ok(op)
}

fn normalize_scheme(raw: &str) -> &str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "huggingface" | "hf" => "hf",
        "local" | "fs" => "fs",
        "mem" | "memory" => "memory",
        "s3" => "s3",
        "http" | "https" => "http",
        _ => raw.trim(),
    }
}

pub fn join_prefix(prefix: Option<&str>, rel: &str) -> String {
    let clean_rel = rel.trim_start_matches('/');
    match prefix.map(|p| p.trim_matches('/')) {
        Some(p) if !p.is_empty() && !clean_rel.is_empty() => format!("{}/{}", p, clean_rel),
        Some(p) if !p.is_empty() => format!("{}/", p),
        _ => clean_rel.to_string(),
    }
}

pub async fn op_read_all(op: &Operator, key: &str) -> Result<Bytes> {
    let buf = op.read(key).await?;
    Ok(buf.to_bytes())
}

pub async fn op_read_range(op: &Operator, key: &str, offset: u64, limit: Option<u64>) -> Result<Bytes> {
    let buf = match limit {
        Some(len) => op.read_with(key).range(offset..(offset + len)).await?,
        None if offset > 0 => op.read_with(key).range(offset..).await?,
        None => op.read(key).await?,
    };
    Ok(buf.to_bytes())
}

pub async fn op_write(op: &Operator, key: &str, data: Bytes) -> Result<()> {
    op.write(key, data).await?;
    Ok(())
}

pub async fn op_stat(op: &Operator, key: &str) -> Result<ObjectStat> {
    let meta = op.stat(key).await?;
    Ok(ObjectStat {
        path: key.to_string(),
        is_dir: meta.mode() == EntryMode::DIR,
        content_length: meta.content_length(),
        etag: meta.etag().map(str::to_string),
        content_type: meta.content_type().map(str::to_string),
        last_modified: meta.last_modified().map(|t| t.to_string()),
    })
}

pub async fn op_exists(op: &Operator, key: &str) -> Result<bool> {
    Ok(op.exists(key).await?)
}

pub async fn op_delete(op: &Operator, key: &str) -> Result<()> {
    op.delete(key).await?;
    Ok(())
}

pub async fn op_list_recursive(op: &Operator, prefix: &str) -> Result<Vec<ObjectEntry>> {
    let list_key = if prefix.is_empty() || prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{}/", prefix)
    };
    let entries = op.list_with(&list_key).recursive(true).await?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let meta = entry.metadata();
        let path = entry.path().to_string();
        if path == list_key {
            continue;
        }
        out.push(ObjectEntry {
            name: entry.name().to_string(),
            is_dir: meta.mode() == EntryMode::DIR,
            content_length: meta.content_length(),
            etag: meta.etag().map(str::to_string),
            path,
        });
    }
    Ok(out)
}
