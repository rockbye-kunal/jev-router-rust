use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use futures_util::StreamExt;
use jev_router::{
    config::Config,
    policy::{self, Protocol},
    registry::{Model, Registry},
    server::{AppState, app},
    store::Store,
    stream::Inspector,
    upstream::Credential,
};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
fn model(id: &str) -> Model {
    Model {
        id: id.into(),
        description: "A general purpose model for the current task".into(),
        context: 128000,
        output: 32768,
        input_price: 0.000001,
        output_price: 0.000002,
        request_price: 0.0,
        modalities: vec!["text".into(), "image".into(), "file".into()],
        parameters: vec![
            "max_tokens".into(),
            "tools".into(),
            "tool_choice".into(),
            "structured_outputs".into(),
            "response_format".into(),
            "reasoning".into(),
        ],
        efforts: vec!["low".into(), "medium".into(), "high".into()],
        control: "effort".into(),
        canonical: id.into(),
    }
}
fn registry() -> Registry {
    Registry {
        fetched_at: jev_router::config::now(),
        models: vec![
            model("vendor/cheap"),
            model("vendor/mid"),
            model("vendor/strong"),
        ],
        excluded: 0,
        aliases: Default::default(),
    }
}
#[derive(Default)]
struct Mock {
    behavior: AtomicUsize,
    generations: AtomicUsize,
    judgements: AtomicUsize,
    requests: Mutex<Vec<Value>>,
    decisions: Mutex<Vec<Value>>,
}
struct System {
    url: String,
    store: Arc<Store>,
    mock: Arc<Mock>,
    state: AppState,
    _dir: tempfile::TempDir,
    handles: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for System {
    fn drop(&mut self) {
        for h in &self.handles {
            h.abort();
        }
    }
}
async fn auth(h: HeaderMap) -> impl IntoResponse {
    if ["Bearer key-a", "Bearer key-b", "Bearer local-key"]
        .iter()
        .any(|k| h.get("authorization").is_some_and(|v| v == *k))
    {
        (
            StatusCode::OK,
            Json(json!({"data":{"is_management_key":false,"limit_remaining":20}})),
        )
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"raw private key must never be returned"})),
        )
    }
}
async fn judge(
    State(m): State<Arc<Mock>>,
    h: HeaderMap,
    Json(v): Json<Value>,
) -> impl IntoResponse {
    assert!(h.get("authorization").is_some());
    m.judgements.fetch_add(1, Ordering::Relaxed);
    m.decisions.lock().unwrap().push(v.clone());
    if m.behavior.load(Ordering::Relaxed) == 22 {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":"private rate limit details"})),
        );
    }
    if m.behavior.load(Ordering::Relaxed) == 3 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    if m.behavior.load(Ordering::Relaxed) == 4 {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"sensitive upstream content"})),
        );
    }
    let criteria = v["questions"]["select"]["criteria"].as_object().unwrap();
    let review = criteria
        .values()
        .any(|c| c.get("adequacy_review").is_some());
    let answer_review = criteria.values().any(|c| c.get("answer_review").is_some());
    let behavior = m.behavior.load(Ordering::Relaxed);
    let reject_answer = answer_review
        && (behavior == 13
            || ([12, 15, 16].contains(&behavior)
                && v["state"]["request"]["proposed_answer"]
                    .as_str()
                    .is_some_and(|s| s.contains("220"))));
    let first = if (review && behavior == 9) || reject_answer {
        "c1".to_owned()
    } else {
        criteria.keys().next().unwrap().clone()
    };
    let n = criteria.len();
    let mut probabilities = serde_json::Map::new();
    for k in criteria.keys() {
        probabilities.insert(
            k.clone(),
            json!(if k == &first {
                if n == 1 { 1.0 } else { 0.96 }
            } else {
                0.04 / (n - 1) as f64
            }),
        );
    }
    let choice = if behavior == 1 || (review && behavior == 10) || (answer_review && behavior == 14)
    {
        "evil".to_owned()
    } else {
        first
    };
    let confidence =
        if !answer_review && (behavior == 2 || ([8, 9, 10].contains(&behavior) && n > 2)) {
            0.2
        } else {
            0.95
        };
    (
        StatusCode::OK,
        Json(
            json!({"model":if behavior==20{"typesafe/jev-0.1-20260917"}else{"typesafe/jev-1.13-20260917"},"answers":{"select":{"type":"choice","choice":choice,"confidence":confidence,"probabilities":probabilities},"advisor":{"type":"noul","noul":if m.behavior.load(Ordering::Relaxed)==7{0.99}else{0.01}}},"usage":{"input_tokens":100,"output_tokens":20,"cost":0.0000042}}),
        ),
    )
}
async fn generate(State(m): State<Arc<Mock>>, Json(v): Json<Value>) -> axum::response::Response {
    if v["response_format"]["json_schema"]["name"] == "jev_answer_review" {
        let behavior = m.behavior.load(Ordering::Relaxed);
        let reject = behavior == 13
            || ([12, 15, 16].contains(&behavior)
                && v["messages"][1]["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("220")));
        let text = if behavior == 14 {
            "malformed".into()
        } else {
            json!({"accepted":!reject}).to_string()
        };
        return Json(json!({"model":v["model"],"choices":[{"message":{"content":text},"finish_reason":"stop"}],"usage":{"cost":0.00001}})).into_response();
    }
    let generation = m.generations.fetch_add(1, Ordering::Relaxed);
    m.requests.lock().unwrap().push(v.clone());
    if m.behavior.load(Ordering::Relaxed) == 21 && v["model"] == "vendor/cheap" {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"private provider outage"})),
        )
            .into_response();
    }
    if m.behavior.load(Ordering::Relaxed) == 23 {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":"private rate limit details"})),
        )
            .into_response();
    }
    if m.behavior.load(Ordering::Relaxed) == 5 && v["model"] == "vendor/cheap" {
        return (StatusCode::NOT_FOUND, Json(json!({"error":"no endpoint"}))).into_response();
    }
    if v["stream"] == true {
        let text = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"id":"s1","model":v["model"],"choices":[{"delta":{"content":"PONG"}}]}),
            json!({"choices":[],"usage":{"cost":0.0001}})
        );
        let slow = m.behavior.load(Ordering::Relaxed) == 6;
        let stream = async_stream::stream! {for c in text.as_bytes().chunks(7){yield Ok::<_,std::io::Error>(bytes::Bytes::copy_from_slice(c));if slow{tokio::time::sleep(std::time::Duration::from_millis(100)).await;}}};
        let mut r = axum::response::Response::new(Body::from_stream(stream));
        r.headers_mut()
            .insert("content-type", "text/event-stream".parse().unwrap());
        return r;
    }
    let message = if v.get("tools").is_some() {
        json!({"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{}"}}]})
    } else {
        json!({"role":"assistant","content":if m.behavior.load(Ordering::Relaxed)==18{if generation==0{"The total is ₹220."}else{"The total is ₹270."}}else if [12,15,16].contains(&m.behavior.load(Ordering::Relaxed)){if generation==0{"₹220"}else{"₹270"}}else{"PONG"}})
    };
    let mut output = json!({"id":"gen-test","model":v["model"],"choices":[{"index":0,"message":message,"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"cost":0.0001}});
    if m.behavior.load(Ordering::Relaxed) == 15 && generation > 0 {
        output["model"] = json!("unexpected/model");
    }
    if m.behavior.load(Ordering::Relaxed) == 16 && generation == 0 {
        output["usage"].as_object_mut().unwrap().remove("cost");
    }
    Json(output).into_response()
}
async fn responses(State(m): State<Arc<Mock>>, Json(v): Json<Value>) -> Json<Value> {
    m.requests.lock().unwrap().push(v.clone());
    Json(
        json!({"id":"resp-test","model":v["model"],"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"PONG"}]}],"usage":{"cost":0.0001}}),
    )
}
async fn messages(State(m): State<Arc<Mock>>, Json(v): Json<Value>) -> Json<Value> {
    m.requests.lock().unwrap().push(v.clone());
    Json(
        json!({"id":"msg-test","type":"message","model":v["model"],"role":"assistant","content":[{"type":"text","text":"PONG"}],"usage":{"cost":0.0001}}),
    )
}
async fn setup(mut cfg: Config) -> System {
    let mock = Arc::new(Mock::default());
    let fake = Router::new()
        .route("/api/v1/key", get(auth))
        .route("/api/alpha/decisions", post(judge))
        .route("/api/v1/chat/completions", post(generate))
        .route("/api/v1/responses", post(responses))
        .route("/api/v1/messages", post(messages))
        .with_state(mock.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    cfg.upstream = format!("http://{}", l.local_addr().unwrap());
    let h = tokio::spawn(async move {
        axum::serve(l, fake).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    cfg.database = dir.path().join("router.db");
    cfg.catalog = dir.path().join("catalog.json");
    cfg.fallback_model = "vendor/strong".into();
    let store = Arc::new(Store::open(&cfg.database).await.unwrap());
    let state = AppState::new(
        cfg,
        registry(),
        store.clone(),
        Some(Credential::new("local-key".into()).unwrap()),
    )
    .unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let a = app(state.clone());
    let h2 = tokio::spawn(async move {
        axum::serve(l, a.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap();
    });
    System {
        url,
        store,
        mock,
        state,
        _dir: dir,
        handles: vec![h, h2],
    }
}
fn public() -> Config {
    Config {
        bind: "0.0.0.0:8788".parse().unwrap(),
        ..Config::default()
    }
}
fn request() -> Value {
    json!({"model":"typesafe/jev-router","messages":[{"role":"user","content":"Reply PONG"}],"max_tokens":512})
}
#[tokio::test]
async fn failing_generation_model_does_not_open_another_models_circuit() {
    let s = setup(public()).await;
    s.mock.behavior.store(21, Ordering::Relaxed);
    for _ in 0..3 {
        assert_eq!(
            send(&s, request(), "key-a", "/v1/chat/completions")
                .await
                .status(),
            503
        );
    }
    let mut v = request();
    v["plugins"] = json!([{"id":"jev-router","models":["vendor/strong"]}]);
    assert_eq!(
        send(&s, v, "key-a", "/v1/chat/completions").await.status(),
        200
    );
}
#[tokio::test]
async fn routing_rate_limit_is_explicit_and_never_calls_generation() {
    let s = setup(public()).await;
    s.mock.behavior.store(22, Ordering::Relaxed);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 429);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"]["code"],
        "routing_rate_limit"
    );
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn generation_rate_limit_is_explicit_and_is_not_retried() {
    let s = setup(public()).await;
    s.mock.behavior.store(23, Ordering::Relaxed);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 429);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"]["code"],
        "generation_rate_limit"
    );
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 1);
}
#[test]
fn json_schema_requires_structured_output_capability_not_just_json_mode() {
    let mut v = request();
    v["response_format"] =
        json!({"type":"json_schema","json_schema":{"name":"test","schema":{"type":"object"}}});
    let task = policy::parse(v, Protocol::Chat, &public()).unwrap();
    let mut r = registry();
    r.models[0].parameters.retain(|s| s != "structured_outputs");
    let (pool, _) = policy::eligible(&task, &r, &public()).unwrap();
    assert!(pool.iter().all(|m| m.id != "vendor/cheap"));
}
#[tokio::test]
async fn unsupported_generation_parameters_are_filtered_before_jev() {
    let s = setup(public()).await;
    let mut v = request();
    v["logprobs"] = json!(true);
    assert_eq!(
        send(&s, v, "key-a", "/v1/chat/completions").await.status(),
        404
    );
    assert_eq!(s.mock.judgements.load(Ordering::Relaxed), 0);
    let mut v = request();
    v["tools"] = json!({"invalid":"array required"});
    assert_eq!(
        send(&s, v, "key-a", "/v1/chat/completions").await.status(),
        400
    );
}
#[tokio::test]
async fn unexpected_jev_version_never_generates() {
    let s = setup(public()).await;
    s.mock.behavior.store(20, Ordering::Relaxed);
    assert_eq!(
        send(&s, request(), "key-a", "/v1/chat/completions")
            .await
            .status(),
        502
    );
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn body_admission_is_bounded_before_json_extraction_and_health_stays_available() {
    use tokio::io::AsyncWriteExt;
    let s = setup(Config {
        max_concurrent: 1,
        ..public()
    })
    .await;
    let addr = s.url.strip_prefix("http://").unwrap();
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    socket.write_all(format!("POST /v1/chat/completions HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: 10000\r\n\r\n{{").as_bytes()).await.unwrap();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();
    let mut saturated = false;
    for _ in 0..30 {
        let r = client
            .post(format!("{}/v1/chat/completions", s.url))
            .header("Content-Type", "application/json")
            .header("Origin", "https://client.example")
            .body("{")
            .send()
            .await
            .unwrap();
        if r.status() == 503 {
            saturated = true;
            assert_eq!(r.headers()["access-control-allow-origin"], "*");
            assert_eq!(
                r.json::<Value>().await.unwrap()["error"]["code"],
                "body_capacity"
            );
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(saturated);
    assert_eq!(
        client
            .get(format!("{}/healthz", s.url))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(s.mock.judgements.load(Ordering::Relaxed), 0);
    drop(socket);
    let mut released = false;
    for _ in 0..30 {
        let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
        if r.status() == 200 {
            released = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(released);
}
#[test]
fn media_requirements_cover_the_entire_forwarded_conversation() {
    let mut v = request();
    let mut messages = vec![
        json!({"role":"user","content":[{"type":"image_url","image_url":{"url":"https://example.invalid/image.png"}}]}),
    ];
    messages
        .extend((0..12).map(|_| json!({"role":"user","content":"Text later in the conversation"})));
    v["messages"] = json!(messages);
    let task = policy::parse(v, Protocol::Chat, &public()).unwrap();
    assert!(task.modalities.contains(&"image".into()));
    let mut r = registry();
    r.models[0].modalities = vec!["text".into()];
    let (pool, _) = policy::eligible(&task, &r, &public()).unwrap();
    assert!(pool.iter().all(|m| m.id != "vendor/cheap"));
}
#[test]
fn classifier_summary_retains_the_latest_request_and_responses_instructions() {
    let mut v = request();
    v["messages"]=json!((0..8).map(|i|json!({"role":"user","content":if i==7{"LATEST_TASK_MARKER".into()}else{"old ".repeat(800)}})).collect::<Vec<_>>());
    let task = policy::parse(v, Protocol::Chat, &public()).unwrap();
    assert!(
        task.summary["recent_text"]
            .as_str()
            .unwrap()
            .contains("LATEST_TASK_MARKER")
    );
    let task = policy::parse(
        json!({"input":"Explain the report","instructions":"IMPORTANT_INSTRUCTION"}),
        Protocol::Responses,
        &public(),
    )
    .unwrap();
    assert!(
        task.summary["recent_text"]
            .as_str()
            .unwrap()
            .contains("IMPORTANT_INSTRUCTION")
    );
}
#[tokio::test]
async fn malformed_control_fields_fail_before_paid_calls() {
    let s = setup(public()).await;
    for patch in [
        json!({"model":7}),
        json!({"reasoning":{"effort":7}}),
        json!({"reasoning_effort":false}),
        json!({"reasoning":{"effort":"low"},"reasoning_effort":"high"}),
        json!({"max_tokens":512,"max_completion_tokens":1024}),
    ] {
        let mut v = request();
        for (k, x) in patch.as_object().unwrap() {
            v[k] = x.clone();
        }
        assert_eq!(
            send(&s, v, "key-a", "/v1/chat/completions").await.status(),
            400
        );
    }
    assert_eq!(
        send(&s, json!({"input":" "}), "key-a", "/v1/responses")
            .await
            .status(),
        400
    );
    assert_eq!(s.mock.judgements.load(Ordering::Relaxed), 0);
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn responses_tool_outputs_retain_model_and_effort() {
    let s = setup(public()).await;
    let tenant = Credential::new("key-a".into()).unwrap().tenant();
    s.store
        .bind_session(&tenant, "response-loop", "vendor/strong", "high")
        .await
        .unwrap();
    let v = json!({"model":"auto","session_id":"response-loop","input":[{"type":"function_call_output","call_id":"call-1","output":"result"},{"role":"user","content":"Continue"}],"max_output_tokens":512});
    let r = send(&s, v, "key-a", "/v1/responses").await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["model"], "vendor/strong");
    let forwarded = s.mock.requests.lock().unwrap();
    assert_eq!(forwarded[0]["reasoning"]["effort"], "high");
    assert!(forwarded[0].get("session_id").is_none());
}
#[tokio::test]
async fn tool_output_without_a_tenant_binding_fails_before_paid_calls() {
    let s = setup(public()).await;
    let v = json!({"model":"auto","session_id":"unknown","input":[{"type":"function_call_output","call_id":"call-1","output":"result"}],"max_output_tokens":512});
    assert_eq!(send(&s, v, "key-a", "/v1/responses").await.status(), 409);
    assert_eq!(s.mock.judgements.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn concurrency_limit_counts_inflight_work_from_the_previous_utc_day() {
    let s = setup(Config {
        tenant_concurrent: 1,
        ..public()
    })
    .await;
    let tenant = Credential::new("key-a".into()).unwrap().tenant();
    sqlx::query("INSERT INTO quotas(tenant,day,active) VALUES(?,?,1)")
        .bind(&tenant)
        .bind(jev_router::config::now() / 86400 - 1)
        .execute(&s.store.pool)
        .await
        .unwrap();
    assert_eq!(s.store.usage(&tenant).await.unwrap()["active"], 1);
    assert_eq!(
        send(&s, request(), "key-a", "/v1/chat/completions")
            .await
            .status(),
        429
    );
    assert_eq!(s.mock.judgements.load(Ordering::Relaxed), 0);
}
#[test]
fn served_model_revision_validation_rejects_spoofed_suffixes() {
    use jev_router::registry::served_matches;
    assert!(served_matches("vendor/model", "vendor/model-20261018"));
    assert!(served_matches("vendor/model", "vendor/model-2026-10-18"));
    for s in [
        "vendor/other",
        "vendor/model---------",
        "vendor/model-202610180000",
        "vendor/model-20261018:free",
    ] {
        assert!(!served_matches("vendor/model", s));
    }
}
#[tokio::test]
async fn fallible_model_review_does_not_reject_unverified_tasks_by_default() {
    let s = setup(public()).await;
    s.mock.behavior.store(13, Ordering::Relaxed);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(
        v["openrouter_metadata"]["pipeline"][0]["data"]["answer_review"]["status"],
        "no_deterministic_validator"
    );
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 1);
}
#[test]
fn money_totals_use_exact_arithmetic_and_reject_conflicting_answers() {
    use jev_router::validation::MoneyTotal;
    let c = MoneyTotal::from_request(
        "A shop sells 3 notebooks at ₹80 each and 2 pens at ₹15 each. What is the total?",
    )
    .unwrap();
    assert!(c.accepts("The total is ₹270."));
    assert!(!c.accepts("The total cost is ₹220."));
    assert!(!c.accepts("The total is ₹270, but the total is ₹280."));
    let d = MoneyTotal::from_request(
        "3 books at $0.10 each and 2 pens at $0.15 each. What is the total?",
    )
    .unwrap();
    assert!(d.accepts("The total is $0.60."));
}
#[tokio::test]
async fn exact_money_check_blocks_wrong_total_even_if_a_model_would_accept_it() {
    let s = setup(Config {
        repair_model: "vendor/mid".into(),
        ..public()
    })
    .await;
    s.mock.behavior.store(18, Ordering::Relaxed);
    let mut v = request();
    v["messages"][0]["content"] =
        json!("A shop sells 3 notebooks at ₹80 each and 2 pens at ₹15 each. What is the total?");
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "The total is ₹270.");
    assert_eq!(
        v["openrouter_metadata"]["pipeline"][0]["data"]["answer_review"]["model"],
        "deterministic_money_total"
    );
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 2);
    assert!(
        s.mock.requests.lock().unwrap()[1]
            .to_string()
            .contains("₹270.00")
    );
}
#[test]
fn ambiguous_money_requests_are_not_claimed_as_exactly_validated() {
    use jev_router::validation::MoneyTotal;
    for s in [
        "3 books at ₹80 each and 2 pens at ₹15 each with tax. What is the total?",
        "1.5 books at ₹80 each. What is the total?",
        "-3 books at ₹80 each. What is the total?",
        "3 books at ₹80 each or 2 pens at ₹15 each. What is the total?",
        "3 books at ₹80 each and 2 pens at $15 each. What is the total?",
        "3 books at ₹80 each plus a fee of 10. What is the total?",
        "9999999999999999999999999999999999999999999 books at ₹80 each. What is the total?",
    ] {
        assert!(MoneyTotal::from_request(s).is_none(), "{s}");
    }
}
#[tokio::test]
async fn incorrect_text_is_repaired_once_and_both_generations_are_accounted() {
    let s = setup(Config {
        repair_model: "vendor/mid".into(),
        model_answer_review: true,
        ..public()
    })
    .await;
    s.mock.behavior.store(12, Ordering::Relaxed);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "₹270");
    assert_eq!(v["model"], "vendor/mid");
    let meta = &v["openrouter_metadata"]["pipeline"][0]["data"];
    assert_eq!(meta["answer_review"]["status"], "repaired");
    assert_eq!(meta["reason"], "answer_repaired");
    let record = s
        .store
        .request(
            &Credential::new("key-a".into()).unwrap().tenant(),
            meta["request_id"].as_str().unwrap(),
        )
        .await
        .unwrap();
    assert!(
        (record["cost_usd"].as_f64().unwrap()
            - (0.0002
                + meta["jev_cost"].as_f64().unwrap()
                + meta["answer_review"]["cost"].as_f64().unwrap()))
        .abs()
            < 1e-10
    );
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 2);
}
#[tokio::test]
async fn repair_does_not_restore_hard_excluded_models() {
    let s = setup(Config {
        model_answer_review: true,
        ..public()
    })
    .await;
    s.mock.behavior.store(12, Ordering::Relaxed);
    let mut v = request();
    v["plugins"] = json!([{"id":"jev-router","excluded_models":["vendor/strong"]}]);
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 502);
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 1);
}
#[tokio::test]
async fn review_failure_after_repair_is_bounded_and_never_returns_rejected_answer() {
    let s = setup(Config {
        model_answer_review: true,
        ..public()
    })
    .await;
    s.mock.behavior.store(13, Ordering::Relaxed);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 502);
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 2);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"]["code"],
        "answer_review_failed"
    );
}
#[tokio::test]
async fn malformed_answer_review_fails_closed() {
    let s = setup(Config {
        model_answer_review: true,
        ..public()
    })
    .await;
    s.mock.behavior.store(14, Ordering::Relaxed);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 502);
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 1);
}
#[tokio::test]
async fn repair_model_identity_is_checked() {
    let s = setup(Config {
        model_answer_review: true,
        ..public()
    })
    .await;
    s.mock.behavior.store(15, Ordering::Relaxed);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 502);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"]["code"],
        "unexpected_model"
    );
}
#[tokio::test]
async fn repair_with_missing_initial_cost_retains_conservative_accounting() {
    let s = setup(Config {
        model_answer_review: true,
        ..public()
    })
    .await;
    s.mock.behavior.store(16, Ordering::Relaxed);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(
        v["openrouter_metadata"]["pipeline"][0]["data"]["billing_exact"],
        false
    );
}
#[tokio::test]
async fn operator_can_disable_text_review_explicitly() {
    let s = setup(Config {
        verify_text_answers: false,
        ..public()
    })
    .await;
    s.mock.behavior.store(14, Ordering::Relaxed);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(
        v["openrouter_metadata"]["pipeline"][0]["data"]["answer_review"]["status"],
        "disabled"
    );
}
#[tokio::test]
async fn expensive_catalog_outlier_cannot_block_affordable_routes() {
    let s = setup(public()).await;
    let mut expensive = model("vendor/outlier");
    expensive.output_price = 0.001;
    s.state.registry.write().await.models.push(expensive);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    assert!(s.mock.decisions.lock().unwrap().iter().all(|v| {
        !v["questions"]["select"]["criteria"]
            .to_string()
            .contains("vendor/outlier")
    }));
}
#[tokio::test]
async fn unaffordable_soft_preference_falls_back_without_restoring_exclusions() {
    let s = setup(public()).await;
    let mut expensive = model("vendor/outlier");
    expensive.output_price = 0.001;
    s.state.registry.write().await.models.push(expensive);
    let mut v = request();
    v["plugins"] =
        json!([{"id":"jev-router","models":["vendor/outlier"],"excluded_models":["vendor/cheap"]}]);
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["model"], "vendor/mid");
    assert_eq!(
        v["openrouter_metadata"]["pipeline"][0]["data"]["list_fallback"],
        "models_ignored"
    );
}
#[tokio::test]
async fn rejected_answer_never_creates_a_session_binding() {
    let s = setup(Config {
        model_answer_review: true,
        ..public()
    })
    .await;
    s.mock.behavior.store(13, Ordering::Relaxed);
    let mut v = request();
    v["session_id"] = json!("unaccepted");
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 502);
    assert!(
        s.store
            .session(
                &Credential::new("key-a".into()).unwrap().tenant(),
                "unaccepted",
                86400
            )
            .await
            .unwrap()
            .is_none()
    );
}
async fn send(s: &System, v: Value, key: &str, path: &str) -> reqwest::Response {
    let mut r = reqwest::Client::new()
        .post(format!("{}{path}", s.url))
        .header("X-OpenRouter-Metadata", "enabled")
        .json(&v);
    if !key.is_empty() {
        r = r.bearer_auth(key);
    }
    r.send().await.unwrap()
}
#[tokio::test]
async fn full_http_route_and_effort() {
    let s = setup(public()).await;
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let id = r.headers()["x-request-id"].to_str().unwrap().to_owned();
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["model"], "vendor/cheap");
    assert_eq!(
        body["openrouter_metadata"]["pipeline"][0]["data"]["effort"],
        "low"
    );
    let record = s
        .store
        .request(&Credential::new("key-a".into()).unwrap().tenant(), &id)
        .await
        .unwrap();
    assert_eq!(record["status"], "completed");
    assert!(record["cost_usd"].as_f64().unwrap() > 0.0001);
    assert!(!body.to_string().contains("key-a"));
}
#[tokio::test]
async fn public_never_uses_operator_key() {
    let s = setup(public()).await;
    assert_eq!(
        send(&s, request(), "", "/v1/chat/completions")
            .await
            .status(),
        401
    );
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn local_key_is_loopback_only() {
    let s = setup(Config::default()).await;
    assert_eq!(
        send(&s, request(), "", "/v1/chat/completions")
            .await
            .status(),
        200
    );
}
#[tokio::test]
async fn invalid_keys_are_sanitized() {
    let s = setup(public()).await;
    let r = send(&s, request(), "bad-key", "/v1/chat/completions").await;
    assert_eq!(r.status(), 401);
    let t = r.text().await.unwrap();
    assert!(!t.contains("raw private"));
    assert!(!t.contains("bad-key"));
}
#[tokio::test]
async fn foreign_origin_cannot_use_local_key() {
    let s = setup(Config::default()).await;
    let r = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", s.url))
        .header("Origin", "https://evil.example")
        .json(&request())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}
#[tokio::test]
async fn malformed_jev_never_calls_generation() {
    let s = setup(public()).await;
    s.mock.behavior.store(1, Ordering::Relaxed);
    assert_eq!(
        send(&s, request(), "key-a", "/v1/chat/completions")
            .await
            .status(),
        502
    );
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn low_confidence_uses_eligible_fallback() {
    let s = setup(public()).await;
    s.mock.behavior.store(2, Ordering::Relaxed);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["model"], "vendor/strong");
}
#[tokio::test]
async fn wide_choice_uncertainty_is_rechecked_before_premium_escalation() {
    let s = setup(public()).await;
    s.mock.behavior.store(8, Ordering::Relaxed);
    let mut v = request();
    v["plugins"] = json!([{"id":"jev-router","cost_tier":"low"}]);
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["model"], "vendor/cheap");
    assert_eq!(
        v["openrouter_metadata"]["pipeline"][0]["data"]["reason"],
        "adequacy_confirmed"
    );
    assert_eq!(
        s.mock.requests.lock().unwrap()[0]["reasoning"]["effort"],
        "low"
    );
}
#[tokio::test]
async fn adequacy_review_can_escalate_when_jev_confidently_requires_it() {
    let s = setup(public()).await;
    s.mock.behavior.store(9, Ordering::Relaxed);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["model"], "vendor/strong");
    assert_eq!(
        v["openrouter_metadata"]["pipeline"][0]["data"]["reason"],
        "adequacy_escalated"
    );
}
#[tokio::test]
async fn inconclusive_low_cost_review_does_not_generate_expensive_output() {
    let s = setup(public()).await;
    s.mock.behavior.store(2, Ordering::Relaxed);
    let mut v = request();
    v["plugins"] = json!([{"id":"jev-router","cost_tier":"low"}]);
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 503);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "uncertain_cost_sensitive_route");
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn malformed_adequacy_review_never_generates() {
    let s = setup(public()).await;
    s.mock.behavior.store(10, Ordering::Relaxed);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 502);
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn uncertainty_review_cannot_restore_an_excluded_fallback() {
    let s = setup(public()).await;
    s.mock.behavior.store(2, Ordering::Relaxed);
    let mut v = request();
    v["plugins"] = json!([{"id":"jev-router","excluded_models":["vendor/strong"]}]);
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 503);
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 0);
    assert!(s.mock.decisions.lock().unwrap().iter().all(|v| {
        !v["questions"]["select"]["criteria"]
            .to_string()
            .contains("vendor/strong")
    }));
}
#[tokio::test]
async fn unresolved_effort_uses_stronger_of_shortlist_not_maximum() {
    let s = setup(public()).await;
    s.mock.behavior.store(2, Ordering::Relaxed);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    // All models advertise low/medium/high;
    // the mocked top two are low and medium, so high is unjustified.
    assert_eq!(
        s.mock.requests.lock().unwrap()[0]["reasoning"]["effort"],
        "medium"
    );
}
#[tokio::test]
async fn only_eligible_model_is_not_replaced_due_to_choice_uncertainty() {
    let s = setup(public()).await;
    s.mock.behavior.store(2, Ordering::Relaxed);
    let mut v = request();
    v["plugins"] = json!([{"id":"jev-router","models":["vendor/cheap"],"cost_tier":"low"}]);
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    assert_eq!(s.mock.requests.lock().unwrap()[0]["model"], "vendor/cheap");
}
#[tokio::test]
async fn exclusions_survive_include_fallback() {
    let s = setup(public()).await;
    let mut v = request();
    v["plugins"] =
        json!([{"id":"jev-router","models":["missing/*"],"excluded_models":["vendor/cheap"]}]);
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_ne!(v["model"], "vendor/cheap");
    assert_eq!(
        v["openrouter_metadata"]["pipeline"][0]["data"]["list_fallback"],
        "models_ignored"
    );
}
#[tokio::test]
async fn excluded_all_fails_before_jev() {
    let s = setup(public()).await;
    let mut v = request();
    v["plugins"] = json!([{"id":"jev-router","excluded_models":["*"]}]);
    assert_eq!(
        send(&s, v, "key-a", "/v1/chat/completions").await.status(),
        404
    );
    assert_eq!(s.mock.judgements.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn unavailable_include_endpoint_rebuilds_pool() {
    let s = setup(public()).await;
    s.mock.behavior.store(5, Ordering::Relaxed);
    let mut v = request();
    v["plugins"] = json!([{"id":"jev-router","models":["vendor/cheap"]}]);
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let b: Value = r.json().await.unwrap();
    assert_ne!(b["model"], "vendor/cheap");
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 2);
}
#[tokio::test]
async fn preview_never_generates() {
    let s = setup(public()).await;
    assert_eq!(
        send(&s, request(), "key-a", "/v1/route").await.status(),
        200
    );
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn tool_calls_forward_without_execution() {
    let s = setup(public()).await;
    let mut v = request();
    v["tools"] = json!([{"type":"function","function":{"name":"get_weather","parameters":{"type":"object"}}}]);
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.json::<Value>().await.unwrap()["choices"][0]["message"]["tool_calls"][0]["id"],
        "call_1"
    );
}
#[tokio::test]
async fn protocol_adapters_preserve_original_input() {
    let s = setup(public()).await;
    for (path, body) in [
        (
            "/v1/responses",
            json!({"input":"Reply PONG","max_output_tokens":512}),
        ),
        (
            "/v1/messages",
            json!({"messages":[{"role":"user","content":"Reply PONG"}],"max_tokens":512}),
        ),
    ] {
        assert_eq!(send(&s, body, "key-a", path).await.status(), 200);
    }
    let r = s.mock.requests.lock().unwrap();
    assert_eq!(r[0]["input"], "Reply PONG");
    assert!(r[1]["messages"].is_array());
}
#[tokio::test]
async fn fragmented_streams_preserve_wire_and_settle_usage() {
    let s = setup(public()).await;
    let mut v = request();
    v["stream"] = json!(true);
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let id = r.headers()["x-request-id"].to_str().unwrap().to_owned();
    let text = r.text().await.unwrap();
    assert!(text.contains("[DONE]"));
    assert!(text.contains("PONG"));
    let tenant = Credential::new("key-a".into()).unwrap().tenant();
    for _ in 0..20 {
        let r = s.store.request(&tenant, &id).await.unwrap();
        if r["status"] == "completed" {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("Stream settlement missing");
}
#[tokio::test]
async fn cancelled_stream_releases_capacity_conservatively() {
    let s = setup(public()).await;
    s.mock.behavior.store(6, Ordering::Relaxed);
    let mut v = request();
    v["stream"] = json!(true);
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    let id = r.headers()["x-request-id"].to_str().unwrap().to_owned();
    let mut stream = r.bytes_stream();
    let _ = stream.next().await;
    drop(stream);
    let tenant = Credential::new("key-a".into()).unwrap().tenant();
    for _ in 0..50 {
        let r = s.store.request(&tenant, &id).await.unwrap();
        if r["status"] != "pending" {
            assert!(r["cost_usd"].as_f64().unwrap() > 0.0);
            assert_eq!(s.store.usage(&tenant).await.unwrap()["active"], 0);
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("Cancellation did not settle");
}
#[tokio::test]
async fn tenant_cannot_read_another_tenants_records() {
    let s = setup(public()).await;
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    let id = r.headers()["x-request-id"].to_str().unwrap().to_owned();
    let _ = r.text().await;
    let r = reqwest::Client::new()
        .get(format!("{}/v1/requests/{id}", s.url))
        .bearer_auth("key-b")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}
#[tokio::test]
async fn sessions_are_tenant_scoped() {
    let s = setup(public()).await;
    let a = Credential::new("key-a".into()).unwrap().tenant();
    let b = Credential::new("key-b".into()).unwrap().tenant();
    s.store
        .bind_session(&a, "same-session", "vendor/strong", "high")
        .await
        .unwrap();
    assert!(
        s.store
            .session(&b, "same-session", 86400)
            .await
            .unwrap()
            .is_none()
    );
    let mut v = request();
    v["session_id"] = json!("same-session");
    v["messages"] = json!([{"role":"tool","tool_call_id":"call_1","content":"Result"}]);
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["model"], "vendor/strong");
}
#[tokio::test]
async fn stale_registry_fails_closed() {
    let s = setup(public()).await;
    s.state.registry.write().await.fetched_at = 0;
    assert_eq!(
        send(&s, request(), "key-a", "/v1/chat/completions")
            .await
            .status(),
        503
    );
}
#[tokio::test]
async fn daily_budget_reservation_is_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("test.db")).await.unwrap());
    let cfg = Arc::new(Config {
        daily_estimated_budget_usd: 2.0,
        max_request_estimated_usd: 1.0,
        tenant_concurrent: 100,
        ..Config::default()
    });
    let mut tasks = vec![];
    for _ in 0..20 {
        let s = store.clone();
        let c = cfg.clone();
        tasks.push(tokio::spawn(async move {
            s.reserve("one-tenant", 1.0, &c).await
        }));
    }
    let mut accepted = vec![];
    for t in tasks {
        if let Ok(r) = t.await.unwrap() {
            accepted.push(r);
        }
    }
    assert_eq!(accepted.len(), 2);
    for r in accepted {
        r.finish(1.0, "completed", &json!({})).await.unwrap();
    }
    assert_eq!(
        store.usage("one-tenant").await.unwrap()["spent_or_conservative_estimate_usd"],
        2.0
    );
}
#[tokio::test]
async fn timeout_does_not_generate_or_leak_active_slots() {
    let cfg = Config {
        request_timeout_secs: 1,
        ..public()
    };
    let s = setup(cfg).await;
    s.mock.behavior.store(3, Ordering::Relaxed);
    assert_eq!(
        send(&s, request(), "key-a", "/v1/chat/completions")
            .await
            .status(),
        504
    );
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 0);
}
#[test]
fn parser_rejects_unknown_cost_tiers_and_unsupported_reasoning() {
    for plugin in [
        json!({"id":"jev-router","cost_tier":"max"}),
        json!({"id":"jev-router","allowed_model":["*"]}),
        json!({"id":"other"}),
    ] {
        let mut v = request();
        v["plugins"] = json!([plugin]);
        assert!(policy::parse(v, Protocol::Chat, &Config::default()).is_err());
    }
    let mut v = request();
    v["reasoning"] = json!({"max_tokens":10});
    assert!(policy::parse(v, Protocol::Chat, &Config::default()).is_err());
}
#[test]
fn cost_tiers_do_not_force_effort() {
    for tier in ["low", "medium", "high"] {
        let mut v = request();
        v["plugins"] = json!([{"id":"jev-router","cost_tier":tier}]);
        v["reasoning"] = json!({"effort":"high"});
        let t = policy::parse(v, Protocol::Chat, &Config::default()).unwrap();
        assert_eq!(t.effort_hint.as_deref(), Some("high"));
        assert_eq!(t.plugin.cost_tier.as_deref(), Some(tier));
    }
}
#[test]
fn unsupported_modalities_are_filtered() {
    let mut r = registry();
    r.models[0].modalities = vec!["text".into()];
    let v = json!({"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]}]});
    let t = policy::parse(v, Protocol::Chat, &Config::default()).unwrap();
    let (pool, _) = policy::eligible(&t, &r, &Config::default()).unwrap();
    assert!(pool.iter().all(|m| m.id != "vendor/cheap"));
}
#[test]
fn inspector_handles_fragmented_frames_and_missing_cost() {
    let mut s = Inspector::default();
    let bytes = b"data: {\"usage\":{\"cost\":0.25}}\n\ndata: [DONE]\n\n";
    for b in bytes {
        s.feed(&[*b]);
    }
    assert_eq!(s.cost, Some(0.25));
    assert!(s.done);
    let mut s = Inspector::default();
    s.feed(b"data: {\"error\":{}}\n\n");
    assert!(s.failed);
}
#[test]
fn upstream_configuration_blocks_external_plain_http() {
    let mut c = Config {
        upstream: "http://example.com".into(),
        ..Config::default()
    };
    assert!(c.validate().is_err());
    c.upstream = "https://secret@example.com".into();
    assert!(c.validate().is_err());
}

#[test]
fn exact_slug_does_not_admit_batch_or_free_variants() {
    let r = registry();
    let mut m = model("vendor/cheap:batch");
    m.canonical = "vendor/cheap".into();
    assert!(!policy::matches("vendor/cheap", &m, &r));
    m.id = "vendor/cheap:free".into();
    assert!(!policy::matches("vendor/cheap", &m, &r));
    assert!(policy::matches("vendor/*", &m, &r));
}
#[tokio::test]
async fn per_tenant_rate_limit_does_not_block_another_key() {
    let s = setup(Config {
        requests_per_minute: 1,
        ..public()
    })
    .await;
    assert_eq!(
        send(&s, request(), "key-a", "/v1/chat/completions")
            .await
            .status(),
        200
    );
    assert_eq!(
        send(&s, request(), "key-a", "/v1/chat/completions")
            .await
            .status(),
        429
    );
    assert_eq!(
        send(&s, request(), "key-b", "/v1/chat/completions")
            .await
            .status(),
        200
    );
}
#[tokio::test]
async fn operator_allowlist_cannot_be_relaxed_by_client_preferences() {
    let s = setup(Config {
        repair_model: "vendor/mid".into(),
        hard_allowed_models: vec!["vendor/mid".into()],
        ..public()
    })
    .await;
    let mut v = request();
    v["plugins"] = json!([{"id":"jev-router","models":["vendor/cheap"]}]);
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["model"], "vendor/mid");
}
#[tokio::test]
async fn tool_binding_never_overrides_a_new_hard_exclusion() {
    let s = setup(public()).await;
    let tenant = Credential::new("key-a".into()).unwrap().tenant();
    s.store
        .bind_session(&tenant, "task", "vendor/strong", "high")
        .await
        .unwrap();
    let mut v = request();
    v["session_id"] = json!("task");
    v["plugins"] = json!([{"id":"jev-router","excluded_models":["vendor/strong"]}]);
    v["messages"] = json!([{"role":"tool","tool_call_id":"id","content":"Result"}]);
    assert_eq!(
        send(&s, v, "key-a", "/v1/chat/completions").await.status(),
        409
    );
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn messages_accepts_same_openrouter_key_in_native_header() {
    let s = setup(public()).await;
    let r = reqwest::Client::new()
        .post(format!("{}/v1/messages", s.url))
        .header("x-api-key", "key-a")
        .json(&request())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}
#[tokio::test]
async fn interrupted_reservations_are_recovered_once() {
    let d = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&d.path().join("db")).await.unwrap());
    let r = store
        .reserve("tenant", 0.25, &Config::default())
        .await
        .unwrap();
    let id = r.id.clone();
    store.recover().await.unwrap();
    store.recover().await.unwrap();
    assert_eq!(
        store.usage("tenant").await.unwrap()["spent_or_conservative_estimate_usd"],
        0.25
    );
    assert_eq!(
        store.request("tenant", &id).await.unwrap()["status"],
        "interrupted_estimate"
    );
    r.finish(0.0, "completed", &json!({})).await.unwrap();
    assert_eq!(
        store.usage("tenant").await.unwrap()["spent_or_conservative_estimate_usd"],
        0.25
    );
}
#[tokio::test]
async fn large_dynamic_pool_is_judged_in_groups_then_compared() {
    let s = setup(Config {
        repair_model: "brand-000/model".into(),
        ..public()
    })
    .await;
    s.state.registry.write().await.models = (0..120)
        .map(|i| model(&format!("brand-{i:03}/model")))
        .collect();
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    assert_eq!(s.mock.judgements.load(Ordering::Relaxed), 5);
}
#[tokio::test]
async fn jev_outage_fails_closed_and_sanitizes_body() {
    let s = setup(public()).await;
    s.mock.behavior.store(4, Ordering::Relaxed);
    let r = send(&s, request(), "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 503);
    assert!(!r.text().await.unwrap().contains("sensitive"));
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn request_body_cap_precedes_paid_calls() {
    let s = setup(Config {
        max_body_bytes: 1024,
        ..public()
    })
    .await;
    let mut v = request();
    v["messages"][0]["content"] = json!("x".repeat(2000));
    assert_eq!(
        send(&s, v, "key-a", "/v1/chat/completions").await.status(),
        413
    );
    assert_eq!(s.mock.judgements.load(Ordering::Relaxed), 0);
}
#[test]
fn stream_model_mismatch_is_detected() {
    let mut s = Inspector::with_model("vendor/allowed".into());
    assert!(!s.feed(b"data: {\"model\":\"vendor/excluded\"}\n\n"));
    assert!(s.failed);
}
#[test]
fn operator_privacy_cannot_be_relaxed() {
    let mut v = request();
    v["provider"] = json!({"data_collection":"allow","zdr":false,"only":["host-a","host-b"]});
    let cfg = Config {
        zdr: true,
        provider_only: vec!["host-a".into()],
        ..Config::default()
    };
    let t = policy::parse(v, Protocol::Chat, &cfg).unwrap();
    let p = policy::provider(&t, &cfg, &model("vendor/cheap")).unwrap();
    assert_eq!(p["data_collection"], "deny");
    assert_eq!(p["zdr"], true);
    assert_eq!(p["only"], json!(["host-a"]));
}
#[test]
fn unknown_reasoning_metadata_does_not_invent_effort_controls() {
    let row = json!({"id":"vendor/new","description":"Test","context_length":10000,"top_provider":{"context_length":10000,"max_completion_tokens":4096},"architecture":{"input_modalities":["text"],"output_modalities":["text"]},"supported_parameters":["max_tokens","reasoning"],"reasoning":{"mandatory":true},"pricing":{"prompt":"0.000001","completion":"0.000002"}});
    let r = Registry::parse(&json!({"data":[row]})).unwrap();
    assert_eq!(r.models[0].control, "default");
    assert_eq!(r.models[0].efforts, vec!["default"]);
}
#[test]
fn omitted_and_null_effort_metadata_are_distinct() {
    let base = json!({"id":"vendor/new","context_length":10000,"top_provider":{"max_completion_tokens":4096},"architecture":{"input_modalities":["text"],"output_modalities":["text"]},"supported_parameters":["max_tokens","reasoning"],"reasoning":{"supported_efforts":null,"mandatory":true},"pricing":{"prompt":"0.000001","completion":"0.000002"}});
    let r = Registry::parse(&json!({"data":[base]})).unwrap();
    assert_eq!(r.models[0].control, "effort");
    assert!(!r.models[0].efforts.contains(&"none".into()));
    assert!(r.models[0].efforts.contains(&"high".into()));
}
#[test]
fn context_and_output_caps_filter_before_judgement() {
    let mut r = registry();
    r.models[0].context = 300;
    r.models[1].output = 100;
    let t = policy::parse(request(), Protocol::Chat, &Config::default()).unwrap();
    let (pool, _) = policy::eligible(&t, &r, &Config::default()).unwrap();
    assert_eq!(pool.len(), 1);
    assert_eq!(pool[0].id, "vendor/strong");
}

#[tokio::test]
async fn concurrent_http_fixture_load_has_no_accounting_errors() {
    let s = setup(Config {
        max_concurrent: 32,
        tenant_concurrent: 16,
        requests_per_minute: 1000,
        daily_estimated_budget_usd: 100.0,
        ..public()
    })
    .await;
    for key in ["key-a", "key-b"] {
        assert_eq!(
            reqwest::Client::new()
                .get(format!("{}/v1/models", s.url))
                .bearer_auth(key)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    let start = std::time::Instant::now();
    let outcomes = futures_util::stream::iter((0..100).map(|i| {
        let s = &s;
        async move {
            let start = std::time::Instant::now();
            let r = send(
                s,
                request(),
                if i % 2 == 0 { "key-a" } else { "key-b" },
                "/v1/chat/completions",
            )
            .await;
            let status = r.status();
            let _ = r.text().await.unwrap();
            (status.as_u16(), start.elapsed().as_millis())
        }
    }))
    .buffer_unordered(16)
    .collect::<Vec<_>>()
    .await;
    assert!(
        outcomes.iter().all(|(code, _)| *code == 200),
        "load returned non-200 statuses: {:?}",
        outcomes
            .iter()
            .filter(|(c, _)| *c != 200)
            .collect::<Vec<_>>()
    );
    for key in ["key-a", "key-b"] {
        let tenant = Credential::new(key.into()).unwrap().tenant();
        assert_eq!(s.store.usage(&tenant).await.unwrap()["active"], 0);
    }
    let mut times = outcomes.iter().map(|(_, t)| *t).collect::<Vec<_>>();
    times.sort_unstable();
    println!(
        "fixture_load_only {}",
        json!({"requests":100,"concurrency":16,"successful":100,"elapsed_ms":start.elapsed().as_millis(),"p95_ms":times[94],"real_provider_calls":0})
    );
}
#[tokio::test]
async fn malformed_json_returns_a_sanitized_json_error() {
    let s = setup(public()).await;
    let r = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", s.url))
        .header("Content-Type", "application/json")
        .bearer_auth("key-a")
        .body("{bad json")
        .send()
        .await
        .unwrap();
    assert!(r.status().is_client_error());
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"]["code"],
        "invalid_json"
    );
    assert_eq!(s.mock.judgements.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn public_cors_supports_explicit_byok_without_cookies() {
    let s = setup(public()).await;
    let r = reqwest::Client::new()
        .request(
            reqwest::Method::OPTIONS,
            format!("{}/v1/chat/completions", s.url),
        )
        .header("Origin", "https://client.example")
        .header("Access-Control-Request-Method", "POST")
        .header(
            "Access-Control-Request-Headers",
            "authorization,content-type",
        )
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    assert_eq!(r.headers()["access-control-allow-origin"], "*");
    assert!(
        r.headers()
            .get("access-control-allow-credentials")
            .is_none()
    );
}
#[tokio::test]
async fn localhost_operator_key_has_no_cross_origin_cors() {
    let s = setup(Config::default()).await;
    let r = reqwest::Client::new()
        .get(format!("{}/v1/usage", s.url))
        .header("Origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert!(r.headers().get("access-control-allow-origin").is_none());
    assert_eq!(r.headers()["cache-control"], "no-store");
}
#[tokio::test]
async fn advisor_is_bounded_and_preserves_client_privacy() {
    let s = setup(Config {
        advisor_model: Some("vendor/strong".into()),
        ..public()
    })
    .await;
    s.mock.behavior.store(7, Ordering::Relaxed);
    let mut v = request();
    v["plugins"] = json!([{"id":"jev-router","cost_tier":"high"}]);
    v["provider"] = json!({"data_collection":"deny","zdr":true});
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert_eq!(
        body["openrouter_metadata"]["pipeline"][0]["data"]["advisor_model"],
        "vendor/strong"
    );
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 2);
    let requests = s.mock.requests.lock().unwrap();
    assert_eq!(requests[0]["provider"]["zdr"], true);
    assert!(
        requests[1]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Untrusted expert advisory")
    );
}
#[tokio::test]
async fn streamed_work_obeys_total_request_deadline() {
    let s = setup(Config {
        request_timeout_secs: 1,
        ..public()
    })
    .await;
    s.mock.behavior.store(6, Ordering::Relaxed);
    let mut v = request();
    v["stream"] = json!(true);
    let start = std::time::Instant::now();
    let r = send(&s, v, "key-a", "/v1/chat/completions").await;
    assert_eq!(r.status(), 200);
    let _ = r.text().await;
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(s.mock.generations.load(Ordering::Relaxed), 1);
}
