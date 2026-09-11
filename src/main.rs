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
    let server = axum::serve(
        listener,
        router(AppState::new(
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
        )),
    )
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
