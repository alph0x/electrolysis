//! Phase 4 — structural sort of a pbxproj text.
//!
//! Ten independent sort passes are applied in sequence:
//!
//! 1. `files = ( … )` lists  → sort entries by the filename portion of their comment.
//! 2. `children = ( … )` lists → sort entries: dirs (no dot) first, then files,
//!    each group alphabetically.  The main-group children are intentionally left
//!    unsorted to preserve the Xcode navigator order.
//! 3. PBXBuildFile and PBXFileReference sections → sort by the filename in the comment.
//! 4. XCBuildConfiguration section → sort multi-line entries by the name (comment) of
//!    their `baseConfigurationReference`; entries with no reference sort by their own name.
//! 5. PBXVariantGroup section → sort multi-line entries alphabetically by name.
//! 6. XCConfigurationList section → sort multi-line entries alphabetically by the
//!    quoted target name found in their opening-line comment.
//! 7. PBXNativeTarget section → sort multi-line entries alphabetically by target name.
//! 8. PBXAggregateTarget section → sort multi-line entries alphabetically by target name.
//! 9. PBXGroup section → sort multi-line entries alphabetically by group name.
//! 10. PBXTargetDependency section → sort multi-line entries alphabetically by the
//!     name found in their `target = UUID /* name */` field.
//! 11. XCRemoteSwiftPackageReference section → sort multi-line entries alphabetically
//!     by opening-line comment name.
//! 12. XCSwiftPackageProductDependency section → sort multi-line entries alphabetically
//!     by opening-line comment name.
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

/// Matches any `/* Begin SECTION section */` line and captures the section name.
static RE_SECTION_BEGIN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/\* Begin (\w+) section \*/").unwrap());

/// Matches any `/* End SECTION section */` line and captures the section name.
static RE_SECTION_END: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/\* End (\w+) section \*/").unwrap());

/// Matches a multi-line section entry opener: 2-tab indent, UUID, optional comment, `= {`.
static RE_ENTRY_OPEN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\t\t([0-9A-Fa-f]{24}|[0-9A-Fa-f]{32})\b.*=\s*\{$").unwrap());

/// Matches `baseConfigurationReference = UUID /* name */` and captures the name.
static RE_BASE_CONFIG_REF: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"baseConfigurationReference\s*=\s*[0-9A-Fa-f]+\s*/\*\s*(.+?)\s*\*/").unwrap());

/// Matches a double-quoted name (used to extract target names from XCConfigurationList comments).
static RE_QUOTED_NAME: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#""([^"]+)""#).unwrap());

/// Matches `target = UUID /* name */` inside a PBXTargetDependency entry and captures the name.
static RE_TARGET_FIELD: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s+target\s*=\s*[0-9A-Fa-f]+\s*/\*\s*(.+?)\s*\*/").unwrap());

#[derive(Debug, Default)]
pub struct SortStats {
    pub files_lists_sorted: usize,
    pub children_lists_sorted: usize,
    pub pbx_sections_sorted: usize,
    pub duplicate_entries_removed: usize,
    pub xc_build_configs_sorted: usize,
    pub pbx_variant_groups_sorted: usize,
    pub xc_config_lists_sorted: usize,
    pub pbx_native_targets_sorted: usize,
    pub pbx_aggregate_targets_sorted: usize,
    pub pbx_groups_sorted: usize,
    pub pbx_target_dependencies_sorted: usize,
    pub xc_remote_package_refs_sorted: usize,
    pub xc_package_product_deps_sorted: usize,
}

/// Sort a pbxproj text and return (sorted_text, stats).
pub fn sort(input: &str) -> (String, SortStats) {
    let mut stats = SortStats::default();

    // We need to know the main-group UUID so we can skip sorting its children.
    // The root object's `mainGroup` field holds it.
    let main_group_uuid = extract_main_group_uuid(input).unwrap_or_default();

    let after_files          = sort_files_lists(input, &mut stats);
    let after_children       = sort_children_lists(&after_files, &main_group_uuid, &mut stats);
    let after_pbx            = sort_pbx_sections(&after_children, &mut stats);
    let after_build_configs  = sort_xc_build_configurations(&after_pbx, &mut stats);
    let after_variant_groups = sort_pbx_variant_groups(&after_build_configs, &mut stats);
    let after_config_lists   = sort_xc_configuration_lists(&after_variant_groups, &mut stats);
    let after_native_targets = sort_section_by_comment_name(&after_config_lists, "PBXNativeTarget", |s| s.pbx_native_targets_sorted += 1, &mut stats);
    let after_agg_targets    = sort_section_by_comment_name(&after_native_targets, "PBXAggregateTarget", |s| s.pbx_aggregate_targets_sorted += 1, &mut stats);
    let after_groups         = sort_section_by_comment_name(&after_agg_targets, "PBXGroup", |s| s.pbx_groups_sorted += 1, &mut stats);
    let after_target_deps    = sort_pbx_target_dependencies(&after_groups, &mut stats);
    let after_pkg_refs       = sort_section_by_comment_name(&after_target_deps, "XCRemoteSwiftPackageReference", |s| s.xc_remote_package_refs_sorted += 1, &mut stats);
    let after_pkg_prod_deps  = sort_section_by_comment_name(&after_pkg_refs, "XCSwiftPackageProductDependency", |s| s.xc_package_product_deps_sorted += 1, &mut stats);

    (after_pkg_prod_deps, stats)
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

// ── Passes 4–6: multi-line PBX section sorters ────────────────────────────────

/// Sort multi-line entries within a named PBX section.
///
/// Each entry occupies multiple lines, opening with `\t\tUUID … = {` and
/// closing with `\t\t};`.  The `sort_key` closure receives the full entry text
/// (all its lines joined with `\n`) and returns a sort key string.
///
/// Non-entry lines that appear inside the section (blank lines, inter-entry
/// comments) are emitted before the sorted entries — they are rare or absent in
/// well-formed pbxproj files.
fn sort_multiline_pbx_section(
    input: &str,
    target_section: &str,
    sort_key: impl Fn(&str) -> String,
    stats: &mut SortStats,
    increment: impl Fn(&mut SortStats),
) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut in_section = false;
    let mut current_section = String::new();
    // Accumulates lines that appear inside the section but outside any entry.
    let mut section_prefix: Vec<&str> = Vec::new();
    let mut entries: Vec<Vec<&str>> = Vec::new();
    let mut current_entry: Vec<&str> = Vec::new();
    let mut in_entry = false;

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];

        if !in_section {
            out.push(line);
            if let Some(cap) = RE_SECTION_BEGIN.captures(line) {
                if &cap[1] == target_section {
                    in_section = true;
                    current_section = cap[1].to_string();
                }
            }
            i += 1;
            continue;
        }

        // Check for section end.
        if let Some(cap) = RE_SECTION_END.captures(line) {
            if cap[1] == current_section {
                // Flush any incomplete entry.
                if !current_entry.is_empty() {
                    entries.push(std::mem::take(&mut current_entry));
                }
                // Emit non-entry prefix lines, then sorted entries.
                out.extend_from_slice(&section_prefix);
                section_prefix.clear();
                if !entries.is_empty() {
                    entries.sort_by_key(|entry| sort_key(&entry.join("\n")));
                    increment(stats);
                    for entry in entries.drain(..) {
                        out.extend(entry);
                    }
                }
                in_section = false;
                in_entry = false;
                out.push(line);
                i += 1;
                continue;
            }
        }

        if !in_entry {
            if RE_ENTRY_OPEN.is_match(line) {
                in_entry = true;
                current_entry.push(line);
            } else {
                // Lines between entries (blank lines, comments, etc.).
                section_prefix.push(line);
            }
        } else {
            current_entry.push(line);
            // The entry closer is exactly two tabs followed by `};`.
            if line == "\t\t};" {
                entries.push(std::mem::take(&mut current_entry));
                in_entry = false;
            }
        }

        i += 1;
    }

    // Safety flush (should not happen in valid pbxproj).
    out.extend_from_slice(&section_prefix);
    out.extend(entries.into_iter().flatten());
    out.extend_from_slice(&current_entry);

    let mut result = out.join("\n");
    if input.ends_with('\n') {
        result.push('\n');
    }
    result
}

// ── Shared helper: sort a section by its opening-line comment name ────────────

/// Sort multi-line entries in `target_section` alphabetically by the name in
/// their opening-line comment (`UUID /* name */ = {`).
fn sort_section_by_comment_name(
    input: &str,
    target_section: &str,
    increment: impl Fn(&mut SortStats),
    stats: &mut SortStats,
) -> String {
    sort_multiline_pbx_section(
        input,
        target_section,
        |entry| {
            entry
                .lines()
                .next()
                .and_then(|l| RE_CHILDREN_NAME.captures(l))
                .map(|c| c[1].to_lowercase())
                .unwrap_or_default()
        },
        stats,
        increment,
    )
}

// ── Pass 4: XCBuildConfiguration ─────────────────────────────────────────────

/// Sort XCBuildConfiguration entries by the name (comment) of their
/// `baseConfigurationReference`.  Entries without a reference sort by their
/// own opening-line comment name.
fn sort_xc_build_configurations(input: &str, stats: &mut SortStats) -> String {
    sort_multiline_pbx_section(
        input,
        "XCBuildConfiguration",
        |entry| {
            if let Some(cap) = RE_BASE_CONFIG_REF.captures(entry) {
                return cap[1].trim().to_lowercase();
            }
            // Fallback: use the entry's own comment name.
            entry
                .lines()
                .next()
                .and_then(|l| RE_CHILDREN_NAME.captures(l))
                .map(|c| c[1].to_lowercase())
                .unwrap_or_default()
        },
        stats,
        |s| s.xc_build_configs_sorted += 1,
    )
}

// ── Pass 5: PBXVariantGroup ───────────────────────────────────────────────────

/// Sort PBXVariantGroup entries alphabetically by their opening-line comment name.
fn sort_pbx_variant_groups(input: &str, stats: &mut SortStats) -> String {
    sort_multiline_pbx_section(
        input,
        "PBXVariantGroup",
        |entry| {
            entry
                .lines()
                .next()
                .and_then(|l| RE_CHILDREN_NAME.captures(l))
                .map(|c| c[1].to_lowercase())
                .unwrap_or_default()
        },
        stats,
        |s| s.pbx_variant_groups_sorted += 1,
    )
}

// ── Pass 6: XCConfigurationList ───────────────────────────────────────────────

/// Sort XCConfigurationList entries alphabetically by the quoted target name
/// found in their opening-line comment, e.g. `"App"` in
/// `/* Build configuration list for PBXNativeTarget "App" */`.
fn sort_xc_configuration_lists(input: &str, stats: &mut SortStats) -> String {
    sort_multiline_pbx_section(
        input,
        "XCConfigurationList",
        |entry| {
            entry
                .lines()
                .next()
                .and_then(|l| RE_QUOTED_NAME.captures(l))
                .map(|c| c[1].to_lowercase())
                .unwrap_or_default()
        },
        stats,
        |s| s.xc_config_lists_sorted += 1,
    )
}

// ── Pass 10: PBXTargetDependency ──────────────────────────────────────────────

/// Sort PBXTargetDependency entries alphabetically by the name found in their
/// `target = UUID /* name */` field (since the opening-line comment is always
/// the generic "PBXTargetDependency" label).
fn sort_pbx_target_dependencies(input: &str, stats: &mut SortStats) -> String {
    sort_multiline_pbx_section(
        input,
        "PBXTargetDependency",
        |entry| {
            entry
                .lines()
                .find_map(|l| RE_TARGET_FIELD.captures(l))
                .map(|c| c[1].trim().to_lowercase())
                .unwrap_or_default()
        },
        stats,
        |s| s.pbx_target_dependencies_sorted += 1,
    )
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

    #[test]
    fn sorts_xc_build_configurations_by_base_config_ref_name() {
        let input = concat!(
            "/* Begin XCBuildConfiguration section */\n",
            "\t\tBBBBBBBBBBBBBBBBBBBBBBBB /* Release */ = {\n",
            "\t\t\tisa = XCBuildConfiguration;\n",
            "\t\t\tbaseConfigurationReference = CCCCCCCCCCCCCCCCCCCCCCCC /* Pods-App.release.xcconfig */;\n",
            "\t\t\tname = Release;\n",
            "\t\t};\n",
            "\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* Debug */ = {\n",
            "\t\t\tisa = XCBuildConfiguration;\n",
            "\t\t\tbaseConfigurationReference = DDDDDDDDDDDDDDDDDDDDDDDD /* Pods-App.debug.xcconfig */;\n",
            "\t\t\tname = Debug;\n",
            "\t\t};\n",
            "/* End XCBuildConfiguration section */\n",
        );
        let (out, stats) = sort(input);
        let pos_debug = out.find("Pods-App.debug").unwrap();
        let pos_release = out.find("Pods-App.release").unwrap();
        assert!(pos_debug < pos_release, "debug should sort before release");
        assert_eq!(stats.xc_build_configs_sorted, 1);
    }

    #[test]
    fn sorts_xc_build_configurations_no_base_config_ref_by_name() {
        // Entries without baseConfigurationReference fall back to their own comment name.
        let input = concat!(
            "/* Begin XCBuildConfiguration section */\n",
            "\t\tBBBBBBBBBBBBBBBBBBBBBBBB /* Release */ = {\n",
            "\t\t\tisa = XCBuildConfiguration;\n",
            "\t\t\tname = Release;\n",
            "\t\t};\n",
            "\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* Debug */ = {\n",
            "\t\t\tisa = XCBuildConfiguration;\n",
            "\t\t\tname = Debug;\n",
            "\t\t};\n",
            "/* End XCBuildConfiguration section */\n",
        );
        let (out, _) = sort(input);
        let pos_debug = out.find("/* Debug */").unwrap();
        let pos_release = out.find("/* Release */").unwrap();
        assert!(pos_debug < pos_release, "Debug should sort before Release");
    }

    #[test]
    fn sorts_pbx_variant_groups_alphabetically() {
        let input = concat!(
            "/* Begin PBXVariantGroup section */\n",
            "\t\tBBBBBBBBBBBBBBBBBBBBBBBB /* Zebra.strings */ = {\n",
            "\t\t\tisa = PBXVariantGroup;\n",
            "\t\t\tname = Zebra;\n",
            "\t\t};\n",
            "\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* Alpha.strings */ = {\n",
            "\t\t\tisa = PBXVariantGroup;\n",
            "\t\t\tname = Alpha;\n",
            "\t\t};\n",
            "/* End PBXVariantGroup section */\n",
        );
        let (out, stats) = sort(input);
        let pos_alpha = out.find("Alpha.strings").unwrap();
        let pos_zebra = out.find("Zebra.strings").unwrap();
        assert!(pos_alpha < pos_zebra, "Alpha should sort before Zebra");
        assert_eq!(stats.pbx_variant_groups_sorted, 1);
    }

    #[test]
    fn sorts_xc_configuration_lists_by_quoted_target_name() {
        let input = concat!(
            "/* Begin XCConfigurationList section */\n",
            "\t\tBBBBBBBBBBBBBBBBBBBBBBBB /* Build configuration list for PBXNativeTarget \"zebra\" */ = {\n",
            "\t\t\tisa = XCConfigurationList;\n",
            "\t\t\tdefaultConfigurationName = Release;\n",
            "\t\t};\n",
            "\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* Build configuration list for PBXNativeTarget \"alpha\" */ = {\n",
            "\t\t\tisa = XCConfigurationList;\n",
            "\t\t\tdefaultConfigurationName = Release;\n",
            "\t\t};\n",
            "/* End XCConfigurationList section */\n",
        );
        let (out, stats) = sort(input);
        let pos_alpha = out.find("\"alpha\"").unwrap();
        let pos_zebra = out.find("\"zebra\"").unwrap();
        assert!(pos_alpha < pos_zebra, "alpha should sort before zebra");
        assert_eq!(stats.xc_config_lists_sorted, 1);
    }

    #[test]
    fn sorts_pbx_native_targets_alphabetically() {
        let input = concat!(
            "/* Begin PBXNativeTarget section */\n",
            "\t\tBBBBBBBBBBBBBBBBBBBBBBBB /* ZebraApp */ = {\n",
            "\t\t\tisa = PBXNativeTarget;\n",
            "\t\t\tname = ZebraApp;\n",
            "\t\t};\n",
            "\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* AlphaApp */ = {\n",
            "\t\t\tisa = PBXNativeTarget;\n",
            "\t\t\tname = AlphaApp;\n",
            "\t\t};\n",
            "/* End PBXNativeTarget section */\n",
        );
        let (out, stats) = sort(input);
        let pos_alpha = out.find("AlphaApp").unwrap();
        let pos_zebra = out.find("ZebraApp").unwrap();
        assert!(pos_alpha < pos_zebra, "AlphaApp should sort before ZebraApp");
        assert_eq!(stats.pbx_native_targets_sorted, 1);
    }

    #[test]
    fn sorts_pbx_aggregate_targets_alphabetically() {
        let input = concat!(
            "/* Begin PBXAggregateTarget section */\n",
            "\t\tBBBBBBBBBBBBBBBBBBBBBBBB /* ZebraFramework */ = {\n",
            "\t\t\tisa = PBXAggregateTarget;\n",
            "\t\t\tname = ZebraFramework;\n",
            "\t\t};\n",
            "\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* AlphaFramework */ = {\n",
            "\t\t\tisa = PBXAggregateTarget;\n",
            "\t\t\tname = AlphaFramework;\n",
            "\t\t};\n",
            "/* End PBXAggregateTarget section */\n",
        );
        let (out, stats) = sort(input);
        let pos_alpha = out.find("AlphaFramework").unwrap();
        let pos_zebra = out.find("ZebraFramework").unwrap();
        assert!(pos_alpha < pos_zebra, "AlphaFramework should sort before ZebraFramework");
        assert_eq!(stats.pbx_aggregate_targets_sorted, 1);
    }

    #[test]
    fn sorts_pbx_groups_alphabetically() {
        let input = concat!(
            "/* Begin PBXGroup section */\n",
            "\t\tBBBBBBBBBBBBBBBBBBBBBBBB /* ZebraGroup */ = {\n",
            "\t\t\tisa = PBXGroup;\n",
            "\t\t\tchildren = (\n",
            "\t\t\t);\n",
            "\t\t\tname = ZebraGroup;\n",
            "\t\t};\n",
            "\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* AlphaGroup */ = {\n",
            "\t\t\tisa = PBXGroup;\n",
            "\t\t\tchildren = (\n",
            "\t\t\t);\n",
            "\t\t\tname = AlphaGroup;\n",
            "\t\t};\n",
            "/* End PBXGroup section */\n",
        );
        let (out, stats) = sort(input);
        let pos_alpha = out.find("AlphaGroup").unwrap();
        let pos_zebra = out.find("ZebraGroup").unwrap();
        assert!(pos_alpha < pos_zebra, "AlphaGroup should sort before ZebraGroup");
        assert_eq!(stats.pbx_groups_sorted, 1);
    }

    #[test]
    fn sorts_pbx_target_dependencies_by_target_name() {
        let input = concat!(
            "/* Begin PBXTargetDependency section */\n",
            "\t\tBBBBBBBBBBBBBBBBBBBBBBBB /* PBXTargetDependency */ = {\n",
            "\t\t\tisa = PBXTargetDependency;\n",
            "\t\t\ttarget = CCCCCCCCCCCCCCCCCCCCCCCC /* ZebraApp */;\n",
            "\t\t\ttargetProxy = DDDDDDDDDDDDDDDDDDDDDDDD /* PBXContainerItemProxy */;\n",
            "\t\t};\n",
            "\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* PBXTargetDependency */ = {\n",
            "\t\t\tisa = PBXTargetDependency;\n",
            "\t\t\ttarget = EEEEEEEEEEEEEEEEEEEEEEEE /* AlphaApp */;\n",
            "\t\t\ttargetProxy = FFFFFFFFFFFFFFFFFFFFFFFF /* PBXContainerItemProxy */;\n",
            "\t\t};\n",
            "/* End PBXTargetDependency section */\n",
        );
        let (out, stats) = sort(input);
        let pos_alpha = out.find("AlphaApp").unwrap();
        let pos_zebra = out.find("ZebraApp").unwrap();
        assert!(pos_alpha < pos_zebra, "AlphaApp dependency should sort before ZebraApp");
        assert_eq!(stats.pbx_target_dependencies_sorted, 1);
    }
}
