//! `scan --sarif` (issue #142): the findings as a SARIF 2.1.0 log, so GitHub
//! code scanning and Azure DevOps turn them into PR annotations and tracked
//! alerts. Another render mode over the same [`ScanOutput`] — plus, under
//! `--compare-root`, the findings that already existed in the other checkout,
//! which SARIF keeps as suppressed results instead of dropping. The contract
//! lives in `docs/output.md` under "SARIF". `scan --azure-devops` (`vso.rs`)
//! renders the same rules, messages, and sites as `##vso` logging commands.

use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};

use ripbi_core::{NameKey, ObjectId, SourceLocation};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::compare::{Entry, Key};
use crate::render::{AutoDateTimeRow, BrokenArtifactOut, BrokenOut, Finding, ScanOutput};
use crate::scan::report_name;

const SCHEMA: &str = "https://json.schemastore.org/sarif-2.1.0.json";
const INFORMATION_URI: &str = "https://bgarcevic.github.io/ripbi/";
const HELP_URI: &str = "https://bgarcevic.github.io/ripbi/output.html#sarif";
/// The `partialFingerprints` key: versioned so a future change to the
/// fingerprint opens new alerts instead of silently matching old ones.
const FINGERPRINT: &str = "ripbiFinding/v1";
/// Relative artifact URIs resolve against the checkout the scan ran in; code
/// scanning maps `%SRCROOT%` to the repository root.
const SRCROOT: &str = "%SRCROOT%";

/// One rule: the stable id, the finding kind it covers, a short
/// description, and its default level.
struct Rule {
    id: &'static str,
    name: &'static str,
    kind: &'static str,
    description: &'static str,
    level: &'static str,
}

/// Every rule, in the fixed order `ruleIndex` points into. Ids are the
/// contract: never renumber or rename one.
const RULES: &[Rule] = &[
    unused(
        "RIPBI-UNUSED-MEASURE",
        "UnusedMeasure",
        "measure",
        "Measure no report reaches",
    ),
    unused(
        "RIPBI-UNUSED-COLUMN",
        "UnusedColumn",
        "column",
        "Column no report reaches",
    ),
    unused(
        "RIPBI-UNUSED-HIERARCHY",
        "UnusedHierarchy",
        "hierarchy",
        "Hierarchy no report reaches",
    ),
    unused(
        "RIPBI-UNUSED-TABLE",
        "UnusedTable",
        "table",
        "Table no report reaches",
    ),
    unused(
        "RIPBI-UNUSED-PARTITION",
        "UnusedPartition",
        "partition",
        "Partition no report reaches",
    ),
    unused(
        "RIPBI-UNUSED-RELATIONSHIP",
        "UnusedRelationship",
        "relationship",
        "Relationship no report reaches",
    ),
    unused(
        "RIPBI-UNUSED-ROLE",
        "UnusedRole",
        "role",
        "Role no report reaches",
    ),
    unused(
        "RIPBI-UNUSED-CALCULATION-ITEM",
        "UnusedCalculationItem",
        "calculation_item",
        "Calculation item no report reaches",
    ),
    unused(
        "RIPBI-UNUSED-EXPRESSION",
        "UnusedExpression",
        "expression",
        "Shared Power Query expression no report reaches",
    ),
    unused(
        "RIPBI-UNUSED-FUNCTION",
        "UnusedFunction",
        "function",
        "DAX function no report reaches",
    ),
    unused(
        "RIPBI-UNUSED-REPORT-MEASURE",
        "UnusedReportMeasure",
        "report_measure",
        "Report-level measure no visual reaches",
    ),
    unused(
        "RIPBI-STALE-BOOKMARK",
        "StaleBookmark",
        "bookmark",
        "Bookmark whose saved pages were all deleted",
    ),
    Rule {
        id: "RIPBI-BROKEN-VISUAL",
        name: "BrokenVisualBinding",
        kind: "broken_visual",
        description: "Visual binding to a field the model no longer has",
        level: "error",
    },
    Rule {
        id: "RIPBI-BROKEN-ARTIFACT",
        name: "BrokenArtifact",
        kind: "broken_artifact",
        description: "DAX expression with references that no longer resolve",
        level: "error",
    },
    Rule {
        id: "RIPBI-AUTO-DATE-TIME",
        name: "AutoDateTime",
        kind: "auto_date_time",
        description: "Auto date/time table no report needs",
        level: "warning",
    },
];

const fn unused(
    id: &'static str,
    name: &'static str,
    kind: &'static str,
    description: &'static str,
) -> Rule {
    Rule {
        id,
        name,
        kind,
        description,
        level: "warning",
    }
}

fn rule_index(kind: &str) -> usize {
    RULES
        .iter()
        .position(|rule| rule.kind == kind)
        .expect("every finding kind has a SARIF rule")
}

/// Findings that already existed in the `--compare-root` checkout: dropped by
/// every other mode, kept by SARIF as suppressed results.
#[derive(Debug, Default)]
pub struct Existing {
    pub findings: Vec<Finding>,
    pub broken: Vec<BrokenOut>,
    pub broken_artifacts: Vec<BrokenArtifactOut>,
    pub auto_date_time: Vec<AutoDateTimeRow>,
}

/// Where a finding points: a file, relative to the scan's working directory
/// when it lies beneath it, and the declaration line when the source records
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Site {
    pub(crate) path: PathBuf,
    pub(crate) line: Option<usize>,
}

/// Resolves findings to source sites, keyed by fingerprint so the renderer can
/// look them up from the presentation data alone.
pub struct Locator {
    model: PathBuf,
    declared: HashMap<ObjectId, SourceLocation>,
    reports: Vec<PathBuf>,
    sites: HashMap<Key, Site>,
}

impl Locator {
    /// `declared` is `ripbi_core::ingest::source_locations` for the model —
    /// empty for single-file formats, which point at the file itself.
    #[must_use]
    pub fn new(model: &Path, declared: Vec<SourceLocation>, reports: &[PathBuf]) -> Self {
        Self {
            model: model.to_path_buf(),
            declared: declared
                .into_iter()
                .map(|location| (location.id.clone(), location))
                .collect(),
            reports: reports.to_vec(),
            sites: HashMap::new(),
        }
    }

    /// Records where the model object or report item `id` of `entry` lives;
    /// `report_index` names the report of a report-level object when known.
    pub fn object(&mut self, entry: &Entry, id: &ObjectId, report_index: Option<usize>) {
        let site = match id {
            ObjectId::Bookmark { report_index, .. } => self.report_site(Some(*report_index)),
            ObjectId::ReportMeasure { .. } => self.report_site(report_index),
            _ => self.model_site(id),
        };
        self.sites.insert(entry.key(), site);
    }

    /// Records where a broken binding lives: its report, found by name.
    pub fn binding(&mut self, entry: &Entry, report: Option<&NameKey>) {
        let index = report.and_then(|name| {
            self.reports
                .iter()
                .position(|path| NameKey::new(report_name(path)) == *name)
        });
        let site = self.report_site(index);
        self.sites.insert(entry.key(), site);
    }

    fn model_site(&self, id: &ObjectId) -> Site {
        match self.declared.get(id) {
            Some(location) => Site {
                path: location.path.clone(),
                line: Some(location.line),
            },
            None => self.model_file(),
        }
    }

    /// The model as a file: the model file itself, or a TMDL folder's
    /// `model.tmdl`, so a result with no recorded declaration still lands on
    /// a file.
    fn model_file(&self) -> Site {
        let model_tmdl = [
            self.model.join("definition").join("model.tmdl"),
            self.model.join("model.tmdl"),
        ]
        .into_iter()
        .find(|path| path.is_file());
        Site {
            path: model_tmdl.unwrap_or_else(|| self.model.clone()),
            line: None,
        }
    }

    /// A report as a file: the archive itself, or a PBIR folder's
    /// `report.json`. An unknown report falls back to the first one.
    fn report_site(&self, index: Option<usize>) -> Site {
        let Some(report) = self.reports.get(index.unwrap_or(0)) else {
            return self.model_file();
        };
        let path = [
            report.join("definition").join("report.json"),
            report.join("report.json"),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| report.clone());
        Site { path, line: None }
    }

    fn site(&self, entry: &Entry) -> Option<&Site> {
        self.sites.get(&entry.key())
    }
}

/// One result, format-neutral: the SARIF log and the Azure DevOps `##vso`
/// lines both render it.
pub(crate) struct Issue<'a> {
    pub(crate) rule: usize,
    pub(crate) message: String,
    pub(crate) name: &'a str,
    pub(crate) site: Option<&'a Site>,
    pub(crate) fingerprint: String,
    /// Already in the `--compare-root` checkout.
    pub(crate) existing: bool,
}

impl Issue<'_> {
    pub(crate) fn rule_id(&self) -> &'static str {
        RULES[self.rule].id
    }

    pub(crate) fn level(&self) -> &'static str {
        RULES[self.rule].level
    }
}

/// Every result of one scan: the reported findings, then the `existing` ones.
pub(crate) fn issues<'a>(
    report: &'a ScanOutput,
    existing: &'a Existing,
    locator: &'a Locator,
) -> Vec<Issue<'a>> {
    let mut issues = Vec::new();
    for (group, is_existing) in [
        (Group::of(report), false),
        (Group::of_existing(existing), true),
    ] {
        let context = Context {
            locator,
            existing: is_existing,
        };
        for finding in group.findings {
            issues.push(context.finding(finding));
        }
        // An in-use table is advice, not a finding: nothing to annotate.
        for row in group
            .auto_date_time
            .iter()
            .filter(|row| row.verdict != "in_use")
        {
            issues.push(context.auto_date_time(row));
        }
        for binding in group.broken {
            issues.push(context.broken(binding));
        }
        for artifact in group.broken_artifacts {
            issues.push(context.artifact(artifact));
        }
    }
    issues
}

/// Writes the SARIF 2.1.0 log for one scan. `cwd` anchors the relative
/// artifact URIs; `existing` holds the `--compare-root` findings to emit as
/// suppressed.
///
/// # Errors
/// Propagates stream write failures.
pub fn write(
    out: &mut dyn io::Write,
    report: &ScanOutput,
    existing: &Existing,
    locator: &Locator,
    cwd: &Path,
) -> io::Result<()> {
    let suppression = report.compare.as_ref().map(|compare| {
        json!([{
            "kind": "external",
            "justification": format!("already in {}", compare.root),
        }])
    });
    let results: Vec<Value> = issues(report, existing, locator)
        .iter()
        .map(|issue| sarif_result(issue, cwd, suppression.as_ref()))
        .collect();

    let rules: Vec<Value> = RULES
        .iter()
        .map(|rule| {
            json!({
                "id": rule.id,
                "name": rule.name,
                "shortDescription": { "text": rule.description },
                "helpUri": HELP_URI,
                "defaultConfiguration": { "level": rule.level },
            })
        })
        .collect();
    let log = Log {
        schema: SCHEMA,
        version: "2.1.0",
        runs: [json!({
            "tool": {
                "driver": {
                    "name": "ripbi",
                    "version": env!("CARGO_PKG_VERSION"),
                    "informationUri": INFORMATION_URI,
                    "rules": rules,
                }
            },
            "columnKind": "unicodeCodePoints",
            "results": results,
        })],
    };
    serde_json::to_writer_pretty(&mut *out, &log)?;
    writeln!(out)
}

#[derive(Serialize)]
struct Log {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: [Value; 1],
}

/// One set of results — reported, or already existing.
struct Group<'a> {
    findings: Vec<&'a Finding>,
    auto_date_time: &'a [AutoDateTimeRow],
    broken: &'a [BrokenOut],
    broken_artifacts: &'a [BrokenArtifactOut],
}

impl<'a> Group<'a> {
    fn of(report: &'a ScanOutput) -> Self {
        Self {
            findings: report.findings.iter().collect(),
            auto_date_time: &report.auto_date_time,
            broken: &report.broken,
            broken_artifacts: &report.broken_artifacts,
        }
    }

    fn of_existing(existing: &'a Existing) -> Self {
        Self {
            findings: existing.findings.iter().collect(),
            auto_date_time: &existing.auto_date_time,
            broken: &existing.broken,
            broken_artifacts: &existing.broken_artifacts,
        }
    }
}

struct Context<'a> {
    locator: &'a Locator,
    existing: bool,
}

impl<'a> Context<'a> {
    fn finding(&self, finding: &'a Finding) -> Issue<'a> {
        let entry = Entry::unused(finding.kind, &finding.id);
        let label = finding.kind.replace('_', " ");
        let mut message = if finding.kind == "bookmark" {
            format!("Stale {}: every page it saved was deleted.", finding.id)
        } else {
            format!(
                "Unused {label} {}: no report reaches it.",
                bare_id(&finding.id)
            )
        };
        append_size(&mut message, finding);
        append_used_by(&mut message, finding);
        self.issue(&entry, finding.kind, message, &finding.id)
    }

    fn auto_date_time(&self, row: &'a AutoDateTimeRow) -> Issue<'a> {
        let entry = Entry::auto_date_time(&row.id, row.verdict);
        let source = row
            .source_column
            .as_deref()
            .map(|column| format!(" for {column}"))
            .unwrap_or_default();
        let verdict = match row.verdict {
            "dead" => "is dead: nothing reaches it; disable auto date/time",
            "unused_by_reports" => "is unused by reports; disable auto date/time",
            _ => "is in use; replace it with a real date table",
        };
        let mut message = format!("Auto date/time {}{source} {verdict}.", row.id);
        if let Some(finding) = &row.finding {
            append_size(&mut message, finding);
        }
        self.issue(&entry, "auto_date_time", message, &row.id)
    }

    fn broken(&self, binding: &'a BrokenOut) -> Issue<'a> {
        let entry = Entry::broken_visual(&binding.target, binding.reason, &binding.provenance);
        let message = format!(
            "Broken visual binding {}: {} ({}).",
            binding.target,
            crate::render::reason_phrase(binding),
            binding.provenance
        );
        self.issue(&entry, "broken_visual", message, &binding.target)
    }

    fn artifact(&self, artifact: &'a BrokenArtifactOut) -> Issue<'a> {
        let entry = Entry::broken_artifact(
            &artifact.id,
            artifact
                .report
                .as_deref()
                .map(|path| report_name(Path::new(path))),
        );
        let message = format!(
            "{} has references that no longer resolve: {}.",
            artifact.id,
            artifact.unresolved_references.join(", ")
        );
        self.issue(&entry, "broken_artifact", message, &artifact.id)
    }

    fn issue(&self, entry: &Entry, kind: &str, message: String, name: &'a str) -> Issue<'a> {
        Issue {
            rule: rule_index(kind),
            message,
            name,
            site: self.locator.site(entry),
            fingerprint: fingerprint(entry),
            existing: self.existing,
        }
    }
}

fn sarif_result(issue: &Issue, cwd: &Path, suppression: Option<&Value>) -> Value {
    let mut location = json!({
        "logicalLocations": [{
            "fullyQualifiedName": issue.name,
            "kind": RULES[issue.rule].kind,
        }],
    });
    if let Some(site) = issue.site {
        let mut physical = json!({});
        let mut artifact = json!({});
        match relative_uri(&site.path, cwd) {
            Some(uri) => {
                artifact["uri"] = json!(uri);
                artifact["uriBaseId"] = json!(SRCROOT);
            }
            None => artifact["uri"] = json!(absolute_uri(&site.path, cwd)),
        }
        physical["artifactLocation"] = artifact;
        if let Some(line) = site.line {
            physical["region"] = json!({ "startLine": line });
        }
        location["physicalLocation"] = physical;
    }
    let mut result = json!({
        "ruleId": issue.rule_id(),
        "ruleIndex": issue.rule,
        "level": issue.level(),
        "message": { "text": issue.message },
        "locations": [location],
        "partialFingerprints": { FINGERPRINT: issue.fingerprint },
    });
    if issue.existing
        && let Some(suppressions) = suppression
    {
        result["suppressions"] = suppressions.clone();
    }
    result
}

/// An id without the kind word the human modes prefix (`table 'Sales'` →
/// `'Sales'`), since the message already names the kind.
fn bare_id(id: &str) -> &str {
    const PREFIXES: &[&str] = &[
        "table ",
        "hierarchy ",
        "partition ",
        "relationship ",
        "role ",
        "calculation item ",
        "expression ",
        "function ",
        "report measure ",
    ];
    PREFIXES
        .iter()
        .find_map(|prefix| id.strip_prefix(prefix))
        .unwrap_or(id)
}

fn append_size(message: &mut String, finding: &Finding) {
    if let Some(bytes) = finding.storage.and_then(|stats| stats.bytes) {
        message.push_str(&format!(
            " It holds {} of storage.",
            crate::render::format_bytes(bytes)
        ));
    }
}

fn append_used_by(message: &mut String, finding: &Finding) {
    if finding.used_by.is_empty() {
        return;
    }
    let names: Vec<String> = finding
        .used_by
        .iter()
        .map(|used| {
            if used.also_unused {
                format!("{} (also unused)", used.id)
            } else {
                used.id.clone()
            }
        })
        .collect();
    message.push_str(&format!(" Referenced only by {}.", names.join(", ")));
}

/// The comparison fingerprint `--compare-root` matches on, hashed so the
/// value is opaque and free of the key's separator characters.
fn fingerprint(entry: &Entry) -> String {
    let digest = Sha256::digest(entry.key().as_str().as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `path` relative to `cwd` as a URI reference with `/` separators, when it
/// lies beneath `cwd`.
fn relative_uri(path: &Path, cwd: &Path) -> Option<String> {
    let segments: Vec<String> = relative_segments(path, cwd)?
        .iter()
        .map(|segment| encode(segment))
        .collect();
    Some(segments.join("/"))
}

/// `path` relative to `cwd`, one entry per component, when it lies beneath
/// `cwd`.
pub(crate) fn relative_segments(path: &Path, cwd: &Path) -> Option<Vec<String>> {
    let absolute = normalize(&cwd.join(path));
    let relative = absolute.strip_prefix(normalize(cwd)).ok()?;
    let segments: Vec<String> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!segments.is_empty()).then_some(segments)
}

/// A `file://` URI for a path outside the working directory.
fn absolute_uri(path: &Path, cwd: &Path) -> String {
    let absolute = normalize(&cwd.join(path));
    let text = absolute.to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    let segments: Vec<String> = text.split('/').map(encode).collect();
    let joined = segments.join("/");
    if joined.starts_with('/') {
        format!("file://{joined}")
    } else {
        format!("file:///{joined}")
    }
}

/// Lexically removes `.` and resolves `..` — no filesystem access, so a
/// path's spelling (and a Windows drive's) stays as the user gave it.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Percent-encodes one path segment: everything but RFC 3986 unreserved
/// characters and the sub-delimiters a path may carry. A drive letter's `:`
/// is kept too.
fn encode(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_finding_kind_has_a_rule() {
        for kind in crate::render::KINDS {
            rule_index(kind);
        }
        for kind in ["broken_visual", "broken_artifact", "auto_date_time"] {
            rule_index(kind);
        }
    }

    #[test]
    fn rule_ids_are_unique() {
        let mut ids: Vec<&str> = RULES.iter().map(|rule| rule.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), RULES.len());
    }

    #[test]
    fn uris_are_relative_and_encoded_beneath_the_working_directory() {
        let cwd = Path::new("/repo");
        assert_eq!(
            relative_uri(
                Path::new("/repo/./Sales Model.SemanticModel/definition/tables/Sales.tmdl"),
                cwd
            )
            .as_deref(),
            Some("Sales%20Model.SemanticModel/definition/tables/Sales.tmdl")
        );
        assert_eq!(
            relative_uri(Path::new("sub/../model.bim"), cwd).as_deref(),
            Some("model.bim")
        );
        assert_eq!(relative_uri(Path::new("/elsewhere/model.bim"), cwd), None);
    }

    #[test]
    fn bare_id_strips_only_the_kind_word() {
        assert_eq!(bare_id("table 'Sales'"), "'Sales'");
        assert_eq!(bare_id("'Sales'[Amount]"), "'Sales'[Amount]");
    }
}
