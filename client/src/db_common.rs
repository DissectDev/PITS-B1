//! Shared Relational Database Core (Dynamic SQLite3 WAL C-API Loader + Pure-Rust PostgreSQL v3.0 Wire Client).
//!
//! Used by both `catalog::sourcing_db` and `hub::db` so zero extra Cargo C-dependencies are needed
//! and both subsystems share identical SQLite WAL + SCRAM-SHA-256/TLS PostgreSQL outbox sync mechanics.

use crate::transport::{build_rustls_client_config, server_name_for_host, AsyncReadWrite};
use base64::Engine as _;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::ffi::{c_void, CStr, CString};
use std::path::Path;
use std::sync::OnceLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use url::Url;

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

pub struct SqliteConn {
    api: &'static SqliteApi,
    db: *mut c_void,
    pub is_memory: bool,
    pub path_label: String,
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
    pub fn open(path: &Path, in_memory: bool) -> Result<Self, String> {
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

    pub fn version(&self) -> String {
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

    pub fn last_err(&self) -> String {
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

    pub fn exec_batch(&self, sql: &str) -> Result<(), String> {
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

    pub fn execute(&self, sql: &str, params: &[SqlVal]) -> Result<(), String> {
        let _ = self.query_rows(sql, params)?;
        Ok(())
    }

    pub fn query_rows(
        &self,
        sql: &str,
        params: &[SqlVal],
    ) -> Result<Vec<Vec<Option<String>>>, String> {
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
            return Err(format!(
                "sqlite3_prepare_v2 failed ({rc}): {} | sql={sql}",
                self.last_err()
            ));
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
                return Err(format!(
                    "sqlite3_bind col {idx} failed ({bind_rc}): {}",
                    self.last_err()
                ));
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

    pub fn pragma_data_version(&self) -> i64 {
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
// URL Redaction & SQL Escaping Helpers
// ---------------------------------------------------------------------------

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

pub fn pg_str(s: &str) -> Result<String, String> {
    if s.contains('\0') {
        return Err("SQL string literal cannot contain NUL bytes".to_string());
    }
    Ok(format!("'{}'", s.replace('\'', "''")))
}

pub fn pg_opt_str(s: Option<&str>) -> Result<String, String> {
    match s {
        Some(v) => pg_str(v),
        None => Ok("NULL".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Native Pure-Rust PostgreSQL v3.0 Wire Client (TCP + TLS + SCRAM-SHA-256)
// ---------------------------------------------------------------------------

pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
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

pub fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
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
                b'N' if sslmode == "require"
                    || sslmode == "verify-ca"
                    || sslmode == "verify-full" =>
                {
                    return Err(
                        "PostgreSQL server refused TLS while sslmode requires it".to_string()
                    );
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
                        0 => {}
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
                            scram_client_nonce =
                                base64::engine::general_purpose::STANDARD.encode(nonce_bytes);
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
                                return Err(
                                    "SCRAM server nonce did not preserve client nonce".to_string()
                                );
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
                                    .ok_or_else(|| {
                                        "SCRAM server-final missing signature".to_string()
                                    })?;
                                let got = base64::engine::general_purpose::STANDARD
                                    .decode(v_b64.as_bytes())
                                    .map_err(|e| format!("Invalid SCRAM v= base64: {e}"))?;
                                if got.len() != expected.len()
                                    || got.iter().zip(expected.iter()).any(|(a, b)| a != b)
                                {
                                    return Err(
                                        "SCRAM-SHA-256 server signature verification failed"
                                            .to_string(),
                                    );
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
                                let s =
                                    String::from_utf8_lossy(&body[pos..pos + ulen]).into_owned();
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
