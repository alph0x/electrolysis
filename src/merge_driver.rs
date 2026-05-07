//! Git merge driver for `project.pbxproj` files.
//!
//! This driver is designed to be registered as a custom git merge driver:
//!
//!   [merge "electrolysis"]
//!       name = Electrolysis pbxproj merge driver
//!       driver = electrolysis merge-driver %O %A %B
//!
//! Strategy
//! --------
//! 1. Run the full electrolysis pipeline (sanitize → uniquify → sort) on **each**
//!    of the three inputs independently: base (%O), current (%A), and other (%B).
//! 2. Because all three are now normalized, identical objects share the exact same
//!    deterministic UUIDs and the same alphabetical ordering. This turns most
//!    merge conflicts into trivial non-conflicts for `git merge-file`.
//! 3. Run `git merge-file --union` on the three normalized files.
//! 4. Sanitize the merged result to strip any remaining conflict markers or
//!    duplicate entries.
//! 5. Validate by re-parsing, then write the final result back to `%A`.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use colored::Colorize;

use crate::parser;
use crate::sanitizer;
use crate::sorter;
use crate::uniquifier;

/// Run the merge driver.
///
/// `base`    = %O (ancestor)
/// `current` = %A (ours) — **overwritten** with the resolved result
/// `other`   = %B (theirs)
pub fn run(base: &Path, current: &Path, other: &Path, verbose: bool) -> Result<()> {
    // ── Step 1: read all three versions ────────────────────────────────────────

    let base_raw = fs::read_to_string(base)
        .with_context(|| format!("cannot read base {}", base.display()))?;
    let current_raw = fs::read_to_string(current)
        .with_context(|| format!("cannot read current {}", current.display()))?;
    let other_raw = fs::read_to_string(other)
        .with_context(|| format!("cannot read other {}", other.display()))?;

    // ── Step 2: normalize each side independently ──────────────────────────────

    if verbose {
        eprintln!("{}", "→ normalizing base…".cyan().dimmed());
    }
    let base_clean = apply_electrolysis(&base_raw, base, verbose)?;

    if verbose {
        eprintln!("{}", "→ normalizing current…".cyan().dimmed());
    }
    let current_clean = apply_electrolysis(&current_raw, current, verbose)?;

    if verbose {
        eprintln!("{}", "→ normalizing other…".cyan().dimmed());
    }
    let other_clean = apply_electrolysis(&other_raw, other, verbose)?;

    // ── Step 3: union-merge the three normalized files ─────────────────────────

    if verbose {
        eprintln!("{}", "→ performing union merge…".cyan().dimmed());
    }
    let merged = union_merge_normalized(&base_clean, &current_clean, &other_clean)?;

    // ── Step 4: sanitize the merged result ─────────────────────────────────────

    let (sanitized, san_stats) = sanitizer::sanitize(&merged);
    if verbose {
        crate::print_sanitize_stats(&san_stats);
    }

    // ── Step 5: validate ───────────────────────────────────────────────────────

    parser::parse_project(&sanitized).context(
        "merge driver produced an invalid pbxproj — this is a bug, please report it",
    )?;

    // ── Step 6: write back to %A ───────────────────────────────────────────────

    crate::write_atomic(current, &sanitized)
        .with_context(|| format!("cannot write merged result to {}", current.display()))?;

    eprintln!(
        "{} merge resolved — written {}",
        "✓".green().bold(),
        current.display()
    );
    Ok(())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Apply the full electrolysis pipeline to a single pbxproj text.
fn apply_electrolysis(raw: &str, path_hint: &Path, _verbose: bool) -> Result<String> {
    let (sanitized, _) = sanitizer::sanitize(raw);

    let proj = parser::parse_project(&sanitized)
        .context("failed to parse project during merge-driver normalization")?;

    let proj_root_name = crate::infer_project_name(path_hint);
    let unique_map = uniquifier::build_unique_map(&proj, &proj_root_name);
    let (uniquified, _) = unique_map.apply(&sanitized)?;

    // Post-uniquify sanitize: the uniquifier can cause UUID collisions
    // (e.g. two XCLocalSwiftPackageReference objects pointing to the same package).
    let (post_sanitized, _) = sanitizer::sanitize(&uniquified);

    let (sorted, _) = sorter::sort(&post_sanitized, false);

    Ok(sorted)
}

/// Union-merge three already-normalized pbxproj texts.
///
/// Because the files are normalized (deterministic UUIDs, sorted lists, etc.),
/// `git merge-file --union` can resolve most changes automatically.
fn union_merge_normalized(
    base: &str,
    current: &str,
    other: &str,
) -> Result<String> {
    let tmp_base = tempfile::NamedTempFile::with_prefix("electrolysis-base-")
        .context("create temp file for base")?;
    let tmp_current = tempfile::NamedTempFile::with_prefix("electrolysis-current-")
        .context("create temp file for current")?;
    let tmp_other = tempfile::NamedTempFile::with_prefix("electrolysis-other-")
        .context("create temp file for other")?;

    fs::write(tmp_base.path(), base)?;
    fs::write(tmp_current.path(), current)?;
    fs::write(tmp_other.path(), other)?;

    let output = Command::new("git")
        .args([
            "merge-file",
            "--union",
            "-p",
            tmp_current.path().to_str().unwrap(),
            tmp_base.path().to_str().unwrap(),
            tmp_other.path().to_str().unwrap(),
        ])
        .output()
        .context("failed to run `git merge-file` — is git installed?")?;

    // git merge-file exits non-zero when conflicts remain.  With --union and
    // pre-normalized inputs this should be rare, but if it happens we still
    // take the stdout output because our sanitize pass can resolve any
    // leftover conflict markers.
    String::from_utf8(output.stdout)
        .context("git merge-file produced invalid UTF-8")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_electrolysis_on_minimal_project() {
        // Well-formed minimal project with mainGroup pointing to the root group.
        let src = r#"// !$*UTF8*$!
{
    archiveVersion = 1;
    classes = {};
    objectVersion = 56;
    objects = {
        BBBBBBBBBBBBBBBBBBBBBBBB /* Project object */ = {
            isa = PBXProject;
            mainGroup = CCCCCCCCCCCCCCCCCCCCCCCC;
            targets = (
            );
        };
        CCCCCCCCCCCCCCCCCCCCCCCC /* root */ = {
            isa = PBXGroup;
            children = (
                AAAAAAAAAAAAAAAAAAAAAAAA /* file.swift */,
            );
            sourceTree = "<group>";
        };
        AAAAAAAAAAAAAAAAAAAAAAAA /* file.swift */ = {
            isa = PBXFileReference;
            path = file.swift;
            sourceTree = "<group>";
        };
    };
    rootObject = BBBBBBBBBBBBBBBBBBBBBBBB;
}
"#;
        // Create a temp directory that looks like an .xcodeproj so infer_project_name works.
        let dir = tempfile::tempdir().unwrap();
        let xcodeproj = dir.path().join("Test.xcodeproj");
        std::fs::create_dir(&xcodeproj).unwrap();
        let pbxproj = xcodeproj.join("project.pbxproj");

        let result = apply_electrolysis(src, &pbxproj, false).unwrap();

        // After uniquify+sort the UUIDs should be deterministic (32-char uppercase).
        assert!(result.contains("/* file.swift */"));
        // The old 24-char UUID should be gone.
        assert!(!result.contains("BBBBBBBBBBBBBBBBBBBBBBBB"));
        assert!(!result.contains("AAAAAAAAAAAAAAAAAAAAAAAA"));
        assert!(!result.contains("CCCCCCCCCCCCCCCCCCCCCCCC"));
    }
}
