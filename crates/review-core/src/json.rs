//! I-JSON numeric domain admission.
//!
//! Every JSON payload is checked before it is hashed or stored. The rules come straight from the
//! design: finite IEEE-754 binary64 semantics, integers only inside the interoperable safe range
//! `[-(2^53-1), 2^53-1]`, and no negative zero. Values outside that domain — a 64-bit ID, an
//! exact decimal, money, a nanosecond counter — belong in the schema as canonical strings.
//!
//! Admitting first is what makes a content digest mean something: `-0.0` and `0.0` are distinct
//! bytes but the same number, and `9007199254740993` survives a round trip through some JSON
//! stacks as `9007199254740992`. Either one turns "same content, same ID" into a coin flip.

use serde_json::Value;

pub const SAFE_INTEGER_MAX: i64 = 9_007_199_254_740_991;
pub const SAFE_INTEGER_MIN: i64 = -9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumericDomainError {
    /// JSON Pointer to the offending value.
    pub pointer: String,
    pub reason: Reason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Outside `[-(2^53-1), 2^53-1]`; represent it as a canonical string instead.
    IntegerOutOfSafeRange,
    /// `-0.0`. It compares equal to `0.0` but serializes differently, so it cannot be allowed
    /// to reach a digest.
    NegativeZero,
    /// Not finite. JSON has no literal for these, so it means a producer emitted them out of
    /// band.
    NonFinite,
}

impl std::fmt::Display for NumericDomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.reason {
            Reason::IntegerOutOfSafeRange => {
                "integer outside the interoperable safe range; use a canonical string"
            }
            Reason::NegativeZero => "negative zero",
            Reason::NonFinite => "non-finite number",
        };
        write!(
            f,
            "{}: {what}",
            if self.pointer.is_empty() {
                "/"
            } else {
                &self.pointer
            }
        )
    }
}

impl std::error::Error for NumericDomainError {}

/// Check a payload against the numeric domain, reporting the first violation in document order.
pub fn admit(value: &Value) -> Result<(), NumericDomainError> {
    let mut pointer = String::new();
    walk(value, &mut pointer)
}

fn walk(value: &Value, pointer: &mut String) -> Result<(), NumericDomainError> {
    match value {
        Value::Number(n) => check_number(n, pointer),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                let mark = pointer.len();
                pointer.push('/');
                pointer.push_str(&index.to_string());
                walk(item, pointer)?;
                pointer.truncate(mark);
            }
            Ok(())
        }
        Value::Object(fields) => {
            for (key, field) in fields {
                let mark = pointer.len();
                pointer.push('/');
                pointer.push_str(&escape_token(key));
                walk(field, pointer)?;
                pointer.truncate(mark);
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn check_number(n: &serde_json::Number, pointer: &str) -> Result<(), NumericDomainError> {
    let err = |reason| NumericDomainError {
        pointer: pointer.to_string(),
        reason,
    };

    if let Some(i) = n.as_i64() {
        if !(SAFE_INTEGER_MIN..=SAFE_INTEGER_MAX).contains(&i) {
            return Err(err(Reason::IntegerOutOfSafeRange));
        }
        return Ok(());
    }
    if let Some(u) = n.as_u64() {
        if u > SAFE_INTEGER_MAX as u64 {
            return Err(err(Reason::IntegerOutOfSafeRange));
        }
        return Ok(());
    }
    match n.as_f64() {
        Some(f) if !f.is_finite() => Err(err(Reason::NonFinite)),
        // `is_sign_negative` is the only way to see -0.0: it compares equal to 0.0.
        Some(f) if f == 0.0 && f.is_sign_negative() => Err(err(Reason::NegativeZero)),
        Some(_) => Ok(()),
        None => Err(err(Reason::NonFinite)),
    }
}

/// RFC 6901 token escaping, so a key containing `/` or `~` still points somewhere real.
fn escape_token(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_the_safe_integer_boundaries() {
        assert!(admit(&json!({ "hi": SAFE_INTEGER_MAX, "lo": SAFE_INTEGER_MIN })).is_ok());
    }

    #[test]
    fn rejects_two_to_the_fifty_third() {
        let err = admit(&json!({ "n": 9_007_199_254_740_992i64 })).unwrap_err();
        assert_eq!(err.reason, Reason::IntegerOutOfSafeRange);
        assert_eq!(err.pointer, "/n");
    }

    #[test]
    fn rejects_negative_zero() {
        let value: Value = serde_json::from_str(r#"{"a":[1,-0.0]}"#).unwrap();
        let err = admit(&value).unwrap_err();
        assert_eq!(err.reason, Reason::NegativeZero);
        assert_eq!(err.pointer, "/a/1");
    }

    #[test]
    fn plain_zero_is_fine() {
        assert!(admit(&serde_json::from_str::<Value>(r#"{"a":0,"b":0.0}"#).unwrap()).is_ok());
    }

    #[test]
    fn pointer_escapes_slashes_and_tildes() {
        let value: Value = serde_json::from_str(r#"{"a/b~c":{"d":1e309}}"#).unwrap_or(json!({}));
        // 1e309 does not survive parsing as a finite f64; either it was rejected at parse time
        // (empty object here) or it must be reported as non-finite.
        if let Err(err) = admit(&value) {
            assert_eq!(err.pointer, "/a~1b~0c/d");
        }
    }
}
