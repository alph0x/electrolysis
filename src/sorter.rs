//! Phase 4 — structural sort of a pbxproj text.
//!
//! Three independent sort passes are applied in sequence:
//!
//! 1. `files = ( … )` lists  → sort entries by the filename portion of their comment.
//! 2. `children = ( … )` lists → sort entries: dirs (no dot) first, then files,
//!    each group alphabetically.  The main-group children are intentionally left
//!    unsorted to preserve the Xcode navigator order.
//! 3. PBXBuildFile and PBXFileReference sections → sort by the filename in the comment.
//!
//! Duplicate entries within any list/section are silently dropped.

use once_cell::sync::Lazy;
use regex::Regex;

/// Matches the entry name inside a `files = (…)` comment: `UUID /* name in Phase */`
static RE_FILES_NAME: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/\* (.+?) in ").unwrap());

/// Matches the entry name inside a `children = (…)` comment: `UUID /* name */`
static RE_CHILDREN_NAME: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/\* (.+?) \*/").unwrap());

/// Matches a UUID at the start of an entry inside a PBX section.
static RE_PBX_UUID: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s+([0-9A-Fa-f]{24}|[0-9A-Fa-f]{32})\s").unwrap());

/// Matches `/* Begin PBXBuildFile section */` or `/* Begin PBXFileReference section */`.
static RE_PBX_BEGIN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/\* Begin (PBXBuildFile|PBXFileReference) section \*/").unwrap());

/// Matches `/* End PBXBuildFile section */` etc.
static RE_PBX_END: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/\* End (PBXBuildFile|PBXFileReference) section \*/").unwrap());

/// `children = (` or `files = (` opener — must be the ONLY content on the line
/// so we don't accidentally match `/* … files = ( … */` comments.
static RE_FILES_START:    Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s+files\s*=\s*\(\s*$").unwrap());
static RE_CHILDREN_START: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s+children\s*=\s*\(\s*$").unwrap());

#[derive(Debug, Default)]
pub struct SortStats {
    pub files_lists_sorted: usize,
    pub children_lists_sorted: usize,
    pub pbx_sections_sorted: usize,
    pub duplicate_entries_removed: usize,
}

/// Sort a pbxproj text and return (sorted_text, stats).
pub fn sort(input: &str) -> (String, SortStats) {
    let mut stats = SortStats::default();

    // We need to know the main-group UUID so we can skip sorting its children.
    // The root object's `mainGroup` field holds it.
    let main_group_uuid = extract_main_group_uuid(input).unwrap_or_default();

    let after_files    = sort_files_lists(input, &mut stats);
    let after_children = sort_children_lists(&after_files, &main_group_uuid, &mut stats);
    let after_pbx      = sort_pbx_sections(&after_children, &mut stats);

    (after_pbx, stats)
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Extract the UUID of the mainGroup from the root PBXProject node.
fn extract_main_group_uuid(text: &str) -> Option<String> {
    // Very lightweight approach: look for `mainGroup = XXXXX;`
    let re = Regex::new(r"mainGroup\s*=\s*([0-9A-Fa-f]{24}|[0-9A-Fa-f]{32})\s*;").ok()?;
    re.captures(text).map(|c| c[1].to_string())
}

/// Check whether a line is an entry in a `files` or `children` list.
fn has_list_uuid(line: &str) -> Option<String> {
    RE_PBX_UUID
        .captures(line)
        .map(|c| c[1].to_uppercase())
}

// ── Pass 1: sort `files = (…)` ───────────────────────────────────────────────

fn sort_files_lists(input: &str, stats: &mut SortStats) -> String {
    sort_list_sections(
        input,
        |line| RE_FILES_START.is_match(line),
        |line| RE_FILES_NAME.captures(line).map(|c| c[1].to_lowercase()).unwrap_or_default(),
        |_, _| true, // always sort files lists
        |s| s.files_lists_sorted += 1,
        stats,
    )
}

// ── Pass 2: sort `children = (…)` ────────────────────────────────────────────

fn sort_children_lists(input: &str, main_group_uuid: &str, stats: &mut SortStats) -> String {
    // The list that belongs to the main group is left unsorted to preserve
    // the Xcode navigator order.  We detect it by checking whether the
    // previous non-empty line contains the main-group UUID.
    let main_group_upper = main_group_uuid.to_uppercase();
    sort_list_sections(
        input,
        |line| RE_CHILDREN_START.is_match(line),
        |line| {
            let name = RE_CHILDREN_NAME
                .captures(line)
                .map(|c| c[1].to_string())
                .unwrap_or_default();
            // Dirs (no dot) before files: prefix "0" for dirs, "1" for files.
            format!("{}{}", u8::from(name.contains('.')), name.to_lowercase())
        },
        |i, lines| {
            // should_sort = true when this list does NOT belong to the main group.
            main_group_upper.is_empty()
                || !lines[..i]
                    .iter()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .map(|l| l.to_uppercase().contains(&main_group_upper))
                    .unwrap_or(false)
        },
        |s| s.children_lists_sorted += 1,
        stats,
    )
}

/// Generic list-section sorter used by both `sort_files_lists` and
/// `sort_children_lists`.
///
/// - `is_start`     — returns `true` for the line that opens a list (`files = (`, etc.)
/// - `sort_key`     — derives a sortable `String` from a list-entry line
/// - `should_sort`  — given the opener's index and all lines, returns `true` if
///                    this list should be sorted (used to skip the main group)
/// - `on_sorted`    — increments the appropriate `SortStats` counter
fn sort_list_sections(
    input: &str,
    is_start: impl Fn(&str) -> bool,
    sort_key: impl Fn(&str) -> String,
    should_sort: impl Fn(usize, &[&str]) -> bool,
    on_sorted: impl Fn(&mut SortStats),
    stats: &mut SortStats,
) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut pending: Vec<&str> = Vec::new();
    let mut in_list = false;
    let mut list_indent = 0usize;
    let mut sort_this_list = true;
    let mut seen_uuids: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];

        if !in_list {
            if is_start(line) {
                in_list = true;
                list_indent = line.len() - line.trim_start().len();
                sort_this_list = should_sort(i, &lines);
                seen_uuids.clear();
                out.push(line);
                i += 1;
                continue;
            }
            out.push(line);
            i += 1;
            continue;
        }

        let trimmed = line.trim();
        let cur_indent = line.len() - line.trim_start().len();

        if trimmed.starts_with(");") && cur_indent == list_indent {
            if sort_this_list && !pending.is_empty() {
                let mut sorted = pending.clone();
                sorted.sort_by_key(|l| sort_key(l));
                on_sorted(stats);
                out.extend_from_slice(&sorted);
            } else {
                // Main group or empty list: emit as-is.
                out.extend_from_slice(&pending);
            }
            pending.clear();
            in_list = false;
            out.push(line);
            i += 1;
            continue;
        }

        if let Some(uuid) = has_list_uuid(line) {
            if seen_uuids.contains(&uuid) {
                stats.duplicate_entries_removed += 1;
                i += 1;
                continue;
            }
            seen_uuids.insert(uuid);
            pending.push(line);
        } else if !trimmed.is_empty() {
            out.push(line);
        }
        i += 1;
    }

    // Flush if file ended mid-list (should not happen in valid pbxproj).
    out.extend_from_slice(&pending);

    let mut result = out.join("\n");
    if input.ends_with('\n') { result.push('\n'); }
    result
}

// ── Pass 3: sort PBXBuildFile / PBXFileReference sections ────────────────────

fn sort_pbx_sections(input: &str, stats: &mut SortStats) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut pending: Vec<&str> = Vec::new();
    let mut in_section = false;
    let mut section_name = String::new();
    let mut seen_uuids: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];

        if !in_section {
            if let Some(cap) = RE_PBX_BEGIN.captures(line) {
                in_section = true;
                section_name = cap[1].to_string();
                seen_uuids.clear();
                out.push(line);
                i += 1;
                continue;
            }
            out.push(line);
            i += 1;
            continue;
        }

        if let Some(cap) = RE_PBX_END.captures(line) {
            if cap[1] == section_name {
                // Flush sorted section — always by element name from comment.
                if !pending.is_empty() {
                    let mut sorted = pending.clone();
                    sorted.sort_by_key(|l| {
                        RE_CHILDREN_NAME
                            .captures(l)
                            .map(|c| c[1].to_lowercase())
                            .unwrap_or_default()
                    });
                    stats.pbx_sections_sorted += 1;
                    out.extend_from_slice(&sorted);
                }
                pending.clear();
                in_section = false;
                out.push(line);
                i += 1;
                continue;
            }
        }

        // Lines inside the section.
        if let Some(uuid) = has_list_uuid(line) {
            if seen_uuids.contains(&uuid) {
                stats.duplicate_entries_removed += 1;
                i += 1;
                continue;
            }
            seen_uuids.insert(uuid);
            pending.push(line);
        } else {
            // Section header/footer comments etc.
            if in_section && !line.trim().is_empty() {
                out.push(line);
            }
        }
        i += 1;
    }

    out.extend_from_slice(&pending);

    let mut result = out.join("\n");
    if input.ends_with('\n') { result.push('\n'); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_files_list() {
        let input = concat!(
            "\t\t\tfiles = (\n",
            "\t\t\t\tBBBBBBBBBBBBBBBBBBBBBBBB /* zebra.swift in Sources */,\n",
            "\t\t\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* alpha.swift in Sources */,\n",
            "\t\t\t);\n",
        );
        let (out, _) = sort(input);
        let pos_alpha = out.find("alpha.swift").unwrap();
        let pos_zebra = out.find("zebra.swift").unwrap();
        assert!(pos_alpha < pos_zebra, "alpha should come before zebra");
    }

    #[test]
    fn removes_duplicate_list_entries() {
        let input = concat!(
            "\t\t\tchildren = (\n",
            "\t\t\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* file.swift */,\n",
            "\t\t\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* file.swift */,\n",
            "\t\t\t);\n",
        );
        let (out, stats) = sort(input);
        assert_eq!(out.matches("AAAAAAAAAAAAAAAAAAAAAAAA").count(), 1);
        assert_eq!(stats.duplicate_entries_removed, 1);
    }
}
