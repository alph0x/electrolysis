//! Integration tests for electrolysis.
//!
//! Each test copies the real-world fixture (`test-subject.xcodeproj.bak/project.pbxproj`)
//! into a temporary directory, runs the compiled binary, and asserts structural
//! invariants on the output.
//!
//! The fixture is not committed — see `.gitignore`.  All tests skip gracefully
//! when the fixture is absent so `cargo test` continues to pass in CI environments
//! that don't have it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

// ── Fixture / binary helpers ──────────────────────────────────────────────────

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_electrolysis"))
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test-subject.xcodeproj.bak")
        .join("project.pbxproj")
}

/// Returns false (and prints a notice) when the fixture is missing.
/// Use this at the top of every test that requires the fixture.
fn require_fixture() -> bool {
    if fixture_path().exists() {
        return true;
    }
    eprintln!(
        "[SKIP] fixture not found: {}\n       \
         Place test-subject.xcodeproj.bak/ next to Cargo.toml to run these tests.",
        fixture_path().display()
    );
    false
}

/// Copy the fixture pbxproj into a fresh temp dir and return (TempDir, path-to-copy).
/// The `TempDir` must be kept alive for the duration of the test.
fn temp_fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let dest = dir.path().join("project.pbxproj");
    fs::copy(fixture_path(), &dest).expect("copy fixture");
    (dir, dest)
}

fn run(args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .output()
        .expect("failed to run electrolysis")
}

fn run_on(pbxproj: &Path, extra_args: &[&str]) -> Output {
    let mut args: Vec<&str> = vec![pbxproj.to_str().expect("utf-8 path")];
    args.extend_from_slice(extra_args);
    run(&args)
}

fn assert_success(out: &Output) {
    assert!(
        out.status.success(),
        "electrolysis exited with {}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ── Structural validators ─────────────────────────────────────────────────────

/// Returns true if the content contains any git merge-conflict marker.
fn has_conflict_markers(content: &str) -> bool {
    content.lines().any(|l| {
        l.starts_with("<<<<<<<") || l.starts_with("=======") || l.starts_with(">>>>>>>")
    })
}

/// Counts how many times each UUID appears as an object declaration
/// (`\t\tUUID /* … */ = {`).
fn uuid_declaration_counts(content: &str) -> HashMap<String, usize> {
    let re = regex::Regex::new(r"^\s+([0-9A-Fa-f]{24})\s+/\*").unwrap();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for line in content.lines() {
        if let Some(cap) = re.captures(line) {
            *counts.entry(cap[1].to_string()).or_insert(0) += 1;
        }
    }
    counts
}

/// Returns all UUIDs that are declared more than once in the objects section.
fn duplicate_uuids(content: &str) -> Vec<String> {
    uuid_declaration_counts(content)
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(uuid, _)| uuid)
        .collect()
}

/// Checks that every declared UUID is exactly 24 or 32 hex characters,
/// and that none are an unusual length (e.g. truncated or padded).
///
/// The uniquifier produces 32-char MD5-based UUIDs.  Some objects that are
/// not reachable via the traversal may retain their original 24-char Xcode
/// UUIDs, which is also acceptable.  Any other length indicates corruption.
fn all_uuids_are_valid_length(content: &str) -> bool {
    let re = regex::Regex::new(r"^\s+([0-9A-Fa-f]+)\b[^,;]*=\s*\{").unwrap();
    content.lines().all(|line| {
        re.captures(line)
            .map(|cap| matches!(cap[1].len(), 24 | 32))
            .unwrap_or(true)
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// The binary must process the fixture without error.
#[test]
fn process_succeeds_on_fixture() {
    if !require_fixture() {
        return;
    }
    let (_dir, pbx) = temp_fixture();
    let out = run_on(&pbx, &[]);
    assert_success(&out);
}

/// Running the binary twice must produce identical output (idempotency).
/// The second run should report "no changes — file already clean".
#[test]
fn process_is_idempotent() {
    if !require_fixture() {
        return;
    }
    let (_dir, pbx) = temp_fixture();

    // First pass — may modify the file.
    let first = run_on(&pbx, &[]);
    assert_success(&first);

    // Second pass — must be a no-op.
    let second = run_on(&pbx, &[]);
    assert_success(&second);

    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("no changes"),
        "second run should report 'no changes', got:\n{stderr}"
    );
}

/// `--sanitize-only` must succeed and leave a parseable file.
#[test]
fn sanitize_only_succeeds() {
    if !require_fixture() {
        return;
    }
    let (_dir, pbx) = temp_fixture();
    let out = run_on(&pbx, &["--sanitize-only"]);
    assert_success(&out);

    let content = fs::read_to_string(&pbx).expect("read output");
    assert!(!has_conflict_markers(&content), "sanitize-only left conflict markers");
}

/// `-o` must write output to the specified path without touching the input.
#[test]
fn output_flag_writes_to_separate_file() {
    if !require_fixture() {
        return;
    }
    let (_dir, pbx) = temp_fixture();
    let out_path = pbx.with_extension("pbxproj.out");

    let original_bytes = fs::read(&pbx).expect("read original");
    let out = run_on(&pbx, &["-o", out_path.to_str().unwrap()]);
    assert_success(&out);

    assert!(out_path.exists(), "-o output file not created");

    // Input file must be untouched.
    let after_bytes = fs::read(&pbx).expect("re-read original");
    assert_eq!(
        original_bytes, after_bytes,
        "input file was modified despite -o flag"
    );

    // Output must be non-empty.
    let written = fs::read_to_string(&out_path).expect("read output");
    assert!(!written.is_empty(), "output file is empty");
}

/// The output must not contain any git merge-conflict markers.
#[test]
fn output_has_no_conflict_markers() {
    if !require_fixture() {
        return;
    }
    let (_dir, pbx) = temp_fixture();
    let out = run_on(&pbx, &[]);
    assert_success(&out);

    let content = fs::read_to_string(&pbx).expect("read output");
    assert!(
        !has_conflict_markers(&content),
        "output contains merge-conflict markers"
    );
}

/// Every UUID object declaration must be unique within the file.
#[test]
fn output_has_no_duplicate_uuid_declarations() {
    if !require_fixture() {
        return;
    }
    let (_dir, pbx) = temp_fixture();
    let out = run_on(&pbx, &[]);
    assert_success(&out);

    let content = fs::read_to_string(&pbx).expect("read output");
    let dups = duplicate_uuids(&content);
    assert!(
        dups.is_empty(),
        "output has {} duplicate UUID declaration(s): {:?}",
        dups.len(),
        &dups[..dups.len().min(10)]
    );
}

/// All UUID object declarations in the processed file must be exactly 24 or 32
/// hex characters.  The uniquifier produces 32-char MD5-based UUIDs; original
/// Xcode 24-char UUIDs may survive for unreachable objects — both are valid.
/// Any other length (e.g. 20 or 48) indicates a parser or regex bug.
#[test]
fn output_uuid_declarations_are_valid_length() {
    if !require_fixture() {
        return;
    }
    let (_dir, pbx) = temp_fixture();
    let out = run_on(&pbx, &[]);
    assert_success(&out);

    let content = fs::read_to_string(&pbx).expect("read output");
    assert!(
        all_uuids_are_valid_length(&content),
        "output contains UUID declarations with an unexpected length (not 24 or 32 chars)"
    );
}

/// The output must begin with the required pbxproj magic comment.
#[test]
fn output_has_pbxproj_magic_header() {
    if !require_fixture() {
        return;
    }
    let (_dir, pbx) = temp_fixture();
    let out = run_on(&pbx, &[]);
    assert_success(&out);

    let content = fs::read_to_string(&pbx).expect("read output");
    assert!(
        content.starts_with("// !$*UTF8*$!"),
        "output is missing the pbxproj magic header"
    );
}

/// `map` subcommand must produce a valid JSON file.
#[test]
fn map_subcommand_generates_valid_json() {
    if !require_fixture() {
        return;
    }
    let (_dir, pbx) = temp_fixture();

    // First process so UUIDs are deterministic.
    assert_success(&run_on(&pbx, &[]));

    let map_path = pbx.with_extension("pbxproj.map.json");
    let out = run(&["map", pbx.to_str().unwrap(), "-o", map_path.to_str().unwrap()]);
    assert_success(&out);

    assert!(map_path.exists(), "map file not created");
    let json_str = fs::read_to_string(&map_path).expect("read map");
    let parsed: serde_json::Value = serde_json::from_str(&json_str)
        .expect("map output is not valid JSON");

    assert!(parsed.get("targets").is_some(), "map JSON missing 'targets' key");
    assert!(parsed.get("uuid_table").is_some(), "map JSON missing 'uuid_table' key");
    assert!(
        parsed["targets"].as_array().map(|a| !a.is_empty()).unwrap_or(false),
        "map JSON has no targets"
    );
}

/// `diff` against the map produced from the same run must report identical.
#[test]
fn diff_against_own_map_is_identical() {
    if !require_fixture() {
        return;
    }
    let (_dir, pbx) = temp_fixture();

    // Process, then map.
    assert_success(&run_on(&pbx, &[]));
    let map_path = pbx.with_extension("pbxproj.ref.json");
    assert_success(&run(&["map", pbx.to_str().unwrap(), "-o", map_path.to_str().unwrap()]));

    // Diff the same project against its own map.
    let out = run(&["diff", pbx.to_str().unwrap(), map_path.to_str().unwrap()]);
    // Exit code 0 means identical; 1 means has changes.
    assert_eq!(
        out.status.code(),
        Some(0),
        "diff detected unexpected structural changes:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("identical"),
        "diff output should say 'identical', got:\n{stderr}"
    );
}

/// The processed file must contain at least one PBXNativeTarget section entry.
/// This guards against a sanitizer regression where list items that share UUIDs
/// with their own object declarations were incorrectly stripped as duplicates.
#[test]
fn output_preserves_native_targets() {
    if !require_fixture() {
        return;
    }
    let (_dir, pbx) = temp_fixture();
    let out = run_on(&pbx, &[]);
    assert_success(&out);

    let content = fs::read_to_string(&pbx).expect("read output");
    assert!(
        content.contains("PBXNativeTarget"),
        "output has no PBXNativeTarget entries — sanitizer may have stripped targets from the targets array"
    );
}

/// `-c` (combine-commit) must succeed (exit 0) when the file is already clean.
#[test]
fn combine_commit_passes_on_already_clean_file() {
    if !require_fixture() {
        return;
    }
    let (_dir, pbx) = temp_fixture();

    // Process once to make the file clean.
    assert_success(&run_on(&pbx, &[]));

    // Second run with -c — should exit 0 because nothing changed.
    let out = run_on(&pbx, &["-c"]);
    assert_success(&out);
}
