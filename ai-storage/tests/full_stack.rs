use ai_store::core::policy::{check_policy, normalize_uri, OperationKind};
use ai_store::mcp::{build_http_router, build_router, handle_mcp_request, AppState};
use ai_store::profile::{sha256_hex, ProjectProfile};
use bytes::Bytes;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::RwLock;

fn temp_dir(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("ai-store-test-{}-{}", name, uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[tokio::test]
async fn test_uri_normalization_and_policy_guard() {
    let dir = temp_dir("policy");
    let profile = ProjectProfile::standalone_default(&dir);

    assert!(normalize_uri("ws://../../etc/passwd").is_err());
    assert!(normalize_uri("ws://foo/bar\0baz").is_err());
    assert!(normalize_uri("unknown://foo").is_err());

    let kb = normalize_uri("kb://docs/arch.md").unwrap();
    assert_eq!(kb.canonical, "kb://docs/arch.md");
    assert!(check_policy(&profile, &kb, OperationKind::Create, Some(10), false).is_err());
    assert!(check_policy(&profile, &kb, OperationKind::Read, None, false).is_ok());

    let art = normalize_uri("artifacts://builds/out.bin").unwrap();
    assert!(check_policy(&profile, &art, OperationKind::Create, Some(10), false).is_ok());
    assert!(check_policy(&profile, &art, OperationKind::Overwrite, Some(10), false).is_err());
    assert!(check_policy(&profile, &art, OperationKind::Delete, None, false).is_err());

    let mem_unscoped = normalize_uri("mem://loose.md").unwrap();
    assert!(check_policy(&profile, &mem_unscoped, OperationKind::Create, Some(10), false).is_err());
    let mem_scoped = normalize_uri("mem://pits/session-1/note.md").unwrap();
    assert!(check_policy(&profile, &mem_scoped, OperationKind::Create, Some(10), false).is_ok());

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_all_eight_archetypes_end_to_end() {
    let dir = temp_dir("archetypes");
    let profile_path = dir.join(".ai-store.yaml");
    let profile = ProjectProfile::standalone_default(&dir);
    let router = build_router(&profile, &profile_path).await.unwrap();

    // 1. Workspace (ws://): write, snapshot, modify, diff, rollback, fork
    router
        .write("ws://proj/main.rs", Bytes::from_static(b"fn main() { println!(\"v1\"); }\n"), "tester", None)
        .await
        .unwrap();
    let ws = router.workspace.as_ref().unwrap();
    let snap1 = ws.snapshot("proj", "v1").await.unwrap();
    assert_eq!(snap1.file_count, 1);

    router
        .write("ws://proj/main.rs", Bytes::from_static(b"fn main() { println!(\"v2\"); }\n"), "tester", None)
        .await
        .unwrap();
    router
        .write("ws://proj/extra.txt", Bytes::from_static(b"extra"), "tester", None)
        .await
        .unwrap();
    let diff = ws.diff(&snap1.snapshot_id, None).await.unwrap();
    assert_eq!(diff.modified, vec!["proj/main.rs".to_string()]);
    assert_eq!(diff.added, vec!["proj/extra.txt".to_string()]);

    let restored = ws.rollback(&snap1.snapshot_id, "proj").await.unwrap();
    assert_eq!(restored, 1);
    let (after_rb, _) = router.read("ws://proj/main.rs", None, None, "tester").await.unwrap();
    assert!(String::from_utf8_lossy(&after_rb).contains("v1"));
    assert!(router.read("ws://proj/extra.txt", None, None, "tester").await.is_err());

    let forked = ws.fork(&snap1.snapshot_id, "forked-proj").await.unwrap();
    assert_eq!(forked, 1);
    let (fork_bytes, _) = router.read("ws://forked-proj/main.rs", None, None, "tester").await.unwrap();
    assert!(String::from_utf8_lossy(&fork_bytes).contains("v1"));

    // 2. Memory (mem://): commit, recall (keyword + vector), forget
    let mem = router.memory.as_ref().unwrap();
    let m1 = mem
        .commit(
            "pits/user-1",
            Some("opendal-note.md"),
            "OpenDAL Layering",
            "Use RetryLayer and TimeoutLayer on every OpenDAL Operator",
            vec!["opendal".to_string(), "architecture".to_string()],
            Some(2.0),
            Some(30),
            Some(vec![1.0, 0.0, 0.0]),
        )
        .await
        .unwrap();
    let hits = mem.recall(Some("pits"), "RetryLayer OpenDAL", Some(&[0.9, 0.1, 0.0]), 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].mem_id, m1.mem_id);
    mem.forget(&m1.mem_id).await.unwrap();
    assert!(mem.recall(Some("pits"), "RetryLayer", None, 5).unwrap().is_empty());

    // 3. Knowledge (kb://): ingest, glob, grep, hybrid search, read-only guard on raw vfs_write
    assert!(router.write("kb://bad.md", Bytes::from_static(b"nope"), "tester", None).await.is_err());
    let kb = router.knowledge.as_ref().unwrap();
    kb.ingest(
        "docs/crossplane.md",
        "Crossplane Pipeline",
        "Crossplane Pipeline aggregates ProjectProfile ConfigMap and Secret.",
        json!({"source": "docs"}),
        Some(vec![0.0, 1.0, 0.0]),
    )
    .await
    .unwrap();
    let globs = router.glob("kb://", "**/*.md", 10, "tester").await.unwrap();
    assert_eq!(globs, vec!["kb://docs/crossplane.md".to_string()]);
    let greps = router.grep("kb://", "ProjectProfile", 10, "tester").await.unwrap();
    assert_eq!(greps.len(), 1);
    let kb_hits = kb.search("ProjectProfile ConfigMap", Some(&[0.0, 1.0, 0.0]), 5).unwrap();
    assert_eq!(kb_hits.len(), 1);

    // 4. Datasets & Models (data://, models://): Byte-Range + NVMe read-through cache
    let op = &router.registry.get("local_fs").unwrap().operator;
    op.write("datasets/sample.bin", Bytes::from_static(b"0123456789ABCDEF")).await.unwrap();
    let (slice1, hit1) = router.read("data://sample.bin", Some(4), Some(6), "tester").await.unwrap();
    assert_eq!(&slice1[..], b"456789");
    assert!(!hit1);
    let (slice2, hit2) = router.read("data://sample.bin", Some(10), Some(4), "tester").await.unwrap();
    assert_eq!(&slice2[..], b"ABCD");
    assert!(hit2);
    let cstat = router.datasets.as_ref().unwrap().cache_status().unwrap();
    assert_eq!(cstat.hits, 1);
    assert_eq!(cstat.misses, 1);

    // 5. Artifacts (artifacts://): Content-addressed SHA256 + conflict rejection + lineage
    let art = router.artifacts.as_ref().unwrap();
    let rec = art
        .publish(
            "builds/v1/report.txt",
            Bytes::from_static(b"immutable-artifact-v1"),
            "run-101",
            "builder-agent",
            Some("ws://proj".to_string()),
            None,
            json!({"format": "txt"}),
        )
        .await
        .unwrap();
    assert_eq!(rec.sha256, sha256_hex(b"immutable-artifact-v1"));
    assert!(art
        .publish("builds/v1/report.txt", Bytes::from_static(b"mutated!"), "run-102", "builder-agent", None, None, json!({}))
        .await
        .is_err());
    let lin = art.lineage(Some("run-101")).unwrap();
    assert_eq!(lin.len(), 1);

    // 6. Traces (traces://): flush and query
    let tr = router.traces.as_ref().unwrap();
    let flushed = tr.flush().await.unwrap();
    assert!(flushed.is_some());
    let events = tr.query(Some("tester"), None, None, 20).await.unwrap();
    assert!(!events.is_empty());

    // 7. State (state://): CAS + Fenced Lease
    let st = router.state.as_ref().unwrap();
    let r1 = st.cas("swarm/checkpoint", &json!({"step": 1}), Some(0), None).await.unwrap();
    assert_eq!(r1.version, 1);
    assert!(st.cas("swarm/checkpoint", &json!({"step": 2}), Some(99), None).await.is_err());
    let r2 = st.cas("swarm/checkpoint", &json!({"step": 2}), Some(1), None).await.unwrap();
    assert_eq!(r2.version, 2);

    let l1 = st.acquire_lease("coord-lock", "agent-a", 60).await.unwrap();
    assert!(st.acquire_lease("coord-lock", "agent-b", 60).await.is_err());
    assert!(st.release_lease("coord-lock", "agent-a").await.unwrap());
    assert!(l1.fencing_token >= 1);

    // 8. Shared Drive (shared://): Lock + guarded write + unlock
    let sh = router.shared.as_ref().unwrap();
    sh.lock("handoff/bundle.json", "agent-a", 60).await.unwrap();
    assert!(router
        .write("shared://handoff/bundle.json", Bytes::from_static(b"{}"), "agent-b", None)
        .await
        .is_err());
    router
        .write("shared://handoff/bundle.json", Bytes::from_static(b"{\"ok\":true}"), "agent-a", None)
        .await
        .unwrap();
    sh.unlock("handoff/bundle.json", "agent-a").await.unwrap();

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_mcp_protocol_and_streamable_http() {
    let dir = temp_dir("mcp");
    let profile_path = dir.join(".ai-store.yaml");
    let profile = ProjectProfile::standalone_default(&dir);
    let router = build_router(&profile, &profile_path).await.unwrap();

    let state = Arc::new(AppState {
        router: Arc::new(RwLock::new(Arc::new(router))),
        profile_path,
        profile_digest: Arc::new(RwLock::new("test-digest".to_string())),
        sessions: Arc::new(RwLock::new(HashSet::new())),
        bearer_token: Some("secret-token".to_string()),
        request_counter: Arc::new(AtomicU64::new(0)),
        error_counter: Arc::new(AtomicU64::new(0)),
    });

    // Direct JSON-RPC checks
    let init_resp = handle_mcp_request(
        &state,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
    )
    .await
    .unwrap();
    assert_eq!(init_resp["result"]["serverInfo"]["name"], "pits-ai-store");

    let tools_resp = handle_mcp_request(&state, json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .await
        .unwrap();
    let tools = tools_resp["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "ws_snapshot"));
    assert!(tools.iter().any(|t| t["name"] == "shared_lock"));

    let res_read = handle_mcp_request(
        &state,
        json!({"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"mounts://tree"}}),
    )
    .await
    .unwrap();
    assert!(res_read["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("ws://"));

    let prompt_get = handle_mcp_request(
        &state,
        json!({"jsonrpc":"2.0","id":4,"method":"prompts/get","params":{"name":"storage-workflow-guide"}}),
    )
    .await
    .unwrap();
    assert!(prompt_get["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .contains("artifacts://"));

    // Streamable HTTP test on ephemeral port
    let app = build_http_router(state.clone(), "/mcp");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    use http_body_util::{BodyExt, Full};
    use hyper_util::rt::TokioIo;
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.unwrap();
    tokio::spawn(async move { let _ = conn.await; });

    // Unauthorized without Bearer
    let unauth_req = hyper::Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", format!("127.0.0.1:{}", port))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from_static(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}")))
        .unwrap();
    let unauth_resp = sender.send_request(unauth_req).await.unwrap();
    assert_eq!(unauth_resp.status(), 401);

    // Authorized with Bearer
    let auth_req = hyper::Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", format!("127.0.0.1:{}", port))
        .header("content-type", "application/json")
        .header("authorization", "Bearer secret-token")
        .body(Full::new(Bytes::from_static(b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/call\",\"params\":{\"name\":\"vfs_write\",\"arguments\":{\"uri\":\"ws://http/hello.txt\",\"content\":\"hello-http\"}}}")))
        .unwrap();
    let auth_resp = sender.send_request(auth_req).await.unwrap();
    assert_eq!(auth_resp.status(), 200);
    assert!(auth_resp.headers().get("mcp-session-id").is_some());
    let body_bytes = auth_resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(parsed["result"]["isError"], false);

    let _ = std::fs::remove_dir_all(&dir);
}
