//! Glob matching for `[scan].ignore` patterns in `ripbi.toml`.
//!
//! `*` matches any run of characters (including none) and `?` matches exactly
//! one. Everything else is literal — brackets and quotes included, because
//! they appear in the display ids patterns are matched against, such as
//! `'Sales'[Draft Amount]`. Matching is case-insensitive, like every object
//! name comparison in the Analysis Services engine.

/// True when `pattern` matches `text`, case-insensitively.
#[must_use]
pub fn matches(pattern: &str, text: &str) -> bool {
    glob(
        pattern.to_lowercase().as_bytes(),
        text.to_lowercase().as_bytes(),
    )
}

/// The classic iterative two-pointer glob with a single `*` backtrack mark:
/// linear time, no recursion.
fn glob(pat: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<usize> = None;
    let mut mark = 0;
    while t < text.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = Some(p);
            p += 1;
            mark = t;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&b| b == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_patterns_match_exact_and_case_insensitive() {
        assert!(matches("Draft", "Draft"));
        assert!(matches("draft", "DRAFT"));
        assert!(!matches("draft", "Drafts"));
    }

    #[test]
    fn star_spans_anything_including_nothing() {
        assert!(matches("*", "'Sales'[Anything]"));
        assert!(matches("*Draft*", "Draft Amount"));
        assert!(matches("*draft*", "'Sales'[Draft Amount]"));
        assert!(!matches("*draft*", "'Sales'[Amount]"));
    }

    #[test]
    fn question_mark_matches_exactly_one_character() {
        assert!(matches("Sales?Amount", "Sales Amount"));
        assert!(!matches("Sales?Amount", "Sales  Amount"));
    }

    #[test]
    fn brackets_and_quotes_are_literal() {
        assert!(matches("'Sales'[*]", "'Sales'[Draft]"));
        assert!(!matches("'Sales'[*]", "'Product'[Draft]"));
        assert!(matches(
            "'Time Intelligence'[*]",
            "'Time Intelligence'[Time Intelligence]"
        ));
    }

    #[test]
    fn a_trailing_star_matches_an_empty_suffix() {
        assert!(matches("Draft*", "Draft"));
        assert!(matches("_*", "_"));
    }
}
