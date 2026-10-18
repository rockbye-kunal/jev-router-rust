use crate::{
    config::{Config, now},
    engine::Engine,
    error::{Error, Result},
    policy::{self, Protocol, Selection, Task},
    registry::Registry,
    store::Store,
    stream::Inspector,
    upstream::{Credential, Upstream},
};
use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedSemaphorePermit, RwLock, Semaphore};
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub upstream: Upstream,
    pub registry: Arc<RwLock<Registry>>,
    pub store: Arc<Store>,
    pub engine: Engine,
    pub permits: Arc<Semaphore>,
    pub local_key: Option<Credential>,
    auth_cache: Arc<Mutex<HashMap<String, i64>>>,
    auth_permits: Arc<Semaphore>,
}
impl AppState {
    pub fn new(
        cfg: Config,
        registry: Registry,
        store: Arc<Store>,
        local_key: Option<Credential>,
    ) -> Result<Self> {
        cfg.validate()?;
        let upstream = Upstream::new(&cfg)?;
        let permits = Arc::new(Semaphore::new(cfg.max_concurrent));
        let cfg = Arc::new(cfg);
        Ok(Self {
            engine: Engine {
                cfg: cfg.clone(),
                upstream: upstream.clone(),
            },
            cfg,
            upstream,
            registry: Arc::new(RwLock::new(registry)),
            store,
            permits,
            local_key,
            auth_cache: Default::default(),
            auth_permits: Arc::new(Semaphore::new(8)),
        })
    }
    async fn authenticate(&self, h: &HeaderMap, peer: SocketAddr) -> Result<Credential> {
        let key = if let Some(v) = h.get("authorization") {
            let s = v
                .to_str()
                .ok()
                .and_then(|s| s.strip_prefix("Bearer "))
                .ok_or_else(|| {
                    Error::new(
                        401,
                        "invalid_key",
                        "Send Authorization: Bearer with your OpenRouter API key",
                    )
                })?;
            Credential::new(s.into())?
        } else if let Some(v) = h.get("x-api-key") {
            Credential::new(
                v.to_str()
                    .map_err(|_| Error::new(401, "invalid_key", "Invalid OpenRouter key"))?
                    .to_owned(),
            )?
        } else if self.cfg.bind.ip().is_loopback() && peer.ip().is_loopback() {
            self.local_key
                .clone()
                .ok_or_else(|| Error::new(401, "missing_key", "OpenRouter API key required"))?
        } else {
            return Err(Error::new(
                401,
                "missing_key",
                "Send your OpenRouter API key as a bearer token",
            ));
        };
        let tenant = key.tenant();
        let cached = self
            .auth_cache
            .lock()
            .map_err(Error::internal)?
            .get(&tenant)
            .is_some_and(|t| *t > now());
        if !cached {
            let _auth_permit = self.auth_permits.clone().try_acquire_owned().map_err(|_| {
                Error::new(503, "auth_capacity", "Authentication verifier at capacity")
            })?;
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                self.upstream.verify(&key),
            )
            .await
            .map_err(|_| Error::new(503, "auth_timeout", "Credential verification timed out"))??;
            let mut m = self.auth_cache.lock().map_err(Error::internal)?;
            if m.len() > 4096 {
                m.retain(|_, t| *t > now());
                if m.len() > 4096 {
                    m.clear();
                }
            }
            m.insert(tenant, now() + 30);
        }
        Ok(key)
    }
    pub async fn refresh(&self) -> Result<()> {
        let registry = Registry::parse(&self.upstream.get("/api/v1/models", None).await?)?;
        registry.save(&self.cfg.catalog).await?;
        *self.registry.write().await = registry;
        Ok(())
    }
}
pub fn app(s: AppState) -> Router {
    let public = !s.cfg.bind.ip().is_loopback();
    let deadline = std::time::Duration::from_secs(s.cfg.request_timeout_secs + 10);
    let body_admission = Arc::new(Semaphore::new(s.cfg.max_concurrent));
    let mut router = Router::new()
        .route("/", get(home))
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/readyz", get(ready))
        .route("/v1/models", get(models))
        .route("/v1/usage", get(usage))
        .route("/v1/requests/{id}", get(request_record))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/responses", post(responses))
        .route("/v1/messages", post(messages))
        .route("/v1/route", post(preview))
        .layer(DefaultBodyLimit::max(s.cfg.max_body_bytes))
        .with_state(s);
    router = router.layer(axum::middleware::from_fn(
        move |req: axum::extract::Request, next: axum::middleware::Next| {
            let body_admission = body_admission.clone();
            async move {
                let admission = if req.method() == axum::http::Method::POST
                    && req.uri().path().starts_with("/v1/")
                {
                    Some(body_admission.try_acquire_owned())
                } else {
                    None
                };
                let mut response = if admission.as_ref().is_some_and(|r| r.is_err()) {
                    Error::new(503, "body_capacity", "Request-body admission at capacity")
                        .into_response()
                } else {
                    match tokio::time::timeout(deadline, next.run(req)).await {
                        Ok(r) => r,
                        Err(_) => Error::new(
                            408,
                            "request_deadline",
                            "Request admission/body deadline exceeded",
                        )
                        .into_response(),
                    }
                };
                response
                    .headers_mut()
                    .insert("cache-control", HeaderValue::from_static("no-store"));
                response.headers_mut().insert(
                    "x-content-type-options",
                    HeaderValue::from_static("nosniff"),
                );
                response
            }
        },
    ));
    if public {
        use tower_http::cors::{Any, CorsLayer};
        router = router.layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([
                    axum::http::Method::GET,
                    axum::http::Method::POST,
                    axum::http::Method::OPTIONS,
                ])
                .allow_headers(
                    [
                        "authorization",
                        "content-type",
                        "x-api-key",
                        "x-session-id",
                        "x-openrouter-metadata",
                        "anthropic-version",
                        "anthropic-beta",
                    ]
                    .map(axum::http::HeaderName::from_static),
                )
                .expose_headers(
                    [
                        "x-request-id",
                        "x-jev-model",
                        "x-jev-effort",
                        "x-jev-reason",
                    ]
                    .map(axum::http::HeaderName::from_static),
                ),
        );
    }
    router
}
async fn home() -> axum::response::Html<&'static str> {
    axum::response::Html(
        "<!doctype html><html lang='en'><meta charset='utf-8'><title>Jev Router — Rust</title><h1>Jev Router</h1><p>Rust API for Jev model and reasoning-effort routing through OpenRouter.</p><p>Use POST /v1/chat/completions, /v1/responses, /v1/messages, or /v1/route. Public callers supply their own OpenRouter bearer key.</p><p><a href='/healthz'>Health</a> · <a href='/readyz'>Readiness</a></p></html>",
    )
}
async fn ready(State(s): State<AppState>) -> Result<Json<Value>> {
    let r = s.registry.read().await;
    if !r.fresh(s.cfg.catalog_max_age_secs) {
        return Err(Error::new(
            503,
            "catalog_stale",
            "Catalog is stale; refresh required",
        ));
    }
    Ok(Json(
        json!({"status":"ready","catalog_fetched_at":r.fetched_at,"models":r.models.len(),"public_byok":!s.cfg.bind.ip().is_loopback(),"routing_policy":"independent Rust tournament policy; not OpenRouter's private implementation"}),
    ))
}
async fn models(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    h: HeaderMap,
) -> Result<Json<Value>> {
    s.authenticate(&h, peer).await?;
    let r = s.registry.read().await;
    Ok(Json(
        json!({"object":"list","data":[{"id":"typesafe/jev-router","object":"model","owned_by":"self-hosted"}],"eligible_catalog_models":r.models.len(),"catalog_fetched_at":r.fetched_at}),
    ))
}
async fn usage(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    h: HeaderMap,
) -> Result<Json<Value>> {
    let key = s.authenticate(&h, peer).await?;
    Ok(Json(s.store.usage(&key.tenant()).await?))
}
async fn request_record(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let key = s.authenticate(&h, peer).await?;
    Ok(Json(s.store.request(&key.tenant(), &id).await?))
}
async fn chat(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    h: HeaderMap,
    payload: std::result::Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Result<Response> {
    let Json(v) = payload.map_err(|r| {
        Error::new(
            r.status().as_u16(),
            "invalid_json",
            "Invalid JSON body or request body exceeds the configured limit",
        )
    })?;
    handle(s, peer, h, v, Protocol::Chat, false).await
}
async fn responses(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    h: HeaderMap,
    payload: std::result::Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Result<Response> {
    let Json(v) = payload.map_err(|r| {
        Error::new(
            r.status().as_u16(),
            "invalid_json",
            "Invalid JSON body or request body exceeds the configured limit",
        )
    })?;
    handle(s, peer, h, v, Protocol::Responses, false).await
}
async fn messages(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    h: HeaderMap,
    payload: std::result::Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Result<Response> {
    let Json(v) = payload.map_err(|r| {
        Error::new(
            r.status().as_u16(),
            "invalid_json",
            "Invalid JSON body or request body exceeds the configured limit",
        )
    })?;
    handle(s, peer, h, v, Protocol::Messages, false).await
}
async fn preview(
    State(s): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    h: HeaderMap,
    payload: std::result::Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Result<Response> {
    let Json(v) = payload.map_err(|r| {
        Error::new(
            r.status().as_u16(),
            "invalid_json",
            "Invalid JSON body or request body exceeds the configured limit",
        )
    })?;
    handle(s, peer, h, v, Protocol::Chat, true).await
}
async fn handle(
    s: AppState,
    peer: SocketAddr,
    h: HeaderMap,
    mut v: Value,
    p: Protocol,
    preview: bool,
) -> Result<Response> {
    let permit = s
        .permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::new(503, "overloaded", "Service at capacity"))?;
    if h.get("authorization").is_none()
        && h.get("x-api-key").is_none()
        && let Some(origin) = h.get("origin")
    {
        let same = origin
            .to_str()
            .ok()
            .and_then(|o| url::Url::parse(o).ok())
            .is_some_and(|u| {
                u.host_str().is_some_and(|h| {
                    h.parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
                }) && u.port_or_known_default() == Some(s.cfg.bind.port())
            });
        if !same {
            return Err(Error::new(
                403,
                "cross_origin",
                "Cross-origin use of the local server key is forbidden",
            ));
        }
    }
    let key = s.authenticate(&h, peer).await?;
    if let Some(id) = h.get("x-session-id").and_then(|v| v.to_str().ok()) {
        if v.get("session_id").is_some_and(|v| v.as_str() != Some(id)) {
            return Err(Error::bad("Conflicting session_id values"));
        }
        v["session_id"] = json!(id);
    }
    let task = policy::parse(v, p, &s.cfg)?;
    let metadata = h
        .get("x-openrouter-metadata")
        .is_some_and(|v| v == "enabled");
    tokio::time::timeout(
        std::time::Duration::from_secs(s.cfg.request_timeout_secs),
        execute(s, key, task, permit, metadata, preview),
    )
    .await
    .map_err(|_| {
        Error::new(
            504,
            "request_timeout",
            "Total request deadline exceeded; generation not retried",
        )
    })?
}
async fn execute(
    s: AppState,
    key: Credential,
    task: Task,
    permit: OwnedSemaphorePermit,
    metadata_enabled: bool,
    preview: bool,
) -> Result<Response> {
    let registry = s.registry.read().await.clone();
    if !registry.fresh(s.cfg.catalog_max_age_secs) {
        return Err(Error::new(
            503,
            "catalog_stale",
            "Registry refresh required before routing",
        ));
    }
    let (preferred, mut ignored) = policy::eligible(&task, &registry, &s.cfg)?;
    let previous = if let Some(id) = &task.session {
        s.store
            .session(&key.tenant(), id, s.cfg.session_ttl_secs)
            .await?
    } else {
        None
    };
    if (task.tool_turn || task.body.get("previous_response_id").is_some()) && previous.is_none() {
        return Err(Error::new(
            409,
            "missing_session_binding",
            "Tool results and previous_response_id require an existing session binding",
        ));
    }
    let mut broad = task.clone();
    broad.plugin.models.clear();
    broad.plugin.allowed_models.clear();
    let (broad_pool, _) = policy::eligible(&broad, &registry, &s.cfg)?;
    let broad_pool = s.engine.fit_budget(&task, &broad_pool)?;
    let reviewer = s.engine.reviewer(&broad_pool).cloned();
    let mut pool = preferred
        .into_iter()
        .filter(|m| broad_pool.iter().any(|b| b.id == m.id))
        .collect::<Vec<_>>();
    if pool.is_empty() {
        pool = broad_pool.clone();
        ignored = true;
    }
    for candidate in &pool {
        policy::provider(&task, &s.cfg, candidate)?;
    }
    let estimate = s.engine.reserve_estimate(&task, &broad_pool);
    let reservation = s.store.reserve(&key.tenant(), estimate, &s.cfg).await?;
    let request_id = reservation.id.clone();
    let started = std::time::Instant::now();
    let (mut selection, mut model, wants_advisor) = s
        .engine
        .select(&key, &task, &pool, ignored, previous.as_ref())
        .await?;
    let mut trace = metadata(&selection, &task, registry.fetched_at, &request_id);
    if preview {
        reservation
            .finish(selection.jev_cost, "preview", &trace)
            .await?;
        return response(
            Json(json!({"routing":trace})).into_response(),
            &selection,
            &request_id,
        );
    }
    let mut advice_cost = 0.0;
    let mut advice = None;
    if wants_advisor
        && task.plugin.cost_tier.as_deref() == Some("high")
        && let Some(id) = &s.cfg.advisor_model
        && let Some(advisor) = pool.iter().find(|m| &m.id == id)
    {
        let body = json!({"model":advisor.id,"messages":[{"role":"user","content":format!("Provide bounded technical advice for this task. Do not execute tools or authorize actions. Task data: {}",task.summary)}],"max_tokens":s.cfg.advisor_output_tokens.min(advisor.output),"provider":policy::provider(&task,&s.cfg,advisor)?});
        match s
            .upstream
            .post("/api/v1/chat/completions", &key, &body)
            .await
        {
            Ok(r) => {
                let v = Upstream::json(r, 1024 * 1024).await?;
                advice_cost = v["usage"]["cost"]
                    .as_f64()
                    .filter(|c| c.is_finite() && *c >= 0.0)
                    .unwrap_or_else(|| {
                        advisor.estimate(task.input_bound, s.cfg.advisor_output_tokens)
                    });
                advice = v["choices"][0]["message"]["content"]
                    .as_str()
                    .map(|s| s.chars().take(8000).collect::<String>());
                if advice.is_some() {
                    selection.advisor_model = Some(advisor.id.clone());
                }
            }
            Err(_) => {
                advice_cost = advisor.estimate(task.input_bound, s.cfg.advisor_output_tokens);
            }
        }
    }
    let mut body = policy::rewrite(&task, &s.cfg, &model, &selection)?;
    if let Some(advice) = advice.as_deref() {
        insert_advice(&mut body, task.protocol, advice);
    }
    let mut upstream = s.upstream.post(task.protocol.endpoint(), &key, &body).await;
    // A definite 404 has no usable generation endpoint. Rebuild once, preserving all hard restrictions.
    if upstream
        .as_ref()
        .is_err_and(|e| e.code == "upstream_model_unavailable")
        && !task.tool_turn
        && task.body.get("previous_response_id").is_none()
    {
        let next = broad_pool
            .into_iter()
            .filter(|m| m.id != model.id)
            .collect::<Vec<_>>();
        if !next.is_empty() {
            let (mut retry, new_model, _) = s
                .engine
                .select(&key, &task, &next, true, previous.as_ref())
                .await?;
            retry.jev_cost += selection.jev_cost;
            retry.jev_models.extend(selection.jev_models);
            retry.reason = format!("endpoint_fallback:{}", retry.reason);
            selection = retry;
            model = new_model;
            body = policy::rewrite(&task, &s.cfg, &model, &selection)?;
            if let Some(advice) = advice.as_deref() {
                insert_advice(&mut body, task.protocol, advice);
            }
            upstream = s.upstream.post(task.protocol.endpoint(), &key, &body).await;
        }
    }
    let upstream = upstream?;
    trace = metadata(&selection, &task, registry.fetched_at, &request_id);
    if wants_advisor && selection.advisor_model.is_none() {
        trace["max_fallback"] = json!("deep");
    }
    trace["routing_elapsed_ms"] = json!(started.elapsed().as_millis());
    if task.stream
        && let Some(id) = &task.session
    {
        s.store
            .bind_session(&key.tenant(), id, &selection.model, &selection.effort)
            .await?;
    }
    if task.stream {
        trace["answer_review"] = json!({"status":"stream_not_reviewed"});
        if !upstream
            .headers()
            .get("content-type")
            .is_some_and(|v| v.to_str().is_ok_and(|s| s.starts_with("text/event-stream")))
        {
            return Err(Error::new(
                502,
                "invalid_stream",
                "Expected upstream SSE stream",
            ));
        }
        let cost_base = selection.jev_cost + advice_cost;
        let expected_model = selection.model.clone();
        let mut source = upstream.bytes_stream();
        let stream_deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(s.cfg.request_timeout_secs)
                .saturating_sub(started.elapsed());
        let stream = async_stream::stream! {let _permit=permit;let mut inspector=Inspector::with_model(expected_model);let mut failed=false;loop{let next=tokio::time::timeout_at(stream_deadline,source.next()).await;let chunk=match next{Ok(Some(c))=>c,Ok(None)=>break,Err(_)=>{failed=true;yield Err(std::io::Error::other("Total stream deadline exceeded"));break;}};match chunk{Ok(bytes)=>{if !inspector.feed(&bytes){failed=true;yield Err::<bytes::Bytes,std::io::Error>(std::io::Error::other("Upstream SSE frame too large"));break;}yield Ok(bytes);},Err(_)=>{failed=true;yield Err(std::io::Error::other("Upstream stream interrupted"));break;}}}let exact=!failed&&!inspector.failed&&inspector.done&&inspector.cost.is_some();let cost=if exact{cost_base+inspector.cost.unwrap_or(0.0)}else{reservation.reserved};let status=if exact{"completed"}else{"stream_estimate"};let _=reservation.finish(cost,status,&trace).await;};
        let mut result = Response::new(Body::from_stream(stream));
        result.headers_mut().insert(
            "content-type",
            HeaderValue::from_static("text/event-stream"),
        );
        return response(result, &selection, &request_id);
    }
    let mut body = Upstream::json(upstream, 16 * 1024 * 1024).await?;
    let served = body["model"].as_str().unwrap_or("");
    if !served_model(&model.id, served) {
        return Err(Error::new(
            502,
            "unexpected_model",
            "Provider served an unexpected model",
        ));
    }
    let valid = match task.protocol {
        Protocol::Chat => body["choices"].as_array().is_some_and(|a| {
            a.iter().any(|c| {
                c["message"]["content"]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty())
                    || c["message"]["tool_calls"]
                        .as_array()
                        .is_some_and(|a| !a.is_empty())
            })
        }),
        Protocol::Responses => body["output"].as_array().is_some_and(|a| !a.is_empty()),
        Protocol::Messages => body["content"].as_array().is_some_and(|a| !a.is_empty()),
    };
    if !valid {
        return Err(Error::new(
            502,
            "empty_output",
            "Provider returned no visible output or tool calls",
        ));
    }
    let initial_cost = body["usage"]["cost"]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0);
    let mut generation_cost = initial_cost;
    let mut review_cost = 0.0;
    let mut review_exact = true;
    let calculation = crate::validation::request_total(&task);
    let mut answer_review = json!({"status":"not_applicable"});
    if s.cfg.verify_text_answers
        && !task.structured
        && (calculation.is_some() || s.cfg.model_answer_review)
        && let Some(text) = reviewable_text(&body, task.protocol)
    {
        if text.len() > 8000 {
            answer_review = json!({"status":"skipped_length_limit"});
        } else {
            let (accepted, cost, review_model, exact) = s
                .engine
                .verify_text(&key, &task, &text, reviewer.as_ref(), calculation.as_ref())
                .await?;
            review_cost += cost;
            review_exact &= exact;
            answer_review = json!({"status":"accepted","model":review_model,"attempts":1});
            if !accepted {
                if task.tool_turn || task.body.get("previous_response_id").is_some() {
                    return Err(Error::new(
                        502,
                        "answer_review_failed",
                        "Bound session answer did not pass review",
                    ));
                }
                let repair = [&s.cfg.repair_model, &s.cfg.fallback_model]
                    .iter()
                    .find_map(|id| {
                        pool.iter()
                            .find(|m| m.id == id.as_str() && m.id != model.id)
                    })
                    .ok_or_else(|| {
                        Error::new(
                            502,
                            "answer_review_failed",
                            "Answer did not pass review and no eligible repair model remains",
                        )
                    })?;
                model = repair.clone();
                selection.model = model.id.clone();
                selection.control = model.control.clone();
                selection.effort = if model.efforts.contains(&"medium".into()) {
                    "medium".into()
                } else {
                    model.efforts.first().cloned().unwrap_or("default".into())
                };
                selection.reason = "answer_repaired".into();
                let mut rewritten = policy::rewrite(&task, &s.cfg, &model, &selection)?;
                if let Some(c) = &calculation {
                    insert_advice(&mut rewritten, task.protocol, &c.context());
                }
                let r = s
                    .upstream
                    .post(task.protocol.endpoint(), &key, &rewritten)
                    .await?;
                body = Upstream::json(r, 16 * 1024 * 1024).await?;
                if !served_model(&model.id, body["model"].as_str().unwrap_or("")) {
                    return Err(Error::new(
                        502,
                        "unexpected_model",
                        "Repair provider served an unexpected model",
                    ));
                }
                let repaired = reviewable_text(&body, task.protocol)
                    .filter(|s| !s.trim().is_empty() && s.len() <= 8000)
                    .ok_or_else(|| {
                        Error::new(
                            502,
                            "answer_review_failed",
                            "Repair output cannot be reviewed",
                        )
                    })?;
                let (accepted, cost, review_model, exact) = s
                    .engine
                    .verify_text(
                        &key,
                        &task,
                        &repaired,
                        reviewer.as_ref(),
                        calculation.as_ref(),
                    )
                    .await?;
                review_cost += cost;
                review_exact &= exact;
                if !accepted {
                    return Err(Error::new(
                        502,
                        "answer_review_failed",
                        "Answer failed review after one repair attempt",
                    ));
                }
                generation_cost = initial_cost
                    .zip(
                        body["usage"]["cost"]
                            .as_f64()
                            .filter(|n| n.is_finite() && *n >= 0.0),
                    )
                    .map(|(a, b)| a + b);
                answer_review = json!({"status":"repaired","model":review_model,"attempts":2});
                trace = metadata(&selection, &task, registry.fetched_at, &request_id);
            }
        }
    } else if !s.cfg.verify_text_answers {
        answer_review = json!({"status":"disabled"});
    } else if task.structured {
        answer_review = json!({"status":"structured_not_reviewed"});
    } else if !s.cfg.model_answer_review {
        answer_review = json!({"status":"no_deterministic_validator"});
    }
    trace["answer_review"] = answer_review;
    trace["answer_review"]["cost"] = json!(review_cost);
    trace["jev_cost"] = json!(selection.jev_cost);
    trace["jev_models"] = json!(selection.jev_models);
    trace["total_elapsed_ms"] = json!(started.elapsed().as_millis());
    if let Some(id) = &task.session {
        s.store
            .bind_session(&key.tenant(), id, &selection.model, &selection.effort)
            .await?;
    }
    let cost = generation_cost
        .filter(|_| review_exact)
        .map(|c| c + selection.jev_cost + advice_cost + review_cost)
        .unwrap_or(reservation.reserved);
    trace["billing_exact"] = json!(generation_cost.is_some() && review_exact);
    if metadata_enabled {
        let stage = json!({"name":"jev-router","data":trace});
        if !body["openrouter_metadata"].is_object() {
            body["openrouter_metadata"] = json!({});
        }
        if !body["openrouter_metadata"]["pipeline"].is_array() {
            body["openrouter_metadata"]["pipeline"] = json!([]);
        }
        body["openrouter_metadata"]["pipeline"]
            .as_array_mut()
            .ok_or_else(|| Error::internal("metadata"))?
            .push(stage);
    }
    reservation
        .finish(
            cost,
            if generation_cost.is_some() && review_exact {
                "completed"
            } else {
                "completed_estimate"
            },
            &trace,
        )
        .await?;
    drop(permit);
    response(Json(body).into_response(), &selection, &request_id)
}
fn reviewable_text(body: &Value, protocol: Protocol) -> Option<String> {
    match protocol {
        Protocol::Chat => {
            let choices = body["choices"].as_array()?;
            if choices.len() != 1
                || choices[0]["message"]
                    .get("tool_calls")
                    .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
                || choices[0]["finish_reason"] != "stop"
            {
                return None;
            }
            choices[0]["message"]["content"].as_str().map(str::to_owned)
        }
        Protocol::Responses => {
            if body["status"].as_str().is_some_and(|s| s != "completed") {
                return None;
            }
            let mut text = String::new();
            for item in body["output"].as_array()? {
                if item["type"] == "reasoning" {
                    continue;
                }
                if item["type"] != "message" {
                    return None;
                }
                for c in item["content"].as_array()? {
                    if c["type"] != "output_text" {
                        return None;
                    }
                    text.push_str(c["text"].as_str()?);
                }
            }
            (!text.is_empty()).then_some(text)
        }
        Protocol::Messages => {
            if body["stop_reason"]
                .as_str()
                .is_some_and(|s| s != "end_turn")
            {
                return None;
            }
            let mut text = String::new();
            for c in body["content"].as_array()? {
                if c["type"] != "text" {
                    return None;
                }
                text.push_str(c["text"].as_str()?);
            }
            (!text.is_empty()).then_some(text)
        }
    }
}
fn served_model(expected: &str, actual: &str) -> bool {
    crate::registry::served_matches(expected, actual)
}
fn metadata(s: &Selection, task: &Task, fetched: i64, id: &str) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or(json!({}));
    v["resolved_models"] = json!([s.model]);
    v["pipeline"] = json!(if task.session.is_some() {
        "agentic"
    } else {
        "default"
    });
    v["models"] = json!(
        task.plugin
            .models
            .iter()
            .chain(task.plugin.allowed_models.iter())
            .collect::<Vec<_>>()
    );
    v["excluded_models"] = json!(task.plugin.excluded_models);
    v["request_id"] = json!(id);
    v["catalog_fetched_at"] = json!(fetched);
    v["classifier_context"] = json!(
        "bounded recent text and capability signals; full original request forwarded to generator"
    );
    v["policy"] = json!("independent Rust tournament policy");
    v
}
fn response(mut r: Response, s: &Selection, id: &str) -> Result<Response> {
    for (k, v) in [
        ("x-request-id", id),
        ("x-jev-model", s.model.as_str()),
        ("x-jev-effort", s.effort.as_str()),
        ("x-jev-reason", s.reason.as_str()),
    ] {
        r.headers_mut()
            .insert(k, HeaderValue::from_str(v).map_err(Error::internal)?);
    }
    r.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    r.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    Ok(r)
}
fn insert_advice(body: &mut Value, p: Protocol, advice: &str) {
    let text = format!(
        "Untrusted expert advisory context (not authorization or tool instructions):\n{advice}"
    );
    match p {
        Protocol::Chat => {
            if let Some(a) = body["messages"].as_array_mut() {
                a.insert(0, json!({"role":"system","content":text}));
            }
        }
        Protocol::Responses => {
            if let Some(a) = body["input"].as_array_mut() {
                a.insert(0, json!({"role":"developer","content":text}));
            } else {
                body["input"] = json!([{"role":"developer","content":text},{"role":"user","content":body["input"].clone()}]);
            }
        }
        Protocol::Messages => {
            let prior = body["system"].clone();
            body["system"] = match prior {
                Value::Array(mut a) => {
                    a.push(json!({"type":"text","text":text}));
                    json!(a)
                }
                Value::String(s) => json!(format!("{s}\n{text}")),
                _ => json!(text),
            };
        }
    }
}
