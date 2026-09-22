use std::{
    path::PathBuf,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    process::{Child, Command},
    time,
};

struct ProxyProcess {
    child: Child,
    root: PathBuf,
    address: String,
}

impl ProxyProcess {
    async fn launch(args: &[&str]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "hoshikage-supervisor-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let config = format!(
            r#"
[server]
port = {port}
default_cwd = {cwd}
[codex]
command = {command}
args = {args}
[security]
allowed_cwds = [{cwd}]
[providers.hoshikage]
codex_id = "hoshikage"
enabled = false
"#,
            cwd = serde_json::to_string(root.to_str().unwrap()).unwrap(),
            command = serde_json::to_string(env!("CARGO_BIN_EXE_fake_codex")).unwrap(),
            args = serde_json::to_string(args).unwrap()
        );
        let config_path = root.join("config.toml");
        std::fs::write(&config_path, config).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_codex-hoshikage-proxy"))
            .env("CODEX_HOSHIKAGE_PROXY_HOME", &root)
            .env("CODEX_HOSHIKAGE_PROXY_CONFIG", config_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self {
            child,
            root,
            address: format!("http://127.0.0.1:{port}"),
        }
    }
}

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn app_server_exit_causes_nonzero_proxy_exit() {
    let mut proxy = ProxyProcess::launch(&["--exit-after-initialize"]).await;
    let status = time::timeout(Duration::from_secs(5), proxy.child.wait())
        .await
        .expect("proxy must exit so the supervisor can restart it")
        .unwrap();
    assert!(!status.success());
}

#[cfg(unix)]
#[tokio::test]
async fn sigterm_stops_proxy_cleanly() {
    let mut proxy = ProxyProcess::launch(&[]).await;
    let client = reqwest::Client::new();
    time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(response) = client.get(format!("{}/readyz", proxy.address)).send().await
                && response.status().is_success()
            {
                break;
            }
            time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("proxy becomes ready");
    let status = Command::new("kill")
        .args(["-TERM", &proxy.child.id().unwrap().to_string()])
        .status()
        .await
        .unwrap();
    assert!(status.success());
    let status = time::timeout(Duration::from_secs(5), proxy.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        status.success(),
        "SIGTERM should gracefully stop without triggering Restart=on-failure"
    );
}
