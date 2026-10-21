use crate::{
    config::Config,
    error::{Error, Result},
    registry::{Model, Registry},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Chat,
    Responses,
    Messages,
}
impl Protocol {
    pub fn endpoint(self) -> &'static str {
        match self {
            Self::Chat => "/api/v1/chat/completions",
            Self::Responses => "/api/v1/responses",
            Self::Messages => "/api/v1/messages",
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    pub id: String,
    pub cost_tier: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub allowed_models: Vec<String>,
    #[serde(default)]
    pub excluded_models: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct Task {
    pub body: Value,
    pub protocol: Protocol,
    pub plugin: Plugin,
    pub stream: bool,
    pub max_output: u64,
    pub input_bound: u64,
    pub summary: Value,
    pub modalities: Vec<String>,
    pub tools: bool,
    pub structured: bool,
    pub session: Option<String>,
    pub tool_turn: bool,
    pub effort_hint: Option<String>,
    /// Operator tier per pooled model ID; filled after eligibility when tiers are enabled.
    pub tiers: std::collections::BTreeMap<String, String>,
}
fn scan_modalities(v: &Value, modalities: &mut Vec<String>) {
    match v {
        Value::Object(o) => {
            let modality = match o.get("type").and_then(Value::as_str) {
                Some("image_url" | "input_image" | "image") => Some("image"),
                Some("input_audio" | "audio_url" | "audio") => Some("audio"),
                Some("video_url" | "input_video") => Some("video"),
                Some("file" | "input_file" | "document") => Some("file"),
                _ => None,
            };
            if let Some(m) = modality
                && !modalities.iter().any(|v| v == m)
            {
                modalities.push(m.into());
            }
            for v in o.values() {
                scan_modalities(v, modalities);
            }
        }
        Value::Array(a) => {
            for v in a {
                scan_modalities(v, modalities);
            }
        }
        _ => {}
    }
}
fn excerpt(s: &str, text: &mut String) {
    if s.chars().count() <= 1500 {
        text.push_str(s);
    } else {
        text.extend(s.chars().take(750));
        text.push_str(" [excerpt omitted] ");
        let tail = s.chars().rev().take(750).collect::<Vec<_>>();
        text.extend(tail.into_iter().rev());
    }
    text.push('\n');
}
fn scan(v: &Value, modalities: &mut Vec<String>, text: &mut String) {
    match v {
        Value::Object(o) => {
            if let Some(t) = o.get("type").and_then(Value::as_str) {
                let m = match t {
                    "image_url" | "input_image" | "image" => Some("image"),
                    "input_audio" | "audio_url" | "audio" => Some("audio"),
                    "video_url" | "input_video" => Some("video"),
                    "file" | "input_file" | "document" => Some("file"),
                    _ => None,
                };
                if let Some(m) = m
                    && !modalities.contains(&m.into())
                {
                    modalities.push(m.into());
                }
            }
            for (k, v) in o {
                if ["text", "input_text"].contains(&k.as_str()) {
                    if let Some(s) = v.as_str() {
                        excerpt(s, text);
                    }
                } else if k == "content" {
                    if let Some(s) = v.as_str() {
                        excerpt(s, text);
                    } else {
                        scan(v, modalities, text);
                    }
                } else if ["messages", "input", "system", "instructions"].contains(&k.as_str()) {
                    scan(v, modalities, text);
                }
            }
        }
        Value::Array(a) => {
            for v in a.iter().rev().take(8).rev() {
                scan(v, modalities, text);
            }
        }
        Value::String(s) => {
            excerpt(s, text);
        }
        _ => {}
    }
}
pub fn parse(body: Value, protocol: Protocol, cfg: &Config) -> Result<Task> {
    let o = body
        .as_object()
        .ok_or_else(|| Error::bad("JSON object required"))?;
    if o.get("model").is_some_and(|v| !v.is_string()) {
        return Err(Error::bad("model must be text"));
    }
    if !["auto", "jev-router", "typesafe/jev-router"]
        .contains(&o.get("model").and_then(Value::as_str).unwrap_or("auto"))
    {
        return Err(Error::bad(
            "Use model typesafe/jev-router, jev-router, or auto",
        ));
    }
    if o.contains_key("models") {
        return Err(Error::bad(
            "Fallback model lists must be configured through router policy",
        ));
    }
    if o.get("tools").is_some_and(|v| !v.is_array()) {
        return Err(Error::bad("tools must be an array"));
    }
    let mut plugin = Plugin {
        id: "jev-router".into(),
        ..Default::default()
    };
    if let Some(p) = o.get("plugins") {
        let a = p
            .as_array()
            .ok_or_else(|| Error::bad("plugins must be an array"))?;
        if a.len() > 1 {
            return Err(Error::bad("Only the jev-router plugin is supported"));
        }
        if let Some(p) = a.first() {
            plugin = serde_json::from_value(p.clone())
                .map_err(|_| Error::bad("Invalid or unknown jev-router plugin field"))?;
            if plugin.id != "jev-router" {
                return Err(Error::bad("Cannot combine router plugins"));
            }
        }
    }
    if let Some(t) = &plugin.cost_tier
        && !["low", "medium", "high"].contains(&t.as_str())
    {
        return Err(Error::bad("cost_tier must be low, medium or high"));
    }
    for list in [
        &plugin.models,
        &plugin.allowed_models,
        &plugin.excluded_models,
    ] {
        if list.len() > 1024
            || list.iter().map(String::len).sum::<usize>() > 65536
            || list.iter().any(|p| p.is_empty() || p.len() > 1024)
        {
            return Err(Error::bad("Model pattern list exceeds limits"));
        }
        for p in list {
            pattern(p)?;
        }
    }
    if o.get("reasoning").is_some_and(|r| !r.is_object()) {
        return Err(Error::bad("reasoning must be an object"));
    }
    if body["reasoning"].get("max_tokens").is_some() || body["reasoning"]["mode"] == "pro" {
        return Err(Error::bad(
            "reasoning.max_tokens and pro mode are unsupported by this router",
        ));
    }
    if body["reasoning"]
        .get("effort")
        .is_some_and(|v| !v.is_string())
        || body.get("reasoning_effort").is_some_and(|v| !v.is_string())
    {
        return Err(Error::bad("Reasoning effort hints must be text"));
    }
    if let (Some(a), Some(b)) = (
        body["reasoning"]["effort"].as_str(),
        body["reasoning_effort"].as_str(),
    ) && a != b
    {
        return Err(Error::bad("Conflicting reasoning effort hints"));
    }
    let effort_hint = body["reasoning"]["effort"]
        .as_str()
        .or_else(|| body["reasoning_effort"].as_str())
        .map(str::to_owned);
    if effort_hint.as_ref().is_some_and(|e| {
        !["none", "minimal", "low", "medium", "high", "xhigh", "max"].contains(&e.as_str())
    }) {
        return Err(Error::bad("Invalid reasoning effort hint"));
    }
    let content = match protocol {
        Protocol::Responses => o.get("input"),
        _ => o.get("messages"),
    }
    .ok_or_else(|| Error::bad("Request input/messages required"))?;
    if !(content.is_string() || content.is_array())
        || content.as_array().is_some_and(Vec::is_empty)
        || content.as_str().is_some_and(|s| s.trim().is_empty())
    {
        return Err(Error::bad("Input/messages cannot be empty"));
    }
    if protocol != Protocol::Responses && !content.is_array() {
        return Err(Error::bad("messages must be an array"));
    }
    let max_output = o
        .get("max_output_tokens")
        .or_else(|| o.get("max_completion_tokens"))
        .or_else(|| o.get("max_tokens"))
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| Error::bad("Output limit must be a positive integer"))
        })
        .transpose()?
        .unwrap_or(4096);
    for key in ["max_tokens", "max_completion_tokens", "max_output_tokens"] {
        if let Some(v) = o.get(key)
            && v.as_u64() != Some(max_output)
        {
            return Err(Error::bad("Invalid or conflicting output limits"));
        }
    }
    if max_output < 32 || max_output > cfg.max_output_tokens {
        return Err(Error::bad("Output limit outside configured range"));
    }
    let stream = o
        .get("stream")
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| Error::bad("stream must be boolean"))
        })
        .transpose()?
        .unwrap_or(false);
    let session = o
        .get("session_id")
        .map(|v| {
            v.as_str()
                .ok_or_else(|| Error::bad("session_id must be text"))
        })
        .transpose()?
        .map(str::to_owned);
    if session
        .as_ref()
        .is_some_and(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
    {
        return Err(Error::bad("Invalid session_id"));
    }
    if o.get("previous_response_id").is_some() && session.is_none() {
        return Err(Error::bad(
            "previous_response_id requires a session_id to preserve the model binding",
        ));
    }
    let mut text = String::new();
    let mut modalities = vec!["text".into()];
    scan(&body, &mut modalities, &mut text);
    for key in ["messages", "input", "system"] {
        if let Some(v) = o.get(key) {
            scan_modalities(v, &mut modalities);
        }
    }
    let recent = text
        .chars()
        .rev()
        .take(5000)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    let summary = json!({"recent_text":recent,"input_modalities":modalities,"tools":o.get("tools").and_then(Value::as_array).map(Vec::len).unwrap_or(0),"max_output_tokens":max_output});
    let input_bound = serde_json::to_vec(&body).map_err(Error::internal)?.len() as u64 + 256;
    let tool_turn = content.as_array().is_some_and(|a| {
        if protocol == Protocol::Responses && a.iter().any(|v| v["type"] == "function_call_output")
        {
            return true;
        }
        a.last().is_some_and(|v| {
            v["role"] == "tool"
                || v["content"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v["type"] == "tool_result"))
        })
    });
    let tools = body["tools"].as_array().is_some_and(|a| !a.is_empty());
    let structured = body.get("response_format").is_some() || body["text"].get("format").is_some();
    Ok(Task {
        body,
        protocol,
        plugin,
        stream,
        max_output,
        input_bound,
        summary,
        modalities,
        tools,
        structured,
        session,
        tool_turn,
        effort_hint,
        tiers: Default::default(),
    })
}
fn pattern(p: &str) -> Result<globset::GlobMatcher> {
    globset::GlobBuilder::new(p)
        .literal_separator(false)
        .backslash_escape(false)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|_| Error::bad("Invalid model pattern"))
}
fn revision_matches(p: &str, id: &str) -> bool {
    id == p
        || id
            .strip_prefix(&format!("{p}-"))
            .is_some_and(|s| s.len() >= 8 && s.bytes().all(|c| c.is_ascii_digit() || c == b'-'))
}
pub fn matches(p: &str, m: &Model, registry: &Registry) -> bool {
    if let Some(base) = registry.aliases.get(p) {
        return revision_matches(base, &m.id) || revision_matches(base, &m.canonical);
    }
    if p.contains('*') || p.contains('?') || p.contains('[') {
        pattern(p).is_ok_and(|g| g.is_match(&m.id) || g.is_match(&m.canonical))
    } else {
        revision_matches(p, &m.id) || (!m.id.contains(':') && revision_matches(p, &m.canonical))
    }
}
pub fn eligible(task: &Task, registry: &Registry, cfg: &Config) -> Result<(Vec<Model>, bool)> {
    let schema = task.body["response_format"]["type"] == "json_schema"
        || task.body["text"]["format"]["type"] == "json_schema";
    let required_parameters = [
        "tool_choice",
        "parallel_tool_calls",
        "temperature",
        "top_p",
        "top_k",
        "stop",
        "seed",
        "logprobs",
        "top_logprobs",
        "logit_bias",
        "presence_penalty",
        "frequency_penalty",
    ]
    .into_iter()
    .filter(|k| task.body.get(*k).is_some_and(|v| !v.is_null()))
    .collect::<Vec<_>>();
    let hard = registry
        .models
        .iter()
        .filter(|m| {
            task.modalities.iter().all(|r| m.modalities.contains(r))
                && m.output >= task.max_output
                && m.context >= task.input_bound + task.max_output
                && (!task.tools || m.parameters.contains(&"tools".into()))
                && required_parameters
                    .iter()
                    .all(|p| m.parameters.iter().any(|v| v == p))
                && (!schema || m.parameters.contains(&"structured_outputs".into()))
                && (!task.structured
                    || m.parameters
                        .iter()
                        .any(|p| p == "response_format" || p == "structured_outputs"))
                && m.estimate(task.input_bound, task.max_output) <= cfg.max_request_estimated_usd
                && (cfg.hard_allowed_models.is_empty()
                    || cfg
                        .hard_allowed_models
                        .iter()
                        .any(|p| matches(p, m, registry)))
                && !cfg
                    .hard_excluded_models
                    .iter()
                    .chain(task.plugin.excluded_models.iter())
                    .any(|p| matches(p, m, registry))
        })
        .cloned()
        .collect::<Vec<_>>();
    let includes = task
        .plugin
        .models
        .iter()
        .chain(task.plugin.allowed_models.iter())
        .collect::<Vec<_>>();
    let selected = if includes.is_empty() {
        hard.clone()
    } else {
        hard.iter()
            .filter(|m| includes.iter().any(|p| matches(p, m, registry)))
            .cloned()
            .collect()
    };
    let ignored = !includes.is_empty() && selected.is_empty();
    let pool = if ignored { hard } else { selected };
    if pool.is_empty() {
        return Err(Error::new(
            404,
            "no_eligible_model",
            "No eligible model; widen allowed_models or remove excluded_models, within operator restrictions",
        ));
    }
    if pool.len() > cfg.max_models {
        return Err(Error::new(
            422,
            "pool_too_large",
            "Eligible pool exceeds configured maximum; narrow model preferences",
        ));
    }
    Ok((pool, ignored))
}
/// Model IDs never used to backfill a tier: slow agentic research, search-priced,
/// image-generation and premium "pro" variants. Name them in a tier list to use them.
const BACKFILL_SKIP: [&str; 5] = ["deep-research", "search", "-image", "-pro", "/o1"];
/// Build the capped routing pool from operator tiers so one Jev comparison covers a
/// cheap, a medium and a hard option. Order: cheap, medium, hard, then reserved models.
///
/// Each tier takes up to `pool_per_tier` models, one per pattern: the newest matching
/// ID by catalog `created` time (then ID). Variant IDs such as `:free` are skipped
/// unless the pattern names a variant. A hard pick must cost at least as much as the
/// most expensive medium pick, so a cheap "fast" variant cannot fill the hard tier.
/// A tier left empty for this request (stale patterns, media, tool or parameter
/// requirements) is backfilled with up to two unlisted eligible models at the 10th,
/// 50th or 85th cost percentile. The configured fallback, repair and recovery models
/// and a session-bound model are always kept so recovery and session rules work.
pub fn tiered_pool(
    task: &Task,
    eligible: &[Model],
    registry: &Registry,
    cfg: &Config,
    bound: Option<&str>,
) -> (Vec<Model>, std::collections::BTreeMap<String, String>) {
    let cost = |m: &Model| m.estimate(task.input_bound, task.max_output);
    let mut chosen: Vec<Model> = Vec::new();
    let mut tiers = std::collections::BTreeMap::<String, String>::new();
    let mut medium_max = 0.0_f64;
    for (tier, patterns) in cfg.pool_tiers.lists() {
        let floor = if tier == "hard" { medium_max } else { 0.0 };
        let mut taken = 0;
        for p in patterns {
            if taken == cfg.pool_per_tier {
                break;
            }
            let newest = eligible
                .iter()
                .filter(|m| {
                    !tiers.contains_key(&m.id)
                        && (p.contains(':') || !m.id.contains(':'))
                        && cost(m) >= floor
                        && matches(p, m, registry)
                })
                .max_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
            if let Some(m) = newest {
                if tier == "medium" {
                    medium_max = medium_max.max(cost(m));
                }
                tiers.insert(m.id.clone(), tier.to_owned());
                chosen.push(m.clone());
                taken += 1;
            }
        }
    }
    let mut rest = eligible
        .iter()
        .filter(|m| {
            !tiers.contains_key(&m.id)
                && !m.id.contains(':')
                && !BACKFILL_SKIP.iter().any(|s| m.id.contains(s))
        })
        .collect::<Vec<_>>();
    rest.sort_by(|a, b| cost(a).total_cmp(&cost(b)).then_with(|| a.id.cmp(&b.id)));
    for (tier, _) in cfg.pool_tiers.lists() {
        if tiers.values().any(|t| t.as_str() == tier) {
            continue;
        }
        let quantile = match tier {
            "cheap" => 0.10,
            "medium" => 0.50,
            _ => 0.85,
        };
        for _ in 0..cfg.pool_per_tier.min(2) {
            if rest.is_empty() {
                break;
            }
            let index = ((rest.len() - 1) as f64 * quantile).round() as usize;
            let m = rest.remove(index);
            tiers.insert(m.id.clone(), tier.to_owned());
            chosen.push(m.clone());
        }
    }
    for (id, label) in [
        (Some(cfg.fallback_model.as_str()), "fallback"),
        (Some(cfg.repair_model.as_str()), "repair"),
        (Some(cfg.output_recovery_model.as_str()), "recovery"),
        (bound, "session"),
    ] {
        let Some(id) = id else { continue };
        if tiers.contains_key(id) {
            continue;
        }
        if let Some(m) = eligible.iter().find(|m| m.id == id) {
            tiers.insert(m.id.clone(), label.to_owned());
            chosen.push(m.clone());
        }
    }
    if chosen.is_empty() {
        return (eligible.to_vec(), std::collections::BTreeMap::new());
    }
    (chosen, tiers)
}
/// Tier patterns that match no catalog model (stale IDs after catalog changes).
pub fn stale_tier_patterns(cfg: &Config, registry: &Registry) -> Vec<(&'static str, String)> {
    cfg.pool_tiers
        .lists()
        .iter()
        .flat_map(|(tier, patterns)| patterns.iter().map(move |p| (*tier, p.clone())))
        .filter(|(_, p)| !registry.models.iter().any(|m| matches(p, m, registry)))
        .collect()
}
/// Log stale tier patterns so a silently degraded pool is visible in production logs.
pub fn warn_stale_tier_patterns(cfg: &Config, registry: &Registry) {
    for (tier, pattern) in stale_tier_patterns(cfg, registry) {
        tracing::warn!(tier, pattern = %pattern, "tier_pattern_matches_no_catalog_model");
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Selection {
    pub model: String,
    pub effort: String,
    pub control: String,
    pub reason: String,
    pub confidence: f64,
    pub margin: f64,
    pub jev_models: Vec<String>,
    pub jev_cost: f64,
    pub list_fallback: Option<String>,
    pub candidates: Vec<Value>,
    pub advisor_model: Option<String>,
}
pub fn provider(task: &Task, cfg: &Config, m: &Model) -> Result<Value> {
    let mut p = task.body.get("provider").cloned().unwrap_or(json!({}));
    let o = p
        .as_object_mut()
        .ok_or_else(|| Error::bad("provider must be an object"))?;
    if o.contains_key("region") || o.contains_key("data_region") {
        return Err(Error::bad(
            "Regional routing is not supported by the Jev classifier",
        ));
    }
    let deny = cfg.data_collection == "deny"
        || o.get("data_collection").and_then(Value::as_str) == Some("deny");
    o.insert(
        "data_collection".into(),
        json!(if deny { "deny" } else { "allow" }),
    );
    let zdr = cfg.zdr || o.get("zdr").and_then(Value::as_bool).unwrap_or(false);
    o.insert("zdr".into(), json!(zdr));
    if !cfg.provider_only.is_empty() {
        let requested = o
            .get("only")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>());
        let only = cfg
            .provider_only
            .iter()
            .filter(|p| requested.as_ref().is_none_or(|r| r.contains(&p.as_str())))
            .cloned()
            .collect::<Vec<_>>();
        if only.is_empty() {
            return Err(Error::new(
                404,
                "no_eligible_provider",
                "Provider restrictions do not intersect",
            ));
        }
        o.insert("only".into(), json!(only));
    }
    let mut prices = json!({"prompt":m.input_price*1e6,"completion":m.output_price*1e6});
    if let Some(c) = o.get("max_price") {
        let c = c
            .as_object()
            .ok_or_else(|| Error::bad("max_price must be an object"))?;
        for k in ["prompt", "completion"] {
            if let Some(v) = c.get(k) {
                let n = v
                    .as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .ok_or_else(|| Error::bad("Invalid provider price ceiling"))?;
                prices[k] = json!(n.min(prices[k].as_f64().unwrap_or(0.0)));
            }
        }
    }
    o.insert("max_price".into(), prices);
    o.insert("require_parameters".into(), json!(true));
    if o.get("allow_fallbacks").is_some_and(|v| !v.is_boolean()) {
        return Err(Error::bad("provider.allow_fallbacks must be a boolean"));
    }
    let allow_fallbacks =
        cfg.allow_provider_fallbacks && o.get("allow_fallbacks") != Some(&json!(false));
    o.insert("allow_fallbacks".into(), json!(allow_fallbacks));
    Ok(p)
}
pub fn rewrite(task: &Task, cfg: &Config, model: &Model, selection: &Selection) -> Result<Value> {
    let mut body = task.body.clone();
    let o = body
        .as_object_mut()
        .ok_or_else(|| Error::bad("Object required"))?;
    o.remove("plugins");
    o.remove("session_id");
    o.remove("max_tokens");
    o.remove("max_completion_tokens");
    o.remove("max_output_tokens");
    o.remove("reasoning_effort");
    o.insert("model".into(), json!(model.id));
    o.insert("provider".into(), provider(task, cfg, model)?);
    match task.protocol {
        Protocol::Chat => {
            o.remove("max_completion_tokens");
            o.insert("max_tokens".into(), json!(task.max_output));
            if task.stream {
                o.insert("stream_options".into(), json!({"include_usage":true}));
            }
        }
        Protocol::Responses => {
            o.remove("max_tokens");
            o.insert("max_output_tokens".into(), json!(task.max_output));
        }
        Protocol::Messages => {
            o.insert("max_tokens".into(), json!(task.max_output));
        }
    }
    if selection.control == "effort" {
        let mut reasoning = o.remove("reasoning").unwrap_or(json!({}));
        reasoning["effort"] = json!(selection.effort);
        reasoning["enabled"] = json!(selection.effort != "none");
        o.insert("reasoning".into(), reasoning);
    } else if selection.control == "budget" {
        let fraction = match selection.effort.as_str() {
            "low" => 0.2,
            "high" => 0.7,
            _ => 0.4,
        };
        o.insert("reasoning".into(),json!({"max_tokens":((task.max_output as f64*fraction)as u64).max(128).min(task.max_output-1)}));
    } else {
        o.remove("reasoning");
    }
    Ok(body)
}
