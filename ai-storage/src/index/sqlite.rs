//! Dynamic SQLite3 WAL Index & State Engine (zero C build dependencies).
//!
//! Loads `winsqlite3.dll` / `sqlite3.dll` on Windows and `libsqlite3.so.0` / `libsqlite3.so`
//! on Unix at runtime, providing WAL-backed metadata tables for all 8 AI Storage Archetypes:
//! - `ws_snapshots` & `ws_entries` (`ws://` Snapshot / Diff / Rollback / Fork)
//! - `mem_entries` (`mem://` Scoped Memory + TTL + Cosine Vector + Keyword recall)
//! - `kb_documents` (`kb://` Trigram/FTS/Regex/Vector Knowledge Index)
//! - `data_cache_entries` (`data://` & `models://` NVMe LRU cache catalog)
//! - `artifact_lineage` (`artifacts://` SHA256 Content-addressed + Run/Agent lineage)
//! - `state_kv` & `leases` (`state://` & `shared://` CAS & Lease coordination)

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::ffi::{c_void, CStr, CString};
use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

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
type FnExec = unsafe extern "C" fn(*mut c_void, *const u8, *mut c_void, *mut c_void, *mut *mut u8) -> i32;
type FnPrepareV2 = unsafe extern "C" fn(*mut c_void, *const u8, i32, *mut *mut c_void, *mut *const u8) -> i32;
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

fn get_sqlite_api() -> Result<&'static SqliteApi> {
    SQLITE_API
        .get_or_init(|| unsafe { load_sqlite_api() })
        .as_ref()
        .map_err(|e| anyhow!(e.clone()))
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
        return Err("Failed to dlopen libsqlite3.so.0 / libsqlite3.so".to_string());
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
pub enum SqlVal {
    Text(String),
    Int(i64),
    Real(f64),
    Null,
}

impl SqlVal {
    pub fn opt_text(v: Option<&str>) -> Self {
        match v {
            Some(s) => Self::Text(s.to_string()),
            None => Self::Null,
        }
    }
}

#[derive(Debug, Clone)]
pub enum OutVal {
    Text(String),
    Int(i64),
    Real(f64),
    Null,
}

impl OutVal {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            OutVal::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            OutVal::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            OutVal::Real(r) => Some(*r),
            OutVal::Int(i) => Some(*i as f64),
            _ => None,
        }
    }
}

struct SqliteConn {
    api: &'static SqliteApi,
    db: *mut c_void,
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
    fn open(path: &Path, in_memory: bool) -> Result<Self> {
        let api = get_sqlite_api()?;
        if !in_memory {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
        }
        let target = if in_memory {
            ":memory:".to_string()
        } else {
            path.to_string_lossy().to_string()
        };
        let c_target = CString::new(target)?;
        let mut db: *mut c_void = std::ptr::null_mut();
        let flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX;
        let rc = unsafe { (api.open_v2)(c_target.as_ptr() as *const u8, &mut db, flags, std::ptr::null()) };
        if rc != SQLITE_OK || db.is_null() {
            return Err(anyhow!("sqlite3_open_v2 failed (rc={})", rc));
        }
        let conn = Self { api, db };
        unsafe {
            (api.busy_timeout)(conn.db, 5000);
        }
        if !in_memory {
            conn.exec_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
        } else {
            conn.exec_batch("PRAGMA foreign_keys=ON;")?;
        }
        Ok(conn)
    }

    fn version(&self) -> String {
        unsafe {
            let p = (self.api.libversion)() as *const std::os::raw::c_char;
            if p.is_null() {
                "unknown".to_string()
            } else {
                CStr::from_ptr(p).to_string_lossy().to_string()
            }
        }
    }

    fn last_err(&self) -> String {
        unsafe {
            let p = (self.api.errmsg)(self.db) as *const std::os::raw::c_char;
            if p.is_null() {
                "sqlite error".to_string()
            } else {
                CStr::from_ptr(p).to_string_lossy().to_string()
            }
        }
    }

    fn exec_batch(&self, sql: &str) -> Result<()> {
        let c_sql = CString::new(sql)?;
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
            return Err(anyhow!("sqlite exec failed: {}", self.last_err()));
        }
        Ok(())
    }

    fn execute(&self, sql: &str, params: &[SqlVal]) -> Result<()> {
        let c_sql = CString::new(sql)?;
        let mut stmt: *mut c_void = std::ptr::null_mut();
        let rc = unsafe {
            (self.api.prepare_v2)(
                self.db,
                c_sql.as_ptr() as *const u8,
                sql.len() as i32,
                &mut stmt,
                std::ptr::null_mut(),
            )
        };
        if rc != SQLITE_OK || stmt.is_null() {
            return Err(anyhow!("sqlite prepare failed: {}", self.last_err()));
        }
        let res = (|| {
            self.bind_params(stmt, params)?;
            let step_rc = unsafe { (self.api.step)(stmt) };
            if step_rc != SQLITE_DONE && step_rc != SQLITE_ROW {
                return Err(anyhow!("sqlite step failed (rc={}): {}", step_rc, self.last_err()));
            }
            Ok(())
        })();
        unsafe {
            (self.api.finalize)(stmt);
        }
        res
    }

    fn query(&self, sql: &str, params: &[SqlVal]) -> Result<Vec<Vec<OutVal>>> {
        let c_sql = CString::new(sql)?;
        let mut stmt: *mut c_void = std::ptr::null_mut();
        let rc = unsafe {
            (self.api.prepare_v2)(
                self.db,
                c_sql.as_ptr() as *const u8,
                sql.len() as i32,
                &mut stmt,
                std::ptr::null_mut(),
            )
        };
        if rc != SQLITE_OK || stmt.is_null() {
            return Err(anyhow!("sqlite prepare failed: {}", self.last_err()));
        }
        let res = (|| {
            self.bind_params(stmt, params)?;
            let col_count = unsafe { (self.api.column_count)(stmt) };
            let mut rows = Vec::new();
            loop {
                let step_rc = unsafe { (self.api.step)(stmt) };
                if step_rc == SQLITE_DONE {
                    break;
                }
                if step_rc != SQLITE_ROW {
                    return Err(anyhow!("sqlite query step failed: {}", self.last_err()));
                }
                let mut row = Vec::with_capacity(col_count as usize);
                for i in 0..col_count {
                    let ctype = unsafe { (self.api.column_type)(stmt, i) };
                    let val = match ctype {
                        SQLITE_NULL => OutVal::Null,
                        1 => OutVal::Int(unsafe { (self.api.column_int64)(stmt, i) }),
                        2 => OutVal::Real(unsafe { (self.api.column_double)(stmt, i) }),
                        _ => {
                            let ptr = unsafe { (self.api.column_text)(stmt, i) };
                            let len = unsafe { (self.api.column_bytes)(stmt, i) }.max(0) as usize;
                            if ptr.is_null() {
                                OutVal::Null
                            } else {
                                let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
                                OutVal::Text(String::from_utf8_lossy(slice).to_string())
                            }
                        }
                    };
                    row.push(val);
                }
                rows.push(row);
            }
            Ok(rows)
        })();
        unsafe {
            (self.api.finalize)(stmt);
        }
        res
    }

    fn bind_params(&self, stmt: *mut c_void, params: &[SqlVal]) -> Result<()> {
        for (idx, val) in params.iter().enumerate() {
            let pos = (idx + 1) as i32;
            let rc = match val {
                SqlVal::Text(s) => unsafe {
                    (self.api.bind_text)(stmt, pos, s.as_ptr(), s.len() as i32, SQLITE_TRANSIENT)
                },
                SqlVal::Int(i) => unsafe { (self.api.bind_int64)(stmt, pos, *i) },
                SqlVal::Real(r) => unsafe { (self.api.bind_double)(stmt, pos, *r) },
                SqlVal::Null => unsafe { (self.api.bind_null)(stmt, pos) },
            };
            if rc != SQLITE_OK {
                return Err(anyhow!("sqlite bind param {} failed: {}", pos, self.last_err()));
            }
        }
        Ok(())
    }
}

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS ws_snapshots (
    snapshot_id TEXT PRIMARY KEY,
    workspace_prefix TEXT NOT NULL,
    label TEXT NOT NULL,
    parent_snapshot_id TEXT,
    archive_uri TEXT,
    file_count INTEGER NOT NULL,
    total_bytes INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS ws_snapshot_entries (
    snapshot_id TEXT NOT NULL,
    rel_path TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    object_key TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, rel_path)
);

CREATE TABLE IF NOT EXISTS mem_entries (
    mem_id TEXT PRIMARY KEY,
    scope TEXT NOT NULL,
    uri TEXT NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    tags_json TEXT NOT NULL,
    embedding_json TEXT,
    importance REAL NOT NULL DEFAULT 1.0,
    created_at TEXT NOT NULL,
    expires_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_mem_scope ON mem_entries(scope);

CREATE TABLE IF NOT EXISTS kb_documents (
    uri TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    embedding_json TEXT,
    metadata_json TEXT NOT NULL,
    indexed_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS data_cache_entries (
    cache_key TEXT PRIMARY KEY,
    uri TEXT NOT NULL,
    local_path TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    last_accessed_unix INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS artifact_lineage (
    artifact_id TEXT PRIMARY KEY,
    uri TEXT NOT NULL,
    logical_path TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    run_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    source_ws TEXT,
    prompt_hash TEXT,
    metadata_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_art_sha256 ON artifact_lineage(sha256);

CREATE TABLE IF NOT EXISTS state_kv (
    state_key TEXT PRIMARY KEY,
    value_json TEXT NOT NULL,
    version INTEGER NOT NULL,
    etag TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS leases (
    lock_key TEXT PRIMARY KEY,
    owner TEXT NOT NULL,
    fencing_token INTEGER NOT NULL,
    expires_at_unix INTEGER NOT NULL
);
"#;

pub struct SqliteStore {
    conn: Mutex<SqliteConn>,
    pub version: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub snapshot_id: String,
    pub workspace_prefix: String,
    pub label: String,
    pub parent_snapshot_id: Option<String>,
    pub archive_uri: Option<String>,
    pub file_count: i64,
    pub total_bytes: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub rel_path: String,
    pub sha256: String,
    pub size_bytes: i64,
    pub object_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub mem_id: String,
    pub scope: String,
    pub uri: String,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub embedding: Option<Vec<f32>>,
    pub importance: f64,
    pub created_at: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KbDocRecord {
    pub uri: String,
    pub title: String,
    pub content: String,
    pub sha256: String,
    pub embedding: Option<Vec<f32>>,
    pub metadata: serde_json::Value,
    pub indexed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactLineageRecord {
    pub artifact_id: String,
    pub uri: String,
    pub logical_path: String,
    pub sha256: String,
    pub size_bytes: i64,
    pub run_id: String,
    pub agent_id: String,
    pub source_ws: Option<String>,
    pub prompt_hash: Option<String>,
    pub metadata: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateCasRecord {
    pub key: String,
    pub value: serde_json::Value,
    pub version: i64,
    pub etag: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRecord {
    pub lock_key: String,
    pub owner: String,
    pub fencing_token: i64,
    pub expires_at_unix: i64,
}

impl SqliteStore {
    pub fn open(path: &Path, in_memory: bool) -> Result<Self> {
        let conn = SqliteConn::open(path, in_memory)?;
        let version = conn.version();
        conn.exec_batch(SCHEMA_SQL)?;
        Ok(Self {
            conn: Mutex::new(conn),
            version,
            path: if in_memory {
                ":memory:".to_string()
            } else {
                path.to_string_lossy().to_string()
            },
        })
    }

    pub fn save_snapshot(&self, meta: &SnapshotMeta, entries: &[SnapshotEntry]) -> Result<()> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        guard.exec_batch("BEGIN IMMEDIATE;")?;
        let res = (|| {
            guard.execute(
                "INSERT INTO ws_snapshots (snapshot_id, workspace_prefix, label, parent_snapshot_id, archive_uri, file_count, total_bytes, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                &[
                    SqlVal::Text(meta.snapshot_id.clone()),
                    SqlVal::Text(meta.workspace_prefix.clone()),
                    SqlVal::Text(meta.label.clone()),
                    SqlVal::opt_text(meta.parent_snapshot_id.as_deref()),
                    SqlVal::opt_text(meta.archive_uri.as_deref()),
                    SqlVal::Int(meta.file_count),
                    SqlVal::Int(meta.total_bytes),
                    SqlVal::Text(meta.created_at.clone()),
                ],
            )?;
            for e in entries {
                guard.execute(
                    "INSERT INTO ws_snapshot_entries (snapshot_id, rel_path, sha256, size_bytes, object_key)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    &[
                        SqlVal::Text(meta.snapshot_id.clone()),
                        SqlVal::Text(e.rel_path.clone()),
                        SqlVal::Text(e.sha256.clone()),
                        SqlVal::Int(e.size_bytes),
                        SqlVal::Text(e.object_key.clone()),
                    ],
                )?;
            }
            Ok(())
        })();
        if res.is_ok() {
            guard.exec_batch("COMMIT;")?;
        } else {
            let _ = guard.exec_batch("ROLLBACK;");
        }
        res
    }

    pub fn list_snapshots(&self, prefix: Option<&str>) -> Result<Vec<SnapshotMeta>> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        let rows = if let Some(p) = prefix {
            guard.query(
                "SELECT snapshot_id, workspace_prefix, label, parent_snapshot_id, archive_uri, file_count, total_bytes, created_at
                 FROM ws_snapshots WHERE workspace_prefix = ?1 ORDER BY created_at DESC",
                &[SqlVal::Text(p.to_string())],
            )?
        } else {
            guard.query(
                "SELECT snapshot_id, workspace_prefix, label, parent_snapshot_id, archive_uri, file_count, total_bytes, created_at
                 FROM ws_snapshots ORDER BY created_at DESC",
                &[],
            )?
        };
        Ok(rows
            .into_iter()
            .map(|r| SnapshotMeta {
                snapshot_id: r[0].as_str().unwrap_or_default().to_string(),
                workspace_prefix: r[1].as_str().unwrap_or_default().to_string(),
                label: r[2].as_str().unwrap_or_default().to_string(),
                parent_snapshot_id: r[3].as_str().map(str::to_string),
                archive_uri: r[4].as_str().map(str::to_string),
                file_count: r[5].as_i64().unwrap_or(0),
                total_bytes: r[6].as_i64().unwrap_or(0),
                created_at: r[7].as_str().unwrap_or_default().to_string(),
            })
            .collect())
    }

    pub fn get_snapshot_entries(&self, snapshot_id: &str) -> Result<Vec<SnapshotEntry>> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        let rows = guard.query(
            "SELECT rel_path, sha256, size_bytes, object_key FROM ws_snapshot_entries WHERE snapshot_id = ?1 ORDER BY rel_path",
            &[SqlVal::Text(snapshot_id.to_string())],
        )?;
        Ok(rows
            .into_iter()
            .map(|r| SnapshotEntry {
                rel_path: r[0].as_str().unwrap_or_default().to_string(),
                sha256: r[1].as_str().unwrap_or_default().to_string(),
                size_bytes: r[2].as_i64().unwrap_or(0),
                object_key: r[3].as_str().unwrap_or_default().to_string(),
            })
            .collect())
    }

    pub fn upsert_memory(&self, rec: &MemoryRecord) -> Result<()> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        let tags_json = serde_json::to_string(&rec.tags)?;
        let emb_json = rec
            .embedding
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        guard.execute(
            "INSERT INTO mem_entries (mem_id, scope, uri, title, content, tags_json, embedding_json, importance, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(mem_id) DO UPDATE SET
               scope=excluded.scope, uri=excluded.uri, title=excluded.title, content=excluded.content,
               tags_json=excluded.tags_json, embedding_json=excluded.embedding_json,
               importance=excluded.importance, expires_at=excluded.expires_at",
            &[
                SqlVal::Text(rec.mem_id.clone()),
                SqlVal::Text(rec.scope.clone()),
                SqlVal::Text(rec.uri.clone()),
                SqlVal::Text(rec.title.clone()),
                SqlVal::Text(rec.content.clone()),
                SqlVal::Text(tags_json),
                SqlVal::opt_text(emb_json.as_deref()),
                SqlVal::Real(rec.importance),
                SqlVal::Text(rec.created_at.clone()),
                SqlVal::opt_text(rec.expires_at.as_deref()),
            ],
        )
    }

    pub fn list_memories(&self, scope_prefix: Option<&str>) -> Result<Vec<MemoryRecord>> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        let rows = if let Some(prefix) = scope_prefix {
            guard.query(
                "SELECT mem_id, scope, uri, title, content, tags_json, embedding_json, importance, created_at, expires_at
                 FROM mem_entries WHERE scope = ?1 OR scope LIKE ?2 ORDER BY created_at DESC",
                &[SqlVal::Text(prefix.to_string()), SqlVal::Text(format!("{}/%", prefix.trim_end_matches('/')))],
            )?
        } else {
            guard.query(
                "SELECT mem_id, scope, uri, title, content, tags_json, embedding_json, importance, created_at, expires_at
                 FROM mem_entries ORDER BY created_at DESC",
                &[],
            )?
        };
        Ok(rows
            .into_iter()
            .map(|r| MemoryRecord {
                mem_id: r[0].as_str().unwrap_or_default().to_string(),
                scope: r[1].as_str().unwrap_or_default().to_string(),
                uri: r[2].as_str().unwrap_or_default().to_string(),
                title: r[3].as_str().unwrap_or_default().to_string(),
                content: r[4].as_str().unwrap_or_default().to_string(),
                tags: r[5]
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default(),
                embedding: r[6]
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok()),
                importance: r[7].as_f64().unwrap_or(1.0),
                created_at: r[8].as_str().unwrap_or_default().to_string(),
                expires_at: r[9].as_str().map(str::to_string),
            })
            .collect())
    }

    pub fn delete_memory(&self, mem_id: &str) -> Result<()> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        guard.execute("DELETE FROM mem_entries WHERE mem_id = ?1 OR uri = ?1", &[SqlVal::Text(mem_id.to_string())])
    }

    pub fn upsert_kb_doc(&self, doc: &KbDocRecord) -> Result<()> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        let emb_json = doc.embedding.as_ref().map(serde_json::to_string).transpose()?;
        let meta_json = serde_json::to_string(&doc.metadata)?;
        guard.execute(
            "INSERT INTO kb_documents (uri, title, content, sha256, embedding_json, metadata_json, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(uri) DO UPDATE SET
               title=excluded.title, content=excluded.content, sha256=excluded.sha256,
               embedding_json=excluded.embedding_json, metadata_json=excluded.metadata_json, indexed_at=excluded.indexed_at",
            &[
                SqlVal::Text(doc.uri.clone()),
                SqlVal::Text(doc.title.clone()),
                SqlVal::Text(doc.content.clone()),
                SqlVal::Text(doc.sha256.clone()),
                SqlVal::opt_text(emb_json.as_deref()),
                SqlVal::Text(meta_json),
                SqlVal::Text(doc.indexed_at.clone()),
            ],
        )
    }

    pub fn list_kb_docs(&self) -> Result<Vec<KbDocRecord>> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        let rows = guard.query(
            "SELECT uri, title, content, sha256, embedding_json, metadata_json, indexed_at FROM kb_documents",
            &[],
        )?;
        Ok(rows
            .into_iter()
            .map(|r| KbDocRecord {
                uri: r[0].as_str().unwrap_or_default().to_string(),
                title: r[1].as_str().unwrap_or_default().to_string(),
                content: r[2].as_str().unwrap_or_default().to_string(),
                sha256: r[3].as_str().unwrap_or_default().to_string(),
                embedding: r[4].as_str().and_then(|s| serde_json::from_str(s).ok()),
                metadata: r[5]
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_else(|| serde_json::json!({})),
                indexed_at: r[6].as_str().unwrap_or_default().to_string(),
            })
            .collect())
    }

    pub fn record_cache_entry(
        &self,
        cache_key: &str,
        uri: &str,
        local_path: &str,
        sha256: &str,
        size_bytes: i64,
        now_unix: i64,
    ) -> Result<()> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        guard.execute(
            "INSERT INTO data_cache_entries (cache_key, uri, local_path, sha256, size_bytes, last_accessed_unix)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(cache_key) DO UPDATE SET
               local_path=excluded.local_path, sha256=excluded.sha256, size_bytes=excluded.size_bytes,
               last_accessed_unix=excluded.last_accessed_unix",
            &[
                SqlVal::Text(cache_key.to_string()),
                SqlVal::Text(uri.to_string()),
                SqlVal::Text(local_path.to_string()),
                SqlVal::Text(sha256.to_string()),
                SqlVal::Int(size_bytes),
                SqlVal::Int(now_unix),
            ],
        )
    }

    pub fn list_cache_lru(&self) -> Result<Vec<(String, String, i64, i64)>> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        let rows = guard.query(
            "SELECT cache_key, local_path, size_bytes, last_accessed_unix FROM data_cache_entries ORDER BY last_accessed_unix ASC",
            &[],
        )?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r[0].as_str().unwrap_or_default().to_string(),
                    r[1].as_str().unwrap_or_default().to_string(),
                    r[2].as_i64().unwrap_or(0),
                    r[3].as_i64().unwrap_or(0),
                )
            })
            .collect())
    }

    pub fn delete_cache_entry(&self, cache_key: &str) -> Result<()> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        guard.execute(
            "DELETE FROM data_cache_entries WHERE cache_key = ?1",
            &[SqlVal::Text(cache_key.to_string())],
        )
    }

    pub fn insert_artifact_lineage(&self, rec: &ArtifactLineageRecord) -> Result<()> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        let meta_json = serde_json::to_string(&rec.metadata)?;
        guard.execute(
            "INSERT INTO artifact_lineage (artifact_id, uri, logical_path, sha256, size_bytes, run_id, agent_id, source_ws, prompt_hash, metadata_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            &[
                SqlVal::Text(rec.artifact_id.clone()),
                SqlVal::Text(rec.uri.clone()),
                SqlVal::Text(rec.logical_path.clone()),
                SqlVal::Text(rec.sha256.clone()),
                SqlVal::Int(rec.size_bytes),
                SqlVal::Text(rec.run_id.clone()),
                SqlVal::Text(rec.agent_id.clone()),
                SqlVal::opt_text(rec.source_ws.as_deref()),
                SqlVal::opt_text(rec.prompt_hash.as_deref()),
                SqlVal::Text(meta_json),
                SqlVal::Text(rec.created_at.clone()),
            ],
        )
    }

    pub fn query_artifact_lineage(&self, filter: Option<&str>) -> Result<Vec<ArtifactLineageRecord>> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        let rows = if let Some(f) = filter {
            guard.query(
                "SELECT artifact_id, uri, logical_path, sha256, size_bytes, run_id, agent_id, source_ws, prompt_hash, metadata_json, created_at
                 FROM artifact_lineage WHERE sha256 = ?1 OR run_id = ?1 OR uri = ?1 OR logical_path = ?1 ORDER BY created_at DESC",
                &[SqlVal::Text(f.to_string())],
            )?
        } else {
            guard.query(
                "SELECT artifact_id, uri, logical_path, sha256, size_bytes, run_id, agent_id, source_ws, prompt_hash, metadata_json, created_at
                 FROM artifact_lineage ORDER BY created_at DESC LIMIT 100",
                &[],
            )?
        };
        Ok(rows
            .into_iter()
            .map(|r| ArtifactLineageRecord {
                artifact_id: r[0].as_str().unwrap_or_default().to_string(),
                uri: r[1].as_str().unwrap_or_default().to_string(),
                logical_path: r[2].as_str().unwrap_or_default().to_string(),
                sha256: r[3].as_str().unwrap_or_default().to_string(),
                size_bytes: r[4].as_i64().unwrap_or(0),
                run_id: r[5].as_str().unwrap_or_default().to_string(),
                agent_id: r[6].as_str().unwrap_or_default().to_string(),
                source_ws: r[7].as_str().map(str::to_string),
                prompt_hash: r[8].as_str().map(str::to_string),
                metadata: r[9]
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_else(|| serde_json::json!({})),
                created_at: r[10].as_str().unwrap_or_default().to_string(),
            })
            .collect())
    }

    pub fn state_get(&self, key: &str) -> Result<Option<StateCasRecord>> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        let rows = guard.query(
            "SELECT state_key, value_json, version, etag, updated_at FROM state_kv WHERE state_key = ?1",
            &[SqlVal::Text(key.to_string())],
        )?;
        Ok(rows.into_iter().next().map(|r| StateCasRecord {
            key: r[0].as_str().unwrap_or_default().to_string(),
            value: r[1]
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(serde_json::Value::Null),
            version: r[2].as_i64().unwrap_or(0),
            etag: r[3].as_str().unwrap_or_default().to_string(),
            updated_at: r[4].as_str().unwrap_or_default().to_string(),
        }))
    }

    pub fn state_cas(
        &self,
        key: &str,
        value: &serde_json::Value,
        expected_version: Option<i64>,
        expected_etag: Option<&str>,
        new_etag: &str,
        now_iso: &str,
    ) -> Result<StateCasRecord> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        guard.exec_batch("BEGIN IMMEDIATE;")?;
        let res = (|| {
            let existing = guard.query(
                "SELECT version, etag FROM state_kv WHERE state_key = ?1",
                &[SqlVal::Text(key.to_string())],
            )?;
            let next_version = if let Some(row) = existing.first() {
                let cur_ver = row[0].as_i64().unwrap_or(0);
                let cur_etag = row[1].as_str().unwrap_or_default();
                if let Some(exp_v) = expected_version {
                    if exp_v != cur_ver {
                        return Err(anyhow!(
                            "CAS conflict on '{}': expected version {}, actual {}",
                            key, exp_v, cur_ver
                        ));
                    }
                }
                if let Some(exp_e) = expected_etag {
                    if exp_e != cur_etag {
                        return Err(anyhow!(
                            "CAS conflict on '{}': expected etag '{}', actual '{}'",
                            key, exp_e, cur_etag
                        ));
                    }
                }
                cur_ver + 1
            } else {
                if let Some(exp_v) = expected_version {
                    if exp_v != 0 {
                        return Err(anyhow!(
                            "CAS conflict on '{}': key does not exist (expected version {})",
                            key, exp_v
                        ));
                    }
                }
                if let Some(exp_e) = expected_etag {
                    if !exp_e.is_empty() {
                        return Err(anyhow!(
                            "CAS conflict on '{}': key does not exist (expected etag '{}')",
                            key, exp_e
                        ));
                    }
                }
                1
            };
            let val_json = serde_json::to_string(value)?;
            guard.execute(
                "INSERT INTO state_kv (state_key, value_json, version, etag, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(state_key) DO UPDATE SET
                   value_json=excluded.value_json, version=excluded.version, etag=excluded.etag, updated_at=excluded.updated_at",
                &[
                    SqlVal::Text(key.to_string()),
                    SqlVal::Text(val_json),
                    SqlVal::Int(next_version),
                    SqlVal::Text(new_etag.to_string()),
                    SqlVal::Text(now_iso.to_string()),
                ],
            )?;
            Ok(StateCasRecord {
                key: key.to_string(),
                value: value.clone(),
                version: next_version,
                etag: new_etag.to_string(),
                updated_at: now_iso.to_string(),
            })
        })();
        if res.is_ok() {
            guard.exec_batch("COMMIT;")?;
        } else {
            let _ = guard.exec_batch("ROLLBACK;");
        }
        res
    }

    pub fn acquire_lease(&self, lock_key: &str, owner: &str, ttl_secs: i64, now_unix: i64) -> Result<LeaseRecord> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        guard.exec_batch("BEGIN IMMEDIATE;")?;
        let res = (|| {
            let rows = guard.query(
                "SELECT owner, fencing_token, expires_at_unix FROM leases WHERE lock_key = ?1",
                &[SqlVal::Text(lock_key.to_string())],
            )?;
            let new_exp = now_unix + ttl_secs.max(1);
            let next_token = if let Some(row) = rows.first() {
                let cur_owner = row[0].as_str().unwrap_or_default();
                let cur_token = row[1].as_i64().unwrap_or(0);
                let cur_exp = row[2].as_i64().unwrap_or(0);
                if cur_exp > now_unix && cur_owner != owner {
                    return Err(anyhow!(
                        "lease '{}' already held by '{}' until unix {}",
                        lock_key, cur_owner, cur_exp
                    ));
                }
                cur_token + 1
            } else {
                1
            };
            guard.execute(
                "INSERT INTO leases (lock_key, owner, fencing_token, expires_at_unix)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(lock_key) DO UPDATE SET
                   owner=excluded.owner, fencing_token=excluded.fencing_token, expires_at_unix=excluded.expires_at_unix",
                &[
                    SqlVal::Text(lock_key.to_string()),
                    SqlVal::Text(owner.to_string()),
                    SqlVal::Int(next_token),
                    SqlVal::Int(new_exp),
                ],
            )?;
            Ok(LeaseRecord {
                lock_key: lock_key.to_string(),
                owner: owner.to_string(),
                fencing_token: next_token,
                expires_at_unix: new_exp,
            })
        })();
        if res.is_ok() {
            guard.exec_batch("COMMIT;")?;
        } else {
            let _ = guard.exec_batch("ROLLBACK;");
        }
        res
    }

    pub fn check_lease_holder(&self, lock_key: &str, now_unix: i64) -> Result<Option<LeaseRecord>> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        let rows = guard.query(
            "SELECT owner, fencing_token, expires_at_unix FROM leases WHERE lock_key = ?1",
            &[SqlVal::Text(lock_key.to_string())],
        )?;
        if let Some(row) = rows.first() {
            let exp = row[2].as_i64().unwrap_or(0);
            if exp > now_unix {
                return Ok(Some(LeaseRecord {
                    lock_key: lock_key.to_string(),
                    owner: row[0].as_str().unwrap_or_default().to_string(),
                    fencing_token: row[1].as_i64().unwrap_or(0),
                    expires_at_unix: exp,
                }));
            }
        }
        Ok(None)
    }

    pub fn release_lease(&self, lock_key: &str, owner: &str) -> Result<bool> {
        let guard = self.conn.lock().map_err(|_| anyhow!("sqlite mutex poisoned"))?;
        let rows = guard.query(
            "SELECT owner FROM leases WHERE lock_key = ?1",
            &[SqlVal::Text(lock_key.to_string())],
        )?;
        if let Some(row) = rows.first() {
            let cur_owner = row[0].as_str().unwrap_or_default();
            if cur_owner != owner {
                return Err(anyhow!(
                    "cannot release lease '{}': held by '{}', not '{}'",
                    lock_key, cur_owner, owner
                ));
            }
            guard.execute("DELETE FROM leases WHERE lock_key = ?1", &[SqlVal::Text(lock_key.to_string())])?;
            return Ok(true);
        }
        Ok(false)
    }
}
