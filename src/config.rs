use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub bind: SocketAddr,
    pub upstream: String,
    pub database: PathBuf,
    pub catalog: PathBuf,
    pub jev_model: String,
    pub catalog_max_age_secs: u64,
    pub request_timeout_secs: u64,
    pub max_body_bytes: usize,
    pub max_output_tokens: u64,
    pub max_concurrent: usize,
    pub tenant_concurrent: i64,
    pub requests_per_minute: i64,
    pub daily_estimated_budget_usd: f64,
    pub max_request_estimated_usd: f64,
    pub confidence_threshold: f64,
    pub margin_threshold: f64,
    pub fallback_model: String,
    pub repair_model: String,
    pub output_recovery_model: String,
    pub allow_provider_fallbacks: bool,
    pub max_generation_failovers: usize,
    pub verify_text_answers: bool,
    pub model_answer_review: bool,
    pub hard_allowed_models: Vec<String>,
    pub hard_excluded_models: Vec<String>,
    pub max_models: usize,
    pub group_size: usize,
    pub judge_parallelism: usize,
    pub data_collection: String,
    pub zdr: bool,
    pub provider_only: Vec<String>,
    pub advisor_model: Option<String>,
    pub advisor_output_tokens: u64,
    pub session_ttl_secs: i64,
    pub circuit_cooldown_secs: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 8788),
            upstream: "https://openrouter.ai".into(),
            database: "data/router.db".into(),
            catalog: "data/catalog.json".into(),
            jev_model: "typesafe/jev-1.13".into(),
            catalog_max_age_secs: 3600,
            request_timeout_secs: 240,
            max_body_bytes: 16 * 1024 * 1024,
            max_output_tokens: 32768,
            max_concurrent: 32,
            tenant_concurrent: 4,
            requests_per_minute: 60,
            daily_estimated_budget_usd: 10.0,
            max_request_estimated_usd: 1.0,
            confidence_threshold: 0.75,
            margin_threshold: 0.15,
            fallback_model: "anthropic/claude-sonnet-4.6".into(),
            repair_model: "google/gemini-3-flash-preview".into(),
            output_recovery_model: "openai/gpt-5.4".into(),
            allow_provider_fallbacks: true,
            max_generation_failovers: 2,
            verify_text_answers: true,
            model_answer_review: false,
            hard_allowed_models: vec![],
            hard_excluded_models: vec![],
            max_models: 1024,
            group_size: 48,
            judge_parallelism: 8,
            data_collection: "deny".into(),
            zdr: false,
            provider_only: vec![],
            advisor_model: None,
            advisor_output_tokens: 1024,
            session_ttl_secs: 86400,
            circuit_cooldown_secs: 30,
        }
    }
}
impl Config {
    pub fn load(path: Option<&std::path::Path>) -> Result<Self> {
        let mut c: Self = if let Some(p) = path {
            serde_json::from_slice(&std::fs::read(p).map_err(Error::internal)?)
                .map_err(|_| Error::bad("Invalid configuration JSON"))?
        } else {
            Self::default()
        };
        if let Ok(bind) = std::env::var("BIND") {
            c.bind = bind.parse().map_err(|_| Error::bad("Invalid BIND"))?;
        }
        c.validate()?;
        Ok(c)
    }
    pub fn validate(&self) -> Result<()> {
        if self.max_generation_failovers > 4 {
            return Err(Error::bad(
                "At most four generation rejection failovers are allowed",
            ));
        }
        let url =
            url::Url::parse(&self.upstream).map_err(|_| Error::bad("Invalid upstream URL"))?;
        let loopback = url
            .host_str()
            .and_then(|h| h.parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            return Err(Error::bad(
                "Upstream must use HTTPS or literal loopback HTTP",
            ));
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(Error::bad("Upstream must be an origin URL"));
        }
        if !self.jev_model.starts_with("typesafe/jev-") && self.jev_model != "~typesafe/jev-latest"
        {
            return Err(Error::bad("JEV model must be an OpenRouter Jev ID"));
        }
        if !(2..=128).contains(&self.group_size)
            || !(2..=2048).contains(&self.max_models)
            || self.judge_parallelism == 0
            || self.max_concurrent == 0
            || self.tenant_concurrent < 1
            || self.requests_per_minute < 1
            || self.request_timeout_secs < 1
            || self.catalog_max_age_secs < 1
            || self.max_body_bytes < 1024
            || self.max_output_tokens < 32
            || self.max_output_tokens > 131072
        {
            return Err(Error::bad("Invalid service limits"));
        }
        if self.session_ttl_secs < 1
            || self.session_ttl_secs > 86400 * 7
            || self.advisor_output_tokens < 32
            || self.advisor_output_tokens > 32768
        {
            return Err(Error::bad("Invalid session/advisor limits"));
        }
        for p in self
            .hard_allowed_models
            .iter()
            .chain(self.hard_excluded_models.iter())
        {
            globset::Glob::new(p).map_err(|_| Error::bad("Invalid operator model pattern"))?;
        }
        for v in [
            self.daily_estimated_budget_usd,
            self.max_request_estimated_usd,
        ] {
            if !v.is_finite() || v <= 0.0 {
                return Err(Error::bad("Budget must be finite and positive"));
            }
        }
        for v in [self.confidence_threshold, self.margin_threshold] {
            if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                return Err(Error::bad("Invalid confidence threshold"));
            }
        }
        if !["allow", "deny"].contains(&self.data_collection.as_str()) {
            return Err(Error::bad("Invalid data_collection"));
        }
        Ok(())
    }
}
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
