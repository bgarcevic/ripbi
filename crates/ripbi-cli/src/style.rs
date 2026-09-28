//! ANSI styling, gated in one place per `docs/cli-ux-guidelines.md`: color is
//! on only when the target stream is a TTY (or `CLICOLOR_FORCE` asks for it
//! anyway) and the user has not disabled it via `--no-color`, `NO_COLOR`, or
//! `TERM=dumb`.
//!
//! The palette is "pruning shears" — the tree-shaker metaphor in four inks:
//! rust for dead wood (unused findings), moss for living wood (a clean scan),
//! brick for breakage and errors, and warm greys for structure. Styling only
//! ever adds color: the text is byte-identical with the palette off, so every
//! output contract (`docs/output.md`) is defined by the uncolored form.

/// One ink as a 24-bit color plus its nearest xterm-256 index, for terminals
/// that do not advertise truecolor.
#[derive(Debug, Clone, Copy)]
struct Ink {
    rgb: (u8, u8, u8),
    xterm: u8,
}

/// Dead wood: unused findings, the unused count, the spinner's live stage.
const RUST: Ink = Ink {
    rgb: (0xC2, 0x70, 0x3D),
    xterm: 173,
};
/// Living wood: a clean result.
const MOSS: Ink = Ink {
    rgb: (0x8A, 0x9A, 0x5B),
    xterm: 107,
};
/// Breakage: `error:`, broken bindings and artifacts.
const BRICK: Ink = Ink {
    rgb: (0xB5, 0x47, 0x3A),
    xterm: 131,
};
/// Structure: secondary notes, finished spinner stages.
const ASH: Ink = Ink {
    rgb: (0x8A, 0x83, 0x78),
    xterm: 245,
};
/// Annotations: `←` chains, sizes, upcoming spinner stages.
const DUSK: Ink = Ink {
    rgb: (0x6E, 0x69, 0x62),
    xterm: 242,
};

/// Whether the terminal advertises 24-bit color. COLORTERM is the de facto
/// signal; Windows Terminal, VS Code, and WezTerm support it without always
/// setting it.
fn truecolor() -> bool {
    let env = |name: &str| std::env::var_os(name);
    env("COLORTERM").is_some_and(|v| v == "truecolor" || v == "24bit")
        || env("WT_SESSION").is_some()
        || env("TERM_PROGRAM").is_some_and(|v| v == "vscode" || v == "WezTerm")
}

/// The palette mapped onto clap's `--help` and usage-error rendering: rust
/// section headers and usage line, bold literals (flags, commands), ash
/// placeholders, brick errors. Clap does its own gating — non-TTY, `NO_COLOR`,
/// and `TERM=dumb` all render plain — so only the color depth is chosen here.
#[must_use]
pub fn help_styles() -> clap::builder::Styles {
    use clap::builder::styling::{Ansi256Color, Color, RgbColor, Style, Styles};
    let truecolor = truecolor();
    let color = |ink: Ink| -> Option<Color> {
        Some(if truecolor {
            let (r, g, b) = ink.rgb;
            Color::Rgb(RgbColor(r, g, b))
        } else {
            Color::Ansi256(Ansi256Color(ink.xterm))
        })
    };
    let ink = |ink: Ink| Style::new().fg_color(color(ink));
    Styles::styled()
        .header(ink(RUST).bold())
        .usage(ink(RUST).bold())
        .literal(Style::new().bold())
        .placeholder(ink(ASH))
        .error(ink(BRICK).bold())
        .valid(ink(MOSS).bold())
        .invalid(ink(RUST).bold())
}

/// How many colors the terminal can show, once color is on at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Depth {
    Off,
    Xterm256,
    TrueColor,
}

/// The styles every command uses, named by role rather than color. Detection
/// happens once per stream; a disabled palette returns its input untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    depth: Depth,
}

impl Palette {
    /// Detects color for one stream: on only when the stream is a TTY — or
    /// `CLICOLOR_FORCE` is set to anything but `0`, for CI logs and recordings
    /// that render ANSI — and no disable switch fired. The disable switches
    /// always win over the force.
    #[must_use]
    pub fn detect(stream_is_tty: bool, no_color_flag: bool) -> Self {
        let env = |name: &str| std::env::var_os(name);
        let forced = env("CLICOLOR_FORCE").is_some_and(|v| !v.is_empty() && v != "0");
        let no_color_env = env("NO_COLOR").is_some_and(|v| !v.is_empty());
        let dumb_term = env("TERM").is_some_and(|v| v == "dumb");
        Self::resolve(
            stream_is_tty || forced,
            no_color_flag,
            no_color_env,
            dumb_term,
            truecolor(),
        )
    }

    /// The pure gating rule, split out so tests never touch process
    /// environment state.
    fn resolve(
        stream_is_tty: bool,
        no_color_flag: bool,
        no_color_env: bool,
        dumb_term: bool,
        truecolor: bool,
    ) -> Self {
        let depth = if !stream_is_tty || no_color_flag || no_color_env || dumb_term {
            Depth::Off
        } else if truecolor {
            Depth::TrueColor
        } else {
            Depth::Xterm256
        };
        Self { depth }
    }

    /// A palette that never styles — for tests and non-stream rendering.
    #[must_use]
    pub fn plain() -> Self {
        Self { depth: Depth::Off }
    }

    /// Whether this palette styles at all. The scan spinner rides on the same
    /// gate: a stream that must not carry color must not carry motion either.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.depth != Depth::Off
    }

    /// Bold — section headers and the summary numbers.
    #[must_use]
    pub fn bold(&self, text: &str) -> String {
        self.wrap("1", text)
    }

    /// Dusk — the `←` annotation lines under a finding, sizes, asides.
    #[must_use]
    pub fn dim(&self, text: &str) -> String {
        self.ink(DUSK, false, text)
    }

    /// Ash — secondary notes: suppression counts, finished stages.
    #[must_use]
    pub fn muted(&self, text: &str) -> String {
        self.ink(ASH, false, text)
    }

    /// Brick — the `error:` prefix and broken-binding headers.
    #[must_use]
    pub fn alert(&self, text: &str) -> String {
        self.ink(BRICK, true, text)
    }

    /// Rust — a nonzero unused count, actionable sections, the live stage.
    #[must_use]
    pub fn warn(&self, text: &str) -> String {
        self.ink(RUST, true, text)
    }

    /// Rust without weight — the spinner glyph.
    #[must_use]
    pub fn accent(&self, text: &str) -> String {
        self.ink(RUST, false, text)
    }

    /// Moss — a zero unused count, the finished-scan check.
    #[must_use]
    pub fn ok(&self, text: &str) -> String {
        self.ink(MOSS, true, text)
    }

    fn ink(&self, ink: Ink, bold: bool, text: &str) -> String {
        let weight = if bold { "1;" } else { "" };
        match self.depth {
            Depth::Off => text.to_string(),
            Depth::Xterm256 => self.wrap(&format!("{weight}38;5;{}", ink.xterm), text),
            Depth::TrueColor => {
                let (r, g, b) = ink.rgb;
                self.wrap(&format!("{weight}38;2;{r};{g};{b}"), text)
            }
        }
    }

    fn wrap(&self, code: &str, text: &str) -> String {
        if self.is_enabled() {
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
        let on = Palette::resolve(true, false, false, false, false);
        assert!(on.is_enabled());
        assert!(
            !Palette::resolve(false, false, false, false, true).is_enabled(),
            "not a TTY"
        );
        assert!(
            !Palette::resolve(true, true, false, false, true).is_enabled(),
            "--no-color"
        );
        assert!(
            !Palette::resolve(true, false, true, false, true).is_enabled(),
            "NO_COLOR"
        );
        assert!(
            !Palette::resolve(true, false, false, true, true).is_enabled(),
            "TERM=dumb"
        );
    }

    #[test]
    fn plain_never_styles() {
        let palette = Palette::plain();
        for style in [
            Palette::bold,
            Palette::dim,
            Palette::muted,
            Palette::alert,
            Palette::warn,
            Palette::accent,
            Palette::ok,
        ] {
            assert_eq!(style(&palette, "x"), "x");
        }
    }

    #[test]
    fn styling_wraps_in_ansi_codes() {
        let palette = Palette::resolve(true, false, false, false, false);
        assert_eq!(palette.bold("x"), "\x1b[1mx\x1b[0m");
        assert_eq!(palette.alert("error:"), "\x1b[1;38;5;131merror:\x1b[0m");
    }

    #[test]
    fn truecolor_terminals_get_the_exact_inks() {
        let palette = Palette::resolve(true, false, false, false, true);
        assert_eq!(palette.warn("41"), "\x1b[1;38;2;194;112;61m41\x1b[0m");
        assert_eq!(palette.dim("←"), "\x1b[38;2;110;105;98m←\x1b[0m");
    }
}
