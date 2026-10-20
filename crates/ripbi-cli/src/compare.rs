//! Finding fingerprints for `scan --compare-root` (issue #141): the same scan
//! runs in another checkout, and only findings that did not exist there are
//! reported and gate the exit code.
//!
//! A fingerprint is the finding's identity as the output modes already print
//! it — kind plus display id, and for breakage the reason and binding site —
//! never a file path or position, so two checkouts in different folders
//! compare cleanly. Matching is case-insensitive, like the engine's own name
//! comparison. The contract lives in `docs/scan/compare.md`.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;

/// One finding's fingerprint: its kind, its display id, and the extra fields
/// that separate two findings sharing an id (a broken binding's reason and
/// site, an auto date/time table's verdict, a report measure's report).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Entry {
    /// The finding's machine kind: an object kind (`measure`, `column`, …),
    /// `broken_visual`, `broken_artifact`, or `auto_date_time`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The display id (`'Sales'[Amount]`), or a broken binding's written target.
    pub id: String,
    /// A broken binding's reason code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// A broken binding's site phrase.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    /// The report item name of a report-level broken artifact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    /// An auto date/time table's verdict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
}

/// A fingerprint's comparison form: every field, case-folded, joined by a
/// separator no display id contains.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(String);

impl Key {
    /// The folded, separator-joined form — what `scan --sarif` hashes into its
    /// `partialFingerprints`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Every finding one scan detected, keyed for comparison and ordered by key
/// so anything listed from it is deterministic.
pub type Detected = BTreeMap<Key, Entry>;

impl Entry {
    fn new(kind: &str, id: &str) -> Self {
        Self {
            kind: kind.to_string(),
            id: id.to_string(),
            reason: None,
            provenance: None,
            report: None,
            verdict: None,
        }
    }

    /// An unused object of `kind`.
    #[must_use]
    pub fn unused(kind: &str, id: &str) -> Self {
        Self::new(kind, id)
    }

    /// A broken visual binding.
    #[must_use]
    pub fn broken_visual(target: &str, reason: &str, provenance: &str) -> Self {
        Self {
            reason: Some(reason.to_string()),
            provenance: Some(provenance.to_string()),
            ..Self::new("broken_visual", target)
        }
    }

    /// A DAX owner with unresolved references; `report` names the report item
    /// of a report-level measure.
    #[must_use]
    pub fn broken_artifact(id: &str, report: Option<String>) -> Self {
        Self {
            report,
            ..Self::new("broken_artifact", id)
        }
    }

    /// An auto date/time table whose verdict gates the exit code.
    #[must_use]
    pub fn auto_date_time(id: &str, verdict: &str) -> Self {
        Self {
            verdict: Some(verdict.to_string()),
            ..Self::new("auto_date_time", id)
        }
    }

    /// The comparison key: two entries match exactly when their keys do.
    #[must_use]
    pub fn key(&self) -> Key {
        const SEP: char = '\u{1f}';
        let optional = |field: &Option<String>| field.as_deref().unwrap_or("").to_string();
        Key(format!(
            "{}{SEP}{}{SEP}{}{SEP}{}{SEP}{}{SEP}{}",
            self.kind,
            self.id,
            optional(&self.reason),
            optional(&self.provenance),
            optional(&self.report),
            optional(&self.verdict),
        )
        .to_lowercase())
    }
}

/// Records `entry` as detected.
pub fn insert(detected: &mut Detected, entry: &Entry) {
    detected.entry(entry.key()).or_insert_with(|| entry.clone());
}

impl Entry {
    /// Whether the finding is a fact about report usage — an unused object or
    /// an auto date/time verdict — rather than about breakage, which the model
    /// alone decides. Only usage findings need connected reports to be judged.
    #[must_use]
    pub fn about_usage(&self) -> bool {
        !matches!(self.kind.as_str(), "broken_visual" | "broken_artifact")
    }

    /// The object a usage finding is about — kind and id, folded, without the
    /// verdict, which itself depends on usage.
    fn subject(&self) -> String {
        format!("{}\u{1f}{}", self.kind, self.id).to_lowercase()
    }
}

/// How much of the other checkout's scan the comparison can trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// The scan ran against connected reports: every finding compares.
    Scanned,
    /// The model was there, but no report was connected to it (or only
    /// `stub-report` stubs), so its usage was never checked: an unused object
    /// that existed there is unverified, not new.
    Skipped,
    /// The scan could not run there at all (a model this change adds): every
    /// finding is new.
    Unavailable,
}

/// One finding's standing against the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Not in the baseline: reported, and gates.
    New,
    /// Detected by the baseline too: dropped, counted in `existing`.
    Existing,
    /// A usage finding on an object the baseline had but could not judge:
    /// listed apart from the findings, never gates.
    Unverified,
}

/// What the other checkout's scan detected, and how far that can be trusted.
#[derive(Debug, Clone)]
pub struct Baseline {
    pub coverage: Coverage,
    /// Why coverage is incomplete; `None` for a scanned baseline.
    pub detail: Option<String>,
    detected: Detected,
    /// Under `Skipped`: the objects the baseline's usage findings were about.
    /// Its analysis ran with no report roots, so every object it had shows up
    /// here unless something else (a `ripbi_keep`, a logged query) kept it.
    subjects: HashSet<String>,
}

impl Baseline {
    /// A baseline whose scan ran against connected reports.
    #[must_use]
    pub fn scanned(detected: Detected) -> Self {
        Self {
            coverage: Coverage::Scanned,
            detail: None,
            detected,
            subjects: HashSet::new(),
        }
    }

    /// A baseline whose model had no connected reports: `detected` is its
    /// analysis with no report roots.
    #[must_use]
    pub fn skipped(detected: Detected, detail: String) -> Self {
        let subjects = detected
            .values()
            .filter(|entry| entry.about_usage())
            .map(Entry::subject)
            .collect();
        Self {
            coverage: Coverage::Skipped,
            detail: Some(detail),
            detected,
            subjects,
        }
    }

    /// A baseline where the scan could not run.
    #[must_use]
    pub fn unavailable(detail: String) -> Self {
        Self {
            coverage: Coverage::Unavailable,
            detail: Some(detail),
            detected: Detected::new(),
            subjects: HashSet::new(),
        }
    }

    /// Where `entry` stands: a skipped baseline vouches for breakage and for
    /// which objects existed, never for whether they were used.
    #[must_use]
    pub fn classify(&self, entry: &Entry) -> Status {
        if self.coverage == Coverage::Skipped && entry.about_usage() {
            return if self.subjects.contains(&entry.subject()) {
                Status::Unverified
            } else {
                Status::New
            };
        }
        if self.detected.contains_key(&entry.key()) {
            Status::Existing
        } else {
            Status::New
        }
    }

    /// The findings the baseline detected that `after` no longer detects at
    /// all, in key order. A skipped baseline's usage findings are left out:
    /// with no reports there, every object read as unused, and an object a
    /// report now binds was never a finding to fix.
    #[must_use]
    pub fn fixed(&self, after: &Detected) -> Vec<Entry> {
        self.detected
            .iter()
            .filter(|(_, entry)| self.coverage != Coverage::Skipped || !entry.about_usage())
            .filter(|(key, _)| !after.contains_key(key))
            .map(|(_, entry)| entry.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(before: &Detected, after: &Detected) -> Vec<Entry> {
        Baseline::scanned(before.clone()).fixed(after)
    }

    fn detected(entries: &[Entry]) -> Detected {
        let mut detected = Detected::new();
        for entry in entries {
            insert(&mut detected, entry);
        }
        detected
    }

    #[test]
    fn matching_ignores_case_like_the_engine() {
        let before = detected(&[Entry::unused("measure", "'Sales'[Legacy]")]);
        assert!(before.contains_key(&Entry::unused("measure", "'SALES'[legacy]").key()));
        assert!(!before.contains_key(&Entry::unused("column", "'Sales'[Legacy]").key()));
    }

    #[test]
    fn breakage_matches_on_reason_and_site() {
        let entry = Entry::broken_visual("'Sales'[Color]", "field_not_found", "visual 'V1'");
        let before = detected(std::slice::from_ref(&entry));
        assert!(before.contains_key(&entry.key()));
        let elsewhere = Entry::broken_visual("'Sales'[Color]", "field_not_found", "visual 'V2'");
        assert!(!before.contains_key(&elsewhere.key()));
    }

    #[test]
    fn fixed_lists_what_the_later_scan_no_longer_detects_in_key_order() {
        let kept = Entry::unused("measure", "'Sales'[Kept]");
        let gone_b = Entry::unused("measure", "'Sales'[B gone]");
        let gone_a = Entry::unused("column", "'Sales'[A gone]");
        let before = detected(&[gone_b.clone(), kept.clone(), gone_a.clone()]);
        assert_eq!(fixed(&before, &detected(&[kept])), vec![gone_a, gone_b]);
    }

    #[test]
    fn a_skipped_baseline_vouches_for_existence_not_usage() {
        let old = Entry::unused("measure", "'Sales'[Old]");
        let broken = Entry::broken_artifact("'Sales'[Bad]", None);
        let adt = Entry::auto_date_time("'LocalDateTable_1'", "unused_by_reports");
        let baseline = Baseline::skipped(
            detected(&[old.clone(), broken.clone(), adt]),
            "no connected reports".to_string(),
        );
        assert_eq!(baseline.classify(&old), Status::Unverified);
        assert_eq!(
            baseline.classify(&Entry::auto_date_time("'LocalDateTable_1'", "dead")),
            Status::Unverified,
            "the verdict itself depends on usage"
        );
        assert_eq!(
            baseline.classify(&Entry::unused("measure", "'Sales'[Added]")),
            Status::New
        );
        assert_eq!(baseline.classify(&broken), Status::Existing);
        assert_eq!(
            baseline.fixed(&Detected::new()),
            vec![broken],
            "usage findings of a skipped baseline are never fixed"
        );
    }

    #[test]
    fn an_unavailable_baseline_makes_everything_new() {
        let baseline = Baseline::unavailable("model not found".to_string());
        let entry = Entry::unused("measure", "'Sales'[Old]");
        assert_eq!(baseline.classify(&entry), Status::New);
        assert!(baseline.fixed(&Detected::new()).is_empty());
    }
}
