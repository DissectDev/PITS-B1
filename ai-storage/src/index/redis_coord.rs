//! Optional Valkey / Redis Distributed Coordination Adapter (`state://` CAS + `shared://` Leases).
//!
//! Uses atomic Lua scripts for Compare-And-Swap (`state_cas`) and Lease fencing locks.

use anyhow::{anyhow, Result};
use chrono::Utc;
use redis::aio::ConnectionManager;
use redis::Script;
use tracing::info;

use crate::index::sqlite::{LeaseRecord, StateCasRecord};

#[derive(Clone)]
pub struct RedisCoordinator {
    conn: ConnectionManager,
    prefix: String,
}

const CAS_LUA: &str = r#"
local key = KEYS[1]
local exp_ver = ARGV[1]
local exp_etag = ARGV[2]
local new_val = ARGV[3]
local new_etag = ARGV[4]
local updated_at = ARGV[5]

local cur_ver = redis.call('HGET', key, 'version')
local cur_etag = redis.call('HGET', key, 'etag')
local next_ver = 1

if cur_ver then
    next_ver = tonumber(cur_ver) + 1
    if exp_ver ~= '' and tonumber(exp_ver) ~= tonumber(cur_ver) then
        return {0, 'CAS version mismatch: expected ' .. exp_ver .. ', actual ' .. cur_ver}
    end
    if exp_etag ~= '' and exp_etag ~= (cur_etag or '') then
        return {0, 'CAS etag mismatch: expected ' .. exp_etag .. ', actual ' .. (cur_etag or '')}
    end
else
    if exp_ver ~= '' and tonumber(exp_ver) ~= 0 then
        return {0, 'CAS key missing: expected version ' .. exp_ver}
    end
    if exp_etag ~= '' then
        return {0, 'CAS key missing: expected etag ' .. exp_etag}
    end
end

redis.call('HSET', key, 'value', new_val, 'version', tostring(next_ver), 'etag', new_etag, 'updated_at', updated_at)
return {1, tostring(next_ver)}
"#;

const LEASE_ACQUIRE_LUA: &str = r#"
local key = KEYS[1]
local owner = ARGV[1]
local ttl_ms = tonumber(ARGV[2])
local seq_key = KEYS[2]

local cur = redis.call('GET', key)
if cur then
    local sep = string.find(cur, ':')
    local cur_owner = sep and string.sub(cur, 1, sep - 1) or cur
    if cur_owner ~= owner then
        return {0, 'lease held by ' .. cur_owner}
    end
end
local token = redis.call('INCR', seq_key)
redis.call('SET', key, owner .. ':' .. tostring(token), 'PX', ttl_ms)
return {1, tostring(token)}
"#;

const LEASE_RELEASE_LUA: &str = r#"
local key = KEYS[1]
local owner = ARGV[1]
local cur = redis.call('GET', key)
if not cur then
    return {1, 'absent'}
end
local sep = string.find(cur, ':')
local cur_owner = sep and string.sub(cur, 1, sep - 1) or cur
if cur_owner ~= owner then
    return {0, 'lease held by ' .. cur_owner}
end
redis.call('DEL', key)
return {1, 'released'}
"#;

impl RedisCoordinator {
    pub async fn connect(url: &str, prefix: &str) -> Result<Self> {
        let client = redis::Client::open(url)?;
        let conn = ConnectionManager::new(client).await?;
        info!("Connected Redis/Valkey coordinator (prefix={})", prefix);
        Ok(Self {
            conn,
            prefix: prefix.trim_matches(':').to_string(),
        })
    }

    fn key(&self, kind: &str, raw: &str) -> String {
        format!("{}:{}:{}", self.prefix, kind, raw)
    }

    pub async fn state_get(&self, key: &str) -> Result<Option<StateCasRecord>> {
        let mut conn = self.conn.clone();
        let full = self.key("state", key);
        let vals: Vec<Option<String>> = redis::cmd("HMGET")
            .arg(&full)
            .arg(&["value", "version", "etag", "updated_at"])
            .query_async(&mut conn)
            .await?;
        if vals.iter().all(|v| v.is_none()) {
            return Ok(None);
        }
        let value_str = vals[0].clone().unwrap_or_else(|| "null".to_string());
        let version = vals[1].as_deref().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
        let etag = vals[2].clone().unwrap_or_default();
        let updated_at = vals[3].clone().unwrap_or_default();
        Ok(Some(StateCasRecord {
            key: key.to_string(),
            value: serde_json::from_str(&value_str).unwrap_or(serde_json::Value::Null),
            version,
            etag,
            updated_at,
        }))
    }

    pub async fn state_cas(
        &self,
        key: &str,
        value: &serde_json::Value,
        expected_version: Option<i64>,
        expected_etag: Option<&str>,
        new_etag: &str,
        now_iso: &str,
    ) -> Result<StateCasRecord> {
        let mut conn = self.conn.clone();
        let full = self.key("state", key);
        let val_json = serde_json::to_string(value)?;
        let script = Script::new(CAS_LUA);
        let res: (i64, String) = script
            .key(&full)
            .arg(expected_version.map(|v| v.to_string()).unwrap_or_default())
            .arg(expected_etag.unwrap_or_default())
            .arg(&val_json)
            .arg(new_etag)
            .arg(now_iso)
            .invoke_async(&mut conn)
            .await?;
        if res.0 == 0 {
            return Err(anyhow!(res.1));
        }
        let ver = res.1.parse::<i64>().unwrap_or(1);
        Ok(StateCasRecord {
            key: key.to_string(),
            value: value.clone(),
            version: ver,
            etag: new_etag.to_string(),
            updated_at: now_iso.to_string(),
        })
    }

    pub async fn acquire_lease(&self, lock_key: &str, owner: &str, ttl_secs: i64) -> Result<LeaseRecord> {
        let mut conn = self.conn.clone();
        let full = self.key("lease", lock_key);
        let seq = self.key("lease_seq", lock_key);
        let ttl_ms = ttl_secs.max(1) * 1000;
        let script = Script::new(LEASE_ACQUIRE_LUA);
        let res: (i64, String) = script
            .key(&full)
            .key(&seq)
            .arg(owner)
            .arg(ttl_ms)
            .invoke_async(&mut conn)
            .await?;
        if res.0 == 0 {
            return Err(anyhow!(res.1));
        }
        let token = res.1.parse::<i64>().unwrap_or(1);
        let now_unix = Utc::now().timestamp();
        Ok(LeaseRecord {
            lock_key: lock_key.to_string(),
            owner: owner.to_string(),
            fencing_token: token,
            expires_at_unix: now_unix + ttl_secs.max(1),
        })
    }

    pub async fn check_lease_holder(&self, lock_key: &str) -> Result<Option<LeaseRecord>> {
        let mut conn = self.conn.clone();
        let full = self.key("lease", lock_key);
        let cur: Option<String> = redis::cmd("GET").arg(&full).query_async(&mut conn).await?;
        let pttl: i64 = redis::cmd("PTTL").arg(&full).query_async(&mut conn).await?;
        if let Some(raw) = cur {
            let mut parts = raw.splitn(2, ':');
            let owner = parts.next().unwrap_or_default().to_string();
            let token = parts.next().and_then(|s| s.parse::<i64>().ok()).unwrap_or(1);
            let now_unix = Utc::now().timestamp();
            return Ok(Some(LeaseRecord {
                lock_key: lock_key.to_string(),
                owner,
                fencing_token: token,
                expires_at_unix: now_unix + (pttl.max(0) / 1000),
            }));
        }
        Ok(None)
    }

    pub async fn release_lease(&self, lock_key: &str, owner: &str) -> Result<bool> {
        let mut conn = self.conn.clone();
        let full = self.key("lease", lock_key);
        let script = Script::new(LEASE_RELEASE_LUA);
        let res: (i64, String) = script.key(&full).arg(owner).invoke_async(&mut conn).await?;
        if res.0 == 0 {
            return Err(anyhow!(res.1));
        }
        Ok(res.1 == "released")
    }
}
