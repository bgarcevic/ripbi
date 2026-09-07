//! ANSI styling, gated in one place per `docs/cli-ux-guidelines.md`: color is
//! on only when the target stream is a TTY and the user has not disabled it
//! via `--no-color`, `NO_COLOR`, or `TERM=dumb`.

/// The handful of styles `scan` uses. Detection happens once per stream; a
/// disabled palette returns its input untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    enabled: bool,
}

impl Palette {
    /// Detects color for one stream: on only when the stream is a TTY and no
    /// disable switch fired.
    #[must_use]
    pub fn detect(stream_is_tty: bool, no_color_flag: bool) -> Self {
        let no_color_env = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let dumb_term = std::env::var_os("TERM").is_some_and(|v| v == "dumb");
        Self::resolve(stream_is_tty, no_color_flag, no_color_env, dumb_term)
    }

    /// The pure gating rule, split out so tests never touch process
    /// environment state.
    fn resolve(
        stream_is_tty: bool,
        no_color_flag: bool,
        no_color_env: bool,
        dumb_term: bool,
    ) -> Self {
        Self {
            enabled: stream_is_tty && !no_color_flag && !no_color_env && !dumb_term,
        }
    }

    /// A palette that never styles — for tests and non-stream rendering.
    #[must_use]
    pub fn plain() -> Self {
        Self { enabled: false }
    }

    /// Bold — section headers and the summary numbers.
    #[must_use]
    pub fn bold(&self, text: &str) -> String {
        self.wrap("1", text)
    }

    /// Dim — the `←` annotation lines under a finding.
    #[must_use]
    pub fn dim(&self, text: &str) -> String {
        self.wrap("2", text)
    }

    /// Red — the `error:` prefix on stderr.
    #[must_use]
    pub fn red(&self, text: &str) -> String {
        self.wrap("31", text)
    }

    /// Yellow — a nonzero unused count in the summary line.
    #[must_use]
    pub fn yellow(&self, text: &str) -> String {
        self.wrap("33", text)
    }

    /// Green — a zero unused count in the summary line.
    #[must_use]
    pub fn green(&self, text: &str) -> String {
        self.wrap("32", text)
    }

    fn wrap(&self, code: &str, text: &str) -> String {
        if self.enabled {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TTY, no flags, no environment overrides: color on. Every one of the
    /// four gates must be able to turn it off alone.
    #[test]
    fn all_four_gates_participate() {
        let on = Palette::resolve(true, false, false, false);
        assert!(on.enabled);
        assert!(
            !Palette::resolve(false, false, false, false).enabled,
            "not a TTY"
        );
        assert!(
            !Palette::resolve(true, true, false, false).enabled,
            "--no-color"
        );
        assert!(
            !Palette::resolve(true, false, true, false).enabled,
            "NO_COLOR"
        );
        assert!(
            !Palette::resolve(true, false, false, true).enabled,
            "TERM=dumb"
        );
    }

    #[test]
    fn plain_never_styles() {
        let palette = Palette::plain();
        assert_eq!(palette.bold("x"), "x");
        assert_eq!(palette.dim("x"), "x");
    }

    #[test]
    fn styling_wraps_in_ansi_codes() {
        let palette = Palette::resolve(true, false, false, false);
        assert_eq!(palette.bold("x"), "\x1b[1mx\x1b[0m");
        assert_eq!(palette.red("error:"), "\x1b[31merror:\x1b[0m");
    }
}
