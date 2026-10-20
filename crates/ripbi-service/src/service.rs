//! [`Service`]: authenticated, retrying, paging calls to the Fabric and
//! Power BI REST APIs, and the fire-then-poll definition download.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;

use crate::api::{
    App, Definition, DefinitionEnvelope, ErrorBody, Item, OperationState, Page, PbiReport,
    Relations, Workspace,
};
use crate::auth::{Audience, Secret, TokenSource, tenant_of};
use crate::error::{Error, Result};
use crate::http::{Clock, Request, Response, SystemClock, Transport, endpoint_of, host_of};

/// Retries for a throttled (`429`) or unavailable (`503`) answer.
const MAX_RETRIES: u32 = 4;
/// The wait when a throttled answer names none.
const DEFAULT_BACKOFF: Duration = Duration::from_secs(5);
/// No single wait is longer than this, whatever `Retry-After` says.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// The poll interval when an operation names none.
const DEFAULT_POLL: Duration = Duration::from_secs(5);
/// Poll waits are capped tighter than throttling waits: the server's
/// `Retry-After: 20` is a hint, not a floor.
const MAX_POLL: Duration = Duration::from_secs(30);
/// The first poll of an export. The service says `Retry-After: 20` whatever
/// the item, but most exports finish in a few seconds; the wait then doubles
/// up to the server's hint, so a slow export costs only a few polls.
const FIRST_POLL: Duration = Duration::from_secs(1);
/// Pages followed before a list is declared runaway.
const MAX_PAGES: usize = 1000;

/// Hosts a bearer token may be sent to, besides the configured endpoints.
/// `getDefinition` answers with a `Location` on a regional host
/// (`wabi-…-redirect.analysis.windows.net`), which must be followed as given.
const TRUSTED_SUFFIXES: [&str; 3] = [
    ".fabric.microsoft.com",
    ".powerbi.com",
    ".analysis.windows.net",
];

/// Where the APIs live. Tests point both at a fake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// The Fabric REST base, `https://api.fabric.microsoft.com/v1`.
    pub fabric: String,
    /// The Power BI REST base, `https://api.powerbi.com/v1.0/myorg`.
    pub power_bi: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            fabric: "https://api.fabric.microsoft.com/v1".to_string(),
            power_bi: "https://api.powerbi.com/v1.0/myorg".to_string(),
        }
    }
}

/// Which way a relations call walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// What depends on the item (a model's reports).
    Downstream,
    /// What the item depends on (a report's model).
    Upstream,
}

/// The item types with a definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionKind {
    /// A report (`/reports/{id}/getDefinition`).
    Report,
    /// A semantic model (`/semanticModels/{id}/getDefinition`).
    SemanticModel,
}

/// One definition to download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionRequest {
    /// The item type.
    pub kind: DefinitionKind,
    /// The owning workspace.
    pub workspace_id: String,
    /// The item.
    pub item_id: String,
    /// The `format` query parameter (`TMDL` for a model), if any.
    pub format: Option<String>,
}

/// How a definition export started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    /// The API answered at once.
    Done(Definition),
    /// A long-running operation: poll `location` after `retry_after`.
    Pending {
        /// The operation URL.
        location: String,
        /// The server's suggested wait.
        retry_after: Duration,
    },
}

/// One poll of a long-running operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Polled {
    /// Still running: poll again after the wait.
    Running(Duration),
    /// Finished, with its result.
    Done(Definition),
}

/// Fire-then-poll tuning for [`Service::fetch_definitions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchOptions {
    /// Operations in flight at once.
    pub max_in_flight: usize,
    /// The budget for one operation, from its POST to its result.
    pub timeout: Duration,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            max_in_flight: 32,
            timeout: Duration::from_secs(600),
        }
    }
}

/// The client. Not `Sync`: one fetch runs on one thread, and the token cache
/// is a plain cell.
pub struct Service {
    transport: Box<dyn Transport>,
    tokens: Box<dyn TokenSource>,
    clock: Box<dyn Clock>,
    endpoints: Endpoints,
    cache: RefCell<HashMap<Audience, Secret>>,
}

impl Service {
    /// A client over the real endpoints and clock.
    #[must_use]
    pub fn new(transport: Box<dyn Transport>, tokens: Box<dyn TokenSource>) -> Self {
        Self {
            transport,
            tokens,
            clock: Box::new(SystemClock),
            endpoints: Endpoints::default(),
            cache: RefCell::default(),
        }
    }

    /// Replaces the endpoints (tests).
    #[must_use]
    pub fn with_endpoints(mut self, endpoints: Endpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    /// Replaces the clock (tests).
    #[must_use]
    pub fn with_clock(mut self, clock: Box<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// How the client signs in, for the "Signed in via …" line.
    #[must_use]
    pub fn auth_description(&self) -> String {
        self.tokens.describe()
    }

    /// A token for `audience`, acquired once per run.
    ///
    /// # Errors
    ///
    /// [`Error::Auth`] when the token source fails.
    pub fn token(&self, audience: Audience) -> Result<Secret> {
        if let Some(token) = self.cache.borrow().get(&audience) {
            return Ok(token.clone());
        }
        let token = self.tokens.token(audience, self.transport.as_ref())?;
        self.cache.borrow_mut().insert(audience, token.clone());
        Ok(token)
    }

    /// The tenant the Fabric token was issued by.
    ///
    /// # Errors
    ///
    /// [`Error::Auth`] when no token can be acquired.
    pub fn tenant_id(&self) -> Result<Option<String>> {
        Ok(tenant_of(&self.token(Audience::Fabric)?))
    }

    // ---- lists -------------------------------------------------------------

    /// Every workspace the caller is a member of, any role.
    ///
    /// # Errors
    ///
    /// Any [`Error`] from the call.
    pub fn workspaces(&self) -> Result<Vec<Workspace>> {
        self.get_all(
            Audience::Fabric,
            &format!("{}/workspaces", self.endpoints.fabric),
        )
    }

    /// The workspaces where the caller holds one of `roles` (`Admin`,
    /// `Member`, `Contributor`, `Viewer`).
    ///
    /// # Errors
    ///
    /// Any [`Error`] from the call.
    pub fn workspaces_with_roles(&self, roles: &[&str]) -> Result<Vec<Workspace>> {
        self.get_all(
            Audience::Fabric,
            &format!(
                "{}/workspaces?roles={}",
                self.endpoints.fabric,
                roles.join(",")
            ),
        )
    }

    /// One workspace by id.
    ///
    /// # Errors
    ///
    /// Any [`Error`] from the call; `404` when the caller cannot see it.
    pub fn workspace(&self, workspace_id: &str) -> Result<Workspace> {
        self.get_json(
            Audience::Fabric,
            &format!("{}/workspaces/{workspace_id}", self.endpoints.fabric),
        )
    }

    /// The semantic models in a workspace.
    ///
    /// # Errors
    ///
    /// Any [`Error`] from the call.
    pub fn semantic_models(&self, workspace_id: &str) -> Result<Vec<Item>> {
        self.get_all(
            Audience::Fabric,
            &format!(
                "{}/workspaces/{workspace_id}/semanticModels",
                self.endpoints.fabric
            ),
        )
    }

    /// One semantic model by id.
    ///
    /// # Errors
    ///
    /// Any [`Error`] from the call; `404` when the caller cannot see it.
    pub fn semantic_model(&self, workspace_id: &str, model_id: &str) -> Result<Item> {
        self.get_json(
            Audience::Fabric,
            &format!(
                "{}/workspaces/{workspace_id}/semanticModels/{model_id}",
                self.endpoints.fabric
            ),
        )
    }

    /// The reports in a workspace. Org-app copies are not workspace items and
    /// never appear here.
    ///
    /// # Errors
    ///
    /// Any [`Error`] from the call.
    pub fn reports(&self, workspace_id: &str) -> Result<Vec<Item>> {
        self.get_all(
            Audience::Fabric,
            &format!(
                "{}/workspaces/{workspace_id}/reports",
                self.endpoints.fabric
            ),
        )
    }

    /// The relations of one item (beta API; `?beta=true` is required).
    /// Pages, if the API ever pages, are merged.
    ///
    /// # Errors
    ///
    /// Any [`Error`] from the call.
    pub fn relations(
        &self,
        workspace_id: &str,
        item_id: &str,
        direction: Direction,
    ) -> Result<Relations> {
        let way = match direction {
            Direction::Downstream => "downstream",
            Direction::Upstream => "upstream",
        };
        let mut url = format!(
            "{}/workspaces/{workspace_id}/items/{item_id}/relations/{way}?beta=true",
            self.endpoints.fabric
        );
        let mut merged = Relations::default();
        for _ in 0..MAX_PAGES {
            let page: Relations = self.get_json(Audience::Fabric, &url)?;
            merged.items.extend(page.items);
            merged.relations.extend(page.relations);
            for workspace in page.workspaces {
                if !merged
                    .workspaces
                    .iter()
                    .any(|known| known.id == workspace.id)
                {
                    merged.workspaces.push(workspace);
                }
            }
            match page.continuation_uri {
                Some(next) if !next.is_empty() => url = next,
                _ => return Ok(merged),
            }
        }
        Err(runaway(&url))
    }

    /// The org apps the caller can see (Power BI API).
    ///
    /// # Errors
    ///
    /// Any [`Error`] from the call.
    pub fn apps(&self) -> Result<Vec<App>> {
        self.get_all(
            Audience::PowerBi,
            &format!("{}/apps", self.endpoints.power_bi),
        )
    }

    /// The reports in one org app; each `id` is an app copy (Power BI API).
    ///
    /// # Errors
    ///
    /// Any [`Error`] from the call.
    pub fn app_reports(&self, app_id: &str) -> Result<Vec<PbiReport>> {
        self.get_all(
            Audience::PowerBi,
            &format!("{}/apps/{app_id}/reports", self.endpoints.power_bi),
        )
    }

    /// A workspace's reports with their bound model (Power BI API).
    ///
    /// # Errors
    ///
    /// Any [`Error`] from the call.
    pub fn group_reports(&self, workspace_id: &str) -> Result<Vec<PbiReport>> {
        self.get_all(
            Audience::PowerBi,
            &format!("{}/groups/{workspace_id}/reports", self.endpoints.power_bi),
        )
    }

    // ---- definitions -------------------------------------------------------

    /// Starts one definition export.
    ///
    /// # Errors
    ///
    /// Any [`Error`] from the call; `404` for an item the caller may not
    /// read (Viewer, no access, or an org-app copy).
    pub fn start_definition(&self, request: &DefinitionRequest) -> Result<Started> {
        let segment = match request.kind {
            DefinitionKind::Report => "reports",
            DefinitionKind::SemanticModel => "semanticModels",
        };
        let mut url = format!(
            "{}/workspaces/{}/{segment}/{}/getDefinition",
            self.endpoints.fabric, request.workspace_id, request.item_id
        );
        if let Some(format) = &request.format {
            url.push_str("?format=");
            url.push_str(format);
        }
        let response = self.send(Audience::Fabric, "POST", &url, Some("{}"))?;
        match response.status {
            200 => Ok(Started::Done(parse_definition(&url, &response)?)),
            202 => {
                let location = response.header("location").ok_or_else(|| Error::Response {
                    endpoint: endpoint_of(&url),
                    detail: "202 Accepted without a Location header".to_string(),
                })?;
                Ok(Started::Pending {
                    location: location.to_string(),
                    retry_after: response.retry_after().unwrap_or(DEFAULT_POLL),
                })
            }
            _ => Err(http_error("POST", &url, &response)),
        }
    }

    /// Polls one operation; on success, also fetches its result.
    ///
    /// # Errors
    ///
    /// [`Error::Operation`] when the operation failed; any [`Error`] from
    /// the calls.
    pub fn poll_definition(&self, location: &str) -> Result<Polled> {
        let response = self.send(Audience::Fabric, "GET", location, None)?;
        if !response.is_success() {
            return Err(http_error("GET", location, &response));
        }
        let state: OperationState = parse_json(location, &response)?;
        let wait = response.retry_after().unwrap_or(DEFAULT_POLL).min(MAX_POLL);
        match state.status.as_str() {
            "Succeeded" => {
                let result_url = response
                    .header("location")
                    .map_or_else(|| format!("{location}/result"), str::to_string);
                let result = self.send(Audience::Fabric, "GET", &result_url, None)?;
                if !result.is_success() {
                    return Err(http_error("GET", &result_url, &result));
                }
                Ok(Polled::Done(parse_definition(&result_url, &result)?))
            }
            "Failed" | "Cancelled" => {
                let error = state.error.unwrap_or_default();
                Err(Error::Operation(match (error.error_code, error.message) {
                    (Some(code), Some(message)) => format!("{code}: {message}"),
                    (Some(text), None) | (None, Some(text)) => text,
                    (None, None) => state.status,
                }))
            }
            // NotStarted, Running, Undefined, and anything new.
            _ => Ok(Polled::Running(wait)),
        }
    }

    /// Downloads many definitions at once: every export is started first
    /// (up to `max_in_flight`), then all are polled together, so the waits
    /// overlap instead of adding up. Each export is polled after 1 s, then
    /// after twice its last wait, never longer than the server's hint
    /// (1, 2, 4, 8, 16, 20 s against `Retry-After: 20`). Results come back in
    /// request order; `progress(done, total)` runs after each one finishes.
    pub fn fetch_definitions(
        &self,
        requests: &[DefinitionRequest],
        options: FetchOptions,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Vec<Result<Definition>> {
        enum Slot {
            Queued,
            Pending {
                location: String,
                due: Instant,
                interval: Duration,
                started: Instant,
            },
            Done(Result<Definition>),
        }
        let total = requests.len();
        let mut slots: Vec<Slot> = requests.iter().map(|_| Slot::Queued).collect();
        let mut next = 0;
        let mut done = 0;
        let max_in_flight = options.max_in_flight.max(1);
        while done < total {
            // Fire.
            let mut in_flight = slots
                .iter()
                .filter(|slot| matches!(slot, Slot::Pending { .. }))
                .count();
            while in_flight < max_in_flight && next < total {
                let now = self.clock.now();
                slots[next] = match self.start_definition(&requests[next]) {
                    Ok(Started::Pending {
                        location,
                        retry_after,
                    }) => {
                        in_flight += 1;
                        let interval = FIRST_POLL.min(retry_after).min(MAX_POLL);
                        Slot::Pending {
                            location,
                            due: now + interval,
                            interval,
                            started: now,
                        }
                    }
                    Ok(Started::Done(definition)) => Slot::Done(Ok(definition)),
                    Err(error) => Slot::Done(Err(error)),
                };
                if matches!(slots[next], Slot::Done(_)) {
                    done += 1;
                    progress(done, total);
                }
                next += 1;
            }
            // Wait for the earliest due poll.
            let earliest = slots
                .iter()
                .filter_map(|slot| match slot {
                    Slot::Pending { due, .. } => Some(*due),
                    _ => None,
                })
                .min();
            let Some(earliest) = earliest else {
                continue;
            };
            let now = self.clock.now();
            if earliest > now {
                self.clock.sleep(earliest - now);
            }
            // Poll everything due.
            for (index, slot) in slots.iter_mut().enumerate() {
                let Slot::Pending {
                    location,
                    due,
                    interval,
                    started,
                } = slot
                else {
                    continue;
                };
                let now = self.clock.now();
                if *due > now {
                    continue;
                }
                let finished = if now.duration_since(*started) > options.timeout {
                    Some(Err(Error::Timeout(format!(
                        "the definition of item {}",
                        requests[index].item_id
                    ))))
                } else {
                    match self.poll_definition(location) {
                        Ok(Polled::Running(hint)) => {
                            *interval = (*interval * 2).min(hint);
                            *due = now + *interval;
                            None
                        }
                        Ok(Polled::Done(definition)) => Some(Ok(definition)),
                        Err(error) => Some(Err(error)),
                    }
                };
                if let Some(result) = finished {
                    *slot = Slot::Done(result);
                    done += 1;
                    progress(done, total);
                }
            }
        }
        slots
            .into_iter()
            .map(|slot| match slot {
                Slot::Done(result) => result,
                // Unreachable: the loop ends only when every slot is done.
                Slot::Queued | Slot::Pending { .. } => {
                    Err(Error::Timeout("an unfinished operation".to_string()))
                }
            })
            .collect()
    }

    // ---- plumbing ----------------------------------------------------------

    /// Whether a token may travel to `url`: the configured endpoints' hosts,
    /// or an HTTPS URL on a Power BI / Fabric host.
    fn trusted(&self, url: &str) -> bool {
        let Some(host) = host_of(url) else {
            return false;
        };
        let configured = [&self.endpoints.fabric, &self.endpoints.power_bi]
            .into_iter()
            .any(|base| host_of(base).as_deref() == Some(host.as_str()));
        configured
            || (url.starts_with("https://")
                && TRUSTED_SUFFIXES.iter().any(|suffix| host.ends_with(suffix)))
    }

    /// Sends one authenticated request, retrying throttled answers.
    fn send(
        &self,
        audience: Audience,
        method: &'static str,
        url: &str,
        body: Option<&str>,
    ) -> Result<Response> {
        if !self.trusted(url) {
            return Err(Error::UntrustedHost(
                host_of(url).unwrap_or_else(|| endpoint_of(url)),
            ));
        }
        let token = self.token(audience)?;
        let request = Request {
            method,
            url: url.to_string(),
            authorization: Some(format!("Bearer {}", token.expose())),
            body: body.map(|body| (body.to_string(), "application/json")),
        };
        let mut attempt = 0;
        loop {
            let response = self.transport.send(&request)?;
            if matches!(response.status, 429 | 503) && attempt < MAX_RETRIES {
                attempt += 1;
                self.clock.sleep(
                    response
                        .retry_after()
                        .unwrap_or(DEFAULT_BACKOFF)
                        .min(MAX_BACKOFF),
                );
                continue;
            }
            return Ok(response);
        }
    }

    fn get_json<T: DeserializeOwned>(&self, audience: Audience, url: &str) -> Result<T> {
        let response = self.send(audience, "GET", url, None)?;
        if !response.is_success() {
            return Err(http_error("GET", url, &response));
        }
        parse_json(url, &response)
    }

    /// Every page of a list: Fabric pages carry `continuationUri`.
    fn get_all<T: DeserializeOwned>(&self, audience: Audience, url: &str) -> Result<Vec<T>> {
        let mut url = url.to_string();
        let mut all = Vec::new();
        for _ in 0..MAX_PAGES {
            let page: Page<T> = self.get_json(audience, &url)?;
            all.extend(page.value);
            match page.continuation_uri {
                Some(next) if !next.is_empty() => url = next,
                _ => return Ok(all),
            }
        }
        Err(runaway(&url))
    }
}

fn parse_json<T: DeserializeOwned>(url: &str, response: &Response) -> Result<T> {
    serde_json::from_slice(&response.body).map_err(|error| Error::Response {
        endpoint: endpoint_of(url),
        detail: format!("invalid JSON: {error}"),
    })
}

fn parse_definition(url: &str, response: &Response) -> Result<Definition> {
    let envelope: DefinitionEnvelope = parse_json(url, response)?;
    Ok(envelope.definition)
}

fn http_error(method: &'static str, url: &str, response: &Response) -> Error {
    Error::Http {
        method,
        endpoint: endpoint_of(url),
        status: response.status,
        code: ErrorBody::code(&response.body),
    }
}

fn runaway(url: &str) -> Error {
    Error::Response {
        endpoint: endpoint_of(url),
        detail: format!("more than {MAX_PAGES} pages"),
    }
}
