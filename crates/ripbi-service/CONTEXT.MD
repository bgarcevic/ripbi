# ripbi-service — CONTEXT

The Power BI / Fabric REST client behind `ripbi fetch`. It signs in, finds a semantic
model's downstream reports, forecasts which ones the caller may read, and downloads
item definitions. User-facing contract: [../ripbi-cli/docs/fetch.md](../ripbi-cli/docs/fetch.md).

## Rules

- **The only crate that talks to the tenant.** `ripbi-core` stays offline; the CLI
  decides and prints, and calls this crate for everything on the network.
- Same discipline as `ripbi-core`: never prints, never exits; typed
  `Result<T, ripbi_service::Error>`. `#![forbid(unsafe_code)]`.
- **Secrets never leak.** Tokens and client secrets live in `auth::Secret` (redacted
  `Debug`); `http::Request`'s `Debug` redacts the `Authorization` header and the body;
  error messages name the endpoint, the status, and the API's error code, never a
  token or a response body. A bearer token is only sent to the configured endpoints
  and `*.fabric.microsoft.com`, `*.powerbi.com`, `*.analysis.windows.net` over HTTPS
  (`Service::trusted`) — the `getDefinition` `Location` header is followed only there —
  and the transport never follows redirects.
- **Parse loosely** (coding standard 3): every response field defaults, unknown fields
  are ignored, and type strings stay strings (`Item::is_report`,
  `Item::is_semantic_model` accept `Model`, `SemanticModel`, `Dataset`). The relations
  API is beta and already differs from its docs.
- Two seams: `http::Transport` (the real one is `ureq` with statuses returned, not
  raised) and `http::Clock` (polling and backoff). Tests replace both — no test touches
  the network or sleeps.

## Layout

| Module | Role |
|---|---|
| `api.rs` | Response shapes: `Workspace`, `Item`, `Relation(s)`, `PbiReport`, `App`, `Definition`/`Part` (`Part::decode` → bytes), operation state, error bodies (`errorCode` / `error.code`), `is_guid` |
| `auth.rs` | `Audience` (Fabric vs Power BI token audiences), `TokenSource` with `AzureCli` (`az account get-access-token`; `az.cmd` on Windows), `StaticTokens` (env), `ServicePrincipal` (client credentials via the transport); `tenant_of` reads the JWT `tid` claim for provenance only |
| `http.rs` | `Transport`/`UreqTransport`, `Request`/`Response`, `Clock`/`SystemClock`, URL helpers (`host_of`, `endpoint_of`, `form_encode`) |
| `service.rs` | `Service`: cached tokens per audience, trusted-host check, `429`/`503` retry honouring `Retry-After`, `continuationUri` paging, the list calls, `relations` (`?beta=true`, pages merged), `start_definition` / `poll_definition` (202 + `Location`, `/result` on success), and `fetch_definitions` — fire every export (up to `max_in_flight`), then poll all together so the per-operation waits overlap |
| `downstream.rs` | `discover`: downstream relations → workspace reports bound **directly** to the model, org-app copies split off (absent from the workspace's report listing, else matched through the org-app listings), everything else as `other_items`; `Access` forecast from two workspace lists (any role / `Admin,Member,Contributor`). `verify` diffs against every workspace's Power BI report listing (`datasetId`). `model_of_report` resolves a report link upstream |
| `link.rs` | Portal links (`/groups/<ws>/datasets|semanticmodels|reports/<id>`) |
| `fake.rs` | Test doubles (scripted transport, fake clock, fixed tokens, `definition_json`); compiled for this crate's tests and behind the `fake` feature for the CLI's |
| `tests.rs` | End-to-end tests over de-identified fixtures of real tenant responses |

## What the tenant taught us

Recorded in the module docs and tests; the short version:

- Downstream crosses workspaces, includes the root item, and does **not** hide reports
  the caller cannot read. Without `?beta=true` it answers `400`.
- Every app-published report appears twice (workspace report + app copy, same name and
  workspace id). The copy always `404`s on `getDefinition`.
- `getDefinition` answers `404`, never `403`, for a Viewer, for no access, and for an
  app copy. Roles predict it exactly.
- `getDefinition` is `202` + `Location` on a regional host + `Retry-After: 20`; poll,
  then `GET {Location}/result`. The `20` is the same for every item, so the first poll
  goes at 1 s and the wait doubles up to it. Parts are `InlineBase64`.
- The Power BI `Export` API (`.pbix`) refuses service principals even as Contributor; it
  is not a fallback.
