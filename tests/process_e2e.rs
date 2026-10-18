//! Full HTTP integration through a separately launched router executable.
//! Set JEV_ROUTER_E2E_BIN to test the release binary; otherwise Cargo's test binary is used.
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use jev_router::config::Config;
use serde_json::{Value, json};
use std::{
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::process::{Child, Command};

#[derive(Default)]
struct Provider {
    slow: AtomicBool,
}
async fn auth(h: HeaderMap) -> impl IntoResponse {
    if ["Bearer tenant-a", "Bearer tenant-b"]
        .iter()
        .any(|k| h.get("authorization").is_some_and(|v| v == *k))
    {
        (
            StatusCode::OK,
            Json(json!({"data":{"is_management_key":false}})),
        )
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"private upstream detail"})),
        )
    }
}
async fn catalog() -> Json<Value> {
    Json(
        json!({"data":(["vendor/cheap","vendor/strong"].map(|id|json!({"id":id,"description":"General text, coding, tools and reasoning","context_length":128000,"top_provider":{"max_completion_tokens":8192},"architecture":{"input_modalities":["text"],"output_modalities":["text"]},"supported_parameters":["max_tokens","reasoning","tools","response_format"],"reasoning":{"supported_efforts":["none","low","high"]},"pricing":{"prompt":"0.000001","completion":"0.000002"}})))}),
    )
}
async fn judge(Json(v): Json<Value>) -> Json<Value> {
    let complex = v["state"]["request"]["recent_text"]
        .as_str()
        .unwrap_or("")
        .contains("complex");
    let c = v["questions"]["select"]["criteria"].as_object().unwrap();
    let wanted_model = if complex {
        "vendor/strong"
    } else {
        "vendor/cheap"
    };
    let wanted_effort = if complex { "high" } else { "none" };
    let chosen = c
        .iter()
        .find(|(_, v)| {
            if v.get("effort").is_some() {
                v["effort"] == wanted_effort
            } else {
                v["model"] == wanted_model
            }
        })
        .map(|(k, _)| k.clone())
        .unwrap_or_else(|| c.keys().next().unwrap().clone());
    let probabilities = c
        .keys()
        .map(|k| {
            (
                k.clone(),
                json!(if k == &chosen {
                    if c.len() == 1 { 1.0 } else { 0.98 }
                } else {
                    0.02 / (c.len() - 1) as f64
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    Json(
        json!({"model":"typesafe/jev-1.13-20260917","answers":{"select":{"type":"choice","choice":chosen,"confidence":0.98,"probabilities":probabilities},"advisor":{"type":"noul","noul":0.0}},"usage":{"input_tokens":200,"cost":0.00001}}),
    )
}
async fn generate(State(p): State<Arc<Provider>>, Json(v): Json<Value>) -> Response {
    answer(p, v, "chat")
}
async fn responses(State(p): State<Arc<Provider>>, Json(v): Json<Value>) -> Response {
    answer(p, v, "responses")
}
async fn messages(State(p): State<Arc<Provider>>, Json(v): Json<Value>) -> Response {
    answer(p, v, "messages")
}
fn answer(p: Arc<Provider>, v: Value, protocol: &str) -> Response {
    if v["stream"] == true {
        let frames = match protocol {
            "responses" => vec![
                json!({"type":"response.created","response":{"model":v["model"]}}),
                json!({"type":"response.output_text.delta","delta":"PONG"}),
                json!({"type":"response.completed","response":{"model":v["model"],"usage":{"cost":0.0001}}}),
            ],
            "messages" => vec![
                json!({"type":"message_start","message":{"model":v["model"]}}),
                json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"PONG"}}),
                json!({"type":"message_delta","usage":{"cost":0.0001}}),
                json!({"type":"message_stop"}),
            ],
            _ => vec![
                json!({"model":v["model"],"choices":[{"delta":{"content":"PONG"}}]}),
                json!({"usage":{"cost":0.0001}}),
            ],
        };
        let wire = frames
            .iter()
            .map(|v| format!("data: {v}\n\n"))
            .collect::<String>()
            + if protocol == "chat" {
                "data: [DONE]\n\n"
            } else {
                ""
            };
        let slow = p.slow.load(Ordering::Relaxed);
        let stream = async_stream::stream! {for chunk in wire.as_bytes().chunks(17){yield Ok::<_,std::io::Error>(bytes::Bytes::copy_from_slice(chunk));if slow{tokio::time::sleep(std::time::Duration::from_secs(2)).await;}}};
        let mut r = Response::new(Body::from_stream(stream));
        r.headers_mut()
            .insert("content-type", "text/event-stream".parse().unwrap());
        return r;
    }
    let mut result = match protocol {
        "responses" => {
            json!({"id":"response-fixture","status":"completed","model":v["model"],"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"PONG"}]}]})
        }
        "messages" => {
            json!({"id":"message-fixture","type":"message","role":"assistant","model":v["model"],"stop_reason":"end_turn","content":[{"type":"text","text":"PONG"}]})
        }
        _ => {
            json!({"id":"chat-fixture","model":v["model"],"choices":[{"message":{"role":"assistant","content":"PONG"},"finish_reason":"stop"}]})
        }
    };
    result["usage"] = json!({"cost":0.0001});
    Json(result).into_response()
}
async fn launch(config: &Path, dir: &Path) -> Child {
    let exe = std::env::var_os("JEV_ROUTER_E2E_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_jev-router").into());
    Command::new(exe)
        .arg("--config")
        .arg(config)
        .arg("serve")
        .current_dir(dir)
        .env_remove("BIND")
        .env("OPENROUTER_API_KEY", "operator-must-not-leak")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}
async fn ready(client: &reqwest::Client, base: &str, child: &mut Child) {
    for _ in 0..100 {
        assert!(
            child.try_wait().unwrap().is_none(),
            "Router exited during startup"
        );
        if client
            .get(format!("{base}/readyz"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("Router startup deadline exceeded");
}
async fn request(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    payload: Value,
    key: &str,
) -> reqwest::Response {
    client
        .post(format!("{base}{path}"))
        .bearer_auth(key)
        .header("X-OpenRouter-Metadata", "enabled")
        .json(&payload)
        .send()
        .await
        .unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn executable_public_byok_protocol_routing_streaming_and_crash_recovery() {
    let provider = Arc::new(Provider::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let fixture = Router::new()
        .route("/api/v1/key", get(auth))
        .route("/api/v1/models", get(catalog))
        .route("/api/alpha/decisions", post(judge))
        .route("/api/v1/chat/completions", post(generate))
        .route("/api/v1/responses", post(responses))
        .route("/api/v1/messages", post(messages))
        .with_state(provider.clone());
    let fixture_task = tokio::spawn(async move {
        axum::serve(listener, fixture).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let port_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = port_listener.local_addr().unwrap().port();
    drop(port_listener);
    let cfg = Config {
        bind: format!("0.0.0.0:{port}").parse().unwrap(),
        upstream,
        database: dir.path().join("router.db"),
        catalog: dir.path().join("catalog.json"),
        fallback_model: "vendor/strong".into(),
        repair_model: "vendor/strong".into(),
        ..Config::default()
    };
    let config = dir.path().join("config.json");
    std::fs::write(&config, serde_json::to_vec(&cfg).unwrap()).unwrap();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let mut child = launch(&config, dir.path()).await;
    ready(&client, &base, &mut child).await;
    let mut duplicate = launch(&config, dir.path()).await;
    assert!(
        !tokio::time::timeout(std::time::Duration::from_secs(5), duplicate.wait())
            .await
            .unwrap()
            .unwrap()
            .success(),
        "A second process must not own the same database"
    );
    std::fs::write(
        dir.path().join("LIVE-RESULTS.json"),
        r#"{"status":"passed","cases":[]}"#,
    )
    .unwrap();
    let exe = std::env::var_os("JEV_ROUTER_E2E_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_jev-router").into());
    let failed = Command::new(exe)
        .arg("smoke")
        .arg("--base-url")
        .arg(&base)
        .current_dir(dir.path())
        .env("OPENROUTER_API_KEY", "bad-key")
        .env_remove("BIND")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .unwrap();
    assert!(!failed.success());
    let report: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("LIVE-RESULTS.json")).unwrap())
            .unwrap();
    assert_eq!(
        report["status"], "failed",
        "Failed checks must replace stale passing reports"
    );
    assert!(!report.to_string().contains("bad-key"));
    let plain = json!({"model":"auto","messages":[{"role":"user","content":"Reply PONG"}],"max_tokens":512});
    assert_eq!(
        client
            .post(format!("{base}/v1/chat/completions"))
            .json(&plain)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        request(
            &client,
            &base,
            "/v1/chat/completions",
            plain.clone(),
            "bad-key"
        )
        .await
        .status(),
        401
    );
    let preflight = client
        .request(
            reqwest::Method::OPTIONS,
            format!("{base}/v1/chat/completions"),
        )
        .header("origin", "https://client.example")
        .header("access-control-request-method", "POST")
        .header(
            "access-control-request-headers",
            "authorization,content-type",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(preflight.headers()["access-control-allow-origin"], "*");
    let mut saved_id = String::new();
    for (task, expected, effort) in [
        ("Reply PONG", "vendor/cheap", "none"),
        ("Solve a complex reasoning task", "vendor/strong", "high"),
    ] {
        let r = request(
            &client,
            &base,
            "/v1/chat/completions",
            json!({"model":"auto","messages":[{"role":"user","content":task}],"max_tokens":512}),
            "tenant-a",
        )
        .await;
        assert_eq!(r.status(), 200);
        saved_id = r.headers()["x-request-id"].to_str().unwrap().into();
        assert_eq!(r.headers()["x-jev-model"], expected);
        assert_eq!(r.headers()["x-jev-effort"], effort);
        assert_eq!(r.json::<Value>().await.unwrap()["model"], expected);
    }
    for (path, input) in [
        (
            "/v1/responses",
            json!({"input":"Reply PONG","max_output_tokens":512}),
        ),
        (
            "/v1/messages",
            json!({"messages":[{"role":"user","content":"Reply PONG"}],"max_tokens":512}),
        ),
    ] {
        let r = request(&client, &base, path, input, "tenant-a").await;
        assert_eq!(r.status(), 200);
        assert!(
            r.json::<Value>()
                .await
                .unwrap()
                .to_string()
                .contains("PONG")
        );
    }
    for (path, mut payload, completion) in [
        ("/v1/chat/completions", plain.clone(), "[DONE]"),
        (
            "/v1/responses",
            json!({"input":"Reply PONG","max_output_tokens":512}),
            "response.completed",
        ),
        (
            "/v1/messages",
            json!({"messages":[{"role":"user","content":"Reply PONG"}],"max_tokens":512}),
            "message_stop",
        ),
    ] {
        payload["stream"] = json!(true);
        let r = request(&client, &base, path, payload, "tenant-a").await;
        assert_eq!(r.status(), 200);
        let wire = r.text().await.unwrap();
        assert!(wire.contains("PONG") && wire.contains(completion));
    }
    assert_eq!(
        client
            .get(format!("{base}/v1/requests/{saved_id}"))
            .bearer_auth("tenant-b")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let persisted = client
        .get(format!("{base}/v1/requests/{saved_id}"))
        .bearer_auth("tenant-a")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(persisted["status"], "completed");
    assert!(!persisted.to_string().contains("Reply PONG"));
    provider.slow.store(true, Ordering::Relaxed);
    let mut payload = plain.clone();
    payload["stream"] = json!(true);
    let interrupted = request(&client, &base, "/v1/chat/completions", payload, "tenant-a").await;
    assert_eq!(interrupted.status(), 200);
    let pending_id = interrupted.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    child.start_kill().unwrap();
    child.wait().await.unwrap();
    drop(interrupted);
    provider.slow.store(false, Ordering::Relaxed);
    child = launch(&config, dir.path()).await;
    ready(&client, &base, &mut child).await;
    let recovery = client
        .get(format!("{base}/v1/requests/{pending_id}"))
        .bearer_auth("tenant-a")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(recovery["status"], "interrupted_estimate");
    let usage_url = format!("{base}/v1/usage");
    let before = client
        .get(&usage_url)
        .bearer_auth("tenant-a")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(before["active"], 0);
    assert_eq!(before["reserved_usd"], 0.0);
    child.start_kill().unwrap();
    child.wait().await.unwrap();
    child = launch(&config, dir.path()).await;
    ready(&client, &base, &mut child).await;
    let after = client
        .get(&usage_url)
        .bearer_auth("tenant-a")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(before, after, "Crash recovery must not charge twice");
    assert_eq!(
        client
            .get(format!("{base}/v1/requests/{saved_id}"))
            .bearer_auth("tenant-a")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    #[cfg(unix)]
    {
        assert!(
            Command::new("/bin/kill")
                .arg("-TERM")
                .arg(child.id().unwrap().to_string())
                .status()
                .await
                .unwrap()
                .success()
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
    #[cfg(not(unix))]
    {
        child.start_kill().unwrap();
        child.wait().await.unwrap();
    }
    fixture_task.abort();
}
