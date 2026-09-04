//! Turn an Exa HTTP response (or transport failure) into a pool decision.
//!
//! Exa is explicit about billing and auth, so the policy is keyed on HTTP
//! status first and the error `tag` second. See
//! <https://exa.ai/docs/reference/error-codes>.
//!
//! | Status | Verdict | Effect on key |
//! |--------|---------|---------------|
//! | 2xx | [`Verdict::Success`] | failures reset, spend accumulated |
//! | 401 | [`Verdict::KeyInvalid`] | marked invalid, rotate |
//! | 402 | [`Verdict::KeyExhausted`] | marked exhausted, rotate |
//! | 429 | [`Verdict::RateLimited`] | cooldown (`Retry-After` or default), rotate |
//! | 5xx, timeout, I/O | [`Verdict::Transient`] | failure counter, quarantine after N, rotate |
//! | other 4xx, 501, 504 `CRAWL_*` | [`Verdict::RequestError`] | untouched, stop |

use std::time::Duration;

use serde::Deserialize;

use crate::transport::{HttpResponse, TransportError};

/// Longest slice of a raw body echoed back in error messages.
const MAX_MESSAGE_LEN: usize = 300;

/// What the pool should do after one attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Request succeeded; `cost` is `costDollars.total` when present.
    Success {
        /// Dollars charged for the call.
        cost: f64,
        /// Raw response body.
        body: String,
    },
    /// HTTP 402: credits or budget gone for this key.
    KeyExhausted {
        /// Exa tag (`NO_MORE_CREDITS`, `API_KEY_BUDGET_EXCEEDED`, ...).
        tag: String,
        /// Exa message.
        message: String,
    },
    /// HTTP 401: key rejected outright.
    KeyInvalid {
        /// Exa message.
        message: String,
    },
    /// HTTP 429: back off this key briefly.
    RateLimited {
        /// Server-suggested wait, when present.
        retry_after: Option<Duration>,
        /// Exa message.
        message: String,
    },
    /// 5xx or transport failure; likely not the key's fault.
    Transient {
        /// Description of the failure.
        message: String,
    },
    /// Exa rejected the request; another key would fail the same way.
    RequestError {
        /// HTTP status.
        status: u16,
        /// Exa tag when present.
        tag: Option<String>,
        /// Exa message.
        message: String,
    },
}

impl Verdict {
    /// One-line summary for diagnostics.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Success { cost, .. } => format!("ok (${cost:.4})"),
            Self::KeyExhausted { tag, .. } => format!("402 {tag} → marked exhausted"),
            Self::KeyInvalid { .. } => "401 INVALID_API_KEY → marked invalid".into(),
            Self::RateLimited { retry_after, .. } => match retry_after {
                Some(d) => format!("429 rate limited → cooldown {}ms", d.as_millis()),
                None => "429 rate limited → cooldown".into(),
            },
            Self::Transient { message } => format!("transient: {message}"),
            Self::RequestError {
                status,
                tag,
                message,
            } => match tag {
                Some(tag) => format!("{status} {tag}: {message}"),
                None => format!("{status}: {message}"),
            },
        }
    }
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    tag: Option<String>,
}

#[derive(Deserialize)]
struct CostBody {
    #[serde(default, rename = "costDollars")]
    cost_dollars: Option<CostDollars>,
}

#[derive(Deserialize)]
struct CostDollars {
    #[serde(default)]
    total: Option<f64>,
}

/// Classify a completed HTTP exchange.
#[must_use]
pub fn classify(resp: HttpResponse) -> Verdict {
    let HttpResponse {
        status,
        retry_after,
        body,
    } = resp;
    if (200..300).contains(&status) {
        let cost = serde_json::from_str::<CostBody>(&body)
            .ok()
            .and_then(|c| c.cost_dollars)
            .and_then(|c| c.total)
            .unwrap_or(0.0);
        return Verdict::Success { cost, body };
    }

    let parsed = serde_json::from_str::<ErrorBody>(&body).ok();
    let tag = parsed.as_ref().and_then(|p| p.tag.clone());
    let message = parsed
        .and_then(|p| p.error)
        .unwrap_or_else(|| truncate(&body));

    match status {
        401 => Verdict::KeyInvalid { message },
        402 => Verdict::KeyExhausted {
            tag: tag.unwrap_or_else(|| "PAYMENT_REQUIRED".into()),
            message,
        },
        429 => Verdict::RateLimited {
            retry_after,
            message,
        },
        501 => Verdict::RequestError {
            status,
            tag,
            message,
        },
        500..=599 if !tag.as_deref().is_some_and(|t| t.starts_with("CRAWL_")) => {
            Verdict::Transient {
                message: match &tag {
                    Some(tag) => format!("{status} {tag}: {message}"),
                    None => format!("{status}: {message}"),
                },
            }
        }
        _ => Verdict::RequestError {
            status,
            tag,
            message,
        },
    }
}

/// Classify a failure that never produced an HTTP response.
#[must_use]
pub fn classify_transport(err: &TransportError) -> Verdict {
    Verdict::Transient {
        message: err.to_string(),
    }
}

fn truncate(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "<empty body>".into();
    }
    let mut out: String = trimmed.chars().take(MAX_MESSAGE_LEN).collect();
    if trimmed.chars().count() > MAX_MESSAGE_LEN {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(status: u16, body: &str) -> HttpResponse {
        HttpResponse {
            status,
            retry_after: None,
            body: body.to_owned(),
        }
    }

    #[test]
    fn success_reads_cost() {
        let v = classify(resp(200, r#"{"results":[],"costDollars":{"total":0.007}}"#));
        assert!(matches!(v, Verdict::Success { cost, .. } if (cost - 0.007).abs() < 1e-9));
    }

    #[test]
    fn success_without_cost_is_zero() {
        let v = classify(resp(200, r"{}"));
        assert!(matches!(v, Verdict::Success { cost, .. } if cost == 0.0));
    }

    #[test]
    fn invalid_key() {
        let v = classify(resp(
            401,
            r#"{"requestId":"x","error":"Invalid API key","tag":"INVALID_API_KEY"}"#,
        ));
        assert_eq!(
            v,
            Verdict::KeyInvalid {
                message: "Invalid API key".into()
            }
        );
    }

    #[test]
    fn exhausted_variants() {
        for tag in [
            "NO_MORE_CREDITS",
            "API_KEY_BUDGET_EXCEEDED",
            "TEAM_BUDGET_EXCEEDED",
        ] {
            let body = format!(r#"{{"error":"nope","tag":"{tag}"}}"#);
            let v = classify(resp(402, &body));
            assert_eq!(
                v,
                Verdict::KeyExhausted {
                    tag: tag.into(),
                    message: "nope".into()
                }
            );
        }
    }

    #[test]
    fn exhausted_without_tag_still_exhausted() {
        let v = classify(resp(402, "Payment Required"));
        assert!(matches!(v, Verdict::KeyExhausted { tag, .. } if tag == "PAYMENT_REQUIRED"));
    }

    #[test]
    fn rate_limited_with_retry_after() {
        let v = classify(HttpResponse {
            status: 429,
            retry_after: Some(Duration::from_secs(2)),
            body: r#"{"error":"You've exceeded your Exa rate limit of 10 requests per second"}"#
                .into(),
        });
        assert!(matches!(v, Verdict::RateLimited { retry_after: Some(d), .. } if d.as_secs() == 2));
    }

    #[test]
    fn server_errors_are_transient() {
        for status in [500, 502, 503, 504] {
            assert!(matches!(
                classify(resp(status, "boom")),
                Verdict::Transient { .. }
            ));
        }
        let internal = classify(resp(500, r#"{"error":"x","tag":"INTERNAL_ERROR"}"#));
        assert!(
            matches!(internal, Verdict::Transient { message } if message.contains("INTERNAL_ERROR"))
        );
    }

    #[test]
    fn crawl_timeouts_and_501_are_request_errors() {
        let crawl = classify(resp(504, r#"{"error":"slow","tag":"CRAWL_TIMEOUT"}"#));
        assert!(matches!(crawl, Verdict::RequestError { status: 504, .. }));
        let answer = classify(resp(
            501,
            r#"{"error":"no","tag":"UNABLE_TO_GENERATE_RESPONSE"}"#,
        ));
        assert!(matches!(answer, Verdict::RequestError { status: 501, .. }));
    }

    #[test]
    fn client_errors_do_not_rotate() {
        for status in [400, 403, 404, 422] {
            let body = r#"{"error":"bad","tag":"INVALID_REQUEST"}"#;
            assert!(matches!(
                classify(resp(status, body)),
                Verdict::RequestError { tag: Some(_), .. }
            ));
        }
    }

    #[test]
    fn transport_failures_are_transient() {
        let v = classify_transport(&TransportError::Timeout);
        assert!(matches!(v, Verdict::Transient { .. }));
    }

    #[test]
    fn truncates_long_bodies() {
        let body = "x".repeat(1_000);
        let v = classify(resp(400, &body));
        assert!(
            matches!(v, Verdict::RequestError { message, .. } if message.chars().count() == MAX_MESSAGE_LEN + 1)
        );
    }
}
