//! Minimal HTTP abstraction so the pool can be exercised without a network.

use std::fmt;
use std::time::Duration;

/// Response body limit (Exa `/contents` can be large).
const BODY_LIMIT_BYTES: u64 = 256 * 1024 * 1024;

/// A completed HTTP exchange, status included (non-2xx is not an error here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Parsed `Retry-After` header (seconds form only).
    pub retry_after: Option<Duration>,
    /// Raw body text.
    pub body: String,
}

/// Failure before any HTTP status was received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The request exceeded its timeout.
    Timeout,
    /// Connection could not be established or was dropped.
    Connect(String),
    /// Anything else (TLS, protocol, body read).
    Other(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => f.write_str("timeout"),
            Self::Connect(msg) => write!(f, "connect: {msg}"),
            Self::Other(msg) => write!(f, "transport: {msg}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// HTTP verbs the CLI needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Read-only fetch (agent run status, lists).
    Get,
    /// JSON body submission.
    Post,
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Get => "GET",
            Self::Post => "POST",
        })
    }
}

/// Sends one request authenticated with an Exa API key.
pub trait Transport {
    /// Send `method` to `url` with `x-api-key: api_key`; `body` is JSON for
    /// POST and ignored for GET.
    ///
    /// # Errors
    /// Only for failures that prevented a response; HTTP error statuses are
    /// returned as `Ok` so the policy can inspect them.
    fn send(
        &self,
        method: Method,
        url: &str,
        api_key: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<HttpResponse, TransportError>;
}

/// Production transport over `ureq` with rustls.
#[derive(Debug, Clone)]
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    /// Build an agent with a global per-request timeout.
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .user_agent(concat!("exa-pool/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: config.into(),
        }
    }
}

impl Transport for UreqTransport {
    fn send(
        &self,
        method: Method,
        url: &str,
        api_key: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<HttpResponse, TransportError> {
        let resp = match method {
            Method::Get => self
                .agent
                .get(url)
                .header("x-api-key", api_key)
                .header("accept", "application/json")
                .call(),
            Method::Post => self
                .agent
                .post(url)
                .header("x-api-key", api_key)
                .header("accept", "application/json")
                .send_json(body.unwrap_or(&serde_json::Value::Null)),
        }
        .map_err(map_error)?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let body = resp
            .into_body()
            .with_config()
            .limit(BODY_LIMIT_BYTES)
            .read_to_string()
            .map_err(map_error)?;
        Ok(HttpResponse {
            status,
            retry_after,
            body,
        })
    }
}

// `ureq::Error` is `#[non_exhaustive]`, so a wildcard arm is unavoidable.
#[allow(clippy::wildcard_enum_match_arm)]
fn map_error(err: ureq::Error) -> TransportError {
    match err {
        ureq::Error::Timeout(_) => TransportError::Timeout,
        ureq::Error::Io(e) => TransportError::Connect(e.to_string()),
        ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => {
            TransportError::Connect(err.to_string())
        }
        other => TransportError::Other(other.to_string()),
    }
}
