//! The CLI's own error type: a human-readable message plus an optional hint
//! naming the fix, per `docs/cli-ux-guidelines.md` ("rewrite errors for
//! humans, with the fix"). Core errors convert losslessly via [`From`].

use std::fmt;

/// A scan failure worth exactly two lines: what went wrong, and what to try.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanError {
    /// What went wrong, as a complete sentence.
    pub message: String,
    /// The fix, when the error has a conventional one.
    pub hint: Option<String>,
}

impl ScanError {
    /// Builds an error with no hint.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            hint: None,
        }
    }

    /// Attaches the fix to suggest under the message.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, "\nhint: {hint}")?;
        }
        Ok(())
    }
}

impl From<std::io::Error> for ScanError {
    fn from(error: std::io::Error) -> Self {
        Self::new(format!("I/O error: {error}"))
    }
}

impl From<ripbi_core::Error> for ScanError {
    fn from(error: ripbi_core::Error) -> Self {
        Self::new(error.to_string())
    }
}
