//! The `search` command: find text in every DAX and Power Query expression a
//! model and its reports carry. Resolves a target the way `deps` does — a
//! model with no reports is fine — ingests through `ripbi-core`, and returns
//! the matching lines. The contract lives in `crates/ripbi-cli/docs/search.md`.

use std::io;
use std::ops::Range;
use std::path::PathBuf;

use regex::RegexBuilder;
use serde::Serialize;

use ripbi_core::{ReportModel, TabularDatabase, ingest, search};

use crate::config;
use crate::error::ScanError;
use crate::output::{KINDS, SkipNoticeOut, kind_of};
use crate::scan::{Observer, Stage, Target, TargetInput, dedupe, resolve_target, skip_notice_out};

/// Everything one search is asked to do.
#[derive(Debug, Clone, Default)]
pub struct SearchRequest {
    /// The working directory: discovery, `ripbi.toml`, and relative paths
    /// resolve against it.
    pub cwd: PathBuf,
    /// The text to find: a substring, or a regular expression under `regex`.
    pub pattern: String,
    /// Read `pattern` as a regular expression.
    pub regex: bool,
    /// Match case exactly; the default folds case, as DAX does.
    pub case_sensitive: bool,
    /// `--model`.
    pub model: Option<PathBuf>,
    /// `--report` anchors; replace the config's `reports` when non-empty.
    pub reports: Vec<PathBuf>,
    /// Search DAX only (`dax`), M only (`m`), or both when neither is set.
    pub dax: bool,
    /// See [`SearchRequest::dax`].
    pub m: bool,
    /// Keep hits whose owner is one of these kinds ([`KINDS`]).
    pub types: Vec<String>,
    /// Never prompt; fail where the picker would appear.
    pub no_input: bool,
    /// Suppress the picker along with the output.
    pub quiet: bool,
}

/// One matching line, shaped for output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hit {
    /// The owner's display id, e.g. `'Sales'[Total]`.
    pub id: String,
    /// The owner's kind: `measure`, `column`, `partition`, …
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Report path of a report-level measure.
    pub report: Option<String>,
    /// `dax` or `m`.
    pub language: &'static str,
    /// Which of the owner's expressions: `expression`, `format_string`, …
    pub property: &'static str,
    /// 1-based line within the expression.
    pub line: usize,
    /// 1-based column of the match, in characters.
    pub column: usize,
    /// The matching line.
    pub text: String,
    /// The first match's byte range within `text`.
    #[serde(skip)]
    pub span: Range<usize>,
}

/// What one search found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOutput {
    /// The pattern, as given.
    pub pattern: String,
    /// The searched model, as resolved.
    pub target: String,
    /// The matching lines, in model order then report order.
    pub hits: Vec<Hit>,
    /// Distinct expressions-owning objects with at least one hit.
    pub objects: usize,
}

/// One finished search.
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    /// The hits.
    pub output: SearchOutput,
    /// Skip notices from ingestion: an object the parser skipped was not
    /// searched.
    pub skips: Vec<SkipNoticeOut>,
}

/// Runs one search. Observer calls, in order: `pick` (discovery),
/// `stage(Model)`, `resolved`, `stage(Reports)`, `stage(Findings)`.
///
/// # Errors
/// An empty or invalid pattern, an unknown `types` entry, and config,
/// discovery, and ingestion failures.
pub fn run(
    request: &SearchRequest,
    observer: &mut dyn Observer,
) -> Result<SearchOutcome, ScanError> {
    // Usage errors fail before any ingestion work.
    validate(request).map(drop)?;
    let searcher = load(request, observer)?;
    observer.stage(Stage::Findings);
    let output = searcher.find(request)?;
    Ok(SearchOutcome {
        output,
        skips: searcher.skips,
    })
}

/// The request's matcher, once its pattern and `types` are known to be valid.
fn validate(request: &SearchRequest) -> Result<Matcher, ScanError> {
    let find = matcher(request)?;
    for kind in &request.types {
        if !KINDS.contains(&kind.as_str()) {
            return Err(
                ScanError::new(format!("--type {kind} is not an object type"))
                    .with_hint(format!("one of: {}", KINDS.join(", "))),
            );
        }
    }
    Ok(find)
}

/// A model and its reports, ingested once, so an interactive front end can
/// run one search after another without reading the files again.
#[derive(Debug)]
pub struct Searcher {
    model: TabularDatabase,
    reports: Vec<ReportModel>,
    report_paths: Vec<PathBuf>,
    target: String,
    /// Skip notices from ingestion: an object the parser skipped is not
    /// searched.
    pub skips: Vec<SkipNoticeOut>,
}

/// Resolves the target and ingests it — everything [`run`] does before
/// matching. Only the target fields of `request` are read. Observer calls, in
/// order: `pick` (discovery), `stage(Model)`, `resolved`, `stage(Reports)`.
///
/// # Errors
/// Config, discovery, and ingestion failures.
pub fn load(request: &SearchRequest, observer: &mut dyn Observer) -> Result<Searcher, ScanError> {
    let cwd = request.cwd.as_path();
    let config = config::find_in(cwd)?.map(|loaded| loaded.config);
    let extras: Vec<PathBuf> = if request.reports.is_empty() {
        config
            .as_ref()
            .map(|config| config.reports.clone())
            .unwrap_or_default()
    } else {
        request.reports.clone()
    };
    // The shared ladder, without scan's zero-report refusal: a model's own
    // expressions are searchable with no report at all.
    let resolved = resolve_target(
        TargetInput {
            model: request.model.as_deref(),
            path: None,
            config_target: config.as_ref().and_then(|config| config.target.as_deref()),
            app_mappings: config.as_ref().map_or(&[], |config| config.apps.as_slice()),
            extras: &extras,
            no_input: request.no_input,
            quiet: request.quiet,
            cwd,
        },
        observer,
    )?;
    observer.stage(Stage::Model);
    let paired = resolved.paired;
    let report_paths = dedupe(paired.reports);
    observer.resolved(&Target {
        model: &paired.model,
        reports: &report_paths,
        bound: resolved.walk.as_ref().map(|walk| &walk.bound),
        discovered: resolved.discovered.as_ref(),
    })?;

    let model = ingest::semantic_model(&paired.model).map_err(|error| {
        ScanError::new(format!("cannot ingest {}: {error}", paired.model.display()))
    })?;
    observer.stage(Stage::Reports);
    let mut skips: Vec<SkipNoticeOut> = model.skips.iter().map(skip_notice_out).collect();
    let mut reports = Vec::new();
    for path in &report_paths {
        let ingested = ingest::report(path).map_err(|error| {
            ScanError::new(format!("cannot ingest report {}: {error}", path.display()))
        })?;
        skips.extend(ingested.skips.iter().map(skip_notice_out));
        reports.push(ingested.value);
    }

    Ok(Searcher {
        model: model.value,
        reports,
        report_paths,
        target: paired.model.display().to_string(),
        skips,
    })
}

impl Searcher {
    /// The lines `request`'s pattern matches. Only the pattern and filter
    /// fields are read: the target fields were spent by [`load`].
    ///
    /// # Errors
    /// An empty or invalid pattern, and an unknown `types` entry.
    pub fn find(&self, request: &SearchRequest) -> Result<SearchOutput, ScanError> {
        let find = validate(request)?;
        let report_paths = &self.report_paths;
        let report_refs: Vec<&ReportModel> = self.reports.iter().collect();
        let both = !request.dax && !request.m;
        let found = search::search(
            &self.model,
            &report_refs,
            request.dax || both,
            request.m || both,
            &*find,
        );
        let mut hits = Vec::new();
        let mut objects = 0;
        let mut last: Option<(ripbi_core::ObjectId, Option<usize>)> = None;
        for hit in found {
            let kind = kind_of(&hit.owner);
            if !request.types.is_empty() && !request.types.iter().any(|wanted| wanted == kind) {
                continue;
            }
            // Hits arrive grouped by owner, so a change of owner is a new object.
            let owner = (hit.owner.clone(), hit.report_index);
            if last.as_ref() != Some(&owner) {
                objects += 1;
                last = Some(owner);
            }
            hits.push(Hit {
                id: hit.owner.to_string(),
                kind,
                report: hit
                    .report_index
                    .map(|index| report_paths[index].display().to_string()),
                language: hit.language.label(),
                property: hit.property,
                line: hit.line,
                column: hit.column,
                text: hit.text,
                span: hit.span,
            });
        }
        Ok(SearchOutput {
            pattern: request.pattern.clone(),
            target: self.target.clone(),
            hits,
            objects,
        })
    }
}

type Matcher = Box<dyn Fn(&str) -> Option<Range<usize>>>;

/// The line matcher the request asks for.
fn matcher(request: &SearchRequest) -> Result<Matcher, ScanError> {
    if request.pattern.is_empty() {
        return Err(ScanError::new("the search pattern is empty")
            .with_hint("pass the text to find, e.g. `ripbi search CALCULATE`"));
    }
    if request.regex {
        let regex = RegexBuilder::new(&request.pattern)
            .case_insensitive(!request.case_sensitive)
            .build()
            .map_err(|error| {
                ScanError::new(format!(
                    "\"{}\" is not a valid regular expression: {}",
                    request.pattern,
                    // The last line is the reason; the lines above it redraw
                    // the pattern.
                    error.to_string().lines().last().unwrap_or_default().trim()
                ))
                .with_hint("drop --regex to search for the text as written")
            })?;
        return Ok(Box::new(move |line| {
            regex.find(line).map(|found| found.range())
        }));
    }
    if request.case_sensitive {
        let needle = request.pattern.clone();
        return Ok(Box::new(move |line| {
            line.find(&needle).map(|start| start..start + needle.len())
        }));
    }
    Ok(Box::new(search::literal(&request.pattern)))
}

/// Writes the search JSON, schema version 1 (`docs/search.md`),
/// pretty-printed and newline-terminated.
///
/// # Errors
/// Propagates stream write failures.
pub fn write_json(out: &mut dyn io::Write, output: &SearchOutput) -> io::Result<()> {
    #[derive(Serialize)]
    struct Payload<'a> {
        schema_version: u8,
        pattern: &'a str,
        target: &'a str,
        summary: Summary,
        hits: &'a [Hit],
    }
    #[derive(Serialize)]
    struct Summary {
        hits: usize,
        objects: usize,
    }
    let payload = Payload {
        schema_version: 1,
        pattern: &output.pattern,
        target: &output.target,
        summary: Summary {
            hits: output.hits.len(),
            objects: output.objects,
        },
        hits: &output.hits,
    };
    serde_json::to_writer_pretty(&mut *out, &payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    writeln!(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(pattern: &str) -> SearchRequest {
        SearchRequest {
            pattern: pattern.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn a_plain_pattern_folds_case_and_is_never_a_regex() {
        let find = matcher(&request("sum(")).unwrap();
        assert_eq!(find("x = SUM(Sales[Amount])"), Some(4..8));
    }

    #[test]
    fn case_sensitive_matches_as_written() {
        let find = matcher(&SearchRequest {
            case_sensitive: true,
            ..request("Sum")
        })
        .unwrap();
        assert_eq!(find("SUM(x)"), None);
        assert_eq!(find("Sum(x)"), Some(0..3));
    }

    #[test]
    fn a_regex_matches_and_folds_case_by_default() {
        let find = matcher(&SearchRequest {
            regex: true,
            ..request(r"sumx?\(")
        })
        .unwrap();
        assert_eq!(find("  SUMX(Sales, 1)"), Some(2..7));
    }

    #[test]
    fn bad_patterns_are_usage_errors() {
        assert!(matcher(&request("")).is_err());
        let error = matcher(&SearchRequest {
            regex: true,
            ..request("(")
        })
        .err()
        .unwrap();
        assert!(error.message.contains("not a valid regular expression"));
        assert!(error.hint.is_some());
    }
}
