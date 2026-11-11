//! Integration tests for `ripbi search` against the mini PBIP fixture: text
//! search over the model's DAX and Power Query, in the three output modes.

// Uses only part of `common`; `expect` (not `allow`) fails this build if
// that stops being true. Contract: common/mod.rs.
#[expect(dead_code)]
mod common;

use ripbi_cli::cli::SearchArgs;

use common::{TempDir, json_payload, mini_pbip, run_search};

fn project(name: &str) -> TempDir {
    let temp = TempDir::new(name);
    temp.copy_tree(&mini_pbip());
    temp
}

fn args(pattern: &str) -> SearchArgs {
    SearchArgs {
        pattern: pattern.to_string(),
        ..SearchArgs::default()
    }
}

#[test]
fn a_substring_finds_every_expression_ignoring_case() {
    let temp = project("search-human");
    let (code, stdout, stderr) = run_search(&args("sum("), &temp.0);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stdout,
        "'Sales'[Total]  measure\n     1:1  SUM('Sales'[Amount])\n\n\
         'Sales'[Legacy Total]  measure\n     1:1  SUM('Sales'[Legacy])\n\n\
         2 matching lines in 2 objects\n"
    );
}

#[test]
fn power_query_is_searched_and_can_be_selected() {
    let temp = project("search-m");
    let (_, stdout, _) = run_search(
        &SearchArgs {
            plain: true,
            ..args("#table")
        },
        &temp.0,
    );
    let fields: Vec<&str> = stdout.trim_end().split('\t').collect();
    assert_eq!(fields[0], "partition", "{stdout}");
    assert_eq!(fields[2], "m");
    assert_eq!(fields[4], "2", "the line within the expression: {stdout}");
    assert!(fields[6].contains("#table({\"Amount\", \"Legacy\"}, {})"));

    let (_, stdout, _) = run_search(
        &SearchArgs {
            plain: true,
            dax: true,
            ..args("#table")
        },
        &temp.0,
    );
    assert_eq!(stdout, "", "--dax leaves Power Query out");
}

#[test]
fn json_carries_the_hits_and_their_counts() {
    let temp = project("search-json");
    let (code, stdout, _) = run_search(
        &SearchArgs {
            json: true,
            regex: true,
            types: vec!["measure".to_string()],
            ..args(r"\[(amount|legacy)\]")
        },
        &temp.0,
    );
    let payload = json_payload(&stdout);
    assert_eq!(code, 0);
    assert_eq!(payload["schema_version"], 1);
    assert_eq!(payload["summary"]["hits"], 2);
    assert_eq!(payload["summary"]["objects"], 2);
    let hit = &payload["hits"][0];
    assert_eq!(hit["id"], "'Sales'[Total]");
    assert_eq!(hit["type"], "measure");
    assert_eq!(hit["language"], "dax");
    assert_eq!(hit["property"], "expression");
    assert_eq!(hit["line"], 1);
    assert_eq!(hit["column"], 12);
    assert_eq!(hit["text"], "SUM('Sales'[Amount])");
}

#[test]
fn no_match_is_a_result_not_an_error() {
    let temp = project("search-none");
    let (code, stdout, _) = run_search(&args("USERELATIONSHIP"), &temp.0);
    assert_eq!(code, 0);
    assert_eq!(stdout, "No matches for \"USERELATIONSHIP\"\n");
}

#[test]
fn case_sensitive_narrows_the_match() {
    let temp = project("search-case");
    let (_, stdout, _) = run_search(
        &SearchArgs {
            plain: true,
            case_sensitive: true,
            ..args("sum(")
        },
        &temp.0,
    );
    assert_eq!(stdout, "");
}

#[test]
fn usage_errors_exit_2_before_any_work() {
    let temp = project("search-errors");
    let (code, _, stderr) = run_search(
        &SearchArgs {
            regex: true,
            ..args("(")
        },
        &temp.0,
    );
    assert_eq!(code, 2);
    assert!(
        stderr.contains("not a valid regular expression"),
        "{stderr}"
    );

    let (code, _, stderr) = run_search(
        &SearchArgs {
            types: vec!["visual".to_string()],
            ..args("sum")
        },
        &temp.0,
    );
    assert_eq!(code, 2);
    assert!(
        stderr.contains("--type visual is not an object type"),
        "{stderr}"
    );
}
