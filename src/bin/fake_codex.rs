use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

fn main() {
    let stdin = io::stdin();
    let approval_mode = std::env::var("FAKE_CODEX_APPROVAL").is_ok()
        || std::env::args().any(|arg| arg == "--approval");
    let exit_after_initialize = std::env::args().any(|arg| arg == "--exit-after-initialize");
    let mut approval_pending = false;
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        if approval_pending && request.get("method").is_none() {
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
                    return;
                }
                continue;
            }
            "thread/start" => {
                json!({"jsonrpc":"2.0","id":id,"result":{"thread":{"id":"thread_fake_1"}}})
            }
            "turn/start" => {
                let response =
                    json!({"jsonrpc":"2.0","id":id,"result":{"turn":{"id":"turn_fake_1"}}});
                write_json(&response);
                if approval_mode {
                    approval_pending = true;
                    write_json(
                        &json!({"jsonrpc":"2.0","id":99,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread_fake_1","turnId":"turn_fake_1","command":"echo approval","availableDecisions":["accept","decline"]}}),
                    );
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
    println!("{}", value);
    io::stdout().flush().expect("stdout flush");
}
