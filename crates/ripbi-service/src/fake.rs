//! Test doubles for [`crate::Service`]: a scripted transport, a clock that
//! never sleeps, and fixed tokens. Compiled only with the `fake` feature,
//! which only test builds enable.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::auth::{Audience, Secret, TokenSource};
use crate::error::Result;
use crate::http::{Clock, Request, Response, Transport};
use crate::service::{Endpoints, Service};

/// The Fabric base the fake serves.
pub const FABRIC: &str = "https://fabric.test/v1";
/// The Power BI base the fake serves.
pub const POWER_BI: &str = "https://powerbi.test/v1.0/myorg";

struct Route {
    method: &'static str,
    url: String,
    replies: VecDeque<Response>,
}

/// A transport that answers from a script. Each route replies in order and
/// repeats its last reply; an unscripted request answers `404` with
/// `errorCode: NoRoute`.
#[derive(Default)]
pub struct FakeTransport {
    routes: RefCell<Vec<Route>>,
    log: RefCell<Vec<String>>,
}

impl FakeTransport {
    /// An empty script.
    #[must_use]
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// Scripts one more reply for `method url`.
    pub fn reply(&self, method: &'static str, url: &str, status: u16, body: &str) {
        self.reply_with(method, url, status, &[], body);
    }

    /// Scripts one more reply with headers.
    pub fn reply_with(
        &self,
        method: &'static str,
        url: &str,
        status: u16,
        headers: &[(&str, &str)],
        body: &str,
    ) {
        let response = Response {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), (*value).to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
        };
        let mut routes = self.routes.borrow_mut();
        if let Some(route) = routes
            .iter_mut()
            .find(|route| route.method == method && route.url == url)
        {
            route.replies.push_back(response);
        } else {
            routes.push(Route {
                method,
                url: url.to_string(),
                replies: VecDeque::from([response]),
            });
        }
    }

    /// Replaces every scripted reply for `method url` with this one.
    pub fn replace(&self, method: &'static str, url: &str, status: u16, body: &str) {
        self.routes
            .borrow_mut()
            .retain(|route| !(route.method == method && route.url == url));
        self.reply(method, url, status, body);
    }

    /// Every request sent so far, as `METHOD url`.
    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
}

impl Transport for Rc<FakeTransport> {
    fn send(&self, request: &Request) -> Result<Response> {
        self.log
            .borrow_mut()
            .push(format!("{} {}", request.method, request.url));
        let mut routes = self.routes.borrow_mut();
        let Some(route) = routes
            .iter_mut()
            .find(|route| route.method == request.method && route.url == request.url)
        else {
            return Ok(Response {
                status: 404,
                headers: Vec::new(),
                body: br#"{"errorCode":"NoRoute"}"#.to_vec(),
            });
        };
        let reply = if route.replies.len() > 1 {
            route.replies.pop_front()
        } else {
            route.replies.front().cloned()
        };
        Ok(reply.unwrap_or_default())
    }
}

/// A clock whose `sleep` only advances time.
pub struct FakeClock {
    start: Instant,
    elapsed: Cell<Duration>,
}

impl Default for FakeClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            elapsed: Cell::new(Duration::ZERO),
        }
    }
}

impl FakeClock {
    /// The total time slept.
    #[must_use]
    pub fn slept(&self) -> Duration {
        self.elapsed.get()
    }
}

impl Clock for Rc<FakeClock> {
    fn now(&self) -> Instant {
        self.start + self.elapsed.get()
    }

    fn sleep(&self, duration: Duration) {
        self.elapsed.set(self.elapsed.get() + duration);
    }
}

/// Fixed tokens: `fabric-token` and `powerbi-token`.
#[derive(Debug, Default, Clone, Copy)]
pub struct FakeTokens;

impl TokenSource for FakeTokens {
    fn token(&self, audience: Audience, _transport: &dyn Transport) -> Result<Secret> {
        Ok(Secret::new(match audience {
            Audience::Fabric => "fabric-token",
            Audience::PowerBi => "powerbi-token",
        }))
    }

    fn describe(&self) -> String {
        "test tokens".to_string()
    }
}

/// A `getDefinition` result body: `{"definition":{"format","parts":[…]}}`
/// with each file base64-encoded, as the API sends it.
#[must_use]
pub fn definition_json(format: &str, parts: &[(String, Vec<u8>)]) -> String {
    use base64::Engine as _;
    let parts: Vec<serde_json::Value> = parts
        .iter()
        .map(|(path, bytes)| {
            serde_json::json!({
                "path": path,
                "payload": base64::engine::general_purpose::STANDARD.encode(bytes),
                "payloadType": "InlineBase64",
            })
        })
        .collect();
    serde_json::json!({ "definition": { "format": format, "parts": parts } }).to_string()
}

/// A service over the fake transport, the fake clock, and fixed tokens.
#[must_use]
pub fn service(transport: &Rc<FakeTransport>, clock: &Rc<FakeClock>) -> Service {
    Service::new(Box::new(Rc::clone(transport)), Box::new(FakeTokens))
        .with_endpoints(Endpoints {
            fabric: FABRIC.to_string(),
            power_bi: POWER_BI.to_string(),
        })
        .with_clock(Box::new(Rc::clone(clock)))
}
