//! The one picker every command shares: `scan` choosing among several
//! projects in a folder, `fetch` choosing a workspace and then a model.
//!
//! On a terminal (stdin and stderr both TTYs) it is a filter-as-you-type
//! list — a tenant easily has 50+ workspaces. Anywhere else (piped input,
//! tests) it is a numbered list on stderr, answered with a line on stdin.

use std::io;

use crate::error::ScanError;
use crate::scan::Streams;
use crate::style::Palette;

/// Rows the terminal picker shows at once; the list scrolls past this.
const ROWS: usize = 15;

/// Asks for one of `labels` and returns its index. `title` heads the
/// numbered list; `prompt` asks the question in both forms.
pub(crate) fn pick(
    title: &str,
    prompt: &str,
    labels: &[String],
    streams: &mut Streams<'_>,
    palette: &Palette,
) -> Result<usize, ScanError> {
    if labels.is_empty() {
        return Err(ScanError::new(format!("nothing to pick from: {title}")));
    }
    if streams.stdin_is_tty && streams.stderr_is_tty {
        return pick_on_terminal(prompt, labels, streams, palette);
    }
    pick_numbered(title, prompt, labels, streams, palette)
}

/// The terminal picker. It drives the console directly (raw keys, redraws)
/// rather than `streams`, which is why only a real TTY reaches it.
fn pick_on_terminal(
    prompt: &str,
    labels: &[String],
    streams: &mut Streams<'_>,
    palette: &Palette,
) -> Result<usize, ScanError> {
    // `scan`'s stage ticker draws on stderr too. A write that leaves the line
    // open holds it off (see `progress`), and a bare `\r` does that without
    // moving anything on screen; the echo below ends the line and releases it.
    write!(streams.err, "\r")?;
    streams.err.flush()?;

    let colorful = dialoguer::theme::ColorfulTheme::default();
    let plain = dialoguer::theme::SimpleTheme;
    let theme: &dyn dialoguer::theme::Theme = if palette.is_enabled() {
        &colorful
    } else {
        &plain
    };
    let choice = dialoguer::FuzzySelect::with_theme(theme)
        .with_prompt(format!(
            "{prompt} ({} — type to filter, Esc to cancel)",
            labels.len()
        ))
        .items(labels)
        .default(0)
        .max_length(ROWS)
        .report(false)
        .interact_on_opt(&dialoguer::console::Term::stderr())
        .map_err(|error| match error {
            dialoguer::Error::IO(error) => ScanError::from(error),
        })?;
    let Some(choice) = choice else {
        return Err(ScanError::new("nothing selected"));
    };
    writeln!(streams.err, "{}: {}", prompt, palette.bold(&labels[choice]))?;
    Ok(choice)
}

/// A numbered list on stderr, a choice read from stdin.
fn pick_numbered(
    title: &str,
    prompt: &str,
    labels: &[String],
    streams: &mut Streams<'_>,
    palette: &Palette,
) -> Result<usize, ScanError> {
    writeln!(streams.err, "{}", palette.bold(title))?;
    for (index, label) in labels.iter().enumerate() {
        writeln!(streams.err, "  {}. {label}", index + 1)?;
    }
    loop {
        write!(streams.err, "{prompt} [1-{}]: ", labels.len())?;
        streams.err.flush()?;
        let Some(line) = read_line(streams.input)? else {
            return Err(ScanError::new("nothing selected"));
        };
        match line.trim().parse::<usize>() {
            Ok(choice) if (1..=labels.len()).contains(&choice) => return Ok(choice - 1),
            _ => writeln!(
                streams.err,
                "Enter a number between 1 and {}.",
                labels.len()
            )?,
        }
    }
}

/// A `[y/N]` question; anything but y/yes is no.
pub(crate) fn confirm(streams: &mut Streams<'_>, question: &str) -> Result<bool, ScanError> {
    write!(streams.err, "{question}")?;
    streams.err.flush()?;
    let line = read_line(streams.input)?.unwrap_or_default();
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// One line from `input`, without reading past it: a buffered reader would
/// swallow the answer to the next prompt. `None` at end of input.
fn read_line(input: &mut dyn io::Read) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match input.read(&mut byte)? {
            0 if bytes.is_empty() => return Ok(None),
            0 => break,
            _ if byte[0] == b'\n' => break,
            _ => bytes.push(byte[0]),
        }
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}
