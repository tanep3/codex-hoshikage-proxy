use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

fn main() {
    let stdin = io::stdin();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let response = match method {
            "initialize" => json!({"jsonrpc":"2.0","id":id,"result":{}}),
            "thread/start" => {
                json!({"jsonrpc":"2.0","id":id,"result":{"thread":{"id":"thread_fake_1"}}})
            }
            "turn/start" => {
                let response =
                    json!({"jsonrpc":"2.0","id":id,"result":{"turn":{"id":"turn_fake_1"}}});
                write_json(&response);
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
