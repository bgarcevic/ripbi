# CLI UX Guidelines

Condensed from the [Command Line Interface Guidelines](https://clig.dev) (Prasad, Firshman,
Tashian, Parish). These govern `ripbi-cli` design decisions.

## Philosophy

- **Human-first:** if a command is used primarily by humans, design it for humans first.
- **Composable:** your tool *will* become part of a larger system (pipes, CI/CD). Behave
  well: stdin/stdout/stderr, exit codes, plain line-based text, JSON when structure helps.
- **Consistent:** follow existing CLI conventions so behavior is guessable. Break
  convention only deliberately, when it demonstrably harms usability.
- **Say (just) enough:** silence looks broken; walls of debug output drown the signal.
- **Discoverable:** help text, examples, and next-command suggestions teach as users go.
- **Conversational:** CLI use is a dialogue — suggest corrections, show intermediate
  state, confirm before scary actions, support dry runs.
- **Robust (and feeling robust):** handle unexpected input gracefully, be idempotent,
  keep the user informed, never print raw stack traces by default.

## The basics (non-negotiable)

- Use an argument-parsing library (Rust: `clap`); don't hand-roll.
- Exit `0` on success, non-zero on failure; map distinct codes to major failure modes.
- Primary/machine-readable output → `stdout`. Messages, logs, errors → `stderr`.

## Help

- `-h`, `--help`, and running with no required args all show help. Never overload `-h`.
- No-args help is concise: what it does, one or two examples, common flags, and a pointer
  to `--help`. Full `--help` is comprehensive.
- Lead with examples; put the most common flags/commands first; use bold headings.
- For `git`-like tools, also support `myapp help [subcommand]` and per-subcommand `--help`.
- Include a support/issues link and link to web docs.
- On a typo, suggest the correction ("Did you mean X?") — offer, don't auto-run.
- If expecting piped input but `stdin` is a TTY, print help and exit instead of hanging.

## Output

- Detect whether each stream is a TTY; format for humans when it is.
- Provide `--plain` (one record per line, for grep/awk) and `--json` (structured).
- Show brief output on success; offer `-q`/`--quiet` for scripts.
- If you change state, say exactly what changed; make current state easy to inspect.
- Suggest the next command in a workflow.
- Any action crossing the program boundary (network calls, touching files not passed as
  args) should be explicit.
- Color: use sparingly and intentionally. Disable when the stream is not a TTY, when
  `NO_COLOR` is set, `TERM=dumb`, or `--no-color` is passed. No animations when `stdout`
  isn't a TTY (keeps CI logs clean).
- Don't print developer-only diagnostics or log-level labels by default — verbose mode only.
- Page long output through `less -FIRX` only when interactive.

## Errors

- Catch expected errors and rewrite them for humans, with the fix:
  "Can't write to file.txt. Try: chmod +w file.txt".
- Guard signal-to-noise: group repeated errors under one header; put the most important
  information last; use red sparingly.
- For unexpected errors, provide debug info (or write it to a file) plus an easy,
  pre-populated bug-report path.

## Arguments and flags

- Prefer flags to positional args; every flag has a full-length `--name`; reserve
  one-letter flags for the most common options.
- Multiple args are fine for the same kind of thing (`rm a b c`); two-plus args for
  *different* things is a smell (exception: `cp <src> <dst>`).
- Use standard names: `-a/--all`, `-d/--debug`, `-f/--force`, `--json`, `-h/--help`,
  `-n/--dry-run`, `--no-input`, `-o/--output`, `-q/--quiet`, `--version`.
- Default behavior should be right for most users; don't hide the good path behind a flag.
- Prompt for missing input, but never *require* a prompt — always allow flags/args, and
  skip prompting when `stdin` isn't a TTY.
- Confirm destructive actions, scaled to danger: mild = maybe; moderate = y/n prompt +
  dry-run option; severe = type the resource name (scriptable via `--confirm=<name>`).
- Support `-` for stdin/stdout when input/output is a file.
- Make flag order independent of subcommand position where possible.
- Never accept secrets via flags or env vars (they leak into `ps`, history, logs). Use
  `--password-file`, stdin, or another IPC mechanism.

## Interactivity

- Prompt only when `stdin` is a TTY; honor `--no-input` by failing with the flag to pass.
- Never echo passwords; always let Ctrl-C work, even during network I/O.

## Subcommands

- Be consistent across subcommands (same flag names, same output shapes).
- For two-level commands prefer `noun verb` (`docker container create`); keep verbs
  consistent across nouns.
- Avoid ambiguous pairs like `update`/`upgrade`.

## Robustness

- Validate input early; fail with understandable errors.
- Respond in <100ms; print something before slow network calls; show progress (with
  motion or ETA) for long operations. If errors occur behind a progress bar, print logs.
- Set and allow configuring network timeouts; make operations recoverable (re-run picks
  up where it left off) and ideally crash-only (no cleanup needed).
- Expect misuse: scripts, flaky networks, parallel instances, case-insensitive filesystems.

## Signals

- On Ctrl-C, say something immediately and exit fast; time-box cleanup. A second Ctrl-C
  skips cleanup (tell the user what that means).

## Configuration & environment variables

- Per-invocation settings → flags. Per-user/per-machine → flags + env vars. Per-project,
  shared by all users → a version-controlled config file.
- Precedence (high → low): flags, env vars, project config, user config, system config.
- Follow the XDG Base Directory spec (`~/.config`); don't scatter dotfiles.
- Env var names: uppercase, digits, underscores; single-line values; don't shadow POSIX
  names. Honor `NO_COLOR`, `DEBUG`, `EDITOR`, `HTTP(S)_PROXY`, `TMPDIR`, `PAGER`, etc.
- Read `.env` for directory-scoped settings, but don't use it as a real config file, and
  never read secrets from env vars.

## Future-proofing

- Subcommands, flags, and config are interfaces: keep changes additive; warn in-program
  before breaking changes; human-readable output may evolve (point scripts at
  `--plain`/`--json`).
- No catch-all default subcommand; no arbitrary subcommand abbreviations (explicit,
  stable aliases are fine).
- No time bombs: don't depend on a server that may vanish.

## Naming & distribution

- Name: simple, memorable, lowercase, short, easy to type (`curl`, not `DownloadURL`).
- Distribute as a single binary where possible; make uninstalling easy.
- Never phone home usage/crash data without explicit, well-documented consent.
