use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::process::{Child, Command};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Error as WebSocketError, Message, client::IntoClientRequest},
};

struct Proxy {
    child: Child,
    root: PathBuf,
    http: String,
    websocket: String,
}

impl Proxy {
    async fn launch() -> Self {
        let root = std::env::temp_dir().join(format!(
            "codex-native-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let config = format!(
            r#"[server]
port = {port}
default_cwd = {cwd}
codex_native_max_connections = 2

[codex]
command = {command}
args = []

[security]
api_key = "native-test-key"
allowed_cwds = [{cwd}]

[defaults]
model = "hoshikage/model"

[providers.hoshikage]
codex_id = "hoshikage"
enabled = true
max_concurrent_turns = 1

[providers.chatgpt]
codex_id = "openai"
enabled = false
"#,
            cwd = serde_json::to_string(root.to_str().unwrap()).unwrap(),
            command = serde_json::to_string(env!("CARGO_BIN_EXE_fake_codex")).unwrap(),
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
        let proxy = Self {
            child,
            root,
            http: format!("http://127.0.0.1:{port}"),
            websocket: format!("ws://127.0.0.1:{port}/codex"),
        };
        let client = reqwest::Client::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if client
                    .get(format!("{}/readyz", proxy.http))
                    .bearer_auth("native-test-key")
                    .send()
                    .await
                    .is_ok_and(|response| response.status().is_success())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("proxy becomes ready");
        proxy
    }

    async fn connect(
        &self,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let mut request = self.websocket.clone().into_client_request().unwrap();
        request
            .headers_mut()
            .insert("authorization", "Bearer native-test-key".parse().unwrap());
        connect_async(request).await.unwrap().0
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn rpc(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    request: Value,
) -> Value {
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .unwrap();
    loop {
        let message = socket.next().await.unwrap().unwrap();
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

#[tokio::test]
async fn native_bridge_preserves_json_and_isolates_connections() {
    let proxy = Proxy::launch().await;
    let mut first = proxy.connect().await;
    let mut second = proxy.connect().await;
    let mut third_request = proxy.websocket.clone().into_client_request().unwrap();
    third_request
        .headers_mut()
        .insert("authorization", "Bearer native-test-key".parse().unwrap());
    match connect_async(third_request).await.unwrap_err() {
        WebSocketError::Http(response) => assert_eq!(response.status(), 503),
        other => panic!("unexpected connection-limit error: {other}"),
    }

    for socket in [&mut first, &mut second] {
        let initialized = rpc(
            socket,
            json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{"clientInfo":{"name":"test"},"capabilities":{"future":true}}}),
        )
        .await;
        assert_eq!(initialized["id"], "init");
        socket
            .send(Message::Text(
                json!({"jsonrpc":"2.0","method":"initialized","params":{}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    }

    let request = json!({"jsonrpc":"2.0","id":"same","method":"test/echo","params":{"future":{"nested":true}},"futureTop":[1,2,3]});
    let one = rpc(&mut first, request.clone()).await;
    let two = rpc(&mut second, request.clone()).await;
    assert_eq!(one["result"], request);
    assert_eq!(two["result"], request);

    let null = rpc(
        &mut first,
        json!({"jsonrpc":"2.0","id":7,"method":"test/null","params":{}}),
    )
    .await;
    assert_eq!(null, json!({"id":7,"result":null}));

    let models = rpc(
        &mut first,
        json!({"jsonrpc":"2.0","id":"models","method":"model/list","params":{}}),
    )
    .await;
    assert_eq!(models["id"], "models");
    assert_eq!(models["result"]["data"][0]["model"], "gpt-test-first");
    let thread = rpc(
        &mut first,
        json!({"jsonrpc":"2.0","id":"thread","method":"thread/start","params":{}}),
    )
    .await;
    let thread_id = thread["result"]["thread"]["id"].clone();
    let turn = rpc(
        &mut first,
        json!({"jsonrpc":"2.0","id":"turn","method":"turn/start","params":{"threadId":thread_id,"model":"gpt-test-first","input":[]}}),
    )
    .await;
    assert_eq!(turn["id"], "turn");

    first
        .send(Message::Text(
            json!({"jsonrpc":"2.0","id":"outer","method":"test/server-request","params":{"serverId":"unsupported_fake"}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let mut saw_request = false;
    let mut saw_outer_result = false;
    let mut saw_reply = false;
    while !(saw_request && saw_outer_result && saw_reply) {
        let Message::Text(text) = first.next().await.unwrap().unwrap() else {
            continue;
        };
        let message: Value = serde_json::from_str(&text).unwrap();
        if message["id"] == "unsupported_fake" && message.get("method").is_some() {
            saw_request = true;
            first
                .send(Message::Text(
                    json!({"jsonrpc":"2.0","id":"unsupported_fake","result":{"decision":"accept"}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        } else if message["id"] == "outer" {
            saw_outer_result = true;
        } else if message["method"] == "test/serverReply" {
            saw_reply = true;
            assert_eq!(message["params"]["result"]["decision"], "accept");
        }
    }
}

#[tokio::test]
async fn native_bridge_requires_proxy_api_key() {
    let proxy = Proxy::launch().await;
    let error = connect_async(&proxy.websocket).await.unwrap_err();
    match error {
        WebSocketError::Http(response) => assert_eq!(response.status(), 401),
        other => panic!("unexpected WebSocket error: {other}"),
    }
}
