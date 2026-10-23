//! Non-secret monthly USD prices, including explicitly dated changes.
use chrono::{DateTime, Utc};
use codexbar_core::sus::Price;
use serde_json::{Value, json};
pub fn to_json(price: &Price) -> Value {
    json!({"monthlyCents":price.monthly_cents,"scheduled":price.scheduled.map(|(cents,at)|json!({"monthlyCents":cents,"effectiveAt":at.to_rfc3339()}))})
}
pub fn from_json(value: &Value) -> Option<Price> {
    let obj = value.as_object()?;
    let cents = |value: &Value| value.as_u64().filter(|c| *c > 0);
    let monthly_cents = match obj.get("monthlyCents") {
        None | Some(Value::Null) => None,
        Some(v) => Some(cents(v)?),
    };
    let scheduled = match obj.get("scheduled") {
        None | Some(Value::Null) => None,
        Some(v) => Some((
            cents(v.get("monthlyCents")?)?,
            DateTime::parse_from_rfc3339(v.get("effectiveAt")?.as_str()?)
                .ok()?
                .with_timezone(&Utc),
        )),
    };
    Some(Price {
        monthly_cents,
        scheduled,
    })
}
/// Decimal USD input, without floating-point rounding or locale-dependent parsing.
pub fn parse_usd(text: &str) -> Result<Option<u64>, &'static str> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let (whole, decimal) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || decimal.len() > 2
        || !decimal.bytes().all(|b| b.is_ascii_digit())
    {
        return Err("Enter a positive USD amount with at most two decimal places.");
    }
    let dollars: u64 = whole.parse().map_err(|_| "Price is too large.")?;
    let minor = if decimal.is_empty() {
        0
    } else {
        decimal.parse::<u64>().unwrap() * if decimal.len() == 1 { 10 } else { 1 }
    };
    let cents = dollars
        .checked_mul(100)
        .and_then(|d| d.checked_add(minor))
        .filter(|n| *n > 0)
        .ok_or("Price must be positive and fit in USD cents.")?;
    Ok(Some(cents))
}
pub fn display_usd(cents: Option<u64>) -> String {
    cents.map(|c| format!("{}.{:02}", c / 100, c % 100)).unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prices_round_trip_without_guessing_dates() {
        let p = Price {
            monthly_cents: Some(20000),
            scheduled: Some((10000, "2026-10-26T13:33:19Z".parse().unwrap())),
        };
        assert_eq!(from_json(&to_json(&p)), Some(p));
        assert_eq!(from_json(&json!({"monthlyCents":0})), None);
        assert_eq!(from_json(&json!({"scheduled":{"monthlyCents":10000}})), None);
    }
    #[test]
    fn decimal_prices_are_exact_and_invalid_inputs_fail() {
        for (s, n) in [("19.99", 1999), ("100", 10000), ("0.01", 1), ("20.5", 2050)] {
            assert_eq!(parse_usd(s), Ok(Some(n)));
        }
        for s in ["NaN", "-20", "0", "20.001", "20,00", "1e3"] {
            assert!(parse_usd(s).is_err(), "{s}");
        }
        assert_eq!(parse_usd(""), Ok(None));
    }
}
