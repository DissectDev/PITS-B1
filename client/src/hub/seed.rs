//! Idempotent initial seed data for PITS Self-Managed Project Hub.
//!
//! Seeds on first initialization (guarded by `seed-init-v1` audit marker + `INSERT OR IGNORE`):
//! - Top-level Project `PITS` (`id = "pits"`, `workspace_path = "c:/Project/PITS"`)
//! - `AGY-1`: Cross-session handoff & incremental acceptance verification (session `7f24bd5e-aadc-4cdf-85dd-852f0d02b0fd`)
//! - `AGY-2`: 16 Resource Type dual-track summary, GoVPP counters & DRA occupancy rollout (`d19c0e6`, session `e49edfd5-7b0b-4652-9b0f-1a0376fcdf3c`, `status = ready_for_review`, `accepted = false`)

use crate::db_common::{SqlVal, SqliteConn};
use serde_json::json;

pub const SEED_MARKER_ID: &str = "seed-init-v1";
pub const SEED_SESSION_AGY1_ID: &str = "seed-session-agy-1";
pub const SEED_SESSION_AGY2_ID: &str = "seed-session-agy-2";

pub fn seed_initial_data(conn: &SqliteConn) -> Result<bool, String> {
    let existing = conn.query_rows(
        "SELECT id FROM hub_audit_events WHERE id = ?1 LIMIT 1;",
        &[SqlVal::Text(SEED_MARKER_ID.to_string())],
    )?;
    if !existing.is_empty() {
        return Ok(false);
    }

    let ts_init = "2026-11-18T10:00:00Z";
    let ts_agy2 = "2026-11-19T03:30:00Z";

    // 1. Single Top-Level Project: PITS
    let pits_body = r#"# PITS Unified Platform Project

**Single top-level project** covering all PITS cross-workspace engineering initiatives (`pits-infra`, `pits-service`, `pits-assistant`, `pits-platform`) with first-class tags for subdomain filtering.

## Workspace Topology
- `c:/Project/PITS/Infra` — NixOS `pits-core`, Rust `pitsctl`, Go `pits-agent`, Backstage v1.56 / shadcn Portal, `cluster-pits-lab` CI/CD & Crossplane
- `c:/Project/PITS/Service` — Application & service workloads
- `c:/Project/PITS/Assistant` — Cross-workspace orchestration & skills

## Governance Rules
1. Every task receives a globally monotonic `AGY-<n>` identifier.
2. Agents pick up tasks (`in_progress`) and submit evidence-backed handoffs (`ready_for_review`, `accepted = false`).
3. **Human Acceptance Gate**: Only human operators via `pits-portal` (`/project-hub`) can accept handoffs and transition tasks to `done`.
"#;

    conn.execute(
        r#"INSERT OR IGNORE INTO hub_projects (
            id, name, status, priority, workspace_path, outcome, target_date, lead, body_md, catalog_refs, created_at, updated_at, sync_pending
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1);"#,
        &[
            SqlVal::Text("pits".to_string()),
            SqlVal::Text("PITS".to_string()),
            SqlVal::Text("in_progress".to_string()),
            SqlVal::Text("high".to_string()),
            SqlVal::Text("c:/Project/PITS".to_string()),
            SqlVal::Text(
                "Self-managed, GitOps-driven hybrid infrastructure, portal, and cross-workspace AI engineering hub with protocol-enforced human acceptance."
                    .to_string(),
            ),
            SqlVal::Null,
            SqlVal::Text("pitsadmin".to_string()),
            SqlVal::Text(pits_body.to_string()),
            SqlVal::Text(
                json!([
                    "system:default/pits-infrastructure",
                    "component:default/edge-ingress-proxy"
                ])
                .to_string(),
            ),
            SqlVal::Text(ts_init.to_string()),
            SqlVal::Text(ts_agy2.to_string()),
        ],
    )?;

    // 2. AGY-1: Cross-session handoff & acceptance workflow verification
    let agy1_ac = json!([
        {
            "id": "ac-1",
            "text": "跨工作区任务均采用单调递增 AGY-<n> 标识并支持标签维度检索",
            "agentVerified": true,
            "humanVerified": false
        },
        {
            "id": "ac-2",
            "text": "Agent 仅能提交 ready_for_review 交接且协议层禁止自行标记 Done/Accepted",
            "agentVerified": true,
            "humanVerified": false
        },
        {
            "id": "ac-3",
            "text": "人类操作者可在 PITS Portal 中一键验收或打回修改并沉淀审计流水",
            "agentVerified": true,
            "humanVerified": false
        }
    ])
    .to_string();

    let agy1_body = r#"## 目标
建立跨工作区（`Infra` / `Service` / `Assistant`）的统一任务追踪与交接验收协议，确保每一次 Agent 执行均有可追溯的 `conversation://<uuid>`、Git Commit 与验证证据，并严格由人类在 Portal 完成最终验收。

## 历史背景
原初始化任务创建于会话 `7f24bd5e-aadc-4cdf-85dd-852f0d02b0fd`，现已迁移至 PITS Self-Managed Project Hub 承接并延续 `AGY-<n>` 序列。
"#;

    conn.execute(
        r#"INSERT OR IGNORE INTO hub_tasks (
            seq_id, task_key, project_id, title, status, priority, tags_json, workspace_path,
            dispatch_target, acceptance_summary, ac_checklist_json, owner, due_date, last_session_at,
            body_md, catalog_refs, created_at, updated_at, sync_pending
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, 1);"#,
        &[
            SqlVal::Int(1),
            SqlVal::Text("AGY-1".to_string()),
            SqlVal::Text("pits".to_string()),
            SqlVal::Text("验证跨 Session 交接与逐步验收流程".to_string()),
            SqlVal::Text("ready_for_review".to_string()),
            SqlVal::Text("p1_high".to_string()),
            SqlVal::Text(json!(["pits-platform"]).to_string()),
            SqlVal::Text("c:/Project/PITS".to_string()),
            SqlVal::Text("pits-assistant".to_string()),
            SqlVal::Text(
                "验证 Antigravity 跨工作区接单、交接记录、证据挂接与人类验收硬门禁闭环。"
                    .to_string(),
            ),
            SqlVal::Text(agy1_ac),
            SqlVal::Text("pitsadmin".to_string()),
            SqlVal::Null,
            SqlVal::Text(ts_init.to_string()),
            SqlVal::Text(agy1_body.to_string()),
            SqlVal::Text("[]".to_string()),
            SqlVal::Text(ts_init.to_string()),
            SqlVal::Text(ts_init.to_string()),
        ],
    )?;

    conn.execute(
        r#"INSERT OR IGNORE INTO hub_session_logs (
            id, project_id, summary, log_type, tags_json, workspace_path, conversation_id,
            commit_refs_json, evidence_refs_json, body_md, accepted, accepted_by, accepted_at,
            review_note, session_date, created_at, updated_at, sync_pending
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, NULL, NULL, '', ?11, ?12, ?13, 1);"#,
        &[
            SqlVal::Text(SEED_SESSION_AGY1_ID.to_string()),
            SqlVal::Text("pits".to_string()),
            SqlVal::Text("[PITS] 初始化 Project Hub 并验证跨 Session 交接与逐步验收流程".to_string()),
            SqlVal::Text("session_handoff".to_string()),
            SqlVal::Text(json!(["pits-platform"]).to_string()),
            SqlVal::Text("c:/Project/PITS".to_string()),
            SqlVal::Text("7f24bd5e-aadc-4cdf-85dd-852f0d02b0fd".to_string()),
            SqlVal::Text("[]".to_string()),
            SqlVal::Text(
                json!(["file:///c:/Project/PITS/Infra/docs/self-managed-project-hub-plan.md"])
                    .to_string(),
            ),
            SqlVal::Text(
                "已完成 Project Hub 初始化协议定义与跨工作区任务流转模型验收准备，等待人工审核确认。"
                    .to_string(),
            ),
            SqlVal::Text("2026-11-18".to_string()),
            SqlVal::Text(ts_init.to_string()),
            SqlVal::Text(ts_init.to_string()),
        ],
    )?;

    conn.execute(
        "INSERT OR IGNORE INTO hub_session_log_tasks (session_log_id, task_key) VALUES (?1, ?2);",
        &[
            SqlVal::Text(SEED_SESSION_AGY1_ID.to_string()),
            SqlVal::Text("AGY-1".to_string()),
        ],
    )?;

    // 3. AGY-2: 16 Resource Type dual-track summary, GoVPP counters & DRA occupancy rollout (d19c0e6)
    let agy2_ac = json!([
        {
            "id": "ac-1",
            "text": "全部 16 项 Resource Type 实现 Policy/Live 双轨 Summary 与 Drift 识别",
            "agentVerified": true,
            "humanVerified": false
        },
        {
            "id": "ac-2",
            "text": "pits-agent GoVPP 实时收发包与接口计数接入 Edge Router 实体遥测",
            "agentVerified": true,
            "humanVerified": false
        },
        {
            "id": "ac-3",
            "text": "Intel NPU 与 work82 H20 GPU DRA v1 ResourceSlice 占用率实时展示",
            "agentVerified": true,
            "humanVerified": false
        },
        {
            "id": "ac-4",
            "text": "完成 cluster-pits-lab 3-shard CI 验证与 pits-core Portal 发布上线 (commit d19c0e6)",
            "agentVerified": true,
            "humanVerified": false
        }
    ])
    .to_string();

    let agy2_body = r#"## 交付摘要 (`d19c0e6`)

本轮完成了 PITS Infra 全部 **16 项一等 Resource Type**（12 项基础设施 + 4 项外采治理）的双轨 Summary 升级，并打通 GoVPP 实时计数、DRA 占用率统计以及 Lab CI/CD 与 AI Storage 统一工作区：

### 关键变更
1. **`client/` (`pitsctl`)**：新增 `catalog/summary.rs` 双轨 Summary 引擎（Policy Declared vs. Live Observed），扩展 `pitsctl get --view auto|policy|live|drift` 与 `/api/pits/v1/resource-types` 契约。
2. **`server/` (`pits-agent`)**：接入 GoVPP 共享内存实时接口与丢包计数，强化 mTLS 鉴权遥测。
3. **`portal/` (`pits-portal`)**：渲染 16 项资源双轨状态胶囊、DRA 占用率进度条与 `ResourceTypeDirectoryPage` 实时刷新的漂移视图。
4. **`CICD/` & `test-pool/`**：统一 `cluster-pits-lab` CI/CD 控制面与 AI Storage Crossplane 编排，通过全量 Rust + Go `-race` + Headless Chromium E2E 门禁并发布至 `pits-core`。

### 待人工验收
请操作者在右侧面板核对 AC 清单与 `d19c0e6` 实网效果后，点击 **「一键验收并标记 Done」** 完成首条自托管 Project Hub 真实闭环。
"#;

    conn.execute(
        r#"INSERT OR IGNORE INTO hub_tasks (
            seq_id, task_key, project_id, title, status, priority, tags_json, workspace_path,
            dispatch_target, acceptance_summary, ac_checklist_json, owner, due_date, last_session_at,
            body_md, catalog_refs, created_at, updated_at, sync_pending
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, 1);"#,
        &[
            SqlVal::Int(2),
            SqlVal::Text("AGY-2".to_string()),
            SqlVal::Text("pits".to_string()),
            SqlVal::Text("16 项 Resource Type 双轨 Summary、GoVPP 计数与 DRA 占用率上线".to_string()),
            SqlVal::Text("ready_for_review".to_string()),
            SqlVal::Text("p1_high".to_string()),
            SqlVal::Text(json!(["pits-infra", "client", "portal", "server", "cicd"]).to_string()),
            SqlVal::Text("c:/Project/PITS/Infra".to_string()),
            SqlVal::Text("pits-infra".to_string()),
            SqlVal::Text(
                "16 项 Resource Type 双轨 Summary、GoVPP 实时计数与 Intel NPU / H20 GPU DRA 占用率上线 (d19c0e6)，待人工在 Portal 内完成首条一键验收。"
                    .to_string(),
            ),
            SqlVal::Text(agy2_ac),
            SqlVal::Text("pitsadmin".to_string()),
            SqlVal::Null,
            SqlVal::Text(ts_agy2.to_string()),
            SqlVal::Text(agy2_body.to_string()),
            SqlVal::Text(
                json!([
                    "resource:default/pits-core-router",
                    "resource:default/netboot-001-npu",
                    "resource:default/work82-vcluster"
                ])
                .to_string(),
            ),
            SqlVal::Text(ts_agy2.to_string()),
            SqlVal::Text(ts_agy2.to_string()),
        ],
    )?;

    let agy2_session_body = r#"### 交接证据与验证结果
- **Git Commit**: `d19c0e6` (`feat(infra): implement 16-type dual-track summary, GoVPP counters, DRA occupancy, and unify Lab CI/CD & AI storage`)
- **Conversation**: `conversation://e49edfd5-7b0b-4652-9b0f-1a0376fcdf3c`
- **Verification**:
  - `Invoke-PitsTestPool.ps1` 3-shard (`rust`, `go-linux`, `portal-e2e`) 全部通过
  - `pits-core` (`100.95.95.88:18090`) 与 `https://portal.lab.martensite.dev` 已发布上线
- **Acceptance State**: `ready_for_review` / `accepted = false`（保留供用户亲自完成首条一键验收）
"#;

    conn.execute(
        r#"INSERT OR IGNORE INTO hub_session_logs (
            id, project_id, summary, log_type, tags_json, workspace_path, conversation_id,
            commit_refs_json, evidence_refs_json, body_md, accepted, accepted_by, accepted_at,
            review_note, session_date, created_at, updated_at, sync_pending
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, NULL, NULL, '', ?11, ?12, ?13, 1);"#,
        &[
            SqlVal::Text(SEED_SESSION_AGY2_ID.to_string()),
            SqlVal::Text("pits".to_string()),
            SqlVal::Text(
                "[PITS] 16 项 Resource Type 双轨 Summary、GoVPP 计数与 DRA 占用率上线 (d19c0e6)"
                    .to_string(),
            ),
            SqlVal::Text("session_handoff".to_string()),
            SqlVal::Text(json!(["pits-infra", "client", "portal", "server", "cicd"]).to_string()),
            SqlVal::Text("c:/Project/PITS/Infra".to_string()),
            SqlVal::Text("e49edfd5-7b0b-4652-9b0f-1a0376fcdf3c".to_string()),
            SqlVal::Text(json!(["d19c0e6"]).to_string()),
            SqlVal::Text(
                json!([
                    "https://portal.lab.martensite.dev/resource-types",
                    "http://100.95.95.88:18090/api/pits/v1/resource-types"
                ])
                .to_string(),
            ),
            SqlVal::Text(agy2_session_body.to_string()),
            SqlVal::Text("2026-11-19".to_string()),
            SqlVal::Text(ts_agy2.to_string()),
            SqlVal::Text(ts_agy2.to_string()),
        ],
    )?;

    conn.execute(
        "INSERT OR IGNORE INTO hub_session_log_tasks (session_log_id, task_key) VALUES (?1, ?2);",
        &[
            SqlVal::Text(SEED_SESSION_AGY2_ID.to_string()),
            SqlVal::Text("AGY-2".to_string()),
        ],
    )?;

    // Record idempotency audit marker
    conn.execute(
        r#"INSERT OR IGNORE INTO hub_audit_events (
            id, entity_type, entity_id, action, channel, actor, detail_json, created_at
        ) VALUES (?1, 'project', 'pits', 'seed', 'seed', 'system', ?2, ?3);"#,
        &[
            SqlVal::Text(SEED_MARKER_ID.to_string()),
            SqlVal::Text(
                json!({
                    "seededProject": "pits",
                    "seededTasks": ["AGY-1", "AGY-2"],
                    "seededSessions": [SEED_SESSION_AGY1_ID, SEED_SESSION_AGY2_ID]
                })
                .to_string(),
            ),
            SqlVal::Text(ts_agy2.to_string()),
        ],
    )?;

    Ok(true)
}
