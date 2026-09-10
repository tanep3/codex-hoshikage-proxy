use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    time::Duration,
};

fn main() {
    let stdin = io::stdin();
    let approval_mode = std::env::var("FAKE_CODEX_APPROVAL").is_ok()
        || std::env::args().any(|arg| arg == "--approval");
    let exit_after_initialize = std::env::args().any(|arg| arg == "--exit-after-initialize");
    let approval_id = if std::env::args().any(|arg| arg == "--string-approval-id") {
        json!("approval_fake_1")
    } else {
        json!(99)
    };
    let workspace_file_approval = std::env::args().any(|arg| arg == "--workspace-file-approval");
    let file_approval =
        workspace_file_approval || std::env::args().any(|arg| arg == "--file-approval");
    let mut approval_pending = false;
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        if approval_pending && request.get("method").is_none() && id == approval_id {
            approval_pending = false;
            write_json(
                &json!({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"thread_fake_1","turnId":"turn_fake_1","delta":"approved response"}}),
            );
            write_json(
                &json!({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thread_fake_1","turnId":"turn_fake_1","turn":{"id":"turn_fake_1","status":"completed"}}}),
            );
            continue;
        }
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let response = match method {
            "initialize" => {
                let response = json!({"jsonrpc":"2.0","id":id,"result":{}});
                write_json(&response);
                if exit_after_initialize {
                    // Give the runtime a chance to send the required
                    // `initialized` notification before simulating a crash.
                    std::thread::sleep(Duration::from_millis(50));
                    return;
                }
                continue;
            }
            "test/null" => json!({"id": id, "result": null}),
            "test/server-request" => {
                let rpc_id = request
                    .pointer("/params/serverId")
                    .cloned()
                    .unwrap_or(id.clone());
                write_json(
                    &json!({"id": rpc_id, "method": "item/commandExecution/requestApproval", "params": {}}),
                );
                json!({"id": id, "result": {"ok": true}})
            }
            "test/error" => {
                json!({"id": id, "error": {"code": -32602, "message": "invalid params"}})
            }
            "thread/start" => {
                json!({"jsonrpc":"2.0","id":id,"result":{"thread":{"id":"thread_fake_1"}}})
            }
            "turn/interrupt" => {
                write_json(&json!({"method":"test/interrupted", "params":request["params"]}));
                json!({"id":id, "result":{}})
            }
            "turn/start" => {
                let response =
                    json!({"jsonrpc":"2.0","id":id,"result":{"turn":{"id":"turn_fake_1"}}});
                write_json(&response);
                if std::env::args().any(|arg| arg == "--exit-during-turn") {
                    return;
                }
                if std::env::args().any(|arg| arg == "--silent-turn") {
                    continue;
                }
                if approval_mode {
                    approval_pending = true;
                    if file_approval {
                        write_json(&json!({"method":"item/started", "params":{
                            "threadId":"thread_fake_1", "turnId":"turn_fake_1",
                            "item":{"id":"file_1", "type":"fileChange", "changes":[
                                {"path": std::env::current_dir().unwrap().join("inside.txt"), "kind":{"type":"add"}},
                                {"path": if workspace_file_approval { std::env::current_dir().unwrap().join("second.txt") } else { "/var/outside.txt".into() }, "kind":{"type":"add"}}
                            ]}
                        }}));
                        write_json(
                            &json!({"id":approval_id,"method":"item/fileChange/requestApproval","params":{
                                "threadId":"thread_fake_1","turnId":"turn_fake_1","itemId":"file_1","grantRoot":null
                            }}),
                        );
                    } else {
                        write_json(
                            &json!({"jsonrpc":"2.0","id":approval_id,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread_fake_1","turnId":"turn_fake_1","command":"echo approval","availableDecisions":["accept","decline"]}}),
                        );
                    }
                    continue;
                }
                write_json(
                    &json!({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"thread_fake_1","turnId":"turn_fake_1","itemId":"item_fake_1","delta":"fake response"}}),
                );
                write_json(
                    &json!({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thread_fake_1","turnId":"turn_fake_1","turn":{"id":"turn_fake_1","status":"completed"}}}),
                );
                continue;
            }
            _ => continue,
        };
        write_json(&response);
    }
}

fn write_json(value: &Value) {
    let mut stdout = io::stdout().lock();
    if let Err(error) = writeln!(stdout, "{value}").and_then(|_| stdout.flush()) {
        // HTTP tests may close the transport as soon as approval is required.
        if error.kind() == io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        panic!("failed to write fake Codex response: {error}");
    }
}
