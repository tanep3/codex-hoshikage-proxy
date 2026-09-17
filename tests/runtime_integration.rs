use codex_hoshikage_proxy::{
    config::{RawConfig, ValidatedConfig},
    domain::RuntimeState,
    runtime::CodexRuntime,
};
use std::{env, path::PathBuf, time::Duration};

fn fake_config(args: &[&str]) -> ValidatedConfig {
    let mut raw = RawConfig::default();
    // Exercise legacy compatibility independently of the v2 service.
    raw.server.v2_enabled = false;
    raw.security.allowed_cwds = vec![
        env::current_dir()
            .expect("current directory")
            .to_string_lossy()
            .into_owned(),
    ];
    let mut config = ValidatedConfig::from_raw(raw).expect("valid fake config");
    config.codex_command = env!("CARGO_BIN_EXE_fake_codex").into();
    config.codex_args = args.iter().map(|arg| (*arg).into()).collect();
    config.codex_home = PathBuf::from(format!(
        "/tmp/codex-hoshikage-proxy-runtime-test-{}",
        std::process::id()
    ));
    config
}

#[tokio::test]
async fn graceful_shutdown_reaps_fake_codex() {
    let runtime = CodexRuntime::launch(&fake_config(&[]))
        .await
        .expect("fake runtime launches");
    assert_eq!(runtime.snapshot().await, RuntimeState::Ready);

    tokio::time::timeout(Duration::from_secs(2), runtime.shutdown())
        .await
        .expect("shutdown does not hang")
        .expect("shutdown succeeds");
    assert_eq!(runtime.snapshot().await, RuntimeState::Stopped);
}

#[tokio::test]
async fn exited_codex_fails_pending_request() {
    let runtime = CodexRuntime::launch(&fake_config(&["--exit-after-initialize"]))
        .await
        .expect("fake runtime initializes before exit");

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        runtime.request("thread/start", serde_json::json!({})),
    )
    .await
    .expect("pending request resolves after transport exit");
    assert!(result.is_err());
}

#[tokio::test]
async fn null_result_is_a_successful_rpc_response() {
    let runtime = CodexRuntime::launch(&fake_config(&[])).await.unwrap();
    let result = runtime.request("test/null", serde_json::json!({})).await;
    runtime.shutdown().await.unwrap();
    assert_eq!(result.unwrap(), serde_json::Value::Null);
}

#[tokio::test]
async fn server_request_ids_do_not_consume_client_responses() {
    let runtime = CodexRuntime::launch(&fake_config(&[])).await.unwrap();
    let mut notifications = runtime.subscribe();
    for params in [
        serde_json::json!({}),
        serde_json::json!({"serverId": "approval-1"}),
    ] {
        let result = runtime.request("test/server-request", params.clone()).await;
        let event = tokio::time::timeout(Duration::from_secs(1), notifications.recv()).await;
        assert_eq!(result.unwrap(), serde_json::json!({"ok": true}));
        let event = event.unwrap().unwrap();
        assert_eq!(event["kind"], "server_request");
        if let Some(id) = params.get("serverId") {
            assert_eq!(&event["rpc_id"], id);
        } else {
            assert_eq!(event["rpc_id"], 2);
        }
    }
    let error = runtime
        .request("test/error", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("invalid params (-32602)"));
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn approval_listener_survives_notification_overflow() {
    use codex_hoshikage_proxy::approval_manager::ApprovalManager;
    let runtime = CodexRuntime::launch(&fake_config(&[])).await.unwrap();
    let manager = ApprovalManager::new(runtime.clone(), Duration::from_secs(30), false);
    manager.start();
    let mut events = runtime.subscribe();
    // Do not yield: overflow the subscriber before it can begin receiving.
    for _ in 0..300 {
        runtime.publish(serde_json::json!({"method": "noise"}));
    }
    runtime.publish(
        serde_json::json!({"kind":"server_request", "rpc_id":"late-approval",
        "method":"item/commandExecution/requestApproval", "params":{"threadId":"thread_1"}}),
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(event) = events.recv().await
                && let Some(id) = event.get("approval_id").and_then(serde_json::Value::as_str)
            {
                let view = manager.get(id).await.unwrap();
                assert_eq!(view.state, "cancelled");
                break;
            }
        }
    })
    .await
    .expect("approval listener remains active after lag and was subscribed at start");
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn upstream_resolved_request_cannot_be_approved_from_stale_ui() {
    use codex_hoshikage_proxy::approval_manager::{ApprovalCapability, ApprovalManager};
    use serde_json::json;
    let runtime = CodexRuntime::launch(&fake_config(&[])).await.unwrap();
    let manager = ApprovalManager::new(runtime.clone(), Duration::from_secs(30), false);
    manager.start();
    manager
        .register_turn(
            "t",
            ApprovalCapability::Interactive,
            std::path::Path::new("/tmp"),
            true,
        )
        .await;
    let mut events = runtime.subscribe();
    runtime.publish(json!({"kind":"server_request","rpc_id":"rpc-1","method":"item/commandExecution/requestApproval","params":{"threadId":"t","turnId":"turn"}}));
    let id = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let e = events.recv().await.unwrap();
            if e["kind"] == "approval_requested" {
                break e["approval_id"].as_str().unwrap().to_owned();
            }
        }
    })
    .await
    .unwrap();
    runtime.publish(
        json!({"method":"serverRequest/resolved","params":{"threadId":"t","requestId":"rpc-1"}}),
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while manager.get(&id).await.unwrap().state == "pending" {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(manager.decide(&id, "accept").await.is_err());
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn catalog_limits_are_rpc_specific_and_transport_survives_rejection() {
    let runtime = CodexRuntime::launch(&fake_config(&[])).await.unwrap();
    for bytes in [1_100_000, 8 * 1024 * 1024 - 2, 8 * 1024 * 1024 - 1] {
        let result = runtime
            .request(
                "mcpServerStatus/list",
                serde_json::json!({"testBytes":bytes}),
            )
            .await;
        if bytes + 2 <= 8 * 1024 * 1024 {
            assert_eq!(result.unwrap().as_str().unwrap().len(), bytes as usize);
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("catalog_too_large")
            );
        }
    }
    assert!(
        runtime
            .request("test/large", serde_json::json!({}))
            .await
            .unwrap()
            .as_str()
            .unwrap()
            .len()
            > 8 * 1024 * 1024
    );
    assert_eq!(
        runtime
            .request("test/null", serde_json::json!({}))
            .await
            .unwrap(),
        serde_json::Value::Null
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn late_response_after_cancel_cannot_satisfy_next_rpc() {
    let runtime = CodexRuntime::launch(&fake_config(&[])).await.unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            runtime.request("test/late", serde_json::json!({}))
        )
        .await
        .is_err()
    );
    assert_eq!(
        runtime
            .request("test/null", serde_json::json!({}))
            .await
            .unwrap(),
        serde_json::Value::Null
    );
    runtime.shutdown().await.unwrap();
}
