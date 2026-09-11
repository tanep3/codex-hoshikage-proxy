use codex_hoshikage_proxy::{
    catalog::ModelCatalogManager,
    config::{ValidatedConfig, default_config_path},
    http::{AppState, router},
    journal::EventJournal,
    runtime::CodexRuntime,
    store::ResponseStore,
};
use std::{future::IntoFuture, time::Duration};
use tracing_subscriber::{EnvFilter, filter::LevelFilter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(LevelFilter::INFO.into()))
        .init();
    let config_path = default_config_path();
    let config = ValidatedConfig::load(&config_path)?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("admin") {
        if args.get(1).map(String::as_str) == Some("backup")
            && args.get(2).map(String::as_str) == Some("restore")
        {
            let index = args
                .iter()
                .position(|a| a == "--from")
                .ok_or("--from required")?;
            let bundle = args.get(index + 1).ok_or("--from path required")?;
            let result = codex_hoshikage_proxy::v2::backup::restore(
                &config.codex_home.parent().unwrap().join("state/v2"),
                std::path::Path::new(bundle),
            )?;
            println!("{result}");
            return Ok(());
        }
        codex_hoshikage_proxy::v2::admin::client(
            &config.codex_home.parent().unwrap().join("state/v2"),
            &args[1..],
        )
        .await?;
        return Ok(());
    }
    if !config.v2_enabled
        && config
            .codex_home
            .parent()
            .unwrap()
            .join("state/v2-restore-pending.json")
            .exists()
    {
        return Err("pending v2 restore cannot be bypassed by disabling v2".into());
    }
    if config.v2_enabled {
        let state_root = config.codex_home.parent().unwrap().join("state");
        let marker = state_root.join("v2-restore-pending.json");
        if marker.exists() {
            let m: serde_json::Value = serde_json::from_slice(&std::fs::read(marker)?)?;
            let db = rusqlite::Connection::open_with_flags(
                state_root.join("v2/metadata.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let completed: Option<String> = rusqlite::OptionalExtension::optional(db.query_row(
                "SELECT value FROM metadata WHERE key='restore_complete'",
                [],
                |r| r.get(0),
            ))?;
            if completed.as_deref() != m["restore_id"].as_str() {
                return Err(
                    "incomplete restore; repeat the same admin backup restore command".into(),
                );
            }
        }
    }
    let v2_root = config.codex_home.parent().unwrap().join("state/v2");
    if !config.v2_enabled && v2_root.join("metadata.sqlite3").exists() {
        return Err("v2 state exists; enable v2 or perform an explicit state retirement before starting without its coordination".into());
    }
    // Acquire the durable owner lock before opening legacy writers or launching Codex.
    let v2_service = if config.v2_enabled {
        Some(std::sync::Arc::new(
            codex_hoshikage_proxy::v2::service::Service::open_with_limits(
                &v2_root,
                &config.default_cwd.join(".managed-workspaces"),
                config.v2_limits.clone(),
            )?,
        ))
    } else {
        None
    };
    config.prepare_codex_home()?;
    let journal = std::sync::Arc::new(
        EventJournal::open(
            config
                .codex_home
                .parent()
                .unwrap_or(config.codex_home.as_path()),
        )
        .await?,
    );
    let responses = std::sync::Arc::new(
        ResponseStore::open(
            config
                .codex_home
                .parent()
                .unwrap_or(config.codex_home.as_path()),
        )
        .await?,
    );
    let listener = tokio::net::TcpListener::bind(config.listen_addr).await?;
    let runtime = CodexRuntime::launch(&config).await?;
    let catalog = ModelCatalogManager::new(config.models.clone(), runtime.clone())?;
    tracing::info!(address = %config.listen_addr, "Codex Hoshikage Proxy listening");
    let (shutdown_sender, shutdown_receiver) = tokio::sync::oneshot::channel::<()>();
    let mut state = AppState::new(
        runtime.clone(),
        catalog,
        config.cwd_policy.clone(),
        config.default_cwd.clone(),
        config.api_key.clone(),
        std::time::Duration::from_secs(config.turn_idle_timeout_seconds),
        std::time::Duration::from_secs(config.turn_stall_detection_seconds),
        config.turn_stall_confirmation_count,
        std::time::Duration::from_secs(config.turn_heartbeat_seconds),
        config.sandbox_mode.clone(),
        std::time::Duration::from_secs(config.approval_timeout_seconds),
        config.auto_approve_workspace,
        journal,
        responses,
    );
    if let Some(service) = v2_service {
        state.v2 = Some(service.clone());
        let _maintenance =
            codex_hoshikage_proxy::v2::events::start_maintenance(state.clone(), service.clone());
        let _admin = codex_hoshikage_proxy::v2::admin::serve(service.clone())?;
        for record in service.store.list("response")? {
            if record["phase"] == "accepted"
                && let Some(id) = record["response_id"].as_str()
            {
                tokio::spawn(codex_hoshikage_proxy::v2::engine::run(
                    state.clone(),
                    service.clone(),
                    id.to_owned(),
                ));
            }
        }
    }
    let server = axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = shutdown_receiver.await;
        })
        .into_future();
    tokio::pin!(server);
    let failed = tokio::select! {
        result = &mut server => {
            runtime.shutdown().await?;
            result?;
            return Ok(());
        }
        _ = shutdown_signal() => false,
        _ = runtime.wait_for_failure() => {
            tracing::error!("Codex App Server failed; stopping proxy for supervisor restart");
            true
        }
    };
    let _ = shutdown_sender.send(());
    runtime.shutdown().await?;
    // Allow active requests to receive the transport error, but do not let
    // an unresponsive client prevent supervisor recovery indefinitely.
    match tokio::time::timeout(Duration::from_secs(5), &mut server).await {
        Ok(result) => result?,
        Err(_) => tracing::warn!("HTTP shutdown drain timed out"),
    }
    if failed {
        return Err(std::io::Error::other("Codex App Server failed").into());
    }
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
