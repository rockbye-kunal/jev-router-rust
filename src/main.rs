use clap::{Parser, Subcommand};
use jev_router::{
    config::Config,
    error::{Error, Result},
    registry::Registry,
    server::{AppState, app},
    store::Store,
    upstream::{Credential, Upstream},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
#[derive(Parser)]
#[command(
    version,
    about = "Rust Jev model/effort router using one OpenRouter key per caller"
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    env_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve,
    Health {
        #[arg(long, default_value = "http://127.0.0.1:8788")]
        base_url: String,
    },
    Catalog,
    CheckConfig,
    Doctor,
    Smoke {
        #[arg(long, default_value = "http://127.0.0.1:8788")]
        base_url: String,
    },
    Acceptance {
        #[arg(long, default_value = "http://127.0.0.1:8788")]
        base_url: String,
        #[arg(long, default_value_t = 5000)]
        delay_ms: u64,
    },
}
#[tokio::main]
async fn main() {
    init_logging();
    if let Err(e) = run().await {
        tracing::error!(code = e.code, message = %e.message, "fatal");
        eprintln!("{}: {}", e.code, e.message);
        std::process::exit(1);
    }
}
/// JSON logs on stderr (stdout stays reserved for command output).
/// Level is set with RUST_LOG, for example `RUST_LOG=jev_router=debug`; default `warn,jev_router=info`.
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,jev_router=info"));
    let _ = tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
async fn run() -> Result<()> {
    let cli = Cli::parse();
    if let Some(path) = &cli.env_file {
        dotenvy::from_path(path)
            .map_err(|_| Error::bad("Cannot load specified environment file"))?;
    } else {
        let _ = dotenvy::dotenv();
    }
    let cfg = Config::load(cli.config.as_deref())?;
    let key = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .map(Credential::new)
        .transpose()?;
    match cli.command {
        Command::Health { base_url } => {
            let r = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(Error::internal)?
                .get(format!("{}/readyz", base_url.trim_end_matches('/')))
                .send()
                .await
                .map_err(|_| Error::new(503, "unhealthy", "Health endpoint unreachable"))?;
            if !r.status().is_success() {
                return Err(Error::new(503, "unhealthy", "Readiness check failed"));
            }
            println!("ready");
        }
        Command::CheckConfig => {
            println!(
                "Configuration valid. Public mode requires caller OpenRouter bearer keys; server key fallback is loopback-only."
            );
        }
        Command::Doctor => {
            println!(
                "{}",
                json!({"openrouter_key_configured":key.is_some(),"bind":cfg.bind.to_string(),"catalog_present":cfg.catalog.exists(),"catalog_fresh":Registry::load(&cfg.catalog).is_ok_and(|r|r.fresh(cfg.catalog_max_age_secs)),"jev_model":cfg.jev_model})
            );
        }
        Command::Catalog => {
            let u = Upstream::new(&cfg)?;
            let r = Registry::parse(&u.get("/api/v1/models", None).await?)?;
            r.save(&cfg.catalog).await?;
            println!(
                "{}",
                json!({"models":r.models.len(),"excluded":r.excluded,"fetched_at":r.fetched_at})
            );
        }
        Command::Smoke { base_url } => {
            smoke(&base_url, key.as_ref(), false, 0).await?;
        }
        Command::Acceptance { base_url, delay_ms } => {
            smoke(&base_url, key.as_ref(), true, delay_ms).await?;
        }
        Command::Serve => {
            serve(cfg, key).await?;
        }
    }
    Ok(())
}
async fn serve(cfg: Config, key: Option<Credential>) -> Result<()> {
    if let Some(parent) = cfg.database.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(Error::internal)?;
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cfg.database.with_extension("lock"))
        .map_err(Error::internal)?;
    fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| {
        Error::new(
            503,
            "database_locked",
            "Another server owns this database; use one writer instance per database",
        )
    })?;
    let upstream = Upstream::new(&cfg)?;
    let registry = match Registry::load(&cfg.catalog) {
        Ok(r) if r.fresh(cfg.catalog_max_age_secs) => r,
        _ => {
            let r = Registry::parse(&upstream.get("/api/v1/models", None).await?)?;
            r.save(&cfg.catalog).await?;
            r
        }
    };
    let store = Arc::new(Store::open(&cfg.database).await?);
    store.recover().await?;
    store.prune().await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cfg.database, std::fs::Permissions::from_mode(0o600))
            .map_err(Error::internal)?;
    }
    let state = AppState::new(cfg.clone(), registry, store.clone(), key)?;
    let refresh_state = state.clone();
    let background = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(
            (refresh_state.cfg.catalog_max_age_secs / 2).max(30),
        ));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(e) = refresh_state.refresh().await {
                tracing::warn!(code = e.code, "catalog_refresh_failed");
            }
            let _ = refresh_state.store.prune().await;
        }
    });
    let listener = tokio::net::TcpListener::bind(cfg.bind)
        .await
        .map_err(|_| Error::new(503, "bind_failed", "Cannot bind server address"))?;
    println!(
        "{}",
        json!({"event":"listening","address":listener.local_addr().map_err(Error::internal)?.to_string(),"authentication":if cfg.bind.ip().is_loopback(){"OpenRouter bearer key or configured local key"}else{"caller OpenRouter bearer key required"}})
    );
    axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await
    .map_err(Error::internal)?;
    background.abort();
    store.pool.close().await;
    drop(lock);
    Ok(())
}
async fn shutdown() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
async fn smoke(base: &str, key: Option<&Credential>, automatic: bool, delay_ms: u64) -> Result<()> {
    let mut results = vec![];
    let outcome = smoke_cases(base, key, automatic, &mut results, delay_ms).await;
    let file = if automatic {
        "AUTO-ROUTING-RESULTS.json"
    } else {
        "LIVE-RESULTS.json"
    };
    let report = json!({"status":if outcome.is_ok(){"passed"}else{"failed"},"automatic_no_model_preferences":automatic,"case_delay_ms":delay_ms,"cases":results,"error":outcome.as_ref().err().map(|e|e.code),"error_message":outcome.as_ref().err().map(|e|&e.message)});
    tokio::fs::write(
        file,
        serde_json::to_vec_pretty(&report).map_err(Error::internal)?,
    )
    .await
    .map_err(Error::internal)?;
    outcome?;
    println!("Live checks passed. Results saved to {file}.");
    Ok(())
}
async fn smoke_cases(
    base: &str,
    key: Option<&Credential>,
    automatic: bool,
    results: &mut Vec<Value>,
    delay_ms: u64,
) -> Result<()> {
    let parsed = url::Url::parse(base).map_err(|_| Error::bad("Invalid smoke base URL"))?;
    if parsed.scheme() != "https"
        && !(parsed.scheme() == "http"
            && parsed.host_str().is_some_and(|h| {
                h.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
            }))
    {
        return Err(Error::bad("Smoke base must be HTTPS or literal loopback"));
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(Error::internal)?;
    let mut cases = vec![
        (
            "/v1/chat/completions",
            json!({"model":"typesafe/jev-router","plugins":[{"id":"jev-router","models":["google/gemini-3-flash-preview","anthropic/claude-sonnet-4.6"],"cost_tier":"low"}],"messages":[{"role":"user","content":"Reply with the single word PONG."}],"max_tokens":512}),
        ),
        (
            "/v1/responses",
            json!({"model":"typesafe/jev-router","plugins":[{"id":"jev-router","models":["google/gemini-3-flash-preview"],"cost_tier":"low"}],"input":"Reply with PONG.","max_output_tokens":512}),
        ),
        (
            "/v1/messages",
            json!({"model":"typesafe/jev-router","plugins":[{"id":"jev-router","models":["anthropic/claude-sonnet-4.6"],"cost_tier":"low"}],"messages":[{"role":"user","content":"Reply with PONG."}],"max_tokens":512}),
        ),
        (
            "/v1/chat/completions",
            json!({"model":"typesafe/jev-router","plugins":[{"id":"jev-router","models":["anthropic/claude-sonnet-4.6"]}],"messages":[{"role":"user","content":"Call get_weather for Pune."}],"tools":[{"type":"function","function":{"name":"get_weather","description":"Get weather","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"],"additionalProperties":false}}}],"tool_choice":{"type":"function","function":{"name":"get_weather"}},"max_tokens":1024}),
        ),
        (
            "/v1/chat/completions",
            json!({"model":"typesafe/jev-router","plugins":[{"id":"jev-router","models":["google/gemini-3-flash-preview"]}],"messages":[{"role":"user","content":"Return ok true."}],"response_format":{"type":"json_schema","json_schema":{"name":"check","strict":true,"schema":{"type":"object","properties":{"ok":{"type":"boolean","const":true}},"required":["ok"],"additionalProperties":false}}},"max_tokens":1024}),
        ),
    ];
    if automatic {
        cases.push(("/v1/chat/completions",json!({"model":"auto","messages":[{"role":"user","content":"A shop sells 3 notebooks at ₹80 each and 2 pens at ₹15 each. What is the total? Give the calculation and final amount in one short sentence."}],"max_tokens":1024})));
    }
    for (path, mut payload) in cases {
        if !results.is_empty() && delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        if automatic {
            payload
                .as_object_mut()
                .ok_or_else(|| Error::internal("payload"))?
                .remove("plugins");
        }
        let mut r = client
            .post(format!("{}{path}", base.trim_end_matches('/')))
            .header("X-OpenRouter-Metadata", "enabled")
            .json(&payload);
        if let Some(k) = key {
            r = r.bearer_auth(k.expose());
        }
        let response = r
            .send()
            .await
            .map_err(|_| Error::new(503, "smoke_unreachable", "Smoke target unreachable"))?;
        let status = response.status().as_u16();
        let body: Value = response
            .json()
            .await
            .map_err(|_| Error::new(502, "smoke_invalid_json", "Smoke returned invalid JSON"))?;
        if status != 200 {
            return Err(Error::new(
                status,
                "live_smoke_failed",
                format!(
                    "{path} failed: {}",
                    body["error"]["code"].as_str().unwrap_or("upstream_error")
                ),
            ));
        }
        let valid = if payload.get("tools").is_some() {
            let call = &body["choices"][0]["message"]["tool_calls"][0]["function"];
            call["name"] == "get_weather"
                && call["arguments"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .is_some_and(|v| v["city"] == "Pune")
        } else if payload.get("response_format").is_some() {
            body["choices"][0]["message"]["content"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .is_some_and(|v| v["ok"] == true && v.as_object().is_some_and(|o| o.len() == 1))
        } else if payload["messages"][0]["content"]
            .as_str()
            .is_some_and(|s| s.starts_with("A shop sells"))
        {
            jev_router::validation::MoneyTotal::from_request(
                payload["messages"][0]["content"].as_str().unwrap(),
            )
            .is_some_and(|c| c.accepts(&visible_text(path, &body)))
        } else {
            visible_text(path, &body).contains("PONG")
        };
        if !valid {
            return Err(Error::new(
                502,
                "live_answer_failed",
                "Expected answer, tool call or structured output absent",
            ));
        }
        let result = json!({"path":path,"model":body["model"],"answer":visible_text(path,&body),"tool_calls":body["choices"][0]["message"]["tool_calls"],"usage":body["usage"],"routing":body["openrouter_metadata"]});
        println!("{}", json!({"event":"live_case_passed","result":result}));
        results.push(result);
    }
    for (path, mut payload) in [
        (
            "/v1/chat/completions",
            json!({"model":"typesafe/jev-router","plugins":[{"id":"jev-router","models":["google/gemini-3-flash-preview"],"cost_tier":"low"}],"messages":[{"role":"user","content":"Reply with PONG."}],"max_tokens":1024,"stream":true,"stream_options":{"include_usage":true}}),
        ),
        (
            "/v1/responses",
            json!({"model":"auto","plugins":[{"id":"jev-router","models":["google/gemini-3-flash-preview"]}],"input":"Reply with PONG.","max_output_tokens":1024,"stream":true}),
        ),
        (
            "/v1/messages",
            json!({"model":"auto","plugins":[{"id":"jev-router","models":["anthropic/claude-sonnet-4.6"]}],"messages":[{"role":"user","content":"Reply with PONG."}],"max_tokens":1024,"stream":true}),
        ),
    ] {
        if delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        if automatic {
            payload.as_object_mut().unwrap().remove("plugins");
        }
        let mut request = client
            .post(format!("{}{path}", base.trim_end_matches('/')))
            .json(&payload);
        if let Some(k) = key {
            request = request.bearer_auth(k.expose());
        }
        let response = request.send().await.map_err(Error::internal)?;
        if !response.status().is_success() {
            return Err(Error::new(
                502,
                "live_stream_failed",
                "Stream request failed",
            ));
        }
        let model = response
            .headers()
            .get("x-jev-model")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let effort = response
            .headers()
            .get("x-jev-effort")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let wire = response.bytes().await.map_err(Error::internal)?;
        let mut inspector = jev_router::stream::Inspector::with_model(model.clone());
        if !inspector.feed(&wire)
            || inspector.failed
            || !inspector.done
            || !stream_text(path, &wire).contains("PONG")
        {
            return Err(Error::new(
                502,
                "live_stream_failed",
                "Stream output or completion marker absent",
            ));
        }
        results.push(json!({"path":path,"stream":true,"model":model,"effort":effort,"completion_verified":true,"reported_cost":inspector.cost}));
    }
    Ok(())
}
fn stream_text(path: &str, wire: &[u8]) -> String {
    String::from_utf8_lossy(wire)
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter_map(|v| match path {
            "/v1/responses" if v["type"] == "response.output_text.delta" => {
                v["delta"].as_str().map(str::to_owned)
            }
            "/v1/messages" if v["type"] == "content_block_delta" => {
                v["delta"]["text"].as_str().map(str::to_owned)
            }
            "/v1/chat/completions" => v["choices"][0]["delta"]["content"]
                .as_str()
                .map(str::to_owned),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}
fn visible_text(path: &str, body: &Value) -> String {
    match path {
        "/v1/responses" => body["output"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|i| i["content"].as_array().into_iter().flatten())
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        "/v1/messages" => body["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_owned(),
    }
}
