//! Reading a JWT's claims without validating it. Used only to name an account (its subject, workspace or email) or
//! to see when a token expires; nothing here authenticates anything.

use chrono::{DateTime, Utc};
use serde_json::Value;

/// The payload of a JWT, when it is three dot-separated parts with a base64url JSON object in the middle.
pub(crate) fn claims(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    let (_, payload, _) = (parts.next()?, parts.next()?, parts.next()?);
    let bytes = base64url_decode(payload)?;
    serde_json::from_slice::<Value>(&bytes).ok().filter(Value::is_object)
}

/// A non-empty string claim, trimmed.
pub(crate) fn string_claim(claims: &Value, name: &str) -> Option<String> {
    claims
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// The `exp` claim.
pub(crate) fn expires_at(token: &str) -> Option<DateTime<Utc>> {
    let seconds = claims(token)?.get("exp")?.as_i64()?;
    DateTime::from_timestamp(seconds, 0)
}

fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let value = |ch: u8| -> Option<u32> {
        Some(match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        } as u32)
    };
    let digits: Vec<u32> = text.trim_end_matches('=').bytes().map(value).collect::<Option<_>>()?;
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    for chunk in digits.chunks(4) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (ix, digit)| acc | digit << (18 - 6 * ix));
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..chunk.len().saturating_sub(1)]);
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An unsigned JWT with these claims, for tests.
    pub(crate) fn token(claims: &Value) -> String {
        format!("e30.{}.sig", encode(&serde_json::to_vec(claims).unwrap()))
    }

    fn encode(bytes: &[u8]) -> String {
        const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |acc, (ix, byte)| acc | u32::from(*byte) << (16 - 8 * ix));
            for ix in 0..=chunk.len() {
                out.push(DIGITS[(n >> (18 - 6 * ix) & 63) as usize] as char);
            }
        }
        out
    }

    #[test]
    fn claims_read_the_payload_and_reject_other_shapes() {
        let token = token(&serde_json::json!({"sub": " user-1 ", "exp": 1_800_000_000}));
        let claims = claims(&token).unwrap();
        assert_eq!(string_claim(&claims, "sub").as_deref(), Some("user-1"));
        assert_eq!(expires_at(&token).map(|at| at.timestamp()), Some(1_800_000_000));
        assert!(super::claims("not-a-jwt").is_none());
        assert!(super::claims("a.!!!.c").is_none());
        assert!(super::claims(&format!("a.{}.c", encode(b"[1]"))).is_none());
    }
}
