//! End-to-end tests of the `ripbi` binary itself: the clap surface (help,
//! conflicts, exit codes) that in-process tests cannot reach.

use assert_cmd::Command;
use predicates::prelude::*;

fn ripbi() -> Command {
    Command::cargo_bin("ripbi").expect("the ripbi binary")
}

fn mini_pbip() -> String {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mini-pbip/Mini.pbip")
        .display()
        .to_string()
}

#[test]
fn help_lists_the_scan_subcommand() {
    ripbi()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("scan"));
}

#[test]
fn scan_help_leads_with_examples() {
    ripbi()
        .args(["scan", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("--report"));
}

#[test]
fn a_typo_subcommand_suggests_the_correction() {
    ripbi()
        .arg("scna")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "a similar subcommand exists: 'scan'",
        ));
}

#[test]
fn json_and_plain_flags_conflict() {
    ripbi()
        .args(["scan", &mini_pbip(), "--json", "--plain"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn scan_sets_the_documented_exit_codes() {
    // Unused objects found → 1.
    ripbi()
        .args(["scan", &mini_pbip(), "--plain"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("measure\t'Sales'[Legacy Total]"));

    // Usage error (no such path) → 2.
    ripbi()
        .args(["scan", "definitely/not/here.pbip"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no such path"));
}

#[test]
fn bare_path_argument_no_longer_works_without_a_subcommand() {
    ripbi()
        .arg(mini_pbip())
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}
