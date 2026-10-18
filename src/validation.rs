//! Narrow, exact checks for unambiguous quantity-times-unit-price totals.
//! Other tasks require their own validators; this is not a general math parser.
use regex::Regex;
use std::sync::LazyLock;
pub fn request_total(task: &crate::policy::Task) -> Option<MoneyTotal> {
    if task.body.get("system").is_some() || task.body.get("instructions").is_some() {
        return None;
    }
    let input = match task.protocol {
        crate::policy::Protocol::Responses => task.body["input"].as_str()?,
        _ => {
            let messages = task.body["messages"].as_array()?;
            if messages.len() != 1 || messages[0]["role"] != "user" {
                return None;
            }
            messages[0]["content"].as_str()?
        }
    };
    MoneyTotal::from_request(input)
}
static ITEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b([0-9]+)\s+[^0-9₹$€£;.,]+?\s+(?:at|@|costing)\s*([₹$€£])\s*([0-9]+(?:\.[0-9]{1,2})?)\s*(?:each|apiece)\b").unwrap()
});
static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9]+(?:[.,][0-9]+)*").unwrap());
static ADJUSTMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(discount|tax|gst|vat|tip|shipping|delivery|refund|return|rebate|percent|off|change|profit|margin|excluding|including|except|ignore|free|buy|sell|sold)\b|%").unwrap()
});
static TOTAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\btotal(?:\s+(?:cost|amount|price))?(?:\s+(?:is|of|equals))?[\s:=]*([₹$€£])\s*([0-9]+(?:,[0-9]{3})*(?:\.[0-9]{1,2})?)\b").unwrap()
});
#[derive(Clone, Debug)]
pub struct MoneyTotal {
    currency: String,
    cents: u128,
}
fn cents(s: &str) -> Option<u128> {
    let s = s.replace(',', "");
    let mut parts = s.split('.');
    let whole = parts.next()?.parse::<u128>().ok()?;
    let fraction = match parts.next() {
        None => 0,
        Some(s) if s.len() == 1 => s.parse::<u128>().ok()?.checked_mul(10)?,
        Some(s) if s.len() == 2 => s.parse::<u128>().ok()?,
        _ => return None,
    };
    if parts.next().is_some() {
        return None;
    }
    whole.checked_mul(100)?.checked_add(fraction)
}
impl MoneyTotal {
    pub fn from_request(text: &str) -> Option<Self> {
        if !text.to_lowercase().contains("total") || ADJUSTMENT.is_match(text) {
            return None;
        }
        if text.contains('-')
            || text.contains('−')
            || ["not", "without", "instead", "minus", "less", "or", "only"]
                .iter()
                .any(|w| {
                    text.split(|c: char| !c.is_alphabetic())
                        .any(|s| s.eq_ignore_ascii_case(w))
                })
        {
            return None;
        }
        let mut total = 0u128;
        let mut currency = None;
        let mut items = 0;
        let mut spans = vec![];
        for c in ITEM.captures_iter(text) {
            let q = c[1].parse::<u128>().ok()?;
            if q == 0 {
                return None;
            }
            if currency.as_ref().is_some_and(|s| s != &c[2]) {
                return None;
            }
            currency = Some(c[2].to_owned());
            total = total.checked_add(q.checked_mul(cents(&c[3])?)?)?;
            items += 1;
            for i in [1, 3] {
                let m = c.get(i)?;
                spans.push((m.start(), m.end()));
            }
        }
        // Do not silently ignore other numbers, fees, or unsupported formats.
        if items == 0
            || NUMBER
                .find_iter(text)
                .map(|m| (m.start(), m.end()))
                .collect::<Vec<_>>()
                != spans
        {
            return None;
        }
        Some(Self {
            currency: currency?,
            cents: total,
        })
    }
    pub fn accepts(&self, answer: &str) -> bool {
        let answer = answer.replace('*', "");
        let mut found = false;
        for c in TOTAL.captures_iter(&answer) {
            found = true;
            if c[1] != self.currency || cents(&c[2]) != Some(self.cents) {
                return false;
            }
        }
        found
    }
    pub fn context(&self) -> String {
        format!(
            "Exact quantity × unit-price check of the original request gives a total of {}{}.{:02}. Independently verify the calculation and include the final answer as 'total {}{}.{:02}'.",
            self.currency,
            self.cents / 100,
            self.cents % 100,
            self.currency,
            self.cents / 100,
            self.cents % 100
        )
    }
}
