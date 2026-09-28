//! Finding fingerprints for `scan --compare-root` (issue #141): the same scan
//! runs in another checkout, and only findings that did not exist there are
//! reported and gate the exit code.
//!
//! A fingerprint is the finding's identity as the output modes already print
//! it — kind plus display id, and for breakage the reason and binding site —
//! never a file path or position, so two checkouts in different folders
//! compare cleanly. Matching is case-insensitive, like the engine's own name
//! comparison. The contract lives in `docs/output.md` under "Comparing
//! against another checkout".

use std::collections::BTreeMap;

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

/// The findings `before` detected that `after` no longer detects at all, in
/// key order.
#[must_use]
pub fn fixed(before: &Detected, after: &Detected) -> Vec<Entry> {
    before
        .iter()
        .filter(|(key, _)| !after.contains_key(key))
        .map(|(_, entry)| entry.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
