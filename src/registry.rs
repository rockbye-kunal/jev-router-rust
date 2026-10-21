use crate::{
    config::now,
    error::{Error, Result},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub fn served_matches(expected: &str, actual: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    if expected == actual {
        return true;
    }
    let Some(tail) = actual.strip_prefix(&format!("{expected}-")) else {
        return false;
    };
    let digits = tail.bytes().filter(u8::is_ascii_digit).collect::<Vec<_>>();
    digits.len() == 8
        && (tail.len() == 8 && tail.bytes().all(|c| c.is_ascii_digit())
            || tail.len() == 10
                && tail.as_bytes()[4] == b'-'
                && tail.as_bytes()[7] == b'-'
                && tail.bytes().all(|c| c.is_ascii_digit() || c == b'-'))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub description: String,
    pub context: u64,
    pub output: u64,
    pub input_price: f64,
    pub output_price: f64,
    pub request_price: f64,
    pub modalities: Vec<String>,
    pub parameters: Vec<String>,
    pub efforts: Vec<String>,
    pub control: String,
    pub canonical: String,
}
impl Model {
    pub fn estimate(&self, input: u64, output: u64) -> f64 {
        input as f64 * self.input_price + output as f64 * self.output_price + self.request_price
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Registry {
    pub fetched_at: i64,
    pub models: Vec<Model>,
    pub excluded: usize,
    pub aliases: std::collections::BTreeMap<String, String>,
}
fn rate(v: &Value) -> Option<f64> {
    let f = v.as_f64().or_else(|| v.as_str()?.parse().ok())?;
    (f.is_finite() && f >= 0.0).then_some(f)
}
fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
const EFFORTS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];
impl Registry {
    pub fn parse(body: &Value) -> Result<Self> {
        let rows = body["data"]
            .as_array()
            .ok_or_else(|| Error::new(502, "invalid_catalog", "Malformed model catalog"))?;
        let mut models = vec![];
        let mut excluded = 0;
        let mut aliases = std::collections::BTreeMap::new();
        let mut seen = std::collections::HashSet::new();
        for m in rows {
            let parsed = (|| -> Option<Model> {
                let id = m["id"].as_str()?.to_owned();
                if !seen.insert(id.clone())
                    || id.len() > 200
                    || !id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"/-_.:~".contains(&c))
                {
                    return None;
                }
                let canonical = m["canonical_slug"].as_str().unwrap_or(&id).to_owned();
                if id.starts_with('~') {
                    aliases.insert(id, canonical);
                    return None;
                }
                if id.ends_with(":batch")
                    || id.starts_with("openrouter/")
                    || id.contains("/auto")
                    || id == "typesafe/jev-router"
                    || m["expiration_date"].as_str().is_some()
                {
                    return None;
                }
                if !strings(&m["architecture"]["output_modalities"]).contains(&"text".into()) {
                    return None;
                }
                let context = m["context_length"].as_u64()?.min(
                    m["top_provider"]["context_length"]
                        .as_u64()
                        .unwrap_or(u64::MAX),
                );
                let output = m["top_provider"]["max_completion_tokens"].as_u64()?;
                if context == 0 || output == 0 {
                    return None;
                }
                let parameters = strings(&m["supported_parameters"]);
                if !parameters
                    .iter()
                    .any(|p| p == "max_tokens" || p == "max_completion_tokens")
                {
                    return None;
                }
                let mut input_price = rate(&m["pricing"]["prompt"])?;
                let mut output_price = rate(&m["pricing"]["completion"])?;
                let mut request_price = rate(&m["pricing"]["request"]).unwrap_or(0.0);
                if let Some(overrides) = m["pricing"]["overrides"].as_array() {
                    for p in overrides {
                        input_price = input_price.max(rate(&p["prompt"]).unwrap_or(input_price));
                        output_price =
                            output_price.max(rate(&p["completion"]).unwrap_or(output_price));
                        request_price =
                            request_price.max(rate(&p["request"]).unwrap_or(request_price));
                    }
                }
                let mandatory = m["reasoning"]["mandatory"].as_bool().unwrap_or(false);
                let supported = m["reasoning"].get("supported_efforts");
                let mut efforts = if supported.is_some_and(Value::is_null) {
                    EFFORTS.iter().map(|s| s.to_string()).collect()
                } else {
                    strings(&m["reasoning"]["supported_efforts"])
                        .into_iter()
                        .filter(|s| EFFORTS.contains(&s.as_str()))
                        .collect::<Vec<_>>()
                };
                if mandatory {
                    efforts.retain(|e| e != "none");
                }
                let control = if !efforts.is_empty() {
                    "effort"
                } else if m["reasoning"]["supports_max_tokens"].as_bool() == Some(true) {
                    efforts = vec!["low".into(), "medium".into(), "high".into()];
                    "budget"
                } else if parameters.contains(&"reasoning".into()) {
                    efforts = vec!["default".into()];
                    "default"
                } else {
                    efforts = vec!["none".into()];
                    "omit"
                };
                Some(Model {
                    id,
                    description: m["description"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(240)
                        .collect(),
                    context,
                    output,
                    input_price,
                    output_price,
                    request_price,
                    modalities: strings(&m["architecture"]["input_modalities"]),
                    parameters,
                    efforts,
                    control: control.into(),
                    canonical,
                })
            })();
            if let Some(m) = parsed {
                models.push(m);
            } else {
                excluded += 1;
            }
        }
        models.sort_by(|a, b| a.id.cmp(&b.id));
        if models.is_empty() {
            return Err(Error::new(
                503,
                "empty_catalog",
                "No usable text models in catalog",
            ));
        }
        Ok(Self {
            fetched_at: now(),
            models,
            excluded,
            aliases,
        })
    }
    pub fn fresh(&self, max_age: u64) -> bool {
        let age = now() - self.fetched_at;
        age >= -60 && age <= max_age as i64
    }
    pub fn load(path: &std::path::Path) -> Result<Self> {
        // A missing cache is expected on first start; do not log it as an internal error.
        let bytes = std::fs::read(path)
            .map_err(|_| Error::new(503, "catalog_missing", "Cached catalog unavailable"))?;
        serde_json::from_slice(&bytes)
            .map_err(|_| Error::new(503, "invalid_catalog", "Invalid cached catalog"))
    }
    pub async fn save(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(Error::internal)?;
        }
        let tmp = path.with_extension("json.tmp");
        tokio::fs::write(&tmp, serde_json::to_vec(self).map_err(Error::internal)?)
            .await
            .map_err(Error::internal)?;
        tokio::fs::rename(tmp, path).await.map_err(Error::internal)
    }
}
