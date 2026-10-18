use serde_json::Value;
#[derive(Default)]
pub struct Inspector {
    buffer: Vec<u8>,
    pub cost: Option<f64>,
    pub done: bool,
    pub failed: bool,
    expected: Option<String>,
}
impl Inspector {
    pub fn with_model(model: String) -> Self {
        Self {
            expected: Some(model),
            ..Self::default()
        }
    }
    fn valid_model(&self, model: &str) -> bool {
        self.expected
            .as_ref()
            .is_none_or(|expected| crate::registry::served_matches(expected, model))
    }
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > 1024 * 1024 {
            self.failed = true;
            return false;
        }
        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line = self.buffer.drain(..=pos).collect::<Vec<_>>();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data == "[DONE]" {
                    self.done = true;
                } else if let Ok(v) = serde_json::from_str::<Value>(data) {
                    for model in [&v["model"], &v["message"]["model"], &v["response"]["model"]] {
                        if let Some(model) = model.as_str()
                            && !self.valid_model(model)
                        {
                            self.failed = true;
                            return false;
                        }
                    }
                    if v.get("error").is_some() || v["type"] == "error" {
                        self.failed = true;
                    }
                    if ["response.completed", "message_stop"]
                        .contains(&v["type"].as_str().unwrap_or(""))
                    {
                        self.done = true;
                    }
                    for usage in [&v["usage"], &v["response"]["usage"], &v["message"]["usage"]] {
                        if let Some(n) = usage["cost"]
                            .as_f64()
                            .filter(|n| n.is_finite() && *n >= 0.0)
                        {
                            self.cost = Some(n);
                        }
                    }
                }
            }
        }
        true
    }
}
