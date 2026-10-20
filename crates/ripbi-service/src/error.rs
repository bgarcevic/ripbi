//! The service's typed error. Messages name the endpoint and the status, never
//! a token, a secret, or a response body that could echo one.

/// Everything that can go wrong talking to the tenant.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No token could be acquired: the Azure CLI is missing or signed out, an
    /// environment variable is unset, or Entra refused the credentials.
    #[error("{0}")]
    Auth(String),

    /// The API answered with a non-success status.
    #[error("{method} {endpoint} returned HTTP {status}{}", code_suffix(.code.as_deref()))]
    Http {
        /// `GET` or `POST`.
        method: &'static str,
        /// The URL without its query string.
        endpoint: String,
        /// The HTTP status code.
        status: u16,
        /// The API's own error code (`ItemNotFound`, `Unauthorized`, …), when
        /// the body carried one.
        code: Option<String>,
    },

    /// The request never got an answer: DNS, TLS, connection, or timeout.
    #[error("cannot reach {endpoint}: {detail}")]
    Transport {
        /// The URL without its query string.
        endpoint: String,
        /// The transport's own description.
        detail: String,
    },

    /// The API answered, but not with the shape this client reads.
    #[error("unexpected response from {endpoint}: {detail}")]
    Response {
        /// The URL without its query string.
        endpoint: String,
        /// What was wrong with it.
        detail: String,
    },

    /// A long-running operation reported failure.
    #[error("the definition export failed: {0}")]
    Operation(String),

    /// A long-running operation did not finish within its budget.
    #[error("timed out waiting for {0}")]
    Timeout(String),

    /// A URL pointed somewhere this client never sends credentials to.
    #[error("refusing to send credentials to {0}: not a Power BI or Fabric host")]
    UntrustedHost(String),
}

impl Error {
    /// The HTTP status, when the API answered with one.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Whether the API said the item does not exist for this caller. The
    /// Fabric API answers `404` both for a missing item and for one the
    /// caller may not read (a Viewer, or no access at all), never `403`.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.status() == Some(404)
    }
}

fn code_suffix(code: Option<&str>) -> String {
    code.map(|code| format!(" ({code})")).unwrap_or_default()
}

/// The service's result type.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_errors_name_the_endpoint_status_and_code() {
        let error = Error::Http {
            method: "POST",
            endpoint: "https://api.fabric.microsoft.com/v1/workspaces/w/reports/r/getDefinition"
                .to_string(),
            status: 404,
            code: Some("ItemNotFound".to_string()),
        };
        assert!(error.is_not_found());
        assert_eq!(
            error.to_string(),
            "POST https://api.fabric.microsoft.com/v1/workspaces/w/reports/r/getDefinition \
             returned HTTP 404 (ItemNotFound)"
        );
    }
}
