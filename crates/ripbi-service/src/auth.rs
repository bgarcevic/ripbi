//! Bearer tokens for the two APIs `fetch` talks to. Three sources, none of
//! which needs an app registration of ripbi's own:
//!
//! - [`AzureCli`] reuses the developer's `az login` (az owns the token cache).
//! - [`StaticTokens`] takes tokens from environment variables.
//! - [`ServicePrincipal`] runs the client-credentials grant — the CI path.
//!
//! Tokens and secrets never reach an error message, a `Debug` string, or disk.

use std::fmt;
use std::process::Command;

use base64::Engine as _;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::http::{Request, Transport, form_encode};

/// Which API a token is for. Fabric and Power BI REST are separate token
/// audiences: a token for one is rejected by the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Audience {
    /// `api.fabric.microsoft.com`: workspaces, items, relations, definitions.
    Fabric,
    /// `api.powerbi.com`: org apps and the per-workspace report listing.
    PowerBi,
}

impl Audience {
    /// The resource URI, as `az account get-access-token --resource` takes it.
    #[must_use]
    pub fn resource(self) -> &'static str {
        match self {
            Self::Fabric => "https://api.fabric.microsoft.com",
            Self::PowerBi => "https://analysis.windows.net/powerbi/api",
        }
    }

    /// The client-credentials scope.
    #[must_use]
    pub fn scope(self) -> String {
        format!("{}/.default", self.resource())
    }
}

/// A string that never prints.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value, for the one place that must send it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Produces bearer tokens. `transport` is the same seam API calls go
/// through, so a token request is as testable as any other.
pub trait TokenSource {
    /// A raw access token (no `Bearer ` prefix) for `audience`.
    fn token(&self, audience: Audience, transport: &dyn Transport) -> Result<Secret>;

    /// A short description for the "Signed in via …" line.
    fn describe(&self) -> String;
}

/// Tokens from the Azure CLI's own cache: `az account get-access-token`.
#[derive(Debug, Default, Clone, Copy)]
pub struct AzureCli;

impl TokenSource for AzureCli {
    fn token(&self, audience: Audience, _transport: &dyn Transport) -> Result<Secret> {
        // `az` is a batch file on Windows, which `Command` only runs when the
        // extension is spelled out.
        let program = if cfg!(windows) { "az.cmd" } else { "az" };
        let output = Command::new(program)
            .args([
                "account",
                "get-access-token",
                "--resource",
                audience.resource(),
                "--query",
                "accessToken",
                "--output",
                "tsv",
            ])
            .output()
            .map_err(|error| {
                Error::Auth(format!(
                    "cannot run the Azure CLI ({program}): {error}; is it installed and on PATH?"
                ))
            })?;
        if !output.status.success() {
            // az's stderr explains the failure ("Please run 'az login'") and
            // never contains a token.
            let stderr = String::from_utf8_lossy(&output.stderr);
            let reason = stderr
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("no message");
            return Err(Error::Auth(format!(
                "the Azure CLI could not get a token for {}: {reason}",
                audience.resource()
            )));
        }
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if token.is_empty() {
            return Err(Error::Auth(
                "the Azure CLI returned an empty token".to_string(),
            ));
        }
        Ok(Secret(token))
    }

    fn describe(&self) -> String {
        "Azure CLI".to_string()
    }
}

/// Pre-acquired tokens, typically from environment variables.
#[derive(Debug, Clone)]
pub struct StaticTokens {
    /// The Fabric API token.
    pub fabric: Secret,
    /// The Power BI REST token; optional — only the org-app lookup and
    /// `--verify` need it.
    pub power_bi: Option<Secret>,
    /// Where the tokens came from, for the sign-in line.
    pub origin: String,
}

impl TokenSource for StaticTokens {
    fn token(&self, audience: Audience, _transport: &dyn Transport) -> Result<Secret> {
        match audience {
            Audience::Fabric => Ok(self.fabric.clone()),
            Audience::PowerBi => self.power_bi.clone().ok_or_else(|| {
                Error::Auth(format!(
                    "no Power BI API token: {} carries only a Fabric token",
                    self.origin
                ))
            }),
        }
    }

    fn describe(&self) -> String {
        self.origin.clone()
    }
}

/// The OAuth 2.0 client-credentials grant against Entra ID.
#[derive(Debug, Clone)]
pub struct ServicePrincipal {
    /// The directory (tenant) id.
    pub tenant_id: String,
    /// The application (client) id.
    pub client_id: String,
    /// The client secret.
    pub client_secret: Secret,
    /// The Entra authority, `https://login.microsoftonline.com` by default.
    pub authority: String,
}

impl ServicePrincipal {
    /// The default Entra authority.
    pub const AUTHORITY: &'static str = "https://login.microsoftonline.com";
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

impl TokenSource for ServicePrincipal {
    fn token(&self, audience: Audience, transport: &dyn Transport) -> Result<Secret> {
        let url = format!(
            "{}/{}/oauth2/v2.0/token",
            self.authority.trim_end_matches('/'),
            form_encode(&self.tenant_id)
        );
        let body = format!(
            "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
            form_encode(&self.client_id),
            form_encode(self.client_secret.expose()),
            form_encode(&audience.scope())
        );
        let response = transport.send(&Request {
            method: "POST",
            url,
            authorization: None,
            body: Some((body, "application/x-www-form-urlencoded")),
        })?;
        let parsed: Option<TokenResponse> = serde_json::from_slice(&response.body).ok();
        match parsed {
            Some(TokenResponse {
                access_token: Some(token),
                ..
            }) if response.is_success() => Ok(Secret(token)),
            Some(TokenResponse {
                error: Some(error),
                error_description,
                ..
            }) => {
                // Entra's description never echoes the secret; its first line
                // names the cause (AADSTS7000215: invalid client secret, …).
                let description = error_description
                    .as_deref()
                    .and_then(|text| text.lines().next())
                    .unwrap_or("");
                Err(Error::Auth(format!(
                    "the service principal sign-in failed: {error} {description}"
                )))
            }
            _ => Err(Error::Auth(format!(
                "the service principal sign-in failed with HTTP {}",
                response.status
            ))),
        }
    }

    fn describe(&self) -> String {
        format!("service principal {}", self.client_id)
    }
}

/// The tenant id (`tid` claim) of a JWT access token, read without
/// verifying the signature — it only labels the fetch's provenance.
#[must_use]
pub fn tenant_of(token: &Secret) -> Option<String> {
    let payload = token.expose().split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("tid")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Response;
    use std::cell::RefCell;

    struct Recorder {
        seen: RefCell<Vec<Request>>,
        reply: Response,
    }

    impl Transport for Recorder {
        fn send(&self, request: &Request) -> Result<Response> {
            self.seen.borrow_mut().push(request.clone());
            Ok(self.reply.clone())
        }
    }

    fn principal() -> ServicePrincipal {
        ServicePrincipal {
            tenant_id: "tenant".to_string(),
            client_id: "client".to_string(),
            client_secret: Secret::new("s3cret&="),
            authority: ServicePrincipal::AUTHORITY.to_string(),
        }
    }

    #[test]
    fn service_principal_posts_the_client_credentials_grant() {
        let transport = Recorder {
            seen: RefCell::default(),
            reply: Response {
                status: 200,
                headers: Vec::new(),
                body: br#"{"token_type":"Bearer","access_token":"abc"}"#.to_vec(),
            },
        };
        let token = principal()
            .token(Audience::Fabric, &transport)
            .expect("token");
        assert_eq!(token.expose(), "abc");
        let seen = transport.seen.borrow();
        assert_eq!(
            seen[0].url,
            "https://login.microsoftonline.com/tenant/oauth2/v2.0/token"
        );
        let (body, content_type) = seen[0].body.clone().expect("form body");
        assert_eq!(content_type, "application/x-www-form-urlencoded");
        assert!(body.contains("client_secret=s3cret%26%3D"));
        assert!(body.contains("scope=https%3A%2F%2Fapi.fabric.microsoft.com%2F.default"));
    }

    #[test]
    fn service_principal_errors_carry_the_entra_code_not_the_secret() {
        let transport = Recorder {
            seen: RefCell::default(),
            reply: Response {
                status: 401,
                headers: Vec::new(),
                body: br#"{"error":"invalid_client","error_description":"AADSTS7000215: Invalid client secret provided.\r\nTrace ID: x"}"#.to_vec(),
            },
        };
        let error = principal()
            .token(Audience::PowerBi, &transport)
            .expect_err("refused");
        let message = error.to_string();
        assert!(message.contains("invalid_client AADSTS7000215"));
        assert!(!message.contains("s3cret") && !message.contains("Trace ID"));
        assert!(!format!("{:?}", principal()).contains("s3cret"));
    }

    #[test]
    fn static_tokens_without_power_bi_say_so() {
        let tokens = StaticTokens {
            fabric: Secret::new("f"),
            power_bi: None,
            origin: "RIPBI_FABRIC_TOKEN".to_string(),
        };
        let transport = Recorder {
            seen: RefCell::default(),
            reply: Response::default(),
        };
        assert_eq!(
            tokens
                .token(Audience::Fabric, &transport)
                .expect("fabric")
                .expose(),
            "f"
        );
        assert!(tokens.token(Audience::PowerBi, &transport).is_err());
    }

    #[test]
    fn tenant_comes_from_the_tid_claim() {
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"aud":"x","tid":"11111111-2222-3333-4444-555555555555"}"#);
        let token = Secret::new(format!("e30.{claims}.sig"));
        assert_eq!(
            tenant_of(&token).as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert_eq!(tenant_of(&Secret::new("not-a-jwt")), None);
    }
}
