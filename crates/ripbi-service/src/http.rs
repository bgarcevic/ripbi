//! The network seam. [`Transport`] sends one request and returns the raw
//! answer, whatever its status; [`Clock`] owns time. Tests replace both, so
//! no test touches the network or sleeps.

use std::fmt;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// Hard cap on one response body. A report definition is a few MB at most;
/// the cap keeps a hostile or broken endpoint from exhausting memory.
const MAX_BODY_BYTES: u64 = 256 * 1024 * 1024;

/// One HTTP request.
#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    /// `GET` or `POST`.
    pub method: &'static str,
    /// The absolute URL.
    pub url: String,
    /// The `Authorization` header value, when the request carries one.
    pub authorization: Option<String>,
    /// The body and its content type, for a `POST`.
    pub body: Option<(String, &'static str)>,
}

impl fmt::Debug for Request {
    /// Never prints the token or the body (a token request's body holds the
    /// client secret).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &self.url)
            .field(
                "authorization",
                &self.authorization.as_ref().map(|_| "<redacted>"),
            )
            .field("body", &self.body.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// One HTTP response, success or not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Response {
    /// The status code.
    pub status: u16,
    /// The headers, names lower-cased.
    pub headers: Vec<(String, String)>,
    /// The body bytes.
    pub body: Vec<u8>,
}

impl Response {
    /// The first header with this (case-insensitive) name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The `Retry-After` header in seconds, when it is a plain number.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        self.header("retry-after")
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs)
    }

    /// Whether the status is `2xx`.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Sends requests. Implementations return every status as a [`Response`];
/// only a request that got no answer at all is an error.
pub trait Transport {
    /// Sends one request.
    fn send(&self, request: &Request) -> Result<Response>;
}

/// The real transport: a synchronous `ureq` agent with a per-request timeout,
/// proxy environment support, and statuses returned rather than raised.
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    /// Builds the agent with the given per-request timeout.
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            // The APIs never redirect; a redirect is not followed, so the
            // bearer token never travels to a host this client did not pick.
            .max_redirects(0)
            .user_agent(concat!("ripbi/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { agent }
    }
}

impl Transport for UreqTransport {
    fn send(&self, request: &Request) -> Result<Response> {
        let endpoint = endpoint_of(&request.url);
        let transport_error = |error: ureq::Error| Error::Transport {
            endpoint: endpoint.clone(),
            detail: error.to_string(),
        };
        let result = match (request.method, &request.body) {
            ("POST", body) => {
                let mut builder = self.agent.post(&request.url);
                if let Some(token) = &request.authorization {
                    builder = builder.header("Authorization", token);
                }
                let (body, content_type) = body.clone().unwrap_or_default();
                builder.content_type(content_type).send(body)
            }
            _ => {
                let mut builder = self.agent.get(&request.url);
                if let Some(token) = &request.authorization {
                    builder = builder.header("Authorization", token);
                }
                builder.call()
            }
        };
        let response = result.map_err(transport_error)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
            })
            .collect();
        let body = response
            .into_body()
            .with_config()
            .limit(MAX_BODY_BYTES)
            .read_to_vec()
            .map_err(transport_error)?;
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

/// Time, behind a trait so polling and retry backoff run instantly in tests.
pub trait Clock {
    /// The current instant.
    fn now(&self) -> Instant;
    /// Blocks for `duration`.
    fn sleep(&self, duration: Duration);
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// The URL without its query string, for error messages.
#[must_use]
pub fn endpoint_of(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or(url).to_string()
}

/// The URL's host, lower-cased, without port or credentials.
#[must_use]
pub fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Percent-encodes one `application/x-www-form-urlencoded` value.
#[must_use]
pub fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_ignores_port_path_and_credentials() {
        assert_eq!(
            host_of("https://WABI-x-redirect.analysis.windows.net/v1/operations/1"),
            Some("wabi-x-redirect.analysis.windows.net".to_string())
        );
        assert_eq!(
            host_of("http://user@127.0.0.1:8080/x"),
            Some("127.0.0.1".to_string())
        );
        assert_eq!(host_of("file:///etc/passwd"), None);
    }

    #[test]
    fn form_values_escape_reserved_bytes() {
        assert_eq!(form_encode("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
    }

    #[test]
    fn request_debug_redacts_token_and_body() {
        let request = Request {
            method: "POST",
            url: "https://login.microsoftonline.com/t/oauth2/v2.0/token".to_string(),
            authorization: Some("Bearer secret-token".to_string()),
            body: Some((
                "client_secret=hunter2".to_string(),
                "application/x-www-form-urlencoded",
            )),
        };
        let debug = format!("{request:?}");
        assert!(!debug.contains("secret-token") && !debug.contains("hunter2"));
    }

    #[test]
    fn retry_after_reads_seconds_only() {
        let response = Response {
            status: 202,
            headers: vec![("retry-after".to_string(), "20".to_string())],
            body: Vec::new(),
        };
        assert_eq!(response.retry_after(), Some(Duration::from_secs(20)));
        assert_eq!(Response::default().retry_after(), None);
    }
}
