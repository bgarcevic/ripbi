//! The stage ticker `scan`, `report`, and `deps` share: one transient stderr
//! line that names the command's pipeline — `◐ discover · model · reports ·
//! graph · findings  0.8s` — with finished stages muted, the live one in the accent,
//! and upcoming ones dim. It closes into a single `✓ scanned <model> in 1.4s`
//! line, the verb and stages coming from the command's [`Pipeline`].
//!
//! Motion follows `docs/cli-ux-guidelines.md`: only when both stdout and
//! stderr are terminals, the stderr palette is on (so `NO_COLOR`, `TERM=dumb`,
//! and `--no-color` all stop it), and the run is in human mode without `-q`.
//! The line only appears after 120 ms, so a fast scan never flickers.
//!
//! The ticker starts before target resolution, so a slow discovery walk (a
//! `--report` folder holding dozens of PBIR reports) has motion too. The
//! ticker thread draws on the process's stderr directly; each command swaps
//! its stderr for [`Progress::writer`] for the whole run, which erases the
//! transient line before every write, so notes and the ticker never
//! interleave. A write that leaves a line open — the project picker's
//! `Select a project [1-N]: ` prompt — holds the ticker off until a later
//! write ends the line, so it never draws over the user's typing.

use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::scan::Streams;
use crate::style::Palette;

/// One command's ticker vocabulary: its stages in pipeline order (target
/// resolution first), and the past-tense verb of the closing line.
#[derive(Debug, Clone, Copy)]
pub struct Pipeline {
    pub stages: &'static [&'static str],
    pub verb: &'static str,
}

/// `ripbi scan`.
pub const SCAN: Pipeline = Pipeline {
    stages: &["discover", "model", "reports", "graph", "findings"],
    verb: "scanned",
};

/// `ripbi report`. A model-less inventory simply never lights `model`.
pub const REPORT: Pipeline = Pipeline {
    stages: &["discover", "model", "reports", "graph", "inventory"],
    verb: "inventoried",
};

/// `ripbi deps`.
pub const DEPS: Pipeline = Pipeline {
    stages: &["discover", "model", "reports", "graph", "slice"],
    verb: "traced",
};

/// How long a scan runs before the ticker shows up.
const DELAY: Duration = Duration::from_millis(120);
/// Redraw interval.
const TICK: Duration = Duration::from_millis(90);
/// The spinner glyph: a quarter-lit circle turning.
const FRAMES: [&str; 4] = ["◐", "◓", "◑", "◒"];
/// Erase the current line and return the cursor to column 0.
const CLEAR: &str = "\r\x1b[2K";

/// What the ticker thread and the scan's own stderr writes share.
struct State {
    stage: usize,
    /// The ticker's line is on screen and must be erased before other output.
    drawn: bool,
    /// The scan's last stderr write did not end a line; drawing now would
    /// split it, so the ticker waits.
    mid_line: bool,
}

struct Active {
    pipeline: Pipeline,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    started: Instant,
    palette: Palette,
}

/// A running (or disabled) stage ticker. Disabled tickers are inert: every
/// method is a no-op and [`Progress::writer`] passes writes straight through.
pub struct Progress {
    active: Option<Active>,
}

impl Progress {
    /// The motion gate every command applies (see the module docs): both
    /// streams are terminals, the stderr palette is on, and the run is in
    /// human mode (`machine` is `--json`/`--plain`) without `-q`.
    #[must_use]
    pub fn wanted(
        streams: &Streams<'_>,
        palette_err: &Palette,
        quiet: bool,
        machine: bool,
    ) -> bool {
        streams.stdout_is_tty && palette_err.is_enabled() && !quiet && !machine
    }

    /// A ticker that never draws.
    #[must_use]
    pub fn disabled() -> Self {
        Self { active: None }
    }

    /// Starts the ticker on the process's stderr when `enabled`; see the
    /// module docs for the gate the caller computes.
    #[must_use]
    pub fn start(enabled: bool, palette: Palette, pipeline: Pipeline) -> Self {
        if !enabled {
            return Self::disabled();
        }
        let state = Arc::new(Mutex::new(State {
            stage: 0,
            drawn: false,
            mid_line: false,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let handle = {
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            thread::spawn(move || tick(&state, &stop, started, palette, pipeline.stages))
        };
        Self {
            active: Some(Active {
                state,
                stop,
                handle: Some(handle),
                pipeline,
                started,
                palette,
            }),
        }
    }

    /// Moves the ticker to `stage` (an index into the pipeline's stages).
    pub fn stage(&self, stage: usize) {
        if let Some(active) = &self.active {
            lock(&active.state).stage = stage.min(active.pipeline.stages.len() - 1);
        }
    }

    /// A stderr writer that erases the ticker line before each write.
    /// It shares the ticker's state rather than borrowing the ticker, so it
    /// can outlive [`Progress::finish`] (it then passes writes through).
    pub fn writer<'a>(&self, inner: &'a mut dyn Write) -> ProgressWriter<'a> {
        ProgressWriter {
            inner,
            state: self.active.as_ref().map(|active| Arc::clone(&active.state)),
        }
    }

    /// Stops the ticker and erases its line; with `done`, prints the closing
    /// `✓ <verb> <what> in <elapsed>` line in its place.
    pub fn finish(mut self, err: &mut dyn Write, done: Option<&str>) -> io::Result<()> {
        let Some(active) = self.active.take() else {
            return Ok(());
        };
        let elapsed = active.started.elapsed();
        let palette = active.palette;
        let verb = active.pipeline.verb;
        let drawn = active.shut_down();
        // Like a frame, one write: the check line overwrites the ticker in
        // place instead of blanking it first.
        let mut text = String::from(if drawn { "\r" } else { "" });
        if let Some(what) = done {
            text.push_str(&format!(
                "{} {verb} {what} {}\x1b[K\n",
                palette.ok("✓"),
                palette.dim(&format!("in {}", format_elapsed(elapsed)))
            ));
        } else if drawn {
            text.push_str("\x1b[K");
        }
        err.write_all(text.as_bytes())?;
        err.flush()
    }
}

impl Drop for Progress {
    /// An early return (an error mid-scan) must not leave the thread
    /// drawing over the error message.
    fn drop(&mut self) {
        if let Some(active) = self.active.take()
            && active.shut_down()
        {
            let _ = write!(io::stderr(), "{CLEAR}");
        }
    }
}

impl Active {
    /// Joins the thread; returns whether its line is still on screen. The
    /// caller takes over that line, so the shared flag is cleared: a
    /// [`ProgressWriter`] must not erase it a second time.
    fn shut_down(mut self) -> bool {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        std::mem::take(&mut lock(&self.state).drawn)
    }
}

/// Wraps the scan's stderr while the ticker runs.
pub struct ProgressWriter<'a> {
    inner: &'a mut dyn Write,
    state: Option<Arc<Mutex<State>>>,
}

impl Write for ProgressWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Some(state) = &self.state else {
            return self.inner.write(buf);
        };
        // Held across the write: the ticker cannot redraw between the
        // erase and the text.
        let mut state = lock(state);
        if state.drawn {
            self.inner.write_all(CLEAR.as_bytes())?;
            state.drawn = false;
        }
        let written = self.inner.write(buf)?;
        if written > 0 {
            state.mid_line = buf[written - 1] != b'\n';
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // A poisoned lock only means a writer panicked mid-line; the state is
    // still a valid pair of flags.
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn tick(
    state: &Mutex<State>,
    stop: &AtomicBool,
    started: Instant,
    palette: Palette,
    stages: &[&str],
) {
    let mut frame = 0;
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(TICK);
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let elapsed = started.elapsed();
        if elapsed < DELAY {
            continue;
        }
        let mut state = lock(state);
        if state.mid_line {
            continue;
        }
        // One write per frame: stderr is unbuffered, so `write!` would hand
        // the terminal the return, the erase, and each styled piece
        // separately, and it would paint the blank line and the cursor's
        // trip back to column 0. Overwrite in place, then erase only the
        // tail the previous (longer) frame may have left.
        let frame_text = format!(
            "\r{}\x1b[K",
            render_line(stages, state.stage, frame, elapsed, &palette)
        );
        let mut err = io::stderr().lock();
        if err
            .write_all(frame_text.as_bytes())
            .and_then(|()| err.flush())
            .is_ok()
        {
            state.drawn = true;
        }
        frame = (frame + 1) % FRAMES.len();
    }
}

/// `◐ model · reports · graph · findings  0.8s`, styled by stage position.
fn render_line(
    stages: &[&str],
    stage: usize,
    frame: usize,
    elapsed: Duration,
    palette: &Palette,
) -> String {
    let stages: Vec<String> = stages
        .iter()
        .enumerate()
        .map(|(index, name)| match index.cmp(&stage) {
            std::cmp::Ordering::Less => palette.muted(name),
            std::cmp::Ordering::Equal => palette.warn(name),
            std::cmp::Ordering::Greater => palette.dim(name),
        })
        .collect();
    format!(
        "{} {}  {}",
        palette.accent(FRAMES[frame]),
        stages.join(&palette.dim(" · ")),
        palette.dim(&format_elapsed(elapsed))
    )
}

/// The closing line's name for an input: its file or folder name
/// (`Sales.SemanticModel`), the whole path only when it has none.
#[must_use]
pub fn item_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// `0.8s`, `12.4s`, `2m 05s`.
fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        let whole = elapsed.as_secs();
        format!("{}m {:02}s", whole / 60, whole % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disabled_ticker_passes_writes_through() {
        let progress = Progress::disabled();
        let mut buffer = Vec::new();
        writeln!(progress.writer(&mut buffer), "Note: hello").unwrap();
        progress.finish(&mut buffer, Some("Sales")).unwrap();
        assert_eq!(String::from_utf8(buffer).unwrap(), "Note: hello\n");
    }

    #[test]
    fn the_line_names_every_stage_in_order() {
        let line = render_line(
            SCAN.stages,
            1,
            0,
            Duration::from_millis(800),
            &Palette::plain(),
        );
        assert_eq!(
            line,
            "◐ discover · model · reports · graph · findings  0.8s"
        );
    }

    #[test]
    fn a_write_erases_a_drawn_line_first() {
        let state = Arc::new(Mutex::new(State {
            stage: 0,
            drawn: true,
            mid_line: false,
        }));
        let mut buffer = Vec::new();
        {
            let mut writer = ProgressWriter {
                inner: &mut buffer,
                state: Some(Arc::clone(&state)),
            };
            write!(writer, "Note: partial").unwrap();
        }
        assert_eq!(
            String::from_utf8(buffer).unwrap(),
            format!("{CLEAR}Note: partial")
        );
        let state = lock(&state);
        assert!(!state.drawn);
        assert!(state.mid_line, "an unfinished line holds the ticker off");
    }

    #[test]
    fn elapsed_switches_to_minutes() {
        assert_eq!(format_elapsed(Duration::from_millis(1440)), "1.4s");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2m 05s");
    }
}
