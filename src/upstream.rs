use crate::{
    config::{Config, now},
    error::{Error, Result},
};
use futures_util::StreamExt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[derive(Clone)]
pub struct Credential(Arc<zeroize::Zeroizing<String>>);
impl Credential {
    pub fn new(s: String) -> Result<Self> {
        if s.is_empty() || s.len() > 4096 || s.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(Error::new(
                401,
                "invalid_key",
                "Invalid OpenRouter bearer key",
            ));
        }
        Ok(Self(Arc::new(zeroize::Zeroizing::new(s))))
    }
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
    pub fn tenant(&self) -> String {
        format!("{:x}", Sha256::digest(self.expose().as_bytes()))
    }
}
/// Upper bound on a provider-supplied Retry-After so one header cannot disable a
/// model for a tenant for hours.
const MAX_RATE_LIMIT_COOLDOWN_SECS: u64 = 900;
#[derive(Clone)]
pub struct Upstream {
    pub client: reqwest::Client,
    pub base: String,
    failures: Arc<Mutex<HashMap<String, (u32, i64)>>>,
    cooldowns: Arc<Mutex<HashMap<String, i64>>>,
    cooldown: u64,
}
impl Upstream {
    pub fn new(c: &Config) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(c.request_timeout_secs))
                .build()
                .map_err(Error::internal)?,
            base: c.upstream.trim_end_matches('/').into(),
            failures: Default::default(),
            cooldowns: Default::default(),
            cooldown: c.circuit_cooldown_secs,
        })
    }
    fn check(&self, id: &str) -> Result<()> {
        if let Some(until) = self
            .cooldowns
            .lock()
            .map_err(Error::internal)?
            .get(id)
            .copied()
            && until > now()
        {
            let mut error = Error::new(
                503,
                "circuit_open",
                "Model is cooling down after an upstream rate limit",
            );
            error.retry_after_secs = Some((until - now()).max(1) as u64);
            return Err(error);
        }
        let map = self.failures.lock().map_err(Error::internal)?;
        if map
            .get(id)
            .is_some_and(|(n, t)| *n >= 3 && now() - *t < self.cooldown as i64)
        {
            return Err(Error::new(
                503,
                "circuit_open",
                "Upstream circuit temporarily open",
            ));
        }
        Ok(())
    }
    pub fn model_available(&self, key: &Credential, path: &str, model: &str) -> bool {
        self.check(&format!("{}:{path}:{model}", key.tenant()))
            .is_ok()
    }
    fn cool_down(&self, id: &str, seconds: u64) {
        if let Ok(mut map) = self.cooldowns.lock() {
            map.retain(|_, until| *until > now());
            // Bound the cache without invalidating existing unexpired cooldowns.
            if map.len() < 8192 || map.contains_key(id) {
                map.insert(
                    id.into(),
                    now().saturating_add(seconds.min(MAX_RATE_LIMIT_COOLDOWN_SECS) as i64),
                );
            }
        }
    }
    fn outcome(&self, id: &str, success: bool) {
        if let Ok(mut m) = self.failures.lock() {
            if m.len() > 8192 {
                m.retain(|_, (_, t)| now() - *t < 120);
            }
            if success {
                m.remove(id);
            } else {
                let v = m.entry(id.into()).or_insert((0, now()));
                v.0 += 1;
                v.1 = now();
            }
        }
    }
    pub async fn get(&self, path: &str, key: Option<&Credential>) -> Result<Value> {
        let mut r = self.client.get(format!("{}{path}", self.base));
        if let Some(k) = key {
            r = r.bearer_auth(k.expose());
        }
        let response = r
            .send()
            .await
            .map_err(|_| Error::new(502, "upstream_unavailable", "Upstream request failed"))?;
        if !response.status().is_success() {
            return Err(Self::http_error(response.status().as_u16()));
        }
        Self::json(response, 16 * 1024 * 1024).await
    }
    pub async fn verify(&self, key: &Credential) -> Result<()> {
        let v = self.get("/api/v1/key", Some(key)).await?;
        if !v["data"].is_object()
            || v["data"]["is_management_key"] == true
            || v["data"]["is_provisioning_key"] == true
        {
            return Err(Error::new(
                401,
                "invalid_key",
                "Use an inference OpenRouter API key",
            ));
        }
        Ok(())
    }
    pub async fn post(
        &self,
        path: &str,
        key: &Credential,
        body: &Value,
    ) -> Result<reqwest::Response> {
        let id = format!(
            "{}:{path}:{}",
            key.tenant(),
            body["model"].as_str().unwrap_or("unknown")
        );
        self.check(&id)?;
        let result = self
            .client
            .post(format!("{}{path}", self.base))
            .header("anthropic-version", "2023-06-01")
            .bearer_auth(key.expose())
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await;
        let response = match result {
            Ok(r) => r,
            Err(_) => {
                self.outcome(&id, false);
                return Err(Error::new(
                    504,
                    "upstream_timeout",
                    "Upstream timed out or disconnected; request not retried",
                ));
            }
        };
        let status = response.status().as_u16();
        if !response.status().is_success() {
            if status >= 500 {
                self.outcome(&id, false);
            }
            if status == 429 {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| {
                        v.parse::<u64>().ok().or_else(|| {
                            httpdate::parse_http_date(v).ok().map(|date| {
                                date.duration_since(std::time::SystemTime::now())
                                    .unwrap_or_default()
                                    .as_secs()
                                    .max(1)
                            })
                        })
                    });
                // Inspect a bounded error envelope; never forward raw provider messages.
                let envelope = Self::json(response, 128 * 1024)
                    .await
                    .unwrap_or(Value::Null);
                let metadata = &envelope["error"]["metadata"];
                let account_limit = metadata["limit_source"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("openrouter_"))
                    || matches!(
                        metadata["scope"].as_str(),
                        Some("account" | "api_key" | "workspace" | "key_limit")
                    );
                let scope = if account_limit {
                    "account"
                } else if metadata["provider_name"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                {
                    "provider"
                } else {
                    "unknown"
                };
                if !account_limit {
                    self.cool_down(&id, retry_after.unwrap_or(self.cooldown).max(self.cooldown));
                }
                let mut error = Error::new(
                    429,
                    if path == "/api/alpha/decisions" {
                        "routing_rate_limit"
                    } else {
                        "generation_rate_limit"
                    },
                    "OpenRouter rejected the request with a rate limit",
                );
                error.rate_limit_scope = Some(scope);
                error.retry_after_secs = retry_after;
                return Err(error);
            }
            return Err(Self::http_error(status));
        }
        self.outcome(&id, true);
        Ok(response)
    }
    pub async fn json(response: reqwest::Response, max: usize) -> Result<Value> {
        let mut s = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(c) = s.next().await {
            let c = c.map_err(|_| {
                Error::new(
                    502,
                    "upstream_disconnected",
                    "Upstream response interrupted",
                )
            })?;
            if bytes.len() + c.len() > max {
                return Err(Error::new(
                    502,
                    "upstream_body_limit",
                    "Upstream response too large",
                ));
            }
            bytes.extend_from_slice(&c);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| Error::new(502, "invalid_upstream", "Malformed upstream JSON"))
    }
    fn http_error(status: u16) -> Error {
        match status {
            401 | 403 => Error::new(
                401,
                "upstream_auth",
                "OpenRouter rejected this key or its permissions",
            ),
            402 => Error::new(
                402,
                "upstream_credits",
                "OpenRouter credit or account budget limit reached",
            ),
            404 => Error::new(
                404,
                "upstream_model_unavailable",
                "No accessible upstream model endpoint satisfies the request",
            ),
            429 => Error::new(429, "upstream_rate_limit", "OpenRouter rate limit reached"),
            503 | 529 => Error::new(
                503,
                "upstream_overloaded",
                "OpenRouter provider temporarily unavailable",
            ),
            _ => Error::new(
                502,
                "upstream_error",
                "OpenRouter rejected or failed the request",
            ),
        }
    }
}
