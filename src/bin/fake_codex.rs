use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    time::Duration,
};

fn main() {
    if std::env::args().any(|a| a == "--version") {
        println!("codex-cli 0.153.4");
        return;
    }
    let read_gate = std::env::args().find_map(|a| {
        a.strip_prefix("--interaction-read-gate=")
            .map(str::to_owned)
    });
    #[cfg(target_os = "linux")]
    if read_gate.is_some() {
        // Test-only backpressure: force a long answer to block until we read.
        assert!(unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_SETPIPE_SZ, 4096) } >= 0);
    }
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
    let mut turn_status = "inProgress";
    let mut thread_id = "thread_fake_1".to_string();
    let mut turn_id = "turn_fake_1".to_string();
    let mut setup_trace: Vec<Value> = Vec::new();
    let mut next_thread = 0;
    let mut mcp_reloads = 0;
    let mut next_turn = 0;
    let mut mcp_call = 0;
    let mut rejected_interrupt = false;
    let mut turns = std::collections::HashMap::<String, (String, String)>::new();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        if request.get("method").is_none()
            && id.as_str().is_some_and(|v| v.starts_with("mcp_fake_"))
        {
            let q = format!("mcp_tool_call_approval_call_{mcp_call}");
            assert_eq!(
                request["result"],
                json!({"answers":{q:{"answers":["Allow"]}}})
            );
            write_json(&json!({"method":"test/serverReply","params":request}));
            if std::env::args().any(|arg| arg == "--exit-after-mcp-reply") {
                return;
            }
            write_json(
                &json!({"method":"serverRequest/resolved","params":{"threadId":thread_id,"requestId":id}}),
            );
            if mcp_call == 1 && std::env::args().any(|arg| arg == "--pause-after-first-mcp") {
                continue;
            }
            if mcp_call < 5 {
                mcp_call += 1;
                mcp_request(&thread_id, &turn_id, mcp_call);
            } else {
                turn_status = "completed";
                turns.insert(turn_id.clone(), (thread_id.clone(), turn_status.into()));
                write_json(
                    &json!({"method":"item/agentMessage/delta","params":{"threadId":thread_id,"turnId":turn_id,"delta":"five tools completed"}}),
                );
                write_json(
                    &json!({"method":"turn/completed","params":{"threadId":thread_id,"turnId":turn_id,"turn":{"id":turn_id,"status":"completed"}}}),
                );
            }
            continue;
        }
        if request.get("method").is_none() && id == "unsupported_fake" {
            write_json(&json!({"method":"test/serverReply","params":request}));
            if request.get("result").is_some() {
                write_json(
                    &json!({"method":"serverRequest/resolved","params":{"threadId":thread_id,"requestId":"unsupported_fake"}}),
                );
                turn_status = "completed";
                turns.insert(turn_id.clone(), (thread_id.clone(), turn_status.into()));
                write_json(
                    &json!({"method":"item/agentMessage/delta","params":{"threadId":thread_id,"turnId":turn_id,"delta":"interaction handled"}}),
                );
                write_json(
                    &json!({"method":"turn/completed","params":{"threadId":thread_id,"turnId":turn_id,"turn":{"id":turn_id,"status":"completed"}}}),
                );
            }
            continue;
        }
        if request.get("method").is_none() && id == "artifact_fake" {
            write_json(
                &json!({"method":"item/agentMessage/delta","params":{"threadId":thread_id,"turnId":turn_id,"delta":if request["result"]["success"]==true{"artifact published"}else{"artifact failed"}}}),
            );
            turn_status = "completed";
            turns.insert(turn_id.clone(), (thread_id.clone(), turn_status.into()));
            write_json(
                &json!({"method":"turn/completed","params":{"threadId":thread_id,"turnId":turn_id,"turn":{"id":turn_id,"status":"completed"}}}),
            );
            continue;
        }
        if approval_pending && request.get("method").is_none() && id == approval_id {
            approval_pending = false;
            turn_status = "completed";
            turns.insert(turn_id.clone(), (thread_id.clone(), turn_status.into()));
            write_json(
                &json!({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":thread_id,"turnId":turn_id,"delta":"approved response"}}),
            );
            write_json(
                &json!({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":thread_id,"turnId":turn_id,"turn":{"id":turn_id,"status":"completed"}}}),
            );
            continue;
        }
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if matches!(
            method,
            "thread/start" | "thread/resume" | "thread/unsubscribe" | "turn/start"
        ) {
            setup_trace.push(json!({"method":method,"params":request["params"]}));
        }
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
            "mcpServerStatus/list" => {
                if std::env::args().any(|a| a == "--v06-notion-catalog") {
                    let result: Value = serde_json::from_str(include_str!(
                        "../../tests/fixtures/mcp-v06-notion-catalog.json"
                    ))
                    .unwrap();
                    write_json(&json!({"id":id,"result":result}));
                    continue;
                }
                if std::env::args().any(|a| a == "--v06-evaluated-catalog") {
                    let result: Value = serde_json::from_str(include_str!(
                        "../../tests/fixtures/mcp-v06-catalog.json"
                    ))
                    .unwrap();
                    write_json(&json!({"id":id,"result":result}));
                    continue;
                }
                if let Some(size) = request.pointer("/params/testBytes").and_then(Value::as_u64) {
                    // Result first deliberately exercises envelope field-order independence.
                    write_json(&json!({"result": "x".repeat(size as usize), "id": id}));
                    continue;
                }
                json!({"id":id,"result":{"data":[{"name":"playwright","tools":{"browser_find":{"name":"browser_find","inputSchema":{"type":"object","properties":{"text":{"type":"string"},"regex":{"type":"string"}}}}}}],"nextCursor":null}})
            }
            "config/mcpServer/reload" => {
                mcp_reloads += 1;
                if std::env::args().any(|a| a == "--fail-first-mcp-reload") && mcp_reloads == 1 {
                    json!({"id":id,"error":{"code":-32603,"message":"reload failed"}})
                } else {
                    json!({"id":id,"result":{}})
                }
            }
            "test/reload-status" => {
                json!({"id":id,"result":{"reloads":mcp_reloads,"threads":next_thread,"turns":next_turn}})
            }
            "test/large" => json!({"id":id,"result":"x".repeat(8 * 1024 * 1024 + 1)}),
            "test/late" => {
                std::thread::sleep(Duration::from_millis(100));
                json!({"id":id,"result":"late"})
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
            "model/list" => {
                let second =
                    request.pointer("/params/cursor").and_then(Value::as_str) == Some("page_2");
                let model = if second {
                    "gpt-test-second"
                } else {
                    "gpt-test-first"
                };
                let next = if !second || std::env::args().any(|arg| arg == "--repeat-model-cursor")
                {
                    json!("page_2")
                } else {
                    Value::Null
                };
                json!({"id":id, "result":{"data":[{"id":model, "model":model, "modelProvider":"openai",
                    "supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}]}], "nextCursor":next}})
            }
            "test/setup-trace" => json!({"id":id,"result":setup_trace}),
            "config/read" if std::env::args().any(|a| a == "--v06-slow-config") => {
                std::thread::sleep(Duration::from_millis(400));
                json!({"id":id,"result":{"config":{}}})
            }
            "thread/start" if std::env::args().any(|a| a == "--v06-setup-error") => {
                json!({"id":id,"error":{"code":-32000,"message":"configuration outcome unavailable"}})
            }
            "thread/start" if std::env::args().any(|a| a == "--v06-setup-unknown") => {
                continue;
            }
            "config/read" => json!({"id":id,"result":{"config":{}}}),
            "configRequirements/read" => json!({"id":id,"result":{"requirements":null}}),
            "thread/unsubscribe" => json!({"id":id,"result":{"status":"unsubscribed"}}),
            "thread/resume" if std::env::args().any(|arg| arg == "--missing-resume-thread") => {
                json!({"id":id, "error":{"code":-32602,"message":"thread not found"}})
            }
            "thread/start" | "thread/resume" => {
                if method == "thread/start" {
                    next_thread += 1;
                    thread_id = format!("thread_fake_{next_thread}");
                } else {
                    thread_id = request["params"]["threadId"].as_str().unwrap().into();
                }
                json!({"jsonrpc":"2.0","id":id,"result":{"thread":{"id":thread_id},"approvalPolicy":"on-request","approvalsReviewer":"user"}})
            }
            "thread/read" => {
                if std::env::args().any(|arg| arg == "--read-unavailable") {
                    json!({"id":id,"error":{"code":-32602,"message":"thread unavailable"}})
                } else {
                    json!({"id":id,"result":{"thread":{"id":request["params"]["threadId"],"status": if std::env::args().any(|arg|arg == "--upstream-waiting") { json!({"type":"active","activeFlags":["waitingOnApproval"]}) } else { json!({"type":"idle"}) },"turns": turns.iter().filter(|(_, (thread, _))| Some(thread.as_str()) == request["params"]["threadId"].as_str()).map(|(id, (_, status))| json!({"id":id,"status":status,"itemsView":"full","items": if std::env::args().any(|arg|arg=="--generated-image") {json!([{"type":"imageGeneration","id":"image_fake","status":"completed","result":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jGZkAAAAASUVORK5CYII="}])} else {json!([])}})).collect::<Vec<_>>()}}})
                }
            }
            "turn/steer" => {
                if std::env::args().any(|arg| arg == "--steer-race")
                    || turn_status != "inProgress"
                    || request["params"]["expectedTurnId"] != turn_id
                {
                    json!({"id":id,"error":{"code":-32602,"message":"no matching active turn"}})
                } else {
                    write_json(&json!({"method":"test/steered","params":request["params"]}));
                    json!({"id":id,"result":{"turnId":turn_id}})
                }
            }
            "turn/interrupt" => {
                if !rejected_interrupt
                    && std::env::args().any(|arg| arg == "--interrupt-not-active-once")
                {
                    rejected_interrupt = true;
                    write_json(
                        &json!({"id":id,"error":{"code":-32600,"message":"no active turn to interrupt"}}),
                    );
                    continue;
                }
                let race = std::env::args().any(|arg| arg == "--interrupt-race");
                if !std::env::args().any(|arg| arg == "--defer-interrupt-completion") {
                    turn_status = if race { "completed" } else { "interrupted" };
                    turns.insert(turn_id.clone(), (thread_id.clone(), turn_status.into()));
                    write_json(
                        &json!({"method":"turn/completed","params":{"threadId":thread_id,"turnId":turn_id,"turn":{"id":turn_id,"status":turn_status}}}),
                    );
                }
                write_json(&json!({"method":"test/interrupted", "params":request["params"]}));
                if race {
                    json!({"id":id,"error":{"code":-32602,"message":"turn already completed"}})
                } else {
                    json!({"id":id,"result":{}})
                }
            }
            "turn/start" => {
                if request["params"]["model"] == "model-rejected" {
                    write_json(
                        &json!({"id":id,"error":{"code":-32602,"message":"model unavailable"}}),
                    );
                    continue;
                }
                next_turn += 1;
                turn_id = format!("turn_fake_{next_turn}");
                turn_status = "inProgress";
                turns.insert(turn_id.clone(), (thread_id.clone(), turn_status.into()));
                if std::env::args().any(|arg| arg == "--exit-before-start-reply") {
                    return;
                }
                if std::env::args().any(|arg| arg == "--approval-before-start-reply") {
                    approval_pending = true;
                    write_json(
                        &json!({"id":approval_id,"method":"item/commandExecution/requestApproval","params":{"threadId":thread_id,"command":"echo approval","availableDecisions":["accept","decline"]}}),
                    );
                    std::thread::sleep(Duration::from_millis(10));
                    write_json(&json!({"id":id,"result":{"turn":{"id":turn_id}}}));
                    continue;
                }
                let response = json!({"jsonrpc":"2.0","id":id,"result":{"turn":{"id":turn_id}}});
                write_json(&response);
                if std::env::args().any(|arg| arg == "--native-mcp-five") {
                    mcp_call = 1;
                    mcp_request(&thread_id, &turn_id, mcp_call);
                    continue;
                }
                if let Some(method) = std::env::args()
                    .find_map(|arg| arg.strip_prefix("--server-request=").map(str::to_owned))
                {
                    write_json(&json!({"id":"unsupported_fake","method":method,"params":{
                        "threadId":thread_id,"turnId":turn_id,"itemId":"question_1","isBlocking":true,"questions":[{"id":"color","header":"Color","question":"Which color?","isOther":read_gate.is_some(),"options":[{"label":"Blue","description":"Blue"}]}],"permissions":{"network":{"enabled":true}},"mode":"form","serverName":"test","message":"Choose","requestedSchema":{"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}
                    }}));
                    if let Some(gate) = &read_gate {
                        let deadline = std::time::Instant::now() + Duration::from_secs(8);
                        while !std::path::Path::new(gate).exists() {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "test read gate timed out"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                    continue;
                }
                if std::env::args().any(|arg| arg == "--artifact-tool") {
                    let cwd = request["params"]["cwd"].as_str().unwrap();
                    std::fs::write(
                        std::path::Path::new(cwd).join("report.txt"),
                        "immutable artifact",
                    )
                    .unwrap();
                    write_json(
                        &json!({"id":"artifact_fake","method":"item/tool/call","params":{"threadId":thread_id,"turnId":turn_id,"callId":format!("call_{turn_id}"),"tool":"hoshikage_publish_artifact","arguments":{"path":"report.txt","display_name":"report.txt"}}}),
                    );
                    continue;
                }
                if std::env::args().any(|arg| arg == "--exit-during-turn") {
                    return;
                }
                if std::env::args().any(|arg| {
                    arg == "--silent-turn" || (arg == "--silent-after-first" && next_turn > 1)
                }) {
                    continue;
                }
                if approval_mode
                    && !(std::env::args().any(|arg| arg == "--approval-after-first")
                        && next_turn == 1)
                {
                    approval_pending = true;
                    if file_approval {
                        write_json(&json!({"method":"item/started", "params":{
                            "threadId":thread_id, "turnId":turn_id,
                            "item":{"id":"file_1", "type":"fileChange", "changes":[
                                {"path": std::env::current_dir().unwrap().join("inside.txt"), "kind":{"type":"add"}},
                                {"path": if workspace_file_approval { std::env::current_dir().unwrap().join("second.txt") } else { "/var/outside.txt".into() }, "kind":{"type":"add"}}
                            ]}
                        }}));
                        write_json(
                            &json!({"id":approval_id,"method":"item/fileChange/requestApproval","params":{
                                "threadId":thread_id,"turnId":turn_id,"itemId":"file_1","grantRoot":null
                            }}),
                        );
                    } else {
                        write_json(
                            &json!({"jsonrpc":"2.0","id":approval_id,"method":"item/commandExecution/requestApproval","params":{"threadId":thread_id,"turnId":turn_id,"command":"echo approval","availableDecisions":["accept","decline"]}}),
                        );
                    }
                    continue;
                }
                let text = if std::env::args().any(|arg| arg == "--echo-turn") {
                    request["params"].to_string()
                } else {
                    "fake response".into()
                };
                write_json(
                    &json!({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":thread_id,"turnId":turn_id,"itemId":"item_fake_1","delta":text}}),
                );
                turn_status = "completed";
                turns.insert(turn_id.clone(), (thread_id.clone(), turn_status.into()));
                write_json(
                    &json!({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":thread_id,"turnId":turn_id,"turn":{"id":turn_id,"status":"completed"}}}),
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

fn mcp_request(thread: &str, turn: &str, n: usize) {
    let item = format!("call_{n}");
    let inline = std::env::args().any(|arg| arg == "--inline-browser");
    let (server, tool, args) = if inline {
        (
            "playwright",
            "browser_find",
            json!({"text":format!("query {n}")}),
        )
    } else {
        ("test", "read_test", json!({"query":n}))
    };
    write_json(
        &json!({"method":"item/started","params":{"threadId":thread,"turnId":turn,"item":{"id":item,"type":"mcpToolCall","server":server,"tool":tool,"arguments":args}}}),
    );
    write_json(
        &json!({"id":format!("mcp_fake_{n}"),"method":"item/tool/requestUserInput","params":{"threadId":thread,"turnId":turn,"itemId":item,"questions":[{"id":format!("mcp_tool_call_approval_{item}"),"header":"Tool","question":"Allow?","isOther":false,"isSecret":false,"options":[{"label":"Allow"},{"label":"Cancel"}]}]}}),
    );
}
