//! Dual-Mode Relational Database SSOT (PostgreSQL v3 Wire Protocol + SQLite3 WAL)
//! & Workspace Dependency / Adapter Discovery Scanner for PITS Sourcing Governance.
//!
//! Architecture:
//! - Primary persistence uses relational ACID tables inside SQLite3 WAL
//!   (`PITS_STATE_DIR/sourcing-audit.sqlite3`, loaded dynamically via `winsqlite3.dll` on Windows
//!   and `libsqlite3.so.0` on Linux with zero new Cargo dependencies).
//! - When `sourcingDbUrl` / `PITS_SOURCING_DB_URL` (`postgres://...`) is configured, a native
//!   pure-Rust PostgreSQL v3.0 wire client (`SCRAM-SHA-256` / cleartext / trust + optional TLS)
//!   performs bi-directional incremental sync with outbox durability (`sync_pending = 1`).
//! - Multi-role blind ballots (`architecture`, `procurement`, `security-compliance`,
//!   `domain-owner`) are stored independently and aggregated automatically, triggering
//!   `blindScoreVarianceAlert` whenever any manual dimension spread reaches `>= 1.5`.

use crate::catalog::sourcing::{
    parse_sourcing_yaml_str, round2, BlindBallotRecord, DiscoveryCandidateRecord, ManualScores,
    SourcingAuditEvent, SourcingDbSyncStatus, SourcingResourceRecord, PERCEIVED_STATUSES,
    SEED_YAMLS, SOURCING_RESOURCE_TYPES, WALLET_SHARE_TIERS,
};
use crate::transport::{build_rustls_client_config, server_name_for_host, AsyncReadWrite};
use base64::Engine as _;
use chrono::{NaiveDate, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use url::Url;
use uuid::Uuid;

pub const REVIEWER_ROLES: [&str; 4] = [
    "architecture",
    "procurement",
    "security-compliance",
    "domain-owner",
];

pub const VARIANCE_SPREAD_THRESHOLD: f64 = 1.5;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubmitBlindBallotRequest {
    pub unit_name: String,
    pub reviewer_role: String,
    pub reviewer_id: String,
    #[serde(rename = "y2_coreCompetitiveness")]
    pub y2_core_competitiveness: f64,
    #[serde(rename = "y4_priceElasticity")]
    pub y4_price_elasticity: f64,
    #[serde(rename = "x2_marketScarcity")]
    pub x2_market_scarcity: f64,
    #[serde(rename = "x3_customizationDepth")]
    pub x3_customization_depth: f64,
    #[serde(rename = "x4_complianceFragility")]
    pub x4_compliance_fragility: f64,
    #[serde(default)]
    pub evidence_notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AssessWindmillRequest {
    pub unit_name: String,
    pub estimated_wallet_share_tier: String,
    pub account_attractiveness_score: f64,
    pub perceived_status: String,
    pub assessed_by: String,
    #[serde(default)]
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CandidateDispositionRequest {
    pub candidate_id: String,
    /// `"promote"` or `"ignore"`
    pub action: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub resource_type: Option<String>,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub cost_allocation_id: Option<String>,
    #[serde(default)]
    pub renewal_date: Option<String>,
    #[serde(default)]
    pub vendor_name: Option<String>,
    #[serde(default)]
    pub adapter_abstracted: Option<bool>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub depends_on_components: Option<Vec<String>>,
    #[serde(default)]
    pub actor: Option<String>,
}

// ---------------------------------------------------------------------------
// Dynamic SQLite3 C-API Loader (`winsqlite3.dll` / `libsqlite3.so.0`)
// ---------------------------------------------------------------------------

const SQLITE_OK: i32 = 0;
const SQLITE_ROW: i32 = 100;
const SQLITE_DONE: i32 = 101;
const SQLITE_NULL: i32 = 5;
const SQLITE_OPEN_READWRITE: i32 = 0x0000_0002;
const SQLITE_OPEN_CREATE: i32 = 0x0000_0004;
const SQLITE_OPEN_FULLMUTEX: i32 = 0x0001_0000;
const SQLITE_TRANSIENT: isize = -1;

type FnLibVersion = unsafe extern "C" fn() -> *const u8;
type FnOpenV2 = unsafe extern "C" fn(*const u8, *mut *mut c_void, i32, *const u8) -> i32;
type FnCloseV2 = unsafe extern "C" fn(*mut c_void) -> i32;
type FnExec = unsafe extern "C" fn(
    *mut c_void,
    *const u8,
    *mut c_void,
    *mut c_void,
    *mut *mut u8,
) -> i32;
type FnPrepareV2 =
    unsafe extern "C" fn(*mut c_void, *const u8, i32, *mut *mut c_void, *mut *const u8) -> i32;
type FnBindText = unsafe extern "C" fn(*mut c_void, i32, *const u8, i32, isize) -> i32;
type FnBindInt64 = unsafe extern "C" fn(*mut c_void, i32, i64) -> i32;
type FnBindDouble = unsafe extern "C" fn(*mut c_void, i32, f64) -> i32;
type FnBindNull = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type FnStep = unsafe extern "C" fn(*mut c_void) -> i32;
type FnColumnCount = unsafe extern "C" fn(*mut c_void) -> i32;
type FnColumnType = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type FnColumnText = unsafe extern "C" fn(*mut c_void, i32) -> *const u8;
type FnColumnBytes = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type FnColumnInt64 = unsafe extern "C" fn(*mut c_void, i32) -> i64;
type FnColumnDouble = unsafe extern "C" fn(*mut c_void, i32) -> f64;
type FnFinalize = unsafe extern "C" fn(*mut c_void) -> i32;
type FnErrMsg = unsafe extern "C" fn(*mut c_void) -> *const u8;
type FnBusyTimeout = unsafe extern "C" fn(*mut c_void, i32) -> i32;

struct SqliteApi {
    libversion: FnLibVersion,
    open_v2: FnOpenV2,
    close_v2: FnCloseV2,
    exec: FnExec,
    prepare_v2: FnPrepareV2,
    bind_text: FnBindText,
    bind_int64: FnBindInt64,
    bind_double: FnBindDouble,
    bind_null: FnBindNull,
    step: FnStep,
    column_count: FnColumnCount,
    column_type: FnColumnType,
    column_text: FnColumnText,
    column_bytes: FnColumnBytes,
    column_int64: FnColumnInt64,
    column_double: FnColumnDouble,
    finalize: FnFinalize,
    errmsg: FnErrMsg,
    busy_timeout: FnBusyTimeout,
}

unsafe impl Send for SqliteApi {}
unsafe impl Sync for SqliteApi {}

static SQLITE_API: OnceLock<Result<SqliteApi, String>> = OnceLock::new();

fn get_sqlite_api() -> Result<&'static SqliteApi, String> {
    SQLITE_API
        .get_or_init(|| unsafe { load_sqlite_api() })
        .as_ref()
        .map_err(Clone::clone)
}

#[cfg(windows)]
unsafe fn load_sqlite_api() -> Result<SqliteApi, String> {
    extern "system" {
        fn LoadLibraryA(lp_lib_file_name: *const u8) -> *mut c_void;
        fn GetProcAddress(h_module: *mut c_void, lp_proc_name: *const u8) -> *mut c_void;
    }
    let handle = LoadLibraryA(b"winsqlite3.dll\0".as_ptr());
    let handle = if handle.is_null() {
        LoadLibraryA(b"sqlite3.dll\0".as_ptr())
    } else {
        handle
    };
    if handle.is_null() {
        return Err("Failed to load winsqlite3.dll / sqlite3.dll".to_string());
    }
    macro_rules! sym {
        ($name:literal, $ty:ty) => {{
            let ptr = GetProcAddress(handle, concat!($name, "\0").as_ptr());
            if ptr.is_null() {
                return Err(format!("Missing SQLite symbol '{}'", $name));
            }
            std::mem::transmute::<*mut c_void, $ty>(ptr)
        }};
    }
    Ok(SqliteApi {
        libversion: sym!("sqlite3_libversion", FnLibVersion),
        open_v2: sym!("sqlite3_open_v2", FnOpenV2),
        close_v2: sym!("sqlite3_close_v2", FnCloseV2),
        exec: sym!("sqlite3_exec", FnExec),
        prepare_v2: sym!("sqlite3_prepare_v2", FnPrepareV2),
        bind_text: sym!("sqlite3_bind_text", FnBindText),
        bind_int64: sym!("sqlite3_bind_int64", FnBindInt64),
        bind_double: sym!("sqlite3_bind_double", FnBindDouble),
        bind_null: sym!("sqlite3_bind_null", FnBindNull),
        step: sym!("sqlite3_step", FnStep),
        column_count: sym!("sqlite3_column_count", FnColumnCount),
        column_type: sym!("sqlite3_column_type", FnColumnType),
        column_text: sym!("sqlite3_column_text", FnColumnText),
        column_bytes: sym!("sqlite3_column_bytes", FnColumnBytes),
        column_int64: sym!("sqlite3_column_int64", FnColumnInt64),
        column_double: sym!("sqlite3_column_double", FnColumnDouble),
        finalize: sym!("sqlite3_finalize", FnFinalize),
        errmsg: sym!("sqlite3_errmsg", FnErrMsg),
        busy_timeout: sym!("sqlite3_busy_timeout", FnBusyTimeout),
    })
}

#[cfg(unix)]
unsafe fn load_sqlite_api() -> Result<SqliteApi, String> {
    extern "C" {
        fn dlopen(filename: *const u8, flags: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const u8) -> *mut c_void;
    }
    const RTLD_NOW: i32 = 2;
    let mut handle = dlopen(b"libsqlite3.so.0\0".as_ptr(), RTLD_NOW);
    if handle.is_null() {
        handle = dlopen(b"libsqlite3.so\0".as_ptr(), RTLD_NOW);
    }
    if handle.is_null() {
        handle = dlopen(b"libsqlite3.dylib\0".as_ptr(), RTLD_NOW);
    }
    if handle.is_null() {
        if let Ok(entries) = std::fs::read_dir("/nix/store") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.contains("-sqlite-") && !name_str.ends_with(".drv") {
                    let cand = entry.path().join("lib").join("libsqlite3.so.0");
                    if cand.is_file() {
                        if let Ok(cstr) = CString::new(cand.to_string_lossy().as_bytes()) {
                            handle = dlopen(cstr.as_ptr() as *const u8, RTLD_NOW);
                            if !handle.is_null() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    if handle.is_null() {
        return Err(
            "Failed to dlopen libsqlite3.so.0 / libsqlite3.so (ensure sqlite runtime library is on LD_LIBRARY_PATH)".to_string(),
        );
    }
    macro_rules! sym {
        ($name:literal, $ty:ty) => {{
            let ptr = dlsym(handle, concat!($name, "\0").as_ptr());
            if ptr.is_null() {
                return Err(format!("Missing SQLite symbol '{}'", $name));
            }
            std::mem::transmute::<*mut c_void, $ty>(ptr)
        }};
    }
    Ok(SqliteApi {
        libversion: sym!("sqlite3_libversion", FnLibVersion),
        open_v2: sym!("sqlite3_open_v2", FnOpenV2),
        close_v2: sym!("sqlite3_close_v2", FnCloseV2),
        exec: sym!("sqlite3_exec", FnExec),
        prepare_v2: sym!("sqlite3_prepare_v2", FnPrepareV2),
        bind_text: sym!("sqlite3_bind_text", FnBindText),
        bind_int64: sym!("sqlite3_bind_int64", FnBindInt64),
        bind_double: sym!("sqlite3_bind_double", FnBindDouble),
        bind_null: sym!("sqlite3_bind_null", FnBindNull),
        step: sym!("sqlite3_step", FnStep),
        column_count: sym!("sqlite3_column_count", FnColumnCount),
        column_type: sym!("sqlite3_column_type", FnColumnType),
        column_text: sym!("sqlite3_column_text", FnColumnText),
        column_bytes: sym!("sqlite3_column_bytes", FnColumnBytes),
        column_int64: sym!("sqlite3_column_int64", FnColumnInt64),
        column_double: sym!("sqlite3_column_double", FnColumnDouble),
        finalize: sym!("sqlite3_finalize", FnFinalize),
        errmsg: sym!("sqlite3_errmsg", FnErrMsg),
        busy_timeout: sym!("sqlite3_busy_timeout", FnBusyTimeout),
    })
}

#[derive(Debug, Clone)]
enum SqlVal {
    Text(String),
    Int(i64),
    Real(f64),
    Null,
}

impl SqlVal {
    fn opt_text(v: Option<&str>) -> Self {
        match v {
            Some(s) => Self::Text(s.to_string()),
            None => Self::Null,
        }
    }
}

struct SqliteConn {
    api: &'static SqliteApi,
    db: *mut c_void,
    is_memory: bool,
    path_label: String,
}

unsafe impl Send for SqliteConn {}

impl Drop for SqliteConn {
    fn drop(&mut self) {
        if !self.db.is_null() {
            unsafe {
                (self.api.close_v2)(self.db);
            }
            self.db = std::ptr::null_mut();
        }
    }
}

impl SqliteConn {
    fn open(path: &Path, in_memory: bool) -> Result<Self, String> {
        let api = get_sqlite_api()?;
        if !in_memory {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        format!("Failed to create SQLite dir '{}': {}", parent.display(), e)
                    })?;
                }
            }
        }
        let target = if in_memory {
            ":memory:".to_string()
        } else {
            path.to_string_lossy().into_owned()
        };
        let c_target = CString::new(target.as_str()).map_err(|e| format!("Invalid path: {e}"))?;
        let mut db: *mut c_void = std::ptr::null_mut();
        let rc = unsafe {
            (api.open_v2)(
                c_target.as_ptr() as *const u8,
                &mut db,
                SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX,
                std::ptr::null(),
            )
        };
        if rc != SQLITE_OK || db.is_null() {
            let msg = if !db.is_null() {
                let m = unsafe {
                    CStr::from_ptr((api.errmsg)(db) as *const i8)
                        .to_string_lossy()
                        .into_owned()
                };
                unsafe { (api.close_v2)(db) };
                m
            } else {
                format!("rc={rc}")
            };
            return Err(format!("sqlite3_open_v2('{target}') failed: {msg}"));
        }
        let conn = Self {
            api,
            db,
            is_memory: in_memory,
            path_label: target,
        };
        unsafe {
            (api.busy_timeout)(conn.db, 5000);
        }
        Ok(conn)
    }

    fn version(&self) -> String {
        unsafe {
            let ptr = (self.api.libversion)();
            if ptr.is_null() {
                "unknown".to_string()
            } else {
                CStr::from_ptr(ptr as *const i8)
                    .to_string_lossy()
                    .into_owned()
            }
        }
    }

    fn last_err(&self) -> String {
        unsafe {
            let ptr = (self.api.errmsg)(self.db);
            if ptr.is_null() {
                "sqlite error".to_string()
            } else {
                CStr::from_ptr(ptr as *const i8)
                    .to_string_lossy()
                    .into_owned()
            }
        }
    }

    fn exec_batch(&self, sql: &str) -> Result<(), String> {
        let c_sql = CString::new(sql).map_err(|e| format!("SQL NUL byte error: {e}"))?;
        let rc = unsafe {
            (self.api.exec)(
                self.db,
                c_sql.as_ptr() as *const u8,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if rc != SQLITE_OK {
            return Err(format!("sqlite3_exec error ({rc}): {}", self.last_err()));
        }
        Ok(())
    }

    fn execute(&self, sql: &str, params: &[SqlVal]) -> Result<(), String> {
        let _ = self.query_rows(sql, params)?;
        Ok(())
    }

    fn query_rows(&self, sql: &str, params: &[SqlVal]) -> Result<Vec<Vec<Option<String>>>, String> {
        let sql_bytes = sql.as_bytes();
        let mut stmt: *mut c_void = std::ptr::null_mut();
        let rc = unsafe {
            (self.api.prepare_v2)(
                self.db,
                sql_bytes.as_ptr(),
                sql_bytes.len() as i32,
                &mut stmt,
                std::ptr::null_mut(),
            )
        };
        if rc != SQLITE_OK || stmt.is_null() {
            return Err(format!("sqlite3_prepare_v2 failed ({rc}): {} | sql={sql}", self.last_err()));
        }

        struct StmtGuard(*mut c_void, FnFinalize);
        impl Drop for StmtGuard {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    unsafe {
                        (self.1)(self.0);
                    }
                }
            }
        }
        let _guard = StmtGuard(stmt, self.api.finalize);

        for (i, param) in params.iter().enumerate() {
            let idx = (i + 1) as i32;
            let bind_rc = unsafe {
                match param {
                    SqlVal::Text(s) => {
                        let bytes = s.as_bytes();
                        let ptr = if bytes.is_empty() {
                            b"\0".as_ptr()
                        } else {
                            bytes.as_ptr()
                        };
                        (self.api.bind_text)(stmt, idx, ptr, bytes.len() as i32, SQLITE_TRANSIENT)
                    }
                    SqlVal::Int(n) => (self.api.bind_int64)(stmt, idx, *n),
                    SqlVal::Real(f) => (self.api.bind_double)(stmt, idx, *f),
                    SqlVal::Null => (self.api.bind_null)(stmt, idx),
                }
            };
            if bind_rc != SQLITE_OK {
                return Err(format!("sqlite3_bind col {idx} failed ({bind_rc}): {}", self.last_err()));
            }
        }

        let mut rows = Vec::new();
        let col_count = unsafe { (self.api.column_count)(stmt) };
        loop {
            let step_rc = unsafe { (self.api.step)(stmt) };
            match step_rc {
                SQLITE_ROW => {
                    let mut row = Vec::with_capacity(col_count as usize);
                    for col in 0..col_count {
                        let ctype = unsafe { (self.api.column_type)(stmt, col) };
                        if ctype == SQLITE_NULL {
                            row.push(None);
                        } else {
                            let val_str = unsafe {
                                let ptr = (self.api.column_text)(stmt, col);
                                let len = (self.api.column_bytes)(stmt, col);
                                if ptr.is_null() || len <= 0 {
                                    if ctype == 1 {
                                        (self.api.column_int64)(stmt, col).to_string()
                                    } else if ctype == 2 {
                                        (self.api.column_double)(stmt, col).to_string()
                                    } else {
                                        String::new()
                                    }
                                } else {
                                    String::from_utf8_lossy(std::slice::from_raw_parts(
                                        ptr,
                                        len as usize,
                                    ))
                                    .into_owned()
                                }
                            };
                            row.push(Some(val_str));
                        }
                    }
                    rows.push(row);
                }
                SQLITE_DONE => break,
                other => {
                    return Err(format!(
                        "sqlite3_step failed ({other}): {}",
                        self.last_err()
                    ));
                }
            }
        }
        Ok(rows)
    }

    fn pragma_data_version(&self) -> i64 {
        if self.is_memory {
            return 0;
        }
        self.query_rows("PRAGMA data_version;", &[])
            .ok()
            .and_then(|rows| rows.first().and_then(|r| r.first()).cloned().flatten())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Schema & Multi-Role Ballot Math Helpers
// ---------------------------------------------------------------------------

const SQLITE_SCHEMA_SQL: &str = r#"
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;

CREATE TABLE IF NOT EXISTS sourcing_units (
    name TEXT PRIMARY KEY,
    namespace TEXT NOT NULL,
    description TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    owner TEXT NOT NULL,
    system_ref TEXT NOT NULL,
    adapter_abstracted INTEGER NOT NULL,
    cost_allocation_id TEXT NOT NULL,
    depends_on_json TEXT NOT NULL,
    vendor_name TEXT NOT NULL,
    renewal_date TEXT NOT NULL,
    last_audited_at TEXT NOT NULL,
    baseline_prod_dependents INTEGER NOT NULL,
    manual_override_variance INTEGER NOT NULL DEFAULT 0,
    fallback_scores_json TEXT NOT NULL,
    source_origin TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL,
    sync_pending INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sourcing_blind_ballots (
    ballot_id TEXT PRIMARY KEY,
    unit_name TEXT NOT NULL,
    reviewer_role TEXT NOT NULL,
    reviewer_id TEXT NOT NULL,
    y2_core_competitiveness REAL NOT NULL,
    y4_price_elasticity REAL NOT NULL,
    x2_market_scarcity REAL NOT NULL,
    x3_customization_depth REAL NOT NULL,
    x4_compliance_fragility REAL NOT NULL,
    evidence_notes TEXT NOT NULL,
    submitted_at TEXT NOT NULL,
    sync_pending INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sourcing_windmill_assessments (
    unit_name TEXT PRIMARY KEY,
    estimated_wallet_share_tier TEXT NOT NULL,
    account_attractiveness_score REAL NOT NULL,
    perceived_status TEXT NOT NULL,
    assessed_by TEXT NOT NULL,
    rationale TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    sync_pending INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sourcing_discovery_candidates (
    candidate_id TEXT PRIMARY KEY,
    package_or_endpoint TEXT NOT NULL,
    ecosystem TEXT NOT NULL,
    suggested_type TEXT NOT NULL,
    detected_in_path TEXT NOT NULL,
    inferred_system TEXT NOT NULL,
    inferred_components_json TEXT NOT NULL,
    adapter_detected INTEGER NOT NULL,
    monolith_risk_note TEXT NOT NULL,
    status TEXT NOT NULL,
    promoted_unit_name TEXT,
    discovered_at TEXT NOT NULL,
    sync_pending INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sourcing_audit_events (
    event_id TEXT PRIMARY KEY,
    unit_name TEXT NOT NULL,
    event_type TEXT NOT NULL,
    actor TEXT NOT NULL,
    channel TEXT NOT NULL,
    summary TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    sync_pending INTEGER NOT NULL DEFAULT 0
);
"#;

const POSTGRES_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS sourcing_units (
    name TEXT PRIMARY KEY,
    namespace TEXT NOT NULL,
    description TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    owner TEXT NOT NULL,
    system_ref TEXT NOT NULL,
    adapter_abstracted INTEGER NOT NULL,
    cost_allocation_id TEXT NOT NULL,
    depends_on_json TEXT NOT NULL,
    vendor_name TEXT NOT NULL,
    renewal_date TEXT NOT NULL,
    last_audited_at TEXT NOT NULL,
    baseline_prod_dependents INTEGER NOT NULL,
    manual_override_variance INTEGER NOT NULL DEFAULT 0,
    fallback_scores_json TEXT NOT NULL,
    source_origin TEXT NOT NULL,
    revision BIGINT NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL,
    sync_pending INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sourcing_blind_ballots (
    ballot_id TEXT PRIMARY KEY,
    unit_name TEXT NOT NULL,
    reviewer_role TEXT NOT NULL,
    reviewer_id TEXT NOT NULL,
    y2_core_competitiveness DOUBLE PRECISION NOT NULL,
    y4_price_elasticity DOUBLE PRECISION NOT NULL,
    x2_market_scarcity DOUBLE PRECISION NOT NULL,
    x3_customization_depth DOUBLE PRECISION NOT NULL,
    x4_compliance_fragility DOUBLE PRECISION NOT NULL,
    evidence_notes TEXT NOT NULL,
    submitted_at TEXT NOT NULL,
    sync_pending INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sourcing_windmill_assessments (
    unit_name TEXT PRIMARY KEY,
    estimated_wallet_share_tier TEXT NOT NULL,
    account_attractiveness_score DOUBLE PRECISION NOT NULL,
    perceived_status TEXT NOT NULL,
    assessed_by TEXT NOT NULL,
    rationale TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    sync_pending INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sourcing_discovery_candidates (
    candidate_id TEXT PRIMARY KEY,
    package_or_endpoint TEXT NOT NULL,
    ecosystem TEXT NOT NULL,
    suggested_type TEXT NOT NULL,
    detected_in_path TEXT NOT NULL,
    inferred_system TEXT NOT NULL,
    inferred_components_json TEXT NOT NULL,
    adapter_detected INTEGER NOT NULL,
    monolith_risk_note TEXT NOT NULL,
    status TEXT NOT NULL,
    promoted_unit_name TEXT,
    discovered_at TEXT NOT NULL,
    sync_pending INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sourcing_audit_events (
    event_id TEXT PRIMARY KEY,
    unit_name TEXT NOT NULL,
    event_type TEXT NOT NULL,
    actor TEXT NOT NULL,
    channel TEXT NOT NULL,
    summary TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    sync_pending INTEGER NOT NULL DEFAULT 0
);
"#;

#[derive(Debug, Clone)]
struct DbUnitRow {
    record: SourcingResourceRecord,
    manual_override_variance: bool,
    source_origin: String,
    revision: i64,
    updated_at: String,
    sync_pending: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct WindmillAssessmentRecord {
    unit_name: String,
    estimated_wallet_share_tier: String,
    account_attractiveness_score: f64,
    perceived_status: String,
    assessed_by: String,
    rationale: String,
    updated_at: String,
    sync_pending: bool,
}

#[derive(Debug, Clone)]
pub struct AggregatedBallotStats {
    pub mean_scores: ManualScores,
    pub max_spread: f64,
    pub variance_dimensions: Vec<String>,
    pub variance_alert: bool,
}

pub fn compute_ballot_stats(ballots: &[BlindBallotRecord]) -> Option<AggregatedBallotStats> {
    if ballots.is_empty() {
        return None;
    }
    let n = ballots.len() as f64;
    let sum_y2: f64 = ballots.iter().map(|b| b.y2_core_competitiveness).sum();
    let sum_y4: f64 = ballots.iter().map(|b| b.y4_price_elasticity).sum();
    let sum_x2: f64 = ballots.iter().map(|b| b.x2_market_scarcity).sum();
    let sum_x3: f64 = ballots.iter().map(|b| b.x3_customization_depth).sum();
    let sum_x4: f64 = ballots.iter().map(|b| b.x4_compliance_fragility).sum();

    let mean_scores = ManualScores {
        y2_core_competitiveness: round2(sum_y2 / n),
        y4_price_elasticity: round2(sum_y4 / n),
        x2_market_scarcity: round2(sum_x2 / n),
        x3_customization_depth: round2(sum_x3 / n),
        x4_compliance_fragility: round2(sum_x4 / n),
    };

    let mut max_spread = 0.0_f64;
    let mut variance_dimensions = Vec::new();
    if ballots.len() >= 2 {
        let dims: [(&str, Vec<f64>); 5] = [
            (
                "y2_coreCompetitiveness",
                ballots.iter().map(|b| b.y2_core_competitiveness).collect(),
            ),
            (
                "y4_priceElasticity",
                ballots.iter().map(|b| b.y4_price_elasticity).collect(),
            ),
            (
                "x2_marketScarcity",
                ballots.iter().map(|b| b.x2_market_scarcity).collect(),
            ),
            (
                "x3_customizationDepth",
                ballots.iter().map(|b| b.x3_customization_depth).collect(),
            ),
            (
                "x4_complianceFragility",
                ballots.iter().map(|b| b.x4_compliance_fragility).collect(),
            ),
        ];
        for (name, vals) in dims {
            let min_v = vals.iter().copied().fold(f64::INFINITY, f64::min);
            let max_v = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let spread = round2((max_v - min_v).max(0.0));
            if spread > max_spread {
                max_spread = spread;
            }
            if spread + 1e-9 >= VARIANCE_SPREAD_THRESHOLD {
                variance_dimensions.push(name.to_string());
            }
        }
    }

    let variance_alert = !variance_dimensions.is_empty();
    Some(AggregatedBallotStats {
        mean_scores,
        max_spread,
        variance_dimensions,
        variance_alert,
    })
}

pub fn redact_db_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Ok(mut parsed) = Url::parse(trimmed) {
        if parsed.password().is_some() {
            let _ = parsed.set_password(Some("***"));
        }
        return parsed.to_string();
    }
    // Fallback mask if malformed URL
    if let Some(at_pos) = trimmed.rfind('@') {
        if let Some(scheme_pos) = trimmed.find("://") {
            let prefix = &trimmed[..scheme_pos + 3];
            if let Some(colon) = trimmed[scheme_pos + 3..at_pos].find(':') {
                let user = &trimmed[scheme_pos + 3..scheme_pos + 3 + colon];
                return format!("{prefix}{user}:***@{}", &trimmed[at_pos + 1..]);
            }
        }
    }
    trimmed.to_string()
}

// ---------------------------------------------------------------------------
// Thread-Safe SQLite WAL Handle & Sync Metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SyncMetaState {
    pg_url: Option<String>,
    postgres_connected: bool,
    last_sync_at: Option<String>,
    last_sync_direction: Option<String>,
    last_sync_error: Option<String>,
}

pub struct SourcingDbHandle {
    conn: Mutex<SqliteConn>,
    sync_meta: Mutex<SyncMetaState>,
    last_seen_data_version: Mutex<i64>,
    sqlite_version: String,
    journal_mode: String,
    is_in_memory: bool,
}

impl std::fmt::Debug for SourcingDbHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourcingDbHandle")
            .field("sqlite_version", &self.sqlite_version)
            .field("journal_mode", &self.journal_mode)
            .field("is_in_memory", &self.is_in_memory)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct LoadedSourcingSnapshot {
    pub records: BTreeMap<String, SourcingResourceRecord>,
    pub source_origins: BTreeMap<String, String>,
    pub ballots_by_unit: BTreeMap<String, Vec<BlindBallotRecord>>,
    pub max_spread_by_unit: BTreeMap<String, f64>,
}

impl SourcingDbHandle {
    pub fn open_or_init(
        sqlite_path: PathBuf,
        in_memory: bool,
        pg_url: Option<String>,
        legacy_records: &BTreeMap<String, SourcingResourceRecord>,
    ) -> Result<Arc<Self>, String> {
        let conn = SqliteConn::open(&sqlite_path, in_memory)?;
        let journal_mode = if in_memory {
            "memory".to_string()
        } else {
            conn.query_rows("PRAGMA journal_mode=WAL;", &[])
                .ok()
                .and_then(|r| r.first().and_then(|c| c.first()).cloned().flatten())
                .unwrap_or_else(|| "wal".to_string())
        };
        if !in_memory {
            conn.exec_batch("PRAGMA synchronous=NORMAL;")?;
        }
        conn.exec_batch(SQLITE_SCHEMA_SQL)?;
        let sqlite_version = conn.version();
        let initial_dv = conn.pragma_data_version();

        let handle = Arc::new(Self {
            conn: Mutex::new(conn),
            sync_meta: Mutex::new(SyncMetaState {
                pg_url: pg_url.filter(|s| !s.trim().is_empty()),
                postgres_connected: false,
                last_sync_at: None,
                last_sync_direction: None,
                last_sync_error: None,
            }),
            last_seen_data_version: Mutex::new(initial_dv),
            sqlite_version,
            journal_mode,
            is_in_memory: in_memory,
        });

        handle.bootstrap_if_empty(legacy_records)?;
        Ok(handle)
    }

    pub fn set_postgres_url(&self, pg_url: Option<String>) {
        if let Ok(mut meta) = self.sync_meta.lock() {
            let cleaned = pg_url.filter(|s| !s.trim().is_empty());
            if meta.pg_url != cleaned {
                meta.postgres_connected = false;
                meta.last_sync_error = None;
            }
            meta.pg_url = cleaned;
        }
    }

    pub fn postgres_url(&self) -> Option<String> {
        self.sync_meta.lock().ok().and_then(|m| m.pg_url.clone())
    }

    pub fn check_external_db_mutation(&self) -> bool {
        if self.is_in_memory {
            return false;
        }
        let dv = {
            let Ok(conn) = self.conn.lock() else {
                return false;
            };
            conn.pragma_data_version()
        };
        let Ok(mut prev) = self.last_seen_data_version.lock() else {
            return false;
        };
        if dv != *prev {
            *prev = dv;
            true
        } else {
            false
        }
    }

    fn update_data_version(&self, conn: &SqliteConn) {
        if !self.is_in_memory {
            if let Ok(mut prev) = self.last_seen_data_version.lock() {
                *prev = conn.pragma_data_version();
            }
        }
    }

    fn bootstrap_if_empty(
        &self,
        legacy_records: &BTreeMap<String, SourcingResourceRecord>,
    ) -> Result<(), String> {
        let now_iso = Utc::now().to_rfc3339();
        let conn = self.conn.lock().map_err(|_| "DB lock poisoned".to_string())?;
        let count_rows = conn.query_rows("SELECT COUNT(*) FROM sourcing_units;", &[])?;
        let existing_count = count_rows
            .first()
            .and_then(|r| r.first())
            .and_then(|o| o.as_deref())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        if existing_count > 0 {
            return Ok(());
        }

        // Layer legacy state/workspace records over embedded seeds so initial DB matches existing truth.
        let mut combined: BTreeMap<String, SourcingResourceRecord> = BTreeMap::new();
        for (_fname, content) in SEED_YAMLS {
            if let Ok(rec) = parse_sourcing_yaml_str(content) {
                combined.insert(rec.name.to_ascii_lowercase(), rec);
            }
        }
        for (k, rec) in legacy_records {
            combined.insert(k.to_ascii_lowercase(), rec.clone());
        }

        conn.exec_batch("BEGIN IMMEDIATE;")?;
        let res = (|| -> Result<(), String> {
            for rec in combined.values() {
                let unit_key = rec.name.to_ascii_lowercase();
                let deps_json = serde_json::to_string(&rec.depends_on_components)
                    .unwrap_or_else(|_| "[]".to_string());
                let scores_json = serde_json::to_string(&rec.audit.manual_scores)
                    .unwrap_or_else(|_| "{}".to_string());
                conn.execute(
                    "INSERT INTO sourcing_units (
                        name, namespace, description, resource_type, owner, system_ref,
                        adapter_abstracted, cost_allocation_id, depends_on_json, vendor_name,
                        renewal_date, last_audited_at, baseline_prod_dependents,
                        manual_override_variance, fallback_scores_json, source_origin,
                        revision, updated_at, sync_pending
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, 1);",
                    &[
                        SqlVal::Text(unit_key.clone()),
                        SqlVal::Text(rec.namespace.clone()),
                        SqlVal::Text(rec.description.clone()),
                        SqlVal::Text(rec.resource_type.to_ascii_lowercase()),
                        SqlVal::Text(rec.owner.clone()),
                        SqlVal::Text(rec.system.clone()),
                        SqlVal::Int(if rec.adapter_abstracted { 1 } else { 0 }),
                        SqlVal::Text(rec.cost_allocation_id.clone()),
                        SqlVal::Text(deps_json),
                        SqlVal::Text(rec.audit.contract_meta.vendor_name.clone()),
                        SqlVal::Text(rec.audit.contract_meta.renewal_date.clone()),
                        SqlVal::Text(rec.audit.contract_meta.last_audited_at.clone()),
                        SqlVal::Int(rec.audit.contract_meta.baseline_prod_dependents as i64),
                        SqlVal::Int(if rec.audit.contract_meta.blind_score_variance_alert {
                            1
                        } else {
                            0
                        }),
                        SqlVal::Text(scores_json),
                        SqlVal::Text("seed-baseline".to_string()),
                        SqlVal::Text(now_iso.clone()),
                    ],
                )?;

                conn.execute(
                    "INSERT INTO sourcing_windmill_assessments (
                        unit_name, estimated_wallet_share_tier, account_attractiveness_score,
                        perceived_status, assessed_by, rationale, updated_at, sync_pending
                    ) VALUES (?, ?, ?, ?, 'procurement-seed', 'Initial Dutch Windmill counterparty baseline', ?, 1);",
                    &[
                        SqlVal::Text(unit_key.clone()),
                        SqlVal::Text(
                            rec.audit
                                .supplier_preference
                                .estimated_wallet_share_tier
                                .clone(),
                        ),
                        SqlVal::Real(
                            rec.audit
                                .supplier_preference
                                .account_attractiveness_score,
                        ),
                        SqlVal::Text(rec.audit.supplier_preference.perceived_status.clone()),
                        SqlVal::Text(now_iso.clone()),
                    ],
                )?;

                // Seed realistic architecture + procurement blind ballots whose averages equal `manual_scores`.
                let m = &rec.audit.manual_scores;
                let (arch_x3, proc_x3, arch_note, proc_note) =
                    if unit_key == "commercial-mip-solver-sdk" {
                        // Mean = (4.4 + 2.6)/2 = 3.50; spread = 1.80 >= 1.5 -> triggers variance alert!
                        (
                            4.4,
                            2.6,
                            "Architecture rates custom MIP constraint hooks high (4.4)".to_string(),
                            "Procurement rates standard solver packaging lower customization (2.6)"
                                .to_string(),
                        )
                    } else {
                        (
                            m.x3_customization_depth,
                            m.x3_customization_depth,
                            "Architecture dependency & integration complexity review".to_string(),
                            "Procurement market & switching flexibility review".to_string(),
                        )
                    };

                for (role, reviewer, x3_val, note) in [
                    ("architecture", "arch-lead@pits.local", arch_x3, arch_note),
                    ("procurement", "sourcing-ops@pits.local", proc_x3, proc_note),
                ] {
                    let ballot_id = format!("{unit_key}:{role}");
                    conn.execute(
                        "INSERT INTO sourcing_blind_ballots (
                            ballot_id, unit_name, reviewer_role, reviewer_id,
                            y2_core_competitiveness, y4_price_elasticity,
                            x2_market_scarcity, x3_customization_depth, x4_compliance_fragility,
                            evidence_notes, submitted_at, sync_pending
                        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1);",
                        &[
                            SqlVal::Text(ballot_id),
                            SqlVal::Text(unit_key.clone()),
                            SqlVal::Text(role.to_string()),
                            SqlVal::Text(reviewer.to_string()),
                            SqlVal::Real(m.y2_core_competitiveness),
                            SqlVal::Real(m.y4_price_elasticity),
                            SqlVal::Real(m.x2_market_scarcity),
                            SqlVal::Real(x3_val),
                            SqlVal::Real(m.x4_compliance_fragility),
                            SqlVal::Text(note),
                            SqlVal::Text(now_iso.clone()),
                        ],
                    )?;
                }
            }

            conn.execute(
                "INSERT INTO sourcing_audit_events (
                    event_id, unit_name, event_type, actor, channel, summary, payload_json, created_at, sync_pending
                ) VALUES (?, '_portfolio', 'db-bootstrap', 'pitsctl', 'sqlite-wal', ?, ?, ?, 1);",
                &[
                    SqlVal::Text(Uuid::new_v4().to_string()),
                    SqlVal::Text(format!(
                        "Initialized relational sourcing store with {} units and multi-role blind ballots",
                        combined.len()
                    )),
                    SqlVal::Text(json!({ "seededUnits": combined.len() }).to_string()),
                    SqlVal::Text(now_iso.clone()),
                ],
            )?;
            Ok(())
        })();

        match res {
            Ok(()) => {
                conn.exec_batch("COMMIT;")?;
                self.update_data_version(&conn);
                Ok(())
            }
            Err(e) => {
                let _ = conn.exec_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    pub fn load_snapshot_records(&self) -> Result<LoadedSourcingSnapshot, String> {
        let conn = self.conn.lock().map_err(|_| "DB lock poisoned".to_string())?;
        self.load_snapshot_records_with_conn(&conn)
    }

    fn load_snapshot_records_with_conn(
        &self,
        conn: &SqliteConn,
    ) -> Result<LoadedSourcingSnapshot, String> {
        let unit_rows = query_unit_rows_sqlite(conn)?;
        let ballot_rows = query_ballots_sqlite(conn)?;
        let windmill_rows = query_windmill_sqlite(conn)?;

        let mut ballots_by_unit: BTreeMap<String, Vec<BlindBallotRecord>> = BTreeMap::new();
        for b in ballot_rows {
            ballots_by_unit
                .entry(b.unit_name.to_ascii_lowercase())
                .or_default()
                .push(b);
        }
        let mut windmills_by_unit: BTreeMap<String, WindmillAssessmentRecord> = BTreeMap::new();
        for w in windmill_rows {
            windmills_by_unit.insert(w.unit_name.to_ascii_lowercase(), w);
        }

        let mut records = BTreeMap::new();
        let mut source_origins = BTreeMap::new();
        let mut max_spread_by_unit = BTreeMap::new();

        for mut row in unit_rows {
            let key = row.record.name.to_ascii_lowercase();
            if let Some(w) = windmills_by_unit.get(&key) {
                row.record.audit.supplier_preference.estimated_wallet_share_tier =
                    w.estimated_wallet_share_tier.clone();
                row.record.audit.supplier_preference.account_attractiveness_score =
                    w.account_attractiveness_score;
                row.record.audit.supplier_preference.perceived_status = w.perceived_status.clone();
            }
            if let Some(ballots) = ballots_by_unit.get(&key) {
                if let Some(stats) = compute_ballot_stats(ballots) {
                    max_spread_by_unit.insert(key.clone(), stats.max_spread);
                    if row.source_origin != "wizard-fallback"
                        && row.source_origin != "action-calibrate"
                    {
                        row.record.audit.manual_scores = stats.mean_scores;
                        row.record.audit.contract_meta.blind_score_variance_alert =
                            stats.variance_alert || row.manual_override_variance;
                    }
                }
            }
            source_origins.insert(key.clone(), row.source_origin);
            records.insert(key, row.record);
        }

        Ok(LoadedSourcingSnapshot {
            records,
            source_origins,
            ballots_by_unit,
            max_spread_by_unit,
        })
    }

    pub fn upsert_unit_record(
        &self,
        record: &SourcingResourceRecord,
        source_origin: &str,
        actor: &str,
        channel: &str,
        event_summary: &str,
        clear_manual_variance_flag: bool,
    ) -> Result<(), String> {
        let now_iso = Utc::now().to_rfc3339();
        let key = record.name.trim().to_ascii_lowercase();
        let deps_json = serde_json::to_string(&record.depends_on_components)
            .unwrap_or_else(|_| "[]".to_string());
        let scores_json = serde_json::to_string(&record.audit.manual_scores)
            .unwrap_or_else(|_| "{}".to_string());

        let conn = self.conn.lock().map_err(|_| "DB lock poisoned".to_string())?;
        conn.exec_batch("BEGIN IMMEDIATE;")?;
        let res = (|| -> Result<(), String> {
            let existing_origin: Option<(String, i64)> = conn
                .query_rows(
                    "SELECT source_origin, revision FROM sourcing_units WHERE name = ?;",
                    &[SqlVal::Text(key.clone())],
                )?
                .into_iter()
                .next()
                .map(|r| {
                    (
                        r.first().cloned().flatten().unwrap_or_else(|| "seed-baseline".to_string()),
                        r.get(1)
                            .cloned()
                            .flatten()
                            .and_then(|s| s.parse::<i64>().ok())
                            .unwrap_or(1),
                    )
                });

            let (effective_origin, next_rev) = match existing_origin {
                Some((prev_origin, prev_rev)) => {
                    let chosen = if source_origin == "preserve" {
                        prev_origin
                    } else {
                        source_origin.to_string()
                    };
                    (chosen, prev_rev + 1)
                }
                None => (
                    if source_origin == "preserve" {
                        "mcp-discovery".to_string()
                    } else {
                        source_origin.to_string()
                    },
                    1,
                ),
            };

            let manual_var = if clear_manual_variance_flag {
                0
            } else if record.audit.contract_meta.blind_score_variance_alert {
                1
            } else {
                0
            };

            conn.execute(
                "INSERT INTO sourcing_units (
                    name, namespace, description, resource_type, owner, system_ref,
                    adapter_abstracted, cost_allocation_id, depends_on_json, vendor_name,
                    renewal_date, last_audited_at, baseline_prod_dependents,
                    manual_override_variance, fallback_scores_json, source_origin,
                    revision, updated_at, sync_pending
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)
                ON CONFLICT(name) DO UPDATE SET
                    namespace=excluded.namespace,
                    description=excluded.description,
                    resource_type=excluded.resource_type,
                    owner=excluded.owner,
                    system_ref=excluded.system_ref,
                    adapter_abstracted=excluded.adapter_abstracted,
                    cost_allocation_id=excluded.cost_allocation_id,
                    depends_on_json=excluded.depends_on_json,
                    vendor_name=excluded.vendor_name,
                    renewal_date=excluded.renewal_date,
                    last_audited_at=excluded.last_audited_at,
                    baseline_prod_dependents=excluded.baseline_prod_dependents,
                    manual_override_variance=excluded.manual_override_variance,
                    fallback_scores_json=excluded.fallback_scores_json,
                    source_origin=excluded.source_origin,
                    revision=excluded.revision,
                    updated_at=excluded.updated_at,
                    sync_pending=1;",
                &[
                    SqlVal::Text(key.clone()),
                    SqlVal::Text(if record.namespace.trim().is_empty() {
                        "default".to_string()
                    } else {
                        record.namespace.trim().to_string()
                    }),
                    SqlVal::Text(record.description.clone()),
                    SqlVal::Text(record.resource_type.trim().to_ascii_lowercase()),
                    SqlVal::Text(record.owner.clone()),
                    SqlVal::Text(record.system.clone()),
                    SqlVal::Int(if record.adapter_abstracted { 1 } else { 0 }),
                    SqlVal::Text(record.cost_allocation_id.clone()),
                    SqlVal::Text(deps_json),
                    SqlVal::Text(record.audit.contract_meta.vendor_name.clone()),
                    SqlVal::Text(record.audit.contract_meta.renewal_date.clone()),
                    SqlVal::Text(record.audit.contract_meta.last_audited_at.clone()),
                    SqlVal::Int(record.audit.contract_meta.baseline_prod_dependents as i64),
                    SqlVal::Int(manual_var),
                    SqlVal::Text(scores_json),
                    SqlVal::Text(effective_origin.clone()),
                    SqlVal::Int(next_rev),
                    SqlVal::Text(now_iso.clone()),
                ],
            )?;

            conn.execute(
                "INSERT INTO sourcing_windmill_assessments (
                    unit_name, estimated_wallet_share_tier, account_attractiveness_score,
                    perceived_status, assessed_by, rationale, updated_at, sync_pending
                ) VALUES (?, ?, ?, ?, ?, ?, ?, 1)
                ON CONFLICT(unit_name) DO UPDATE SET
                    estimated_wallet_share_tier=excluded.estimated_wallet_share_tier,
                    account_attractiveness_score=excluded.account_attractiveness_score,
                    perceived_status=excluded.perceived_status,
                    assessed_by=excluded.assessed_by,
                    rationale=excluded.rationale,
                    updated_at=excluded.updated_at,
                    sync_pending=1;",
                &[
                    SqlVal::Text(key.clone()),
                    SqlVal::Text(
                        record
                            .audit
                            .supplier_preference
                            .estimated_wallet_share_tier
                            .clone(),
                    ),
                    SqlVal::Real(
                        record
                            .audit
                            .supplier_preference
                            .account_attractiveness_score,
                    ),
                    SqlVal::Text(record.audit.supplier_preference.perceived_status.clone()),
                    SqlVal::Text(actor.to_string()),
                    SqlVal::Text(event_summary.to_string()),
                    SqlVal::Text(now_iso.clone()),
                ],
            )?;

            insert_event_sqlite(
                &conn,
                &key,
                &effective_origin,
                actor,
                channel,
                event_summary,
                json!({
                    "resourceType": record.resource_type,
                    "system": record.system,
                    "adapterAbstracted": record.adapter_abstracted,
                    "revision": next_rev,
                }),
                &now_iso,
            )?;
            Ok(())
        })();

        match res {
            Ok(()) => {
                conn.exec_batch("COMMIT;")?;
                self.update_data_version(&conn);
                Ok(())
            }
            Err(e) => {
                let _ = conn.exec_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    pub fn submit_blind_ballot(
        &self,
        req: &SubmitBlindBallotRequest,
        channel: &str,
    ) -> Result<AggregatedBallotStats, String> {
        let unit_key = req.unit_name.trim().to_ascii_lowercase();
        let role = req.reviewer_role.trim().to_ascii_lowercase();
        if !REVIEWER_ROLES.contains(&role.as_str()) {
            return Err(format!(
                "Invalid reviewerRole '{}'; must be one of [{}]",
                req.reviewer_role,
                REVIEWER_ROLES.join(", ")
            ));
        }
        if req.reviewer_id.trim().is_empty() {
            return Err("reviewerId cannot be empty".to_string());
        }
        for (k, v) in [
            ("y2_coreCompetitiveness", req.y2_core_competitiveness),
            ("y4_priceElasticity", req.y4_price_elasticity),
            ("x2_marketScarcity", req.x2_market_scarcity),
            ("x3_customizationDepth", req.x3_customization_depth),
            ("x4_complianceFragility", req.x4_compliance_fragility),
        ] {
            if !v.is_finite() || !(1.0..=5.0).contains(&v) {
                return Err(format!("Ballot score '{k}' = {v} must be within 1.0..=5.0"));
            }
        }

        let now_iso = Utc::now().to_rfc3339();
        let today_str = Utc::now().format("%Y-%m-%d").to_string();
        let ballot_id = format!("{unit_key}:{role}");

        let conn = self.conn.lock().map_err(|_| "DB lock poisoned".to_string())?;
        let exists = !conn
            .query_rows(
                "SELECT name FROM sourcing_units WHERE name = ?;",
                &[SqlVal::Text(unit_key.clone())],
            )?
            .is_empty();
        if !exists {
            return Err(format!(
                "Unknown sourcing unit '{}'; promote or create it first",
                req.unit_name
            ));
        }

        conn.exec_batch("BEGIN IMMEDIATE;")?;
        let res = (|| -> Result<AggregatedBallotStats, String> {
            conn.execute(
                "INSERT INTO sourcing_blind_ballots (
                    ballot_id, unit_name, reviewer_role, reviewer_id,
                    y2_core_competitiveness, y4_price_elasticity,
                    x2_market_scarcity, x3_customization_depth, x4_compliance_fragility,
                    evidence_notes, submitted_at, sync_pending
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)
                ON CONFLICT(ballot_id) DO UPDATE SET
                    reviewer_id=excluded.reviewer_id,
                    y2_core_competitiveness=excluded.y2_core_competitiveness,
                    y4_price_elasticity=excluded.y4_price_elasticity,
                    x2_market_scarcity=excluded.x2_market_scarcity,
                    x3_customization_depth=excluded.x3_customization_depth,
                    x4_compliance_fragility=excluded.x4_compliance_fragility,
                    evidence_notes=excluded.evidence_notes,
                    submitted_at=excluded.submitted_at,
                    sync_pending=1;",
                &[
                    SqlVal::Text(ballot_id.clone()),
                    SqlVal::Text(unit_key.clone()),
                    SqlVal::Text(role.clone()),
                    SqlVal::Text(req.reviewer_id.trim().to_string()),
                    SqlVal::Real(req.y2_core_competitiveness),
                    SqlVal::Real(req.y4_price_elasticity),
                    SqlVal::Real(req.x2_market_scarcity),
                    SqlVal::Real(req.x3_customization_depth),
                    SqlVal::Real(req.x4_compliance_fragility),
                    SqlVal::Text(req.evidence_notes.clone()),
                    SqlVal::Text(now_iso.clone()),
                ],
            )?;

            let unit_ballots = query_ballots_for_unit_sqlite(&conn, &unit_key)?;
            let stats = compute_ballot_stats(&unit_ballots)
                .ok_or_else(|| "Ballot aggregation unexpectedly empty".to_string())?;
            let mean_json =
                serde_json::to_string(&stats.mean_scores).unwrap_or_else(|_| "{}".to_string());

            conn.execute(
                "UPDATE sourcing_units
                 SET last_audited_at = ?,
                     manual_override_variance = ?,
                     fallback_scores_json = ?,
                     source_origin = 'mcp-blind-ballot',
                     revision = revision + 1,
                     updated_at = ?,
                     sync_pending = 1
                 WHERE name = ?;",
                &[
                    SqlVal::Text(today_str),
                    SqlVal::Int(if stats.variance_alert { 1 } else { 0 }),
                    SqlVal::Text(mean_json),
                    SqlVal::Text(now_iso.clone()),
                    SqlVal::Text(unit_key.clone()),
                ],
            )?;

            let summary = format!(
                "Submitted '{}' blind ballot by {} (spread={:.2}, varianceAlert={})",
                role, req.reviewer_id, stats.max_spread, stats.variance_alert
            );
            insert_event_sqlite(
                &conn,
                &unit_key,
                "blind-ballot",
                req.reviewer_id.trim(),
                channel,
                &summary,
                json!({
                    "role": role,
                    "ballotCount": unit_ballots.len(),
                    "maxSpread": stats.max_spread,
                    "varianceDimensions": stats.variance_dimensions,
                    "varianceAlert": stats.variance_alert,
                }),
                &now_iso,
            )?;

            Ok(stats)
        })();

        match res {
            Ok(stats) => {
                conn.exec_batch("COMMIT;")?;
                self.update_data_version(&conn);
                Ok(stats)
            }
            Err(e) => {
                let _ = conn.exec_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    pub fn assess_windmill(&self, req: &AssessWindmillRequest, channel: &str) -> Result<(), String> {
        let unit_key = req.unit_name.trim().to_ascii_lowercase();
        let status = req.perceived_status.trim().to_ascii_lowercase();
        let wallet = req.estimated_wallet_share_tier.trim().to_ascii_lowercase();
        if !PERCEIVED_STATUSES.contains(&status.as_str()) {
            return Err(format!(
                "Invalid perceivedStatus '{}'; expected [{}]",
                req.perceived_status,
                PERCEIVED_STATUSES.join(", ")
            ));
        }
        if !WALLET_SHARE_TIERS.contains(&wallet.as_str()) {
            return Err(format!(
                "Invalid estimatedWalletShareTier '{}'; expected [{}]",
                req.estimated_wallet_share_tier,
                WALLET_SHARE_TIERS.join(", ")
            ));
        }
        if !req.account_attractiveness_score.is_finite()
            || !(1.0..=5.0).contains(&req.account_attractiveness_score)
        {
            return Err("accountAttractivenessScore must be finite within 1.0..=5.0".to_string());
        }
        let now_iso = Utc::now().to_rfc3339();
        let today_str = Utc::now().format("%Y-%m-%d").to_string();

        let conn = self.conn.lock().map_err(|_| "DB lock poisoned".to_string())?;
        let exists = !conn
            .query_rows(
                "SELECT name FROM sourcing_units WHERE name = ?;",
                &[SqlVal::Text(unit_key.clone())],
            )?
            .is_empty();
        if !exists {
            return Err(format!("Unknown sourcing unit '{}'", req.unit_name));
        }

        conn.exec_batch("BEGIN IMMEDIATE;")?;
        let res = (|| -> Result<(), String> {
            conn.execute(
                "INSERT INTO sourcing_windmill_assessments (
                    unit_name, estimated_wallet_share_tier, account_attractiveness_score,
                    perceived_status, assessed_by, rationale, updated_at, sync_pending
                ) VALUES (?, ?, ?, ?, ?, ?, ?, 1)
                ON CONFLICT(unit_name) DO UPDATE SET
                    estimated_wallet_share_tier=excluded.estimated_wallet_share_tier,
                    account_attractiveness_score=excluded.account_attractiveness_score,
                    perceived_status=excluded.perceived_status,
                    assessed_by=excluded.assessed_by,
                    rationale=excluded.rationale,
                    updated_at=excluded.updated_at,
                    sync_pending=1;",
                &[
                    SqlVal::Text(unit_key.clone()),
                    SqlVal::Text(wallet.clone()),
                    SqlVal::Real(req.account_attractiveness_score),
                    SqlVal::Text(status.clone()),
                    SqlVal::Text(req.assessed_by.trim().to_string()),
                    SqlVal::Text(req.rationale.clone()),
                    SqlVal::Text(now_iso.clone()),
                ],
            )?;
            conn.execute(
                "UPDATE sourcing_units SET last_audited_at = ?, revision = revision + 1, updated_at = ?, sync_pending = 1 WHERE name = ?;",
                &[
                    SqlVal::Text(today_str),
                    SqlVal::Text(now_iso.clone()),
                    SqlVal::Text(unit_key.clone()),
                ],
            )?;
            insert_event_sqlite(
                &conn,
                &unit_key,
                "windmill-assessment",
                req.assessed_by.trim(),
                channel,
                &format!(
                    "Assessed supplier perceivedStatus='{}', wallet='{}', attr={:.1}",
                    status, wallet, req.account_attractiveness_score
                ),
                json!({
                    "perceivedStatus": status,
                    "estimatedWalletShareTier": wallet,
                    "accountAttractivenessScore": req.account_attractiveness_score,
                    "rationale": req.rationale,
                }),
                &now_iso,
            )?;
            Ok(())
        })();

        match res {
            Ok(()) => {
                conn.exec_batch("COMMIT;")?;
                self.update_data_version(&conn);
                Ok(())
            }
            Err(e) => {
                let _ = conn.exec_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    pub fn upsert_discovery_candidates(
        &self,
        candidates: &[DiscoveryCandidateRecord],
        actor: &str,
        channel: &str,
    ) -> Result<Vec<DiscoveryCandidateRecord>, String> {
        let now_iso = Utc::now().to_rfc3339();
        let conn = self.conn.lock().map_err(|_| "DB lock poisoned".to_string())?;
        conn.exec_batch("BEGIN IMMEDIATE;")?;
        let res = (|| -> Result<(), String> {
            for c in candidates {
                let comps_json = serde_json::to_string(&c.inferred_components)
                    .unwrap_or_else(|_| "[]".to_string());
                // Preserve `promoted` or `ignored` status if already triaged unless newly promoted.
                let existing_status: Option<(String, Option<String>)> = conn
                    .query_rows(
                        "SELECT status, promoted_unit_name FROM sourcing_discovery_candidates WHERE candidate_id = ?;",
                        &[SqlVal::Text(c.candidate_id.clone())],
                    )?
                    .into_iter()
                    .next()
                    .map(|r| {
                        (
                            r.first().cloned().flatten().unwrap_or_else(|| "pending".to_string()),
                            r.get(1).cloned().flatten(),
                        )
                    });
                let (eff_status, eff_promoted) = match existing_status {
                    Some((st, prom)) if st == "promoted" || st == "ignored" => (st, prom),
                    _ => (c.status.clone(), c.promoted_unit_name.clone()),
                };

                conn.execute(
                    "INSERT INTO sourcing_discovery_candidates (
                        candidate_id, package_or_endpoint, ecosystem, suggested_type,
                        detected_in_path, inferred_system, inferred_components_json,
                        adapter_detected, monolith_risk_note, status, promoted_unit_name,
                        discovered_at, sync_pending
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)
                    ON CONFLICT(candidate_id) DO UPDATE SET
                        package_or_endpoint=excluded.package_or_endpoint,
                        ecosystem=excluded.ecosystem,
                        suggested_type=excluded.suggested_type,
                        detected_in_path=excluded.detected_in_path,
                        inferred_system=excluded.inferred_system,
                        inferred_components_json=excluded.inferred_components_json,
                        adapter_detected=excluded.adapter_detected,
                        monolith_risk_note=excluded.monolith_risk_note,
                        status=excluded.status,
                        promoted_unit_name=excluded.promoted_unit_name,
                        discovered_at=excluded.discovered_at,
                        sync_pending=1;",
                    &[
                        SqlVal::Text(c.candidate_id.clone()),
                        SqlVal::Text(c.package_or_endpoint.clone()),
                        SqlVal::Text(c.ecosystem.clone()),
                        SqlVal::Text(c.suggested_type.clone()),
                        SqlVal::Text(c.detected_in_path.clone()),
                        SqlVal::Text(c.inferred_system.clone()),
                        SqlVal::Text(comps_json),
                        SqlVal::Int(if c.adapter_detected { 1 } else { 0 }),
                        SqlVal::Text(c.monolith_risk_note.clone()),
                        SqlVal::Text(eff_status),
                        SqlVal::opt_text(eff_promoted.as_deref()),
                        SqlVal::Text(c.discovered_at.clone()),
                    ],
                )?;
            }

            insert_event_sqlite(
                &conn,
                "_discovery",
                "workspace-scan",
                actor,
                channel,
                &format!(
                    "MCP workspace dependency & adapter scanner indexed {} candidate(s)",
                    candidates.len()
                ),
                json!({ "candidateCount": candidates.len() }),
                &now_iso,
            )?;
            Ok(())
        })();

        match res {
            Ok(()) => {
                conn.exec_batch("COMMIT;")?;
                self.update_data_version(&conn);
                query_candidates_sqlite(&conn, None)
            }
            Err(e) => {
                let _ = conn.exec_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    pub fn update_candidate_disposition(
        &self,
        candidate_id: &str,
        status: &str,
        promoted_unit_name: Option<&str>,
        actor: &str,
        channel: &str,
    ) -> Result<DiscoveryCandidateRecord, String> {
        let now_iso = Utc::now().to_rfc3339();
        let conn = self.conn.lock().map_err(|_| "DB lock poisoned".to_string())?;
        let found = query_candidates_sqlite(&conn, None)?
            .into_iter()
            .find(|c| c.candidate_id == candidate_id)
            .ok_or_else(|| format!("Discovery candidate '{}' not found", candidate_id))?;

        conn.exec_batch("BEGIN IMMEDIATE;")?;
        let res = (|| -> Result<(), String> {
            conn.execute(
                "UPDATE sourcing_discovery_candidates
                 SET status = ?, promoted_unit_name = ?, sync_pending = 1
                 WHERE candidate_id = ?;",
                &[
                    SqlVal::Text(status.to_string()),
                    SqlVal::opt_text(promoted_unit_name),
                    SqlVal::Text(candidate_id.to_string()),
                ],
            )?;
            insert_event_sqlite(
                &conn,
                promoted_unit_name.unwrap_or("_discovery"),
                &format!("candidate-{status}"),
                actor,
                channel,
                &format!(
                    "Candidate '{}' ({}) marked '{}'",
                    found.package_or_endpoint, candidate_id, status
                ),
                json!({
                    "candidateId": candidate_id,
                    "status": status,
                    "promotedUnitName": promoted_unit_name,
                }),
                &now_iso,
            )?;
            Ok(())
        })();

        match res {
            Ok(()) => {
                conn.exec_batch("COMMIT;")?;
                self.update_data_version(&conn);
                query_candidates_sqlite(&conn, None)?
                    .into_iter()
                    .find(|c| c.candidate_id == candidate_id)
                    .ok_or_else(|| "Disappeared candidate".to_string())
            }
            Err(e) => {
                let _ = conn.exec_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    pub fn append_event(
        &self,
        unit_name: &str,
        event_type: &str,
        actor: &str,
        channel: &str,
        summary: &str,
        payload: Value,
    ) -> Result<(), String> {
        let now_iso = Utc::now().to_rfc3339();
        let conn = self.conn.lock().map_err(|_| "DB lock poisoned".to_string())?;
        insert_event_sqlite(
            &conn,
            unit_name,
            event_type,
            actor,
            channel,
            summary,
            payload,
            &now_iso,
        )?;
        self.update_data_version(&conn);
        Ok(())
    }

    pub fn list_ballots(&self, unit_name: Option<&str>) -> Result<Vec<BlindBallotRecord>, String> {
        let conn = self.conn.lock().map_err(|_| "DB lock poisoned".to_string())?;
        let mut all = query_ballots_sqlite(&conn)?;
        if let Some(u) = unit_name {
            let low = u.trim().to_ascii_lowercase();
            all.retain(|b| b.unit_name.eq_ignore_ascii_case(&low));
        }
        Ok(all)
    }

    pub fn list_candidates(
        &self,
        status_filter: Option<&str>,
    ) -> Result<Vec<DiscoveryCandidateRecord>, String> {
        let conn = self.conn.lock().map_err(|_| "DB lock poisoned".to_string())?;
        query_candidates_sqlite(&conn, status_filter)
    }

    pub fn list_recent_events(&self, limit: usize) -> Result<Vec<SourcingAuditEvent>, String> {
        let conn = self.conn.lock().map_err(|_| "DB lock poisoned".to_string())?;
        query_events_sqlite(&conn, limit)
    }

    pub fn sync_status(&self) -> SourcingDbSyncStatus {
        let (pending_outbox_rows, total_units, total_ballots, total_candidates, total_events, sqlite_path) =
            match self.conn.lock() {
                Ok(conn) => {
                    let count_q = |sql: &str| -> usize {
                        conn.query_rows(sql, &[])
                            .ok()
                            .and_then(|r| r.first().and_then(|c| c.first()).cloned().flatten())
                            .and_then(|s| s.parse::<usize>().ok())
                            .unwrap_or(0)
                    };
                    let pending = count_q("SELECT COUNT(*) FROM sourcing_units WHERE sync_pending = 1;")
                        + count_q("SELECT COUNT(*) FROM sourcing_blind_ballots WHERE sync_pending = 1;")
                        + count_q("SELECT COUNT(*) FROM sourcing_windmill_assessments WHERE sync_pending = 1;")
                        + count_q("SELECT COUNT(*) FROM sourcing_discovery_candidates WHERE sync_pending = 1;")
                        + count_q("SELECT COUNT(*) FROM sourcing_audit_events WHERE sync_pending = 1;");
                    let u = count_q("SELECT COUNT(*) FROM sourcing_units;");
                    let b = count_q("SELECT COUNT(*) FROM sourcing_blind_ballots;");
                    let c = count_q("SELECT COUNT(*) FROM sourcing_discovery_candidates;");
                    let e = count_q("SELECT COUNT(*) FROM sourcing_audit_events;");
                    (pending, u, b, c, e, conn.path_label.clone())
                }
                Err(_) => (0, 0, 0, 0, 0, String::new()),
            };

        let meta = self
            .sync_meta
            .lock()
            .map(|m| m.clone())
            .unwrap_or(SyncMetaState {
                pg_url: None,
                postgres_connected: false,
                last_sync_at: None,
                last_sync_direction: None,
                last_sync_error: None,
            });

        let pg_configured = meta.pg_url.is_some();
        let backend_mode = if pg_configured {
            if meta.postgres_connected {
                "postgresql+sqlite-wal".to_string()
            } else {
                "sqlite-wal-outbox-degraded".to_string()
            }
        } else {
            "sqlite-wal-local".to_string()
        };

        SourcingDbSyncStatus {
            backend_mode,
            sqlite_version: self.sqlite_version.clone(),
            sqlite_path,
            sqlite_journal_mode: self.journal_mode.clone(),
            postgres_configured: pg_configured,
            postgres_endpoint_redacted: meta.pg_url.as_deref().map(redact_db_url),
            postgres_connected: meta.postgres_connected,
            pending_outbox_rows,
            last_sync_at: meta.last_sync_at,
            last_sync_direction: meta.last_sync_direction,
            last_sync_error: meta.last_sync_error,
            total_units,
            total_ballots,
            total_candidates,
            total_events,
        }
    }

    pub async fn sync_with_postgres(&self, pg_url_override: Option<&str>) -> SourcingDbSyncStatus {
        let target_url = pg_url_override
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| self.postgres_url());

        let Some(pg_url) = target_url else {
            return self.sync_status();
        };

        // Update configured URL if caller provided an explicit override
        self.set_postgres_url(Some(pg_url.clone()));

        match self.run_postgres_bidirectional_sync(&pg_url).await {
            Ok((pushed, pulled)) => {
                if let Ok(mut meta) = self.sync_meta.lock() {
                    meta.postgres_connected = true;
                    meta.last_sync_at = Some(Utc::now().to_rfc3339());
                    meta.last_sync_direction = Some(format!("push:{pushed}/pull:{pulled}"));
                    meta.last_sync_error = None;
                }
            }
            Err(err) => {
                if let Ok(mut meta) = self.sync_meta.lock() {
                    meta.postgres_connected = false;
                    meta.last_sync_at = Some(Utc::now().to_rfc3339());
                    meta.last_sync_direction = Some("failed".to_string());
                    meta.last_sync_error = Some(err);
                }
            }
        }
        self.sync_status()
    }

    async fn run_postgres_bidirectional_sync(&self, pg_url: &str) -> Result<(usize, usize), String> {
        // 1. Collect pending outbox rows & current local versions from SQLite (short lock, zero await).
        let (pending_units, pending_ballots, pending_windmills, pending_candidates, pending_events, local_unit_revs) = {
            let conn = self.conn.lock().map_err(|_| "DB lock poisoned".to_string())?;
            let all_units = query_unit_rows_sqlite(&conn)?;
            let local_revs: BTreeMap<String, i64> = all_units
                .iter()
                .map(|u| (u.record.name.to_ascii_lowercase(), u.revision))
                .collect();
            let pu: Vec<DbUnitRow> = all_units.into_iter().filter(|u| u.sync_pending).collect();
            let pb: Vec<BlindBallotRecord> = query_ballots_sqlite(&conn)?
                .into_iter()
                .filter(|b| b.sync_pending)
                .collect();
            let pw: Vec<WindmillAssessmentRecord> = query_windmill_sqlite(&conn)?
                .into_iter()
                .filter(|w| w.sync_pending)
                .collect();
            let pc: Vec<DiscoveryCandidateRecord> = query_candidates_sqlite(&conn, None)?
                .into_iter()
                .filter(|c| c.sync_pending)
                .collect();
            let pe: Vec<SourcingAuditEvent> = query_events_sqlite(&conn, 500)?
                .into_iter()
                .filter(|e| e.sync_pending)
                .collect();
            (pu, pb, pw, pc, pe, local_revs)
        };

        // 2. Connect to PostgreSQL over v3.0 wire protocol and push pending outbox rows.
        let mut pg = PgWireClient::connect(pg_url).await?;
        pg.query("SET standard_conforming_strings = on;").await?;
        for stmt in POSTGRES_SCHEMA_SQL.split(';') {
            let s = stmt.trim();
            if !s.is_empty() {
                pg.query(&format!("{s};")).await?;
            }
        }

        let mut pushed_count = 0usize;
        let mut pushed_unit_names = Vec::new();
        for u in &pending_units {
            let key = u.record.name.to_ascii_lowercase();
            let deps_json = serde_json::to_string(&u.record.depends_on_components)
                .unwrap_or_else(|_| "[]".to_string());
            let scores_json = serde_json::to_string(&u.record.audit.manual_scores)
                .unwrap_or_else(|_| "{}".to_string());
            let sql = format!(
                "INSERT INTO sourcing_units (
                    name, namespace, description, resource_type, owner, system_ref,
                    adapter_abstracted, cost_allocation_id, depends_on_json, vendor_name,
                    renewal_date, last_audited_at, baseline_prod_dependents,
                    manual_override_variance, fallback_scores_json, source_origin,
                    revision, updated_at, sync_pending
                ) VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0)
                ON CONFLICT (name) DO UPDATE SET
                    namespace=EXCLUDED.namespace,
                    description=EXCLUDED.description,
                    resource_type=EXCLUDED.resource_type,
                    owner=EXCLUDED.owner,
                    system_ref=EXCLUDED.system_ref,
                    adapter_abstracted=EXCLUDED.adapter_abstracted,
                    cost_allocation_id=EXCLUDED.cost_allocation_id,
                    depends_on_json=EXCLUDED.depends_on_json,
                    vendor_name=EXCLUDED.vendor_name,
                    renewal_date=EXCLUDED.renewal_date,
                    last_audited_at=EXCLUDED.last_audited_at,
                    baseline_prod_dependents=EXCLUDED.baseline_prod_dependents,
                    manual_override_variance=EXCLUDED.manual_override_variance,
                    fallback_scores_json=EXCLUDED.fallback_scores_json,
                    source_origin=EXCLUDED.source_origin,
                    revision=EXCLUDED.revision,
                    updated_at=EXCLUDED.updated_at,
                    sync_pending=0
                WHERE sourcing_units.revision <= EXCLUDED.revision;",
                pg_str(&key)?,
                pg_str(&u.record.namespace)?,
                pg_str(&u.record.description)?,
                pg_str(&u.record.resource_type)?,
                pg_str(&u.record.owner)?,
                pg_str(&u.record.system)?,
                if u.record.adapter_abstracted { 1 } else { 0 },
                pg_str(&u.record.cost_allocation_id)?,
                pg_str(&deps_json)?,
                pg_str(&u.record.audit.contract_meta.vendor_name)?,
                pg_str(&u.record.audit.contract_meta.renewal_date)?,
                pg_str(&u.record.audit.contract_meta.last_audited_at)?,
                u.record.audit.contract_meta.baseline_prod_dependents,
                if u.manual_override_variance { 1 } else { 0 },
                pg_str(&scores_json)?,
                pg_str(&u.source_origin)?,
                u.revision,
                pg_str(&u.updated_at)?,
            );
            pg.query(&sql).await?;
            pushed_unit_names.push(key);
            pushed_count += 1;
        }

        let mut pushed_ballot_ids = Vec::new();
        for b in &pending_ballots {
            let sql = format!(
                "INSERT INTO sourcing_blind_ballots (
                    ballot_id, unit_name, reviewer_role, reviewer_id,
                    y2_core_competitiveness, y4_price_elasticity,
                    x2_market_scarcity, x3_customization_depth, x4_compliance_fragility,
                    evidence_notes, submitted_at, sync_pending
                ) VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0)
                ON CONFLICT (ballot_id) DO UPDATE SET
                    reviewer_id=EXCLUDED.reviewer_id,
                    y2_core_competitiveness=EXCLUDED.y2_core_competitiveness,
                    y4_price_elasticity=EXCLUDED.y4_price_elasticity,
                    x2_market_scarcity=EXCLUDED.x2_market_scarcity,
                    x3_customization_depth=EXCLUDED.x3_customization_depth,
                    x4_compliance_fragility=EXCLUDED.x4_compliance_fragility,
                    evidence_notes=EXCLUDED.evidence_notes,
                    submitted_at=EXCLUDED.submitted_at,
                    sync_pending=0
                WHERE sourcing_blind_ballots.submitted_at <= EXCLUDED.submitted_at;",
                pg_str(&b.ballot_id)?,
                pg_str(&b.unit_name)?,
                pg_str(&b.reviewer_role)?,
                pg_str(&b.reviewer_id)?,
                b.y2_core_competitiveness,
                b.y4_price_elasticity,
                b.x2_market_scarcity,
                b.x3_customization_depth,
                b.x4_compliance_fragility,
                pg_str(&b.evidence_notes)?,
                pg_str(&b.submitted_at)?,
            );
            pg.query(&sql).await?;
            pushed_ballot_ids.push(b.ballot_id.clone());
            pushed_count += 1;
        }

        let mut pushed_windmill_names = Vec::new();
        for w in &pending_windmills {
            let sql = format!(
                "INSERT INTO sourcing_windmill_assessments (
                    unit_name, estimated_wallet_share_tier, account_attractiveness_score,
                    perceived_status, assessed_by, rationale, updated_at, sync_pending
                ) VALUES ({}, {}, {}, {}, {}, {}, {}, 0)
                ON CONFLICT (unit_name) DO UPDATE SET
                    estimated_wallet_share_tier=EXCLUDED.estimated_wallet_share_tier,
                    account_attractiveness_score=EXCLUDED.account_attractiveness_score,
                    perceived_status=EXCLUDED.perceived_status,
                    assessed_by=EXCLUDED.assessed_by,
                    rationale=EXCLUDED.rationale,
                    updated_at=EXCLUDED.updated_at,
                    sync_pending=0
                WHERE sourcing_windmill_assessments.updated_at <= EXCLUDED.updated_at;",
                pg_str(&w.unit_name)?,
                pg_str(&w.estimated_wallet_share_tier)?,
                w.account_attractiveness_score,
                pg_str(&w.perceived_status)?,
                pg_str(&w.assessed_by)?,
                pg_str(&w.rationale)?,
                pg_str(&w.updated_at)?,
            );
            pg.query(&sql).await?;
            pushed_windmill_names.push(w.unit_name.clone());
            pushed_count += 1;
        }

        let mut pushed_candidate_ids = Vec::new();
        for c in &pending_candidates {
            let comps_json = serde_json::to_string(&c.inferred_components)
                .unwrap_or_else(|_| "[]".to_string());
            let sql = format!(
                "INSERT INTO sourcing_discovery_candidates (
                    candidate_id, package_or_endpoint, ecosystem, suggested_type,
                    detected_in_path, inferred_system, inferred_components_json,
                    adapter_detected, monolith_risk_note, status, promoted_unit_name,
                    discovered_at, sync_pending
                ) VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0)
                ON CONFLICT (candidate_id) DO UPDATE SET
                    package_or_endpoint=EXCLUDED.package_or_endpoint,
                    ecosystem=EXCLUDED.ecosystem,
                    suggested_type=EXCLUDED.suggested_type,
                    detected_in_path=EXCLUDED.detected_in_path,
                    inferred_system=EXCLUDED.inferred_system,
                    inferred_components_json=EXCLUDED.inferred_components_json,
                    adapter_detected=EXCLUDED.adapter_detected,
                    monolith_risk_note=EXCLUDED.monolith_risk_note,
                    status=EXCLUDED.status,
                    promoted_unit_name=EXCLUDED.promoted_unit_name,
                    discovered_at=EXCLUDED.discovered_at,
                    sync_pending=0;",
                pg_str(&c.candidate_id)?,
                pg_str(&c.package_or_endpoint)?,
                pg_str(&c.ecosystem)?,
                pg_str(&c.suggested_type)?,
                pg_str(&c.detected_in_path)?,
                pg_str(&c.inferred_system)?,
                pg_str(&comps_json)?,
                if c.adapter_detected { 1 } else { 0 },
                pg_str(&c.monolith_risk_note)?,
                pg_str(&c.status)?,
                pg_opt_str(c.promoted_unit_name.as_deref())?,
                pg_str(&c.discovered_at)?,
            );
            pg.query(&sql).await?;
            pushed_candidate_ids.push(c.candidate_id.clone());
            pushed_count += 1;
        }

        let mut pushed_event_ids = Vec::new();
        for ev in &pending_events {
            let sql = format!(
                "INSERT INTO sourcing_audit_events (
                    event_id, unit_name, event_type, actor, channel, summary, payload_json, created_at, sync_pending
                ) VALUES ({}, {}, {}, {}, {}, {}, {}, {}, 0)
                ON CONFLICT (event_id) DO NOTHING;",
                pg_str(&ev.event_id)?,
                pg_str(&ev.unit_name)?,
                pg_str(&ev.event_type)?,
                pg_str(&ev.actor)?,
                pg_str(&ev.channel)?,
                pg_str(&ev.summary)?,
                pg_str(&ev.payload_json)?,
                pg_str(&ev.created_at)?,
            );
            pg.query(&sql).await?;
            pushed_event_ids.push(ev.event_id.clone());
            pushed_count += 1;
        }

        // 3. Pull remote rows from PostgreSQL so external team updates sync down to SQLite WAL.
        let remote_units_rows = pg
            .query(
                "SELECT name, namespace, description, resource_type, owner, system_ref,
                        adapter_abstracted, cost_allocation_id, depends_on_json, vendor_name,
                        renewal_date, last_audited_at, baseline_prod_dependents,
                        manual_override_variance, fallback_scores_json, source_origin,
                        revision, updated_at
                 FROM sourcing_units;",
            )
            .await?;
        let remote_ballots_rows = pg
            .query(
                "SELECT ballot_id, unit_name, reviewer_role, reviewer_id,
                        y2_core_competitiveness, y4_price_elasticity,
                        x2_market_scarcity, x3_customization_depth, x4_compliance_fragility,
                        evidence_notes, submitted_at
                 FROM sourcing_blind_ballots;",
            )
            .await?;
        let remote_windmill_rows = pg
            .query(
                "SELECT unit_name, estimated_wallet_share_tier, account_attractiveness_score,
                        perceived_status, assessed_by, rationale, updated_at
                 FROM sourcing_windmill_assessments;",
            )
            .await?;
        let remote_candidate_rows = pg
            .query(
                "SELECT candidate_id, package_or_endpoint, ecosystem, suggested_type,
                        detected_in_path, inferred_system, inferred_components_json,
                        adapter_detected, monolith_risk_note, status, promoted_unit_name,
                        discovered_at
                 FROM sourcing_discovery_candidates;",
            )
            .await?;
        let remote_event_rows = pg
            .query(
                "SELECT event_id, unit_name, event_type, actor, channel, summary, payload_json, created_at
                 FROM sourcing_audit_events ORDER BY created_at DESC LIMIT 200;",
            )
            .await?;

        pg.close().await;

        // 4. Commit outbox clear + pulled remote rows atomically in SQLite WAL.
        let mut pulled_count = 0usize;
        let conn = self.conn.lock().map_err(|_| "DB lock poisoned".to_string())?;
        conn.exec_batch("BEGIN IMMEDIATE;")?;
        let commit_res = (|| -> Result<(), String> {
            for k in &pushed_unit_names {
                conn.execute(
                    "UPDATE sourcing_units SET sync_pending = 0 WHERE name = ?;",
                    &[SqlVal::Text(k.clone())],
                )?;
            }
            for id in &pushed_ballot_ids {
                conn.execute(
                    "UPDATE sourcing_blind_ballots SET sync_pending = 0 WHERE ballot_id = ?;",
                    &[SqlVal::Text(id.clone())],
                )?;
            }
            for k in &pushed_windmill_names {
                conn.execute(
                    "UPDATE sourcing_windmill_assessments SET sync_pending = 0 WHERE unit_name = ?;",
                    &[SqlVal::Text(k.clone())],
                )?;
            }
            for id in &pushed_candidate_ids {
                conn.execute(
                    "UPDATE sourcing_discovery_candidates SET sync_pending = 0 WHERE candidate_id = ?;",
                    &[SqlVal::Text(id.clone())],
                )?;
            }
            for id in &pushed_event_ids {
                conn.execute(
                    "UPDATE sourcing_audit_events SET sync_pending = 0 WHERE event_id = ?;",
                    &[SqlVal::Text(id.clone())],
                )?;
            }

            for r in &remote_units_rows {
                if let Some(ru) = parse_unit_row_slice(r) {
                    let key = ru.record.name.to_ascii_lowercase();
                    let local_rev = local_unit_revs.get(&key).copied().unwrap_or(0);
                    if local_rev == 0 || ru.revision > local_rev {
                        let deps_json = serde_json::to_string(&ru.record.depends_on_components)
                            .unwrap_or_else(|_| "[]".to_string());
                        let scores_json = serde_json::to_string(&ru.record.audit.manual_scores)
                            .unwrap_or_else(|_| "{}".to_string());
                        conn.execute(
                            "INSERT INTO sourcing_units (
                                name, namespace, description, resource_type, owner, system_ref,
                                adapter_abstracted, cost_allocation_id, depends_on_json, vendor_name,
                                renewal_date, last_audited_at, baseline_prod_dependents,
                                manual_override_variance, fallback_scores_json, source_origin,
                                revision, updated_at, sync_pending
                            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)
                            ON CONFLICT(name) DO UPDATE SET
                                namespace=excluded.namespace,
                                description=excluded.description,
                                resource_type=excluded.resource_type,
                                owner=excluded.owner,
                                system_ref=excluded.system_ref,
                                adapter_abstracted=excluded.adapter_abstracted,
                                cost_allocation_id=excluded.cost_allocation_id,
                                depends_on_json=excluded.depends_on_json,
                                vendor_name=excluded.vendor_name,
                                renewal_date=excluded.renewal_date,
                                last_audited_at=excluded.last_audited_at,
                                baseline_prod_dependents=excluded.baseline_prod_dependents,
                                manual_override_variance=excluded.manual_override_variance,
                                fallback_scores_json=excluded.fallback_scores_json,
                                source_origin=excluded.source_origin,
                                revision=excluded.revision,
                                updated_at=excluded.updated_at,
                                sync_pending=0;",
                            &[
                                SqlVal::Text(key),
                                SqlVal::Text(ru.record.namespace),
                                SqlVal::Text(ru.record.description),
                                SqlVal::Text(ru.record.resource_type),
                                SqlVal::Text(ru.record.owner),
                                SqlVal::Text(ru.record.system),
                                SqlVal::Int(if ru.record.adapter_abstracted { 1 } else { 0 }),
                                SqlVal::Text(ru.record.cost_allocation_id),
                                SqlVal::Text(deps_json),
                                SqlVal::Text(ru.record.audit.contract_meta.vendor_name),
                                SqlVal::Text(ru.record.audit.contract_meta.renewal_date),
                                SqlVal::Text(ru.record.audit.contract_meta.last_audited_at),
                                SqlVal::Int(ru.record.audit.contract_meta.baseline_prod_dependents as i64),
                                SqlVal::Int(if ru.manual_override_variance { 1 } else { 0 }),
                                SqlVal::Text(scores_json),
                                SqlVal::Text(ru.source_origin),
                                SqlVal::Int(ru.revision),
                                SqlVal::Text(ru.updated_at),
                            ],
                        )?;
                        pulled_count += 1;
                    }
                }
            }

            let local_ballot_times: BTreeMap<String, String> = query_ballots_sqlite(&conn)?
                .into_iter()
                .map(|b| (b.ballot_id, b.submitted_at))
                .collect();
            for r in &remote_ballots_rows {
                if let Some(rb) = parse_ballot_row_slice(r) {
                    let should_upsert = match local_ballot_times.get(&rb.ballot_id) {
                        None => true,
                        Some(ts) => rb.submitted_at > *ts,
                    };
                    if should_upsert {
                        conn.execute(
                            "INSERT INTO sourcing_blind_ballots (
                                ballot_id, unit_name, reviewer_role, reviewer_id,
                                y2_core_competitiveness, y4_price_elasticity,
                                x2_market_scarcity, x3_customization_depth, x4_compliance_fragility,
                                evidence_notes, submitted_at, sync_pending
                            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)
                            ON CONFLICT(ballot_id) DO UPDATE SET
                                reviewer_id=excluded.reviewer_id,
                                y2_core_competitiveness=excluded.y2_core_competitiveness,
                                y4_price_elasticity=excluded.y4_price_elasticity,
                                x2_market_scarcity=excluded.x2_market_scarcity,
                                x3_customization_depth=excluded.x3_customization_depth,
                                x4_compliance_fragility=excluded.x4_compliance_fragility,
                                evidence_notes=excluded.evidence_notes,
                                submitted_at=excluded.submitted_at,
                                sync_pending=0;",
                            &[
                                SqlVal::Text(rb.ballot_id),
                                SqlVal::Text(rb.unit_name),
                                SqlVal::Text(rb.reviewer_role),
                                SqlVal::Text(rb.reviewer_id),
                                SqlVal::Real(rb.y2_core_competitiveness),
                                SqlVal::Real(rb.y4_price_elasticity),
                                SqlVal::Real(rb.x2_market_scarcity),
                                SqlVal::Real(rb.x3_customization_depth),
                                SqlVal::Real(rb.x4_compliance_fragility),
                                SqlVal::Text(rb.evidence_notes),
                                SqlVal::Text(rb.submitted_at),
                            ],
                        )?;
                        pulled_count += 1;
                    }
                }
            }

            let local_windmill_times: BTreeMap<String, String> = query_windmill_sqlite(&conn)?
                .into_iter()
                .map(|w| (w.unit_name, w.updated_at))
                .collect();
            for r in &remote_windmill_rows {
                if let Some(rw) = parse_windmill_row_slice(r) {
                    let should_upsert = match local_windmill_times.get(&rw.unit_name) {
                        None => true,
                        Some(ts) => rw.updated_at > *ts,
                    };
                    if should_upsert {
                        conn.execute(
                            "INSERT INTO sourcing_windmill_assessments (
                                unit_name, estimated_wallet_share_tier, account_attractiveness_score,
                                perceived_status, assessed_by, rationale, updated_at, sync_pending
                            ) VALUES (?, ?, ?, ?, ?, ?, ?, 0)
                            ON CONFLICT(unit_name) DO UPDATE SET
                                estimated_wallet_share_tier=excluded.estimated_wallet_share_tier,
                                account_attractiveness_score=excluded.account_attractiveness_score,
                                perceived_status=excluded.perceived_status,
                                assessed_by=excluded.assessed_by,
                                rationale=excluded.rationale,
                                updated_at=excluded.updated_at,
                                sync_pending=0;",
                            &[
                                SqlVal::Text(rw.unit_name),
                                SqlVal::Text(rw.estimated_wallet_share_tier),
                                SqlVal::Real(rw.account_attractiveness_score),
                                SqlVal::Text(rw.perceived_status),
                                SqlVal::Text(rw.assessed_by),
                                SqlVal::Text(rw.rationale),
                                SqlVal::Text(rw.updated_at),
                            ],
                        )?;
                        pulled_count += 1;
                    }
                }
            }

            let local_cand_times: BTreeMap<String, String> = query_candidates_sqlite(&conn, None)?
                .into_iter()
                .map(|c| (c.candidate_id, c.discovered_at))
                .collect();
            for r in &remote_candidate_rows {
                if let Some(rc) = parse_candidate_row_slice(r) {
                    let should_upsert = match local_cand_times.get(&rc.candidate_id) {
                        None => true,
                        Some(ts) => rc.discovered_at > *ts,
                    };
                    if should_upsert {
                        let comps_json = serde_json::to_string(&rc.inferred_components)
                            .unwrap_or_else(|_| "[]".to_string());
                        conn.execute(
                            "INSERT INTO sourcing_discovery_candidates (
                                candidate_id, package_or_endpoint, ecosystem, suggested_type,
                                detected_in_path, inferred_system, inferred_components_json,
                                adapter_detected, monolith_risk_note, status, promoted_unit_name,
                                discovered_at, sync_pending
                            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)
                            ON CONFLICT(candidate_id) DO UPDATE SET
                                status=excluded.status,
                                promoted_unit_name=excluded.promoted_unit_name,
                                discovered_at=excluded.discovered_at,
                                sync_pending=0;",
                            &[
                                SqlVal::Text(rc.candidate_id),
                                SqlVal::Text(rc.package_or_endpoint),
                                SqlVal::Text(rc.ecosystem),
                                SqlVal::Text(rc.suggested_type),
                                SqlVal::Text(rc.detected_in_path),
                                SqlVal::Text(rc.inferred_system),
                                SqlVal::Text(comps_json),
                                SqlVal::Int(if rc.adapter_detected { 1 } else { 0 }),
                                SqlVal::Text(rc.monolith_risk_note),
                                SqlVal::Text(rc.status),
                                SqlVal::opt_text(rc.promoted_unit_name.as_deref()),
                                SqlVal::Text(rc.discovered_at),
                            ],
                        )?;
                        pulled_count += 1;
                    }
                }
            }

            for r in &remote_event_rows {
                if let Some(re) = parse_event_row_slice(r) {
                    conn.execute(
                        "INSERT INTO sourcing_audit_events (
                            event_id, unit_name, event_type, actor, channel, summary, payload_json, created_at, sync_pending
                        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0)
                        ON CONFLICT(event_id) DO NOTHING;",
                        &[
                            SqlVal::Text(re.event_id),
                            SqlVal::Text(re.unit_name),
                            SqlVal::Text(re.event_type),
                            SqlVal::Text(re.actor),
                            SqlVal::Text(re.channel),
                            SqlVal::Text(re.summary),
                            SqlVal::Text(re.payload_json),
                            SqlVal::Text(re.created_at),
                        ],
                    )?;
                }
            }
            Ok(())
        })();

        match commit_res {
            Ok(()) => {
                conn.exec_batch("COMMIT;")?;
                self.update_data_version(&conn);
                Ok((pushed_count, pulled_count))
            }
            Err(e) => {
                let _ = conn.exec_batch("ROLLBACK;");
                Err(e)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_event_sqlite(
    conn: &SqliteConn,
    unit_name: &str,
    event_type: &str,
    actor: &str,
    channel: &str,
    summary: &str,
    payload: Value,
    created_at: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO sourcing_audit_events (
            event_id, unit_name, event_type, actor, channel, summary, payload_json, created_at, sync_pending
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1);",
        &[
            SqlVal::Text(Uuid::new_v4().to_string()),
            SqlVal::Text(unit_name.to_string()),
            SqlVal::Text(event_type.to_string()),
            SqlVal::Text(actor.to_string()),
            SqlVal::Text(channel.to_string()),
            SqlVal::Text(summary.to_string()),
            SqlVal::Text(payload.to_string()),
            SqlVal::Text(created_at.to_string()),
        ],
    )
}

fn col_str(row: &[Option<String>], idx: usize) -> String {
    row.get(idx).and_then(|o| o.as_ref()).cloned().unwrap_or_default()
}

fn col_opt(row: &[Option<String>], idx: usize) -> Option<String> {
    row.get(idx).and_then(|o| o.clone()).filter(|s| !s.is_empty())
}

fn col_bool(row: &[Option<String>], idx: usize) -> bool {
    matches!(
        col_str(row, idx).trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "t"
    )
}

fn col_i64(row: &[Option<String>], idx: usize, default: i64) -> i64 {
    col_str(row, idx).trim().parse::<i64>().unwrap_or(default)
}

fn col_f64(row: &[Option<String>], idx: usize, default: f64) -> f64 {
    col_str(row, idx).trim().parse::<f64>().unwrap_or(default)
}

fn parse_unit_row_slice(row: &[Option<String>]) -> Option<DbUnitRow> {
    if row.len() < 18 {
        return None;
    }
    let name = col_str(row, 0);
    if name.trim().is_empty() {
        return None;
    }
    let depends_on: Vec<String> = serde_json::from_str(&col_str(row, 8)).unwrap_or_default();
    let manual_scores: ManualScores =
        serde_json::from_str(&col_str(row, 14)).unwrap_or(ManualScores {
            y2_core_competitiveness: 3.0,
            y4_price_elasticity: 3.0,
            x2_market_scarcity: 3.0,
            x3_customization_depth: 3.0,
            x4_compliance_fragility: 3.0,
        });
    let manual_override_variance = col_bool(row, 13);
    let record = SourcingResourceRecord {
        name,
        namespace: {
            let ns = col_str(row, 1);
            if ns.is_empty() {
                "default".to_string()
            } else {
                ns
            }
        },
        description: col_str(row, 2),
        resource_type: col_str(row, 3),
        owner: col_str(row, 4),
        system: col_str(row, 5),
        adapter_abstracted: col_bool(row, 6),
        cost_allocation_id: col_str(row, 7),
        depends_on_components: depends_on,
        audit: crate::catalog::sourcing::SourcingAuditSpec {
            contract_meta: crate::catalog::sourcing::ContractMeta {
                vendor_name: col_str(row, 9),
                renewal_date: col_str(row, 10),
                last_audited_at: col_str(row, 11),
                baseline_prod_dependents: col_i64(row, 12, 1).max(0) as usize,
                blind_score_variance_alert: manual_override_variance,
            },
            manual_scores,
            supplier_preference: crate::catalog::sourcing::SupplierPreference {
                estimated_wallet_share_tier: "1-to-5pct".to_string(),
                account_attractiveness_score: 3.0,
                perceived_status: "development".to_string(),
            },
        },
    };
    Some(DbUnitRow {
        record,
        manual_override_variance,
        source_origin: col_str(row, 15),
        revision: col_i64(row, 16, 1),
        updated_at: col_str(row, 17),
        sync_pending: if row.len() > 18 {
            col_bool(row, 18)
        } else {
            false
        },
    })
}

fn parse_ballot_row_slice(row: &[Option<String>]) -> Option<BlindBallotRecord> {
    if row.len() < 11 {
        return None;
    }
    Some(BlindBallotRecord {
        ballot_id: col_str(row, 0),
        unit_name: col_str(row, 1),
        reviewer_role: col_str(row, 2),
        reviewer_id: col_str(row, 3),
        y2_core_competitiveness: col_f64(row, 4, 3.0),
        y4_price_elasticity: col_f64(row, 5, 3.0),
        x2_market_scarcity: col_f64(row, 6, 3.0),
        x3_customization_depth: col_f64(row, 7, 3.0),
        x4_compliance_fragility: col_f64(row, 8, 3.0),
        evidence_notes: col_str(row, 9),
        submitted_at: col_str(row, 10),
        sync_pending: if row.len() > 11 {
            col_bool(row, 11)
        } else {
            false
        },
    })
}

fn parse_windmill_row_slice(row: &[Option<String>]) -> Option<WindmillAssessmentRecord> {
    if row.len() < 7 {
        return None;
    }
    Some(WindmillAssessmentRecord {
        unit_name: col_str(row, 0),
        estimated_wallet_share_tier: col_str(row, 1),
        account_attractiveness_score: col_f64(row, 2, 3.0),
        perceived_status: col_str(row, 3),
        assessed_by: col_str(row, 4),
        rationale: col_str(row, 5),
        updated_at: col_str(row, 6),
        sync_pending: if row.len() > 7 {
            col_bool(row, 7)
        } else {
            false
        },
    })
}

fn parse_candidate_row_slice(row: &[Option<String>]) -> Option<DiscoveryCandidateRecord> {
    if row.len() < 12 {
        return None;
    }
    Some(DiscoveryCandidateRecord {
        candidate_id: col_str(row, 0),
        package_or_endpoint: col_str(row, 1),
        ecosystem: col_str(row, 2),
        suggested_type: col_str(row, 3),
        detected_in_path: col_str(row, 4),
        inferred_system: col_str(row, 5),
        inferred_components: serde_json::from_str(&col_str(row, 6)).unwrap_or_default(),
        adapter_detected: col_bool(row, 7),
        monolith_risk_note: col_str(row, 8),
        status: col_str(row, 9),
        promoted_unit_name: col_opt(row, 10),
        discovered_at: col_str(row, 11),
        sync_pending: if row.len() > 12 {
            col_bool(row, 12)
        } else {
            false
        },
    })
}

fn parse_event_row_slice(row: &[Option<String>]) -> Option<SourcingAuditEvent> {
    if row.len() < 8 {
        return None;
    }
    Some(SourcingAuditEvent {
        event_id: col_str(row, 0),
        unit_name: col_str(row, 1),
        event_type: col_str(row, 2),
        actor: col_str(row, 3),
        channel: col_str(row, 4),
        summary: col_str(row, 5),
        payload_json: col_str(row, 6),
        created_at: col_str(row, 7),
        sync_pending: if row.len() > 8 {
            col_bool(row, 8)
        } else {
            false
        },
    })
}

fn query_unit_rows_sqlite(conn: &SqliteConn) -> Result<Vec<DbUnitRow>, String> {
    let rows = conn.query_rows(
        "SELECT name, namespace, description, resource_type, owner, system_ref,
                adapter_abstracted, cost_allocation_id, depends_on_json, vendor_name,
                renewal_date, last_audited_at, baseline_prod_dependents,
                manual_override_variance, fallback_scores_json, source_origin,
                revision, updated_at, sync_pending
         FROM sourcing_units ORDER BY name ASC;",
        &[],
    )?;
    Ok(rows.iter().filter_map(|r| parse_unit_row_slice(r)).collect())
}

fn query_ballots_sqlite(conn: &SqliteConn) -> Result<Vec<BlindBallotRecord>, String> {
    let rows = conn.query_rows(
        "SELECT ballot_id, unit_name, reviewer_role, reviewer_id,
                y2_core_competitiveness, y4_price_elasticity,
                x2_market_scarcity, x3_customization_depth, x4_compliance_fragility,
                evidence_notes, submitted_at, sync_pending
         FROM sourcing_blind_ballots ORDER BY unit_name ASC, reviewer_role ASC;",
        &[],
    )?;
    Ok(rows.iter().filter_map(|r| parse_ballot_row_slice(r)).collect())
}

fn query_ballots_for_unit_sqlite(
    conn: &SqliteConn,
    unit_name: &str,
) -> Result<Vec<BlindBallotRecord>, String> {
    let rows = conn.query_rows(
        "SELECT ballot_id, unit_name, reviewer_role, reviewer_id,
                y2_core_competitiveness, y4_price_elasticity,
                x2_market_scarcity, x3_customization_depth, x4_compliance_fragility,
                evidence_notes, submitted_at, sync_pending
         FROM sourcing_blind_ballots WHERE unit_name = ? ORDER BY reviewer_role ASC;",
        &[SqlVal::Text(unit_name.to_string())],
    )?;
    Ok(rows.iter().filter_map(|r| parse_ballot_row_slice(r)).collect())
}

fn query_windmill_sqlite(conn: &SqliteConn) -> Result<Vec<WindmillAssessmentRecord>, String> {
    let rows = conn.query_rows(
        "SELECT unit_name, estimated_wallet_share_tier, account_attractiveness_score,
                perceived_status, assessed_by, rationale, updated_at, sync_pending
         FROM sourcing_windmill_assessments ORDER BY unit_name ASC;",
        &[],
    )?;
    Ok(rows.iter().filter_map(|r| parse_windmill_row_slice(r)).collect())
}

fn query_candidates_sqlite(
    conn: &SqliteConn,
    status_filter: Option<&str>,
) -> Result<Vec<DiscoveryCandidateRecord>, String> {
    let rows = if let Some(st) = status_filter.filter(|s| !s.eq_ignore_ascii_case("all")) {
        conn.query_rows(
            "SELECT candidate_id, package_or_endpoint, ecosystem, suggested_type,
                    detected_in_path, inferred_system, inferred_components_json,
                    adapter_detected, monolith_risk_note, status, promoted_unit_name,
                    discovered_at, sync_pending
             FROM sourcing_discovery_candidates WHERE status = ? ORDER BY candidate_id ASC;",
            &[SqlVal::Text(st.to_ascii_lowercase())],
        )?
    } else {
        conn.query_rows(
            "SELECT candidate_id, package_or_endpoint, ecosystem, suggested_type,
                    detected_in_path, inferred_system, inferred_components_json,
                    adapter_detected, monolith_risk_note, status, promoted_unit_name,
                    discovered_at, sync_pending
             FROM sourcing_discovery_candidates ORDER BY status ASC, candidate_id ASC;",
            &[],
        )?
    };
    Ok(rows.iter().filter_map(|r| parse_candidate_row_slice(r)).collect())
}

fn query_events_sqlite(conn: &SqliteConn, limit: usize) -> Result<Vec<SourcingAuditEvent>, String> {
    let rows = conn.query_rows(
        "SELECT event_id, unit_name, event_type, actor, channel, summary, payload_json, created_at, sync_pending
         FROM sourcing_audit_events ORDER BY created_at DESC LIMIT ?;",
        &[SqlVal::Int(limit as i64)],
    )?;
    Ok(rows.iter().filter_map(|r| parse_event_row_slice(r)).collect())
}

// ---------------------------------------------------------------------------
// Native Pure-Rust PostgreSQL v3.0 Wire Client (TCP + TLS + SCRAM-SHA-256)
// ---------------------------------------------------------------------------

fn pg_str(s: &str) -> Result<String, String> {
    if s.contains('\0') {
        return Err("SQL string literal cannot contain NUL bytes".to_string());
    }
    Ok(format!("'{}'", s.replace('\'', "''")))
}

fn pg_opt_str(s: Option<&str>) -> Result<String, String> {
    match s {
        Some(v) => pg_str(v),
        None => Ok("NULL".to_string()),
    }
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let mut h = Sha256::new();
        h.update(key);
        k[..32].copy_from_slice(&h.finalize());
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(msg);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&inner_digest);
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer.finalize());
    out
}

fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut initial_msg = Vec::with_capacity(salt.len() + 4);
    initial_msg.extend_from_slice(salt);
    initial_msg.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha256(password, &initial_msg);
    let mut t = u;
    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        for (ti, ui) in t.iter_mut().zip(u.iter()) {
            *ti ^= *ui;
        }
    }
    t
}

pub struct PgWireClient {
    stream: Box<dyn AsyncReadWrite>,
}

impl PgWireClient {
    pub async fn connect(pg_url: &str) -> Result<Self, String> {
        let url = Url::parse(pg_url).map_err(|e| format!("Invalid PostgreSQL URL: {e}"))?;
        let scheme = url.scheme().to_ascii_lowercase();
        if scheme != "postgres" && scheme != "postgresql" {
            return Err(format!("Unsupported database URL scheme '{}'", url.scheme()));
        }
        let host = url
            .host_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "PostgreSQL URL missing host".to_string())?
            .to_string();
        let port = url.port().unwrap_or(5432);
        let user = if url.username().is_empty() {
            "postgres".to_string()
        } else {
            url.username().to_string()
        };
        let password = url.password().unwrap_or_default().to_string();
        let db = url
            .path()
            .trim_start_matches('/')
            .split('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&user)
            .to_string();
        let sslmode = url
            .query_pairs()
            .find(|(k, _)| k.eq_ignore_ascii_case("sslmode"))
            .map(|(_, v)| v.to_ascii_lowercase())
            .unwrap_or_else(|| "prefer".to_string());

        let mut tcp = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            TcpStream::connect((host.as_str(), port)),
        )
        .await
        .map_err(|_| format!("Timed out connecting to PostgreSQL {host}:{port}"))?
        .map_err(|e| format!("TCP connect PostgreSQL {host}:{port}: {e}"))?;
        let _ = tcp.set_nodelay(true);

        let stream: Box<dyn AsyncReadWrite> = if sslmode == "disable" {
            Box::new(tcp)
        } else {
            // Send SSLRequest packet (len=8, code=80877103)
            let mut req = Vec::with_capacity(8);
            req.extend_from_slice(&8i32.to_be_bytes());
            req.extend_from_slice(&80_877_103i32.to_be_bytes());
            tcp.write_all(&req)
                .await
                .map_err(|e| format!("PostgreSQL SSLRequest write: {e}"))?;
            let mut reply = [0u8; 1];
            tcp.read_exact(&mut reply)
                .await
                .map_err(|e| format!("PostgreSQL SSLRequest read: {e}"))?;
            match reply[0] {
                b'S' => {
                    let (cfg, _) = build_rustls_client_config(None, None, None, true, vec![])
                        .map_err(|e| format!("PostgreSQL TLS config: {e}"))?;
                    let connector = TlsConnector::from(cfg);
                    let sname = server_name_for_host(&host)
                        .map_err(|e| format!("PostgreSQL TLS server name: {e}"))?;
                    let tls_stream = connector
                        .connect(sname, tcp)
                        .await
                        .map_err(|e| format!("PostgreSQL TLS handshake failed: {e}"))?;
                    Box::new(tls_stream)
                }
                b'N' if sslmode == "require" || sslmode == "verify-ca" || sslmode == "verify-full" => {
                    return Err("PostgreSQL server refused TLS while sslmode requires it".to_string());
                }
                _ => Box::new(tcp),
            }
        };

        let mut client = Self { stream };
        client.authenticate(&user, &password, &db).await?;
        Ok(client)
    }

    async fn send_startup(&mut self, user: &str, db: &str) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(&196_608i32.to_be_bytes()); // v3.0
        for (k, v) in [("user", user), ("database", db), ("application_name", "pitsctl")] {
            body.extend_from_slice(k.as_bytes());
            body.push(0);
            body.extend_from_slice(v.as_bytes());
            body.push(0);
        }
        body.push(0);
        let len = (body.len() + 4) as i32;
        let mut pkt = Vec::with_capacity(len as usize);
        pkt.extend_from_slice(&len.to_be_bytes());
        pkt.extend_from_slice(&body);
        self.stream
            .write_all(&pkt)
            .await
            .map_err(|e| format!("PG startup write: {e}"))
    }

    async fn send_packet(&mut self, tag: u8, payload: &[u8]) -> Result<(), String> {
        let len = (payload.len() + 4) as i32;
        let mut buf = Vec::with_capacity(1 + len as usize);
        buf.push(tag);
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(payload);
        self.stream
            .write_all(&buf)
            .await
            .map_err(|e| format!("PG write '{}': {e}", tag as char))
    }

    async fn read_packet(&mut self) -> Result<(u8, Vec<u8>), String> {
        let mut hdr = [0u8; 5];
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.stream.read_exact(&mut hdr),
        )
        .await
        .map_err(|_| "Timed out waiting for PostgreSQL packet".to_string())?
        .map_err(|e| format!("PG read packet header: {e}"))?;

        let tag = hdr[0];
        let total_len = i32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]);
        if total_len < 4 || total_len > 64 * 1024 * 1024 {
            return Err(format!("Invalid PostgreSQL packet length: {total_len}"));
        }
        let body_len = (total_len - 4) as usize;
        let mut body = vec![0u8; body_len];
        if body_len > 0 {
            self.stream
                .read_exact(&mut body)
                .await
                .map_err(|e| format!("PG read packet body: {e}"))?;
        }
        Ok((tag, body))
    }

    async fn authenticate(&mut self, user: &str, password: &str, db: &str) -> Result<(), String> {
        self.send_startup(user, db).await?;

        let mut scram_client_nonce = String::new();
        let mut scram_client_first_bare = String::new();
        let mut scram_expected_server_sig: Option<[u8; 32]> = None;

        loop {
            let (tag, body) = self.read_packet().await?;
            match tag {
                b'R' => {
                    if body.len() < 4 {
                        return Err("Truncated PostgreSQL Auth packet".to_string());
                    }
                    let code = i32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                    match code {
                        0 => {
                            // AuthenticationOk
                        }
                        3 => {
                            let mut p = password.as_bytes().to_vec();
                            p.push(0);
                            self.send_packet(b'p', &p).await?;
                        }
                        5 => {
                            return Err("PostgreSQL AuthenticationMD5Password unsupported; configure scram-sha-256 or trust".to_string());
                        }
                        10 => {
                            let mechs_raw = String::from_utf8_lossy(&body[4..]);
                            if !mechs_raw.split('\0').any(|m| m == "SCRAM-SHA-256") {
                                return Err(format!(
                                    "PostgreSQL offered unsupported SASL mechanisms: {}",
                                    mechs_raw.replace('\0', ", ")
                                ));
                            }
                            let mut nonce_bytes = [0u8; 18];
                            rand::thread_rng().fill_bytes(&mut nonce_bytes);
                            scram_client_nonce = base64::engine::general_purpose::STANDARD.encode(nonce_bytes);
                            scram_client_first_bare = format!("n=,r={scram_client_nonce}");
                            let client_first = format!("n,,{scram_client_first_bare}");

                            let mut payload = Vec::new();
                            payload.extend_from_slice(b"SCRAM-SHA-256\0");
                            payload.extend_from_slice(&(client_first.len() as i32).to_be_bytes());
                            payload.extend_from_slice(client_first.as_bytes());
                            self.send_packet(b'p', &payload).await?;
                        }
                        11 => {
                            let server_first = std::str::from_utf8(&body[4..])
                                .map_err(|_| "Invalid UTF-8 in SCRAM server-first".to_string())?
                                .to_string();
                            let mut full_nonce = String::new();
                            let mut salt_b64 = String::new();
                            let mut iters: u32 = 4096;
                            for part in server_first.split(',') {
                                if let Some(v) = part.strip_prefix("r=") {
                                    full_nonce = v.to_string();
                                } else if let Some(v) = part.strip_prefix("s=") {
                                    salt_b64 = v.to_string();
                                } else if let Some(v) = part.strip_prefix("i=") {
                                    iters = v.parse::<u32>().unwrap_or(4096);
                                }
                            }
                            if !full_nonce.starts_with(&scram_client_nonce) {
                                return Err("SCRAM server nonce did not preserve client nonce".to_string());
                            }
                            let salt = base64::engine::general_purpose::STANDARD
                                .decode(salt_b64.as_bytes())
                                .map_err(|e| format!("Invalid SCRAM salt base64: {e}"))?;

                            let salted = pbkdf2_hmac_sha256(password.as_bytes(), &salt, iters);
                            let client_key = hmac_sha256(&salted, b"Client Key");
                            let stored_key: [u8; 32] = {
                                let mut h = Sha256::new();
                                h.update(&client_key);
                                h.finalize().into()
                            };
                            let client_final_no_proof = format!("c=biws,r={full_nonce}");
                            let auth_msg = format!(
                                "{scram_client_first_bare},{server_first},{client_final_no_proof}"
                            );
                            let client_sig = hmac_sha256(&stored_key, auth_msg.as_bytes());
                            let mut client_proof = [0u8; 32];
                            for i in 0..32 {
                                client_proof[i] = client_key[i] ^ client_sig[i];
                            }
                            let server_key = hmac_sha256(&salted, b"Server Key");
                            scram_expected_server_sig =
                                Some(hmac_sha256(&server_key, auth_msg.as_bytes()));

                            let proof_b64 =
                                base64::engine::general_purpose::STANDARD.encode(client_proof);
                            let client_final = format!("{client_final_no_proof},p={proof_b64}");
                            self.send_packet(b'p', client_final.as_bytes()).await?;
                        }
                        12 => {
                            let server_final = std::str::from_utf8(&body[4..])
                                .map_err(|_| "Invalid UTF-8 in SCRAM server-final".to_string())?;
                            if let Some(expected) = scram_expected_server_sig {
                                let v_b64 = server_final
                                    .split(',')
                                    .find_map(|p| p.strip_prefix("v="))
                                    .ok_or_else(|| "SCRAM server-final missing signature".to_string())?;
                                let got = base64::engine::general_purpose::STANDARD
                                    .decode(v_b64.as_bytes())
                                    .map_err(|e| format!("Invalid SCRAM v= base64: {e}"))?;
                                if got.len() != expected.len()
                                    || got.iter().zip(expected.iter()).any(|(a, b)| a != b)
                                {
                                    return Err("SCRAM-SHA-256 server signature verification failed".to_string());
                                }
                            }
                        }
                        other => {
                            return Err(format!("Unsupported PostgreSQL auth code: {other}"));
                        }
                    }
                }
                b'E' => return Err(parse_pg_error(&body)),
                b'Z' => return Ok(()),
                _ => {}
            }
        }
    }

    pub async fn query(&mut self, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
        let mut payload = sql.as_bytes().to_vec();
        payload.push(0);
        self.send_packet(b'Q', &payload).await?;

        let mut rows = Vec::new();
        let mut pending_err: Option<String> = None;
        loop {
            let (tag, body) = self.read_packet().await?;
            match tag {
                b'D' => {
                    if body.len() >= 2 {
                        let ncols = i16::from_be_bytes([body[0], body[1]]) as usize;
                        let mut pos = 2usize;
                        let mut row = Vec::with_capacity(ncols);
                        for _ in 0..ncols {
                            if pos + 4 > body.len() {
                                break;
                            }
                            let len = i32::from_be_bytes([
                                body[pos],
                                body[pos + 1],
                                body[pos + 2],
                                body[pos + 3],
                            ]);
                            pos += 4;
                            if len < 0 {
                                row.push(None);
                            } else {
                                let ulen = len as usize;
                                if pos + ulen > body.len() {
                                    break;
                                }
                                let s = String::from_utf8_lossy(&body[pos..pos + ulen]).into_owned();
                                pos += ulen;
                                row.push(Some(s));
                            }
                        }
                        rows.push(row);
                    }
                }
                b'E' => {
                    pending_err = Some(parse_pg_error(&body));
                }
                b'Z' => {
                    if let Some(err) = pending_err {
                        return Err(err);
                    }
                    return Ok(rows);
                }
                _ => {}
            }
        }
    }

    pub async fn close(mut self) {
        let _ = self.send_packet(b'X', &[]).await;
    }
}

fn parse_pg_error(body: &[u8]) -> String {
    let mut msg = String::new();
    let mut code = String::new();
    let mut idx = 0usize;
    while idx < body.len() {
        let field = body[idx];
        idx += 1;
        if field == 0 {
            break;
        }
        let end = body[idx..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| idx + p)
            .unwrap_or(body.len());
        let val = String::from_utf8_lossy(&body[idx..end]).into_owned();
        if field == b'M' {
            msg = val;
        } else if field == b'C' {
            code = val;
        }
        idx = end + 1;
    }
    if code.is_empty() {
        format!("PostgreSQL error: {msg}")
    } else {
        format!("PostgreSQL [{code}]: {msg}")
    }
}

// ---------------------------------------------------------------------------
// Workspace Dependency & Adapter/Anti-Corruption Discovery Scanner
// ---------------------------------------------------------------------------

struct KnownExternalDep {
    token: &'static str,
    suggested_name: &'static str,
    suggested_type: &'static str,
    ecosystem: &'static str,
    inferred_system: &'static str,
    inferred_components: &'static [&'static str],
    import_needles: &'static [&'static str],
    notes: &'static str,
}

const DISCOVERY_CATALOG_PATTERNS: &[KnownExternalDep] = &[
    KnownExternalDep {
        token: "rustls",
        suggested_name: "rustls-tls-crypto-sdk",
        suggested_type: "commercial-sdk",
        ecosystem: "cargo",
        inferred_system: "pits-infrastructure",
        inferred_components: &["component:default/edge-ingress-proxy"],
        import_needles: &["rustls", "tokio_rustls"],
        notes: "TLS 1.2/1.3 cryptography boundary; verify confined to transport/adapter module.",
    },
    KnownExternalDep {
        token: "@backstage/",
        suggested_name: "backstage-frontend-framework",
        suggested_type: "commercial-sdk",
        ecosystem: "npm",
        inferred_system: "pits-infrastructure",
        inferred_components: &["component:default/edge-ingress-proxy"],
        import_needles: &["@backstage/"],
        notes: "Backstage v1.56 UI/catalog framework across portal modules.",
    },
    KnownExternalDep {
        token: "go.fd.io/govpp",
        suggested_name: "govpp-dataplane-sdk",
        suggested_type: "commercial-sdk",
        ecosystem: "go-mod",
        inferred_system: "pits-infrastructure",
        inferred_components: &["component:default/tailnet-relay-controller", "component:default/edge-ingress-proxy"],
        import_needles: &["govpp"],
        notes: "Shared-memory VPP dataplane IPC dependency on pits-agent.",
    },
    KnownExternalDep {
        token: "tailscale",
        suggested_name: "tailscale-coordination-saas",
        suggested_type: "vendor-saas",
        ecosystem: "nix-flake",
        inferred_system: "pits-infrastructure",
        inferred_components: &["component:default/tailnet-relay-controller", "component:default/edge-ingress-proxy"],
        import_needles: &["tailscaled", "100.74."],
        notes: "Mesh coordination SaaS referenced in deploy NixOS config.",
    },
    KnownExternalDep {
        token: "rke2",
        suggested_name: "suse-rke2-controlplane",
        suggested_type: "managed-infra",
        ecosystem: "k8s-manifest",
        inferred_system: "pits-infrastructure",
        inferred_components: &["component:default/edge-ingress-proxy"],
        import_needles: &["rke2"],
        notes: "Kubernetes distribution runtime on pits-core.",
    },
];

#[allow(clippy::too_many_arguments)]
fn insert_manifest_candidate(
    found: &mut BTreeMap<String, DiscoveryCandidateRecord>,
    registered_units: &BTreeMap<String, SourcingResourceRecord>,
    candidate_id: String,
    package_or_endpoint: String,
    ecosystem: &str,
    suggested_type: &str,
    detected_in_path: &str,
    monolith_risk_note: String,
    discovered_at: &str,
) {
    let normalized_slug = package_or_endpoint
        .to_ascii_lowercase()
        .trim_matches('@')
        .trim_end_matches('/')
        .replace('/', "-");
    let promoted_unit_name = registered_units
        .keys()
        .find(|name| {
            name.eq_ignore_ascii_case(&normalized_slug)
                || (!normalized_slug.is_empty() && name.contains(&normalized_slug))
        })
        .cloned();
    let status = if promoted_unit_name.is_some() {
        "promoted".to_string()
    } else {
        "pending".to_string()
    };
    found.insert(
        candidate_id.clone(),
        DiscoveryCandidateRecord {
            candidate_id,
            package_or_endpoint,
            ecosystem: ecosystem.to_string(),
            suggested_type: suggested_type.to_string(),
            detected_in_path: detected_in_path.to_string(),
            inferred_system: "pits-infrastructure".to_string(),
            inferred_components: vec!["component:default/edge-ingress-proxy".to_string()],
            adapter_detected: false,
            monolith_risk_note,
            status,
            promoted_unit_name,
            discovered_at: discovered_at.to_string(),
            sync_pending: true,
        },
    );
}

pub fn scan_workspace_dependencies(
    workspace_root: &Path,
    registered_units: &BTreeMap<String, SourcingResourceRecord>,
) -> Vec<DiscoveryCandidateRecord> {
    let now_iso = Utc::now().to_rfc3339();
    let mut found: BTreeMap<String, DiscoveryCandidateRecord> = BTreeMap::new();

    // Collect source file contents for adapter/isolation detection
    let mut rust_adapter_hits = 0usize;
    let mut rust_domain_hits: BTreeMap<&'static str, BTreeSet<String>> = BTreeMap::new();
    collect_source_imports(workspace_root, &mut rust_adapter_hits, &mut rust_domain_hits);

    let manifests_to_check: Vec<(&str, PathBuf)> = vec![
        ("cargo", workspace_root.join("client").join("Cargo.toml")),
        ("npm", workspace_root.join("portal").join("package.json")),
        ("go-mod", workspace_root.join("server").join("go.mod")),
        (
            "nix-flake",
            workspace_root.join("deploy").join("nixos").join("pits-portal-module.nix"),
        ),
        (
            "k8s-manifest",
            workspace_root.join("deploy").join("nixos").join("rke2-daemonset.yaml"),
        ),
    ];

    for (eco, path) in manifests_to_check {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lower = raw.to_ascii_lowercase();
        let rel_path = path
            .strip_prefix(workspace_root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.display().to_string());

        for spec in DISCOVERY_CATALOG_PATTERNS {
            if spec.ecosystem == eco && lower.contains(&spec.token.to_ascii_lowercase()) {
                let is_registered = registered_units.keys().any(|name| {
                    name.eq_ignore_ascii_case(spec.suggested_name)
                        || name.contains(spec.token.trim_matches('@').trim_end_matches('/'))
                });
                let hit_files = rust_domain_hits
                    .get(spec.token)
                    .map(|s| s.len())
                    .unwrap_or(1);
                let adapter_detected = hit_files <= 2;
                let cid = format!("{eco}:{}", spec.suggested_name);
                let mut note = spec.notes.to_string();
                if !adapter_detected {
                    note = format!(
                        "{} (Direct imports spread across {} source files — extract thin Adapter layer)",
                        note, hit_files
                    );
                }
                found.insert(
                    cid.clone(),
                    DiscoveryCandidateRecord {
                        candidate_id: cid,
                        package_or_endpoint: spec.token.to_string(),
                        ecosystem: eco.to_string(),
                        suggested_type: spec.suggested_type.to_string(),
                        detected_in_path: rel_path.clone(),
                        inferred_system: spec.inferred_system.to_string(),
                        inferred_components: spec
                            .inferred_components
                            .iter()
                            .map(|s| (*s).to_string())
                            .collect(),
                        adapter_detected,
                        monolith_risk_note: note,
                        status: if is_registered {
                            "promoted".to_string()
                        } else {
                            "pending".to_string()
                        },
                        promoted_unit_name: if is_registered {
                            registered_units
                                .keys()
                                .find(|name| {
                                    name.eq_ignore_ascii_case(spec.suggested_name)
                                        || name.contains(
                                            spec.token.trim_matches('@').trim_end_matches('/'),
                                        )
                                })
                                .cloned()
                        } else {
                            None
                        },
                        discovered_at: now_iso.clone(),
                        sync_pending: true,
                    },
                );
            }
        }

        if eco == "cargo" {
            const CORE_CARGO_CRATES: &[&str] = &[
                "serde",
                "serde_json",
                "serde_yaml",
                "tokio",
                "tokio-util",
                "tokio-rustls",
                "anyhow",
                "thiserror",
                "clap",
                "uuid",
                "chrono",
                "bytes",
                "http",
                "http-body-util",
                "hyper",
                "hyper-util",
                "axum",
                "tower",
                "tower-service",
                "base64",
                "hex",
                "rand",
                "regex",
                "sha1",
                "sha2",
                "hmac",
                "pbkdf2",
                "url",
                "crc32fast",
                "ipnet",
                "ed25519-dalek",
                "x509-cert",
                "webpki-roots",
                "rustls",
                "rustls-pemfile",
                "rustls-pki-types",
                "flate2",
                "tar",
                "walkdir",
                "dirs",
                "tracing",
                "tracing-subscriber",
                "futures",
                "futures-util",
                "pin-project-lite",
                "once_cell",
                "parking_lot",
            ];
            let mut in_deps_section = false;
            for line in raw.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if trimmed.starts_with('[') && trimmed.ends_with(']') {
                    in_deps_section =
                        trimmed == "[dependencies]" || trimmed == "[workspace.dependencies]";
                    continue;
                }
                if !in_deps_section {
                    continue;
                }
                let Some((key, _)) = trimmed.split_once('=') else {
                    continue;
                };
                let raw_key = key.trim().trim_matches('"').trim_matches('\'');
                let pkg = raw_key.split('.').next().unwrap_or(raw_key).trim();
                if pkg.is_empty()
                    || !pkg
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                {
                    continue;
                }
                let pkg_lower = pkg.to_ascii_lowercase();
                if CORE_CARGO_CRATES.contains(&pkg_lower.as_str())
                    || DISCOVERY_CATALOG_PATTERNS.iter().any(|s| {
                        s.ecosystem == "cargo" && pkg_lower.contains(&s.token.to_ascii_lowercase())
                    })
                {
                    continue;
                }
                insert_manifest_candidate(
                    &mut found,
                    registered_units,
                    format!("cargo:{pkg}"),
                    pkg.to_string(),
                    "cargo",
                    if pkg_lower.contains("stripe")
                        || pkg_lower.contains("datadog")
                        || pkg_lower.contains("sentry")
                        || pkg_lower.contains("auth0")
                    {
                        "vendor-saas"
                    } else {
                        "commercial-sdk"
                    },
                    &rel_path,
                    format!(
                        "External Cargo dependency '{}' declared in {}; audit lock-in & FinOps cost center.",
                        pkg, rel_path
                    ),
                    &now_iso,
                );
            }
        } else if eco == "npm" {
            const CORE_NPM_PACKAGES: &[&str] = &[
                "react",
                "react-dom",
                "react-router",
                "react-router-dom",
                "lucide-react",
                "clsx",
            ];
            const CORE_NPM_PREFIXES: &[&str] = &[
                "@backstage/",
                "@mui/",
                "@emotion/",
                "@types/",
                "@vitejs/",
            ];
            if let Ok(pkg_json) = serde_json::from_str::<Value>(&raw) {
                if let Some(deps) = pkg_json.get("dependencies").and_then(|v| v.as_object()) {
                    for pkg in deps.keys() {
                        let pkg_lower = pkg.to_ascii_lowercase();
                        if CORE_NPM_PACKAGES.contains(&pkg_lower.as_str())
                            || CORE_NPM_PREFIXES.iter().any(|pfx| pkg_lower.starts_with(pfx))
                            || DISCOVERY_CATALOG_PATTERNS.iter().any(|s| {
                                s.ecosystem == "npm"
                                    && pkg_lower.contains(&s.token.to_ascii_lowercase())
                            })
                        {
                            continue;
                        }
                        let slug = pkg.trim_start_matches('@').replace('/', "-");
                        insert_manifest_candidate(
                            &mut found,
                            registered_units,
                            format!("npm:{slug}"),
                            pkg.clone(),
                            "npm",
                            if pkg_lower.contains("datadog")
                                || pkg_lower.contains("sentry")
                                || pkg_lower.contains("stripe")
                                || pkg_lower.contains("auth0")
                                || pkg_lower.contains("segment")
                            {
                                "vendor-saas"
                            } else {
                                "commercial-sdk"
                            },
                            &rel_path,
                            format!(
                                "External NPM dependency '{}' declared in {}; audit SaaS telemetry/SDK boundary.",
                                pkg, rel_path
                            ),
                            &now_iso,
                        );
                    }
                }
            }
        }
    }

    // Also scan extra `package.json` HTTP external SaaS/API references or `catalog/sourcing/*.yaml` monolith check
    let catalog_sourcing_dir = workspace_root.join("catalog").join("sourcing");
    if let Ok(entries) = std::fs::read_dir(&catalog_sourcing_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension()
                .and_then(|s| s.to_str())
                .map(|e| e.eq_ignore_ascii_case("yaml") || e.eq_ignore_ascii_case("yml"))
                .unwrap_or(false)
            {
                if let Ok(raw) = std::fs::read_to_string(&p) {
                    if let Ok(rec) = parse_sourcing_yaml_str(&raw) {
                        let k = rec.name.to_ascii_lowercase();
                        if !registered_units.contains_key(&k) {
                            let rel = p
                                .strip_prefix(workspace_root)
                                .map(|x| x.to_string_lossy().replace('\\', "/"))
                                .unwrap_or_else(|_| p.display().to_string());
                            let cid = format!("catalog-yaml:{k}");
                            found.insert(
                                cid.clone(),
                                DiscoveryCandidateRecord {
                                    candidate_id: cid,
                                    package_or_endpoint: rec.name.clone(),
                                    ecosystem: "catalog-yaml".to_string(),
                                    suggested_type: rec.resource_type.clone(),
                                    detected_in_path: rel,
                                    inferred_system: rec.system.clone(),
                                    inferred_components: rec.depends_on_components.clone(),
                                    adapter_detected: rec.adapter_abstracted,
                                    monolith_risk_note:
                                        "Unregistered catalog YAML discovered on disk; promote to DB SSOT"
                                            .to_string(),
                                    status: "pending".to_string(),
                                    promoted_unit_name: None,
                                    discovered_at: now_iso.clone(),
                                    sync_pending: true,
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    found.into_values().collect()
}

fn collect_source_imports(
    root: &Path,
    adapter_hits: &mut usize,
    domain_hits: &mut BTreeMap<&'static str, BTreeSet<String>>,
) {
    let dirs = [
        root.join("client").join("src"),
        root.join("portal").join("src"),
        root.join("server"),
    ];
    for dir in dirs {
        scan_dir_for_imports(&dir, root, 0, adapter_hits, domain_hits);
    }
}

fn scan_dir_for_imports(
    dir: &Path,
    root: &Path,
    depth: usize,
    adapter_hits: &mut usize,
    domain_hits: &mut BTreeMap<&'static str, BTreeSet<String>>,
) {
    if depth > 4 || !dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or_default();
            if matches!(name, "target" | "node_modules" | "dist" | ".git") {
                continue;
            }
            scan_dir_for_imports(&p, root, depth + 1, adapter_hits, domain_hits);
        } else if p.is_file() {
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or_default();
            if !matches!(ext, "rs" | "ts" | "tsx" | "go" | "nix" | "yaml") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&p) {
                let rel = p
                    .strip_prefix(root)
                    .map(|x| x.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| p.display().to_string());
                if rel.contains("transport") || rel.contains("Adapter") || rel.contains("pitsClient") {
                    *adapter_hits += 1;
                }
                for spec in DISCOVERY_CATALOG_PATTERNS {
                    if spec.import_needles.iter().any(|needle| content.contains(needle)) {
                        domain_hits
                            .entry(spec.token)
                            .or_default()
                            .insert(rel.clone());
                    }
                }
            }
        }
    }
}

/// Validates a date string in `YYYY-MM-DD` format, falling back safely.
pub fn normalize_renewal_date(raw: Option<&str>) -> String {
    raw.map(str::trim)
        .filter(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok())
        .map(str::to_string)
        .unwrap_or_else(|| "2027-09-30".to_string())
}

pub fn validate_suggested_sourcing_type(raw: &str) -> Result<String, String> {
    let low = raw.trim().to_ascii_lowercase();
    if SOURCING_RESOURCE_TYPES.contains(&low.as_str()) {
        Ok(low)
    } else {
        Err(format!(
            "Invalid sourcing resourceType '{}'; must be one of [{}]",
            raw,
            SOURCING_RESOURCE_TYPES.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("pits_db_test_{tag}_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn sqlite_wal_bootstrap_and_cross_connection_data_version_sync() {
        let dir = temp_path("wal_dv");
        let db_file = dir.join("sourcing-audit.sqlite3");
        let legacy = BTreeMap::new();

        let h1 = SourcingDbHandle::open_or_init(db_file.clone(), false, None, &legacy)
            .expect("open h1");
        assert!(!h1.sqlite_version.is_empty());
        assert_eq!(h1.journal_mode.to_ascii_lowercase(), "wal");
        let snap1 = h1.load_snapshot_records().unwrap();
        assert_eq!(snap1.records.len(), 5);
        assert_eq!(
            snap1
                .ballots_by_unit
                .get("commercial-mip-solver-sdk")
                .map(|b| b.len()),
            Some(2)
        );
        // commercial-mip-solver-sdk seed has 4.4 vs 2.6 = 1.8 spread -> triggers variance alert!
        assert!(
            snap1
                .records
                .get("commercial-mip-solver-sdk")
                .unwrap()
                .audit
                .contract_meta
                .blind_score_variance_alert
        );

        // Open second connection (simulating external `pitsctl mcp` process) and mutate WAL
        let h2 = SourcingDbHandle::open_or_init(db_file.clone(), false, None, &legacy)
            .expect("open h2");
        assert!(!h1.check_external_db_mutation());

        let stats = h2
            .submit_blind_ballot(
                &SubmitBlindBallotRequest {
                    unit_name: "commercial-mip-solver-sdk".to_string(),
                    reviewer_role: "procurement".to_string(),
                    reviewer_id: "cpo@pits.local".to_string(),
                    y2_core_competitiveness: 2.5,
                    y4_price_elasticity: 1.5,
                    x2_market_scarcity: 5.0,
                    x3_customization_depth: 4.0, // spread 4.4 - 4.0 = 0.4 < 1.5 -> alert clears!
                    x4_compliance_fragility: 3.0,
                    evidence_notes: "Calibrated after reviewing adapter roadmap".to_string(),
                },
                "mcp-stdio",
            )
            .expect("h2 submit ballot");
        assert!(!stats.variance_alert);
        assert_eq!(stats.max_spread, 0.4);

        // h1 must immediately detect h2's WAL commit via PRAGMA data_version!
        assert!(h1.check_external_db_mutation());
        let snap1_after = h1.load_snapshot_records().unwrap();
        assert!(
            !snap1_after
                .records
                .get("commercial-mip-solver-sdk")
                .unwrap()
                .audit
                .contract_meta
                .blind_score_variance_alert
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn postgres_wire_scram_sha256_and_bidirectional_outbox_sync() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Spawn lightweight in-process mock PostgreSQL v3 server enforcing RFC 5802 SCRAM-SHA-256
        let server_task = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut len_buf = [0u8; 4];
            sock.read_exact(&mut len_buf).await.unwrap();
            let len = i32::from_be_bytes(len_buf);
            let mut body = vec![0u8; (len - 4) as usize];
            sock.read_exact(&mut body).await.unwrap();
            // Handle optional SSLRequest ('N') then StartupMessage
            if i32::from_be_bytes([body[0], body[1], body[2], body[3]]) == 80_877_103 {
                sock.write_all(b"N").await.unwrap();
                sock.read_exact(&mut len_buf).await.unwrap();
                let s_len = i32::from_be_bytes(len_buf);
                let mut s_body = vec![0u8; (s_len - 4) as usize];
                sock.read_exact(&mut s_body).await.unwrap();
            }

            async fn write_pkt(sock: &mut TcpStream, tag: u8, payload: &[u8]) {
                let mut out = Vec::with_capacity(5 + payload.len());
                out.push(tag);
                out.extend_from_slice(&((payload.len() + 4) as i32).to_be_bytes());
                out.extend_from_slice(payload);
                sock.write_all(&out).await.unwrap();
            }
            async fn read_pkt(sock: &mut TcpStream) -> (u8, Vec<u8>) {
                let mut hdr = [0u8; 5];
                sock.read_exact(&mut hdr).await.unwrap();
                let l = i32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize - 4;
                let mut b = vec![0u8; l];
                if l > 0 {
                    sock.read_exact(&mut b).await.unwrap();
                }
                (hdr[0], b)
            }

            // 1. Send AuthenticationSASL (10) offering SCRAM-SHA-256
            let mut sasl_init = Vec::new();
            sasl_init.extend_from_slice(&10i32.to_be_bytes());
            sasl_init.extend_from_slice(b"SCRAM-SHA-256\0\0");
            write_pkt(&mut sock, b'R', &sasl_init).await;

            // Read SASLInitialResponse ('p')
            let (tag, body) = read_pkt(&mut sock).await;
            assert_eq!(tag, b'p');
            let nul = body.iter().position(|&b| b == 0).unwrap();
            assert_eq!(&body[..nul], b"SCRAM-SHA-256");
            let c_first = std::str::from_utf8(&body[nul + 5..]).unwrap().to_string();
            let c_first_bare = c_first.strip_prefix("n,,").unwrap().to_string();
            let c_nonce = c_first_bare
                .split(',')
                .find_map(|p| p.strip_prefix("r="))
                .unwrap();

            // 2. Send AuthenticationSASLContinue (11)
            let salt = b"pits-scram-salt!";
            let salt_b64 = base64::engine::general_purpose::STANDARD.encode(salt);
            let full_nonce = format!("{c_nonce}srvnonce123");
            let s_first = format!("r={full_nonce},s={salt_b64},i=4096");
            let mut sasl_cont = Vec::new();
            sasl_cont.extend_from_slice(&11i32.to_be_bytes());
            sasl_cont.extend_from_slice(s_first.as_bytes());
            write_pkt(&mut sock, b'R', &sasl_cont).await;

            // Read SASLResponse ('p') and verify client proof
            let (tag2, body2) = read_pkt(&mut sock).await;
            assert_eq!(tag2, b'p');
            let c_final = std::str::from_utf8(&body2).unwrap();
            let proof_b64 = c_final.split(',').find_map(|p| p.strip_prefix("p=")).unwrap();
            let got_proof = base64::engine::general_purpose::STANDARD
                .decode(proof_b64)
                .unwrap();

            let salted = pbkdf2_hmac_sha256(b"secretpass", salt, 4096);
            let client_key = hmac_sha256(&salted, b"Client Key");
            let stored_key: [u8; 32] = Sha256::digest(&client_key).into();
            let auth_msg = format!("{c_first_bare},{s_first},c=biws,r={full_nonce}");
            let expected_client_sig = hmac_sha256(&stored_key, auth_msg.as_bytes());
            let mut expected_proof = [0u8; 32];
            for i in 0..32 {
                expected_proof[i] = client_key[i] ^ expected_client_sig[i];
            }
            assert_eq!(got_proof, expected_proof);

            let server_key = hmac_sha256(&salted, b"Server Key");
            let server_sig = hmac_sha256(&server_key, auth_msg.as_bytes());
            let s_final = format!(
                "v={}",
                base64::engine::general_purpose::STANDARD.encode(server_sig)
            );
            let mut sasl_fin = Vec::new();
            sasl_fin.extend_from_slice(&12i32.to_be_bytes());
            sasl_fin.extend_from_slice(s_final.as_bytes());
            write_pkt(&mut sock, b'R', &sasl_fin).await;

            // AuthenticationOk (0) + ReadyForQuery ('Z')
            write_pkt(&mut sock, b'R', &0i32.to_be_bytes()).await;
            write_pkt(&mut sock, b'Z', b"I").await;

            let mut pushed_queries = Vec::new();
            loop {
                let (qtag, qbody) = read_pkt(&mut sock).await;
                if qtag == b'X' {
                    break;
                }
                if qtag == b'Q' {
                    let sql = String::from_utf8_lossy(&qbody[..qbody.len().saturating_sub(1)])
                        .to_string();
                    let lower = sql.to_ascii_lowercase();
                    pushed_queries.push(sql.clone());
                    if lower.starts_with("select") && lower.contains("from sourcing_units") {
                        // Return 1 remote unit pushed from another team's workstation
                        let cols: Vec<&str> = vec![
                            "remote-pg-kms-saas",
                            "default",
                            "External HSM/KMS SaaS synced from PostgreSQL",
                            "vendor-saas",
                            "group:default/risk-engineering",
                            "core-checkout",
                            "1",
                            "cc-kms-pg-001",
                            "[\"component:default/checkout-risk-gateway\"]",
                            "CloudHSM Vault Corp",
                            "2028-01-15",
                            "2026-11-15",
                            "1",
                            "0",
                            "{\"y2_coreCompetitiveness\":4.0,\"y4_priceElasticity\":2.5,\"x2_marketScarcity\":3.5,\"x3_customizationDepth\":2.0,\"x4_complianceFragility\":4.0}",
                            "postgres-remote",
                            "99",
                            "2026-11-15T12:00:00Z",
                        ];
                        let mut data = Vec::new();
                        data.extend_from_slice(&(cols.len() as i16).to_be_bytes());
                        for c in cols {
                            data.extend_from_slice(&(c.len() as i32).to_be_bytes());
                            data.extend_from_slice(c.as_bytes());
                        }
                        write_pkt(&mut sock, b'D', &data).await;
                    }
                    write_pkt(&mut sock, b'C', b"OK\0").await;
                    write_pkt(&mut sock, b'Z', b"I").await;
                }
            }
            pushed_queries
        });

        let dir = temp_path("pg_sync");
        let db_file = dir.join("sourcing-audit.sqlite3");
        let pg_url = format!("postgres://pitsuser:secretpass@{addr}/pitsdb?sslmode=disable");
        let handle = SourcingDbHandle::open_or_init(db_file, false, Some(pg_url), &BTreeMap::new())
            .expect("init handle");
        assert!(handle.sync_status().pending_outbox_rows > 0);

        let status = handle.sync_with_postgres(None).await;
        assert!(
            status.postgres_connected,
            "expected pg connected, err={:?}",
            status.last_sync_error
        );
        assert_eq!(status.backend_mode, "postgresql+sqlite-wal");
        assert_eq!(status.pending_outbox_rows, 0);
        assert!(status
            .postgres_endpoint_redacted
            .as_deref()
            .unwrap_or_default()
            .contains("***"));
        assert_eq!(status.total_units, 6);

        let snap = handle.load_snapshot_records().unwrap();
        let pulled = snap
            .records
            .get("remote-pg-kms-saas")
            .expect("pulled remote pg unit");
        assert_eq!(pulled.cost_allocation_id, "cc-kms-pg-001");
        assert_eq!(
            snap.source_origins.get("remote-pg-kms-saas").map(String::as_str),
            Some("postgres-remote")
        );

        let queries = server_task.await.unwrap();
        assert!(queries.iter().any(|q| q.contains("INSERT INTO sourcing_units")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
