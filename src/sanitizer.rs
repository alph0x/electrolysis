//! Phase 1 — text-level corruption repair.
//!
//! Handles:
//! - Git merge conflict markers (keeps HEAD / ours side)
//! - Duplicate UUID declarations inside `objects = { … }`
//! - Duplicate items inside `files = (…)` / `children = (…)` arrays
//! - Unclosed `/* Begin … section */` blocks (drops the orphan)
//! - Orphaned object bodies (object fields without a UUID declaration header,
//!   left behind when a previous uniquifier run dropped the header line)
//! - Trailing garbage after the final `}`

use std::collections::HashSet;

use once_cell::sync::Lazy;
use regex::Regex;

/// Conflict-marker patterns.
static RE_CONFLICT_OURS: Lazy<Regex> = Lazy::new(|| Regex::new(r"^<{7}[^\n]*$").unwrap());
static RE_CONFLICT_SEP:  Lazy<Regex> = Lazy::new(|| Regex::new(r"^={7}$").unwrap());
static RE_CONFLICT_THEIRS: Lazy<Regex> = Lazy::new(|| Regex::new(r"^>{7}[^\n]*$").unwrap());

/// A 24-char Xcode UUID or a 32-char xUnique UUID at the start of an object declaration.
/// Must end with `= {` (with or without an inline `/* comment */`) to distinguish
/// true declarations from list items such as `UUID /* name */,`.
///
/// Forms handled:
///   `\t\tABCDEF… /* comment */ = {`   (commented)
///   `\t\tABCDEF… = {`                 (bare, no comment)
static RE_OBJECT_DECL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s+([0-9A-Fa-f]{24}|[0-9A-Fa-f]{32})\b[^,;]*=\s*\{").unwrap());

/// An item inside a `files = (…)` or `children = (…)` list:
///   `\t\t\tABCDEF… /* comment */,`
static RE_LIST_ITEM: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s+([0-9A-Fa-f]{24}|[0-9A-Fa-f]{32})\s").unwrap());

static RE_BEGIN_SECTION: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/\* Begin (\S+) section \*/").unwrap());
static RE_END_SECTION: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/\* End (\S+) section \*/").unwrap());

/// A UUID declaration at the start of an object entry — matches both the
/// commented form `UUID /* name */ = {` and the bare form `UUID = {`.
static RE_OBJECT_DECL_ANY: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s+([0-9A-Fa-f]{24}|[0-9A-Fa-f]{32})(\s+/\*|\s*=)").unwrap());

/// Stats collected during sanitization for reporting.
#[derive(Debug, Default)]
pub struct SanitizeStats {
    pub conflict_hunks_resolved: usize,
    pub duplicate_objects_removed: usize,
    pub duplicate_list_items_removed: usize,
    pub orphan_sections_removed: usize,
    pub orphan_object_bodies_removed: usize,
    pub target_attributes_wrapper_added: bool,
}

/// Fix all known corruption patterns and return the cleaned text plus stats.
pub fn sanitize(input: &str) -> (String, SanitizeStats) {
    let mut stats = SanitizeStats::default();

    // Pass 1 — resolve merge conflicts (keep "ours" / HEAD side).
    let after_conflicts = resolve_conflicts(input, &mut stats);

    // Pass 2 — remove duplicate UUID declarations in the objects block.
    let after_dedup_objects = dedup_object_declarations(&after_conflicts, &mut stats);

    // Pass 3 — remove duplicate items inside list arrays.
    let after_dedup_lists = dedup_list_items(&after_dedup_objects, &mut stats);

    // Pass 4 — remove orphaned Begin sections (no matching End).
    let after_orphans = remove_orphan_sections(&after_dedup_lists, &mut stats);

    // Pass 5 — remove orphaned object bodies: fields/closers left behind when a
    // previous uniquifier run stripped the UUID declaration header line.
    let after_bodies = remove_orphan_object_bodies(&after_orphans, &mut stats);

    // Pass 6 — ensure `PBXProject.attributes.TargetAttributes` exists.  Older
    // electrolysis versions (and other tools) sometimes strip this wrapper
    // entirely, leaving fastlane's `automatic_code_signing` /
    // `update_code_signing_settings` actions to abort with the misleading
    // "Seems to be a very old project file format" error.  An empty wrapper
    // is sufficient — fastlane auto-populates per-target entries on demand.
    let after_target_attrs = ensure_pbxproject_target_attributes(&after_bodies, &mut stats);

    (after_target_attrs, stats)
}

// ── Pass 1 ──────────────────────────────────────────────────────────────────

fn resolve_conflicts(input: &str, stats: &mut SanitizeStats) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_conflict = false;
    let mut past_separator = false;

    for line in input.lines() {
        if RE_CONFLICT_OURS.is_match(line) {
            in_conflict = true;
            past_separator = false;
            stats.conflict_hunks_resolved += 1;
            continue;
        }
        if in_conflict && RE_CONFLICT_SEP.is_match(line) {
            past_separator = true;
            continue;
        }
        if in_conflict && RE_CONFLICT_THEIRS.is_match(line) {
            in_conflict = false;
            past_separator = false;
            continue;
        }

        // Inside a conflict: keep only "ours" (before the separator).
        if in_conflict && past_separator {
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

// ── Pass 2 ──────────────────────────────────────────────────────────────────

fn dedup_object_declarations(input: &str, stats: &mut SanitizeStats) -> String {
    let mut out = String::with_capacity(input.len());
    let mut seen: HashSet<String> = HashSet::new();
    // We only deduplicate inside the objects = { … } block.
    // Track brace depth relative to "objects = {" to know when we leave it.
    let mut in_objects = false;
    let mut objects_depth: i32 = 0; // depth increases at '{', decreases at '}'
    // When Some(target), skip lines until objects_depth drops to target.
    // This correctly handles nested braces (e.g. buildSettings = { … };) inside
    // a duplicate object body without terminating the skip too early.
    let mut skip_to_depth: Option<i32> = None;

    for line in input.lines() {
        // Detect the start of the objects block.
        if !in_objects {
            if line.trim_start().starts_with("objects = {") || line.trim() == "objects = {" {
                in_objects = true;
                objects_depth = 1;
                out.push_str(line);
                out.push('\n');
                continue;
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }

        // Track brace depth to detect end of objects block.
        // Use the quoted-string-aware counter so braces inside shellScript etc.
        // don't corrupt the depth.
        let delta = brace_delta(line);
        objects_depth += delta;

        if objects_depth <= 0 {
            // Closing brace of the objects block itself.
            in_objects = false;
            skip_to_depth = None;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        // Check if we are currently skipping a duplicate multi-line object body.
        // We stop as soon as depth returns to the level it was BEFORE the object
        // opened — that line (the closing `};`) is also part of the duplicate and
        // must be skipped.
        if let Some(target) = skip_to_depth {
            stats.duplicate_objects_removed += 1;
            if objects_depth <= target {
                skip_to_depth = None;
            }
            continue;
        }

        // Only consider a UUID-keyed `… = {` line as an *object declaration*
        // when it sits as a direct child of the `objects = {…}` wrapper —
        // i.e. the depth before opening this brace was exactly 1.  Deeper
        // nestings are UUID-keyed dicts that happen to share their key with
        // a top-level object UUID (e.g. `attributes.TargetAttributes` in
        // PBXProject indexes per-target signing metadata by target UUID).
        // Treating those as duplicates strips the entry body and breaks
        // tooling — fastlane's `automatic_code_signing` action requires
        // the surviving entries to populate `code_sign_identity` and team.
        let depth_before = objects_depth - delta;
        if depth_before != 1 {
            out.push_str(line);
            out.push('\n');
            continue;
        }

        // Check for a UUID object declaration line.
        if let Some(cap) = RE_OBJECT_DECL.captures(line) {
            let uuid = cap[1].to_uppercase();
            if seen.contains(&uuid) {
                stats.duplicate_objects_removed += 1;
                // target = depth before this object's opening brace(s)
                let target = objects_depth - delta;
                if objects_depth > target {
                    // Multi-line object: skip body lines until depth returns.
                    skip_to_depth = Some(target);
                }
                // If net delta == 0 (single-line with balanced braces) we are
                // already back at target depth — nothing more to skip.
                continue;
            }
            seen.insert(uuid);
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

/// Count the net `{` minus `}` in `line`, ignoring characters inside
/// double-quoted strings.  This prevents shell-script values like
/// `shellScript = "if [ \"${VAR}\" ];"` from corrupting the depth counter.
pub(crate) fn brace_delta(line: &str) -> i32 {
    let mut delta = 0i32;
    let mut in_quotes = false;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_quotes => i += 1, // skip escaped char
            b'"' => in_quotes = !in_quotes,
            b'{' if !in_quotes => delta += 1,
            b'}' if !in_quotes => delta -= 1,
            _ => {}
        }
        i += 1;
    }
    delta
}

// ── Pass 3 ──────────────────────────────────────────────────────────────────

fn dedup_list_items(input: &str, stats: &mut SanitizeStats) -> String {
    let mut out = String::with_capacity(input.len());
    // We deduplicate inside any `files = (…)` or `children = (…)` list.
    let mut in_list = false;
    let mut seen: HashSet<String> = HashSet::new();

    for line in input.lines() {
        let trimmed = line.trim();

        if !in_list {
            if trimmed.ends_with("= (") || trimmed == "(" {
                in_list = true;
                seen.clear();
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }

        // End of list.
        if trimmed == ");" || trimmed == ")" {
            in_list = false;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        // Check for a UUID list item.
        if let Some(cap) = RE_LIST_ITEM.captures(line) {
            let uuid = cap[1].to_uppercase();
            if seen.contains(&uuid) {
                stats.duplicate_list_items_removed += 1;
                continue;
            }
            seen.insert(uuid);
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

// ── Pass 4 ──────────────────────────────────────────────────────────────────

fn remove_orphan_sections(input: &str, stats: &mut SanitizeStats) -> String {
    // First pass: collect which sections have both a Begin and an End.
    let mut has_end: HashSet<String> = HashSet::new();
    let mut has_begin: HashSet<String> = HashSet::new();

    for line in input.lines() {
        if let Some(cap) = RE_BEGIN_SECTION.captures(line) {
            has_begin.insert(cap[1].to_string());
        }
        if let Some(cap) = RE_END_SECTION.captures(line) {
            has_end.insert(cap[1].to_string());
        }
    }

    let orphan_begins: HashSet<&String> = has_begin.difference(&has_end).collect();
    let orphan_ends:   HashSet<&String> = has_end.difference(&has_begin).collect();

    if orphan_begins.is_empty() && orphan_ends.is_empty() {
        return input.to_string();
    }

    // Second pass: drop content between orphan Begin…(no matching End).
    let mut out = String::with_capacity(input.len());
    let mut skip_section: Option<String> = None;

    for line in input.lines() {
        if let Some(cap) = RE_BEGIN_SECTION.captures(line) {
            let name = cap[1].to_string();
            if orphan_begins.contains(&name) {
                skip_section = Some(name);
                stats.orphan_sections_removed += 1;
                continue;
            }
        }
        if let Some(ref name) = skip_section.clone() {
            // Keep looking until we find the End (shouldn't exist, but be safe).
            if let Some(cap) = RE_END_SECTION.captures(line) {
                if &cap[1] == name {
                    skip_section = None;
                    continue;
                }
            }
            continue;
        }
        // Drop lines from orphan End markers.
        if let Some(cap) = RE_END_SECTION.captures(line) {
            let name = cap[1].to_string();
            if orphan_ends.contains(&name) {
                stats.orphan_sections_removed += 1;
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

// ── Pass 5 ──────────────────────────────────────────────────────────────────

/// Remove orphaned object bodies from the `objects = { … }` block.
///
/// An orphaned body is an object's field-list and closing `};` that appears
/// without a preceding `UUID = {` declaration header.  This happens when a
/// prior uniquifier run dropped the header because the UUID was unreachable,
/// but left the body lines behind (they contain no UUIDs so the text-replace
/// pass kept them).
///
/// Detection: inside the objects block, at depth 1, any non-empty,
/// non-comment line that is NOT a UUID declaration is an orphaned fragment.
/// We skip it and all following lines until the `};` that would close the
/// (phantom) object — identified by the brace depth dropping to 0.  We do
/// NOT actually decrement the depth past 1 for that closer, because the
/// matching opener was already removed.
fn remove_orphan_object_bodies(input: &str, stats: &mut SanitizeStats) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_objects = false;
    let mut objects_depth: i32 = 0;
    let mut in_orphan_body = false;

    for line in input.lines() {
        if !in_objects {
            if line.trim_start().starts_with("objects = {") || line.trim() == "objects = {" {
                in_objects = true;
                objects_depth = 1;
                out.push_str(line);
                out.push('\n');
                continue;
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }

        let delta = brace_delta(line);

        if in_orphan_body {
            // Compute what depth WOULD become if we applied the delta.
            // When that value hits 0 we've consumed the phantom closing brace
            // of the headerless object.  We discard the line but do NOT update
            // objects_depth, keeping us at depth 1 (still inside objects).
            let next_depth = objects_depth + delta;
            stats.orphan_object_bodies_removed += 1;
            if next_depth <= 0 {
                in_orphan_body = false;
                // objects_depth deliberately not updated — stays at 1.
            } else {
                objects_depth = next_depth;
            }
            continue;
        }

        // Apply delta normally for non-orphan lines.
        objects_depth += delta;

        if objects_depth <= 0 {
            // Real closing brace of the objects block.
            in_objects = false;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        // At the top level of the objects block (pre-delta depth == 1, meaning
        // the line is the first thing we see at object-entry depth) a valid line
        // is either a UUID declaration, a section comment, or whitespace.
        // Anything else is an orphaned body fragment.
        let pre_delta_depth = objects_depth - delta;
        if pre_delta_depth == 1 {
            let trimmed = line.trim();
            let is_uuid_decl   = RE_OBJECT_DECL_ANY.is_match(line);
            let is_comment     = trimmed.starts_with("/*") || trimmed.starts_with("//");
            let is_close_brace = trimmed == "}" || trimmed == "};";
            let is_blank       = trimmed.is_empty();

            if !is_blank && !is_comment && !is_close_brace && !is_uuid_decl {
                // Orphaned body detected.
                in_orphan_body = true;
                stats.orphan_object_bodies_removed += 1;
                // Undo the depth change we applied above — the `{` that would
                // have opened this object is gone, so the current line's delta
                // should not count toward our depth tracking.
                objects_depth = pre_delta_depth;
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

// ── Pass 6 ──────────────────────────────────────────────────────────────────

/// Ensure that `PBXProject.attributes.TargetAttributes` exists.
///
/// fastlane's `automatic_code_signing` and `update_code_signing_settings`
/// actions both bail with the misleading error `"Seems to be a very old
/// project file format - please open your project file in a more recent
/// version of Xcode"` when `attributes["TargetAttributes"]` is `nil` (Ruby
/// nil — i.e. the wrapper is fully absent).  Older electrolysis versions
/// stripped the wrapper entirely; modern Xcode 16+ doesn't write it back
/// automatically; the actions that need it can't generate it themselves.
///
/// This pass adds an empty `TargetAttributes = { };` inside `PBXProject.
/// attributes` when the wrapper is missing.  fastlane then auto-populates
/// per-target entries on demand, so an empty wrapper is fully sufficient.
///
/// Idempotent: pbxprojs that already have any `TargetAttributes` block
/// inside the PBXProject attributes are left untouched.
fn ensure_pbxproject_target_attributes(input: &str, stats: &mut SanitizeStats) -> String {
    let mut out = String::with_capacity(input.len() + 64);
    let mut in_pbx_project_section = false;
    let mut in_pbx_project_object = false;
    let mut in_attributes = false;
    let mut attr_depth: i32 = 0;
    let mut saw_target_attributes = false;
    let mut just_opened_attributes = false;
    let mut injected = false;

    for line in input.lines() {
        if line.contains("/* Begin PBXProject section */") {
            in_pbx_project_section = true;
        }
        if line.contains("/* End PBXProject section */") {
            in_pbx_project_section = false;
            in_pbx_project_object = false;
        }

        if in_pbx_project_section && !in_pbx_project_object && line.contains("isa = PBXProject;") {
            in_pbx_project_object = true;
        }

        // Detect the opening line of `attributes = {`.
        if in_pbx_project_object
            && !in_attributes
            && line.trim_start().starts_with("attributes = {")
        {
            in_attributes = true;
            attr_depth = 1;
            saw_target_attributes = false;
            just_opened_attributes = true;
        }

        if in_attributes && !just_opened_attributes {
            let trimmed = line.trim_start();
            if trimmed.starts_with("TargetAttributes =") || trimmed.starts_with("TargetAttributes=") {
                saw_target_attributes = true;
            }

            let delta = brace_delta(line);
            if attr_depth + delta <= 0 {
                // This line closes the attributes block.  Inject before it
                // when no TargetAttributes child was present.
                if !saw_target_attributes && !injected {
                    let indent = leading_whitespace(line);
                    out.push_str(indent);
                    out.push_str("\tTargetAttributes = {\n");
                    out.push_str(indent);
                    out.push_str("\t};\n");
                    injected = true;
                    stats.target_attributes_wrapper_added = true;
                }
                in_attributes = false;
                in_pbx_project_object = false;
            }
            attr_depth += delta;
        }

        just_opened_attributes = false;

        out.push_str(line);
        out.push('\n');
    }

    out
}

fn leading_whitespace(s: &str) -> &str {
    let trimmed = s.trim_start();
    &s[..s.len() - trimmed.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_merge_conflict() {
        let input = concat!(
            "line before\n",
            "<<<<<<< HEAD\n",
            "ours content\n",
            "=======\n",
            "theirs content\n",
            ">>>>>>> feature/branch\n",
            "line after\n",
        );
        let (out, stats) = sanitize(input);
        assert!(out.contains("ours content"));
        assert!(!out.contains("theirs content"));
        assert_eq!(stats.conflict_hunks_resolved, 1);
    }

    #[test]
    fn deduplicates_list_items() {
        let input = concat!(
            "\t\t\tfiles = (\n",
            "\t\t\t\tABCDEF123456789012345678 /* file.swift in Sources */,\n",
            "\t\t\t\tABCDEF123456789012345678 /* file.swift in Sources */,\n",
            "\t\t\t);\n",
        );
        let (out, stats) = sanitize(input);
        let count = out.matches("ABCDEF123456789012345678").count();
        assert_eq!(count, 1);
        assert_eq!(stats.duplicate_list_items_removed, 1);
    }

    // ── PBXProject.attributes.TargetAttributes preservation ─────────────────
    //
    // Regression: the dedup pass walks every UUID-keyed `UUID = { … }` line
    // inside `objects = {…}` regardless of nesting depth.  When a UUID
    // appears as a *key* of `attributes.TargetAttributes` (signing metadata
    // indexed by target UUID) **and** the same UUID is also the declaration
    // of a top-level `PBXNativeTarget`, the dedup pass treats the inner
    // entry as a duplicate and strips its body.  The wrapper survives empty.
    //
    // Fastlane's `automatic_code_signing` action requires
    // `attributes["TargetAttributes"]` to exist with target entries; an
    // empty wrapper auto-populates, but we still want the pre-existing
    // metadata (DevelopmentTeam, ProvisioningStyle, LastSwiftMigration…)
    // preserved because it carries non-default values.

    fn pbxproj_with_target_attributes() -> String {
        concat!(
            "// !$*UTF8*$!\n",
            "{\n",
            "\tarchiveVersion = 1;\n",
            "\tclasses = {\n",
            "\t};\n",
            "\tobjectVersion = 77;\n",
            "\tobjects = {\n",
            "/* Begin PBXNativeTarget section */\n",
            "\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* WAPA */ = {\n",
            "\t\t\tisa = PBXNativeTarget;\n",
            "\t\t\tname = WAPA;\n",
            "\t\t};\n",
            "/* End PBXNativeTarget section */\n",
            "\n",
            "/* Begin PBXProject section */\n",
            "\t\tBBBBBBBBBBBBBBBBBBBBBBBB /* Project object */ = {\n",
            "\t\t\tisa = PBXProject;\n",
            "\t\t\tattributes = {\n",
            "\t\t\t\tBuildIndependentTargetsInParallel = YES;\n",
            "\t\t\t\tLastUpgradeCheck = 1330;\n",
            "\t\t\t\tTargetAttributes = {\n",
            "\t\t\t\t\tAAAAAAAAAAAAAAAAAAAAAAAA = {\n",
            "\t\t\t\t\t\tDevelopmentTeam = P78229D8QW;\n",
            "\t\t\t\t\t\tProvisioningStyle = Manual;\n",
            "\t\t\t\t\t};\n",
            "\t\t\t\t};\n",
            "\t\t\t};\n",
            "\t\t};\n",
            "/* End PBXProject section */\n",
            "\t};\n",
            "\trootObject = BBBBBBBBBBBBBBBBBBBBBBBB;\n",
            "}\n",
        )
        .to_string()
    }

    #[test]
    fn preserves_target_attributes_entries_keyed_by_native_target_uuid() {
        let input = pbxproj_with_target_attributes();
        let (out, stats) = sanitize(&input);

        assert!(
            out.contains("DevelopmentTeam = P78229D8QW"),
            "TargetAttributes inner entry must survive sanitize:\n{out}"
        );
        assert!(
            out.contains("ProvisioningStyle = Manual"),
            "all signing metadata inside TargetAttributes must survive:\n{out}"
        );
        // The native target itself must obviously also still be there.
        assert!(out.contains("isa = PBXNativeTarget"));
        // The native target's UUID must appear at least 3 times: section
        // declaration, TargetAttributes key, plus possibly references.  The
        // bug was that the second occurrence (TargetAttributes key) and its
        // body were stripped, dropping the count to 1.
        let count = out.matches("AAAAAAAAAAAAAAAAAAAAAAAA").count();
        assert!(
            count >= 2,
            "expected UUID to appear in both PBXNativeTarget and \
             TargetAttributes; got {count} occurrence(s):\n{out}"
        );
        assert_eq!(
            stats.duplicate_objects_removed, 0,
            "TargetAttributes entry must not be counted as a duplicate \
             top-level object"
        );
    }

    #[test]
    fn still_dedups_real_duplicate_top_level_objects() {
        // Regression guard for Step 4 of the plan: the depth fix must not
        // weaken legitimate top-level dedup.  Same UUID declared twice as a
        // direct child of `objects = {…}` should still collapse to one.
        let input = concat!(
            "// !$*UTF8*$!\n",
            "{\n",
            "\tarchiveVersion = 1;\n",
            "\tobjectVersion = 77;\n",
            "\tobjects = {\n",
            "\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* one */ = {\n",
            "\t\t\tisa = PBXFileReference;\n",
            "\t\t\tpath = a.swift;\n",
            "\t\t};\n",
            "\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* dup */ = {\n",
            "\t\t\tisa = PBXFileReference;\n",
            "\t\t\tpath = a.swift;\n",
            "\t\t};\n",
            "\t};\n",
            "\trootObject = AAAAAAAAAAAAAAAAAAAAAAAA;\n",
            "}\n",
        );
        let (out, stats) = sanitize(input);
        let count = out.matches("AAAAAAAAAAAAAAAAAAAAAAAA").count();
        assert_eq!(
            count, 2,
            "one top-level decl plus rootObject reference; the duplicate \
             body must be removed"
        );
        assert!(stats.duplicate_objects_removed >= 1);
    }

    #[test]
    fn target_attributes_preservation_is_idempotent() {
        let once = sanitize(&pbxproj_with_target_attributes()).0;
        let (twice, stats) = sanitize(&once);
        assert_eq!(once, twice, "second pass must be a no-op");
        assert_eq!(stats.duplicate_objects_removed, 0);
    }

    // ── ensure_pbxproject_target_attributes (Pass 6) ─────────────────────────
    //
    // When `PBXProject.attributes.TargetAttributes` is fully absent — the
    // wrapper isn't even there — fastlane's `automatic_code_signing` action
    // bails with "very old project file format".  We add an empty wrapper so
    // fastlane can auto-populate entries on demand.

    fn pbxproj_without_target_attributes() -> String {
        concat!(
            "// !$*UTF8*$!\n",
            "{\n",
            "\tarchiveVersion = 1;\n",
            "\tclasses = {\n",
            "\t};\n",
            "\tobjectVersion = 77;\n",
            "\tobjects = {\n",
            "/* Begin PBXNativeTarget section */\n",
            "\t\tAAAAAAAAAAAAAAAAAAAAAAAA /* WAPA */ = {\n",
            "\t\t\tisa = PBXNativeTarget;\n",
            "\t\t\tname = WAPA;\n",
            "\t\t};\n",
            "/* End PBXNativeTarget section */\n",
            "\n",
            "/* Begin PBXProject section */\n",
            "\t\tBBBBBBBBBBBBBBBBBBBBBBBB /* Project object */ = {\n",
            "\t\t\tisa = PBXProject;\n",
            "\t\t\tattributes = {\n",
            "\t\t\t\tBuildIndependentTargetsInParallel = YES;\n",
            "\t\t\t\tLastUpgradeCheck = 1330;\n",
            "\t\t\t};\n",
            "\t\t\tbuildConfigurationList = CCCCCCCCCCCCCCCCCCCCCCCC;\n",
            "\t\t};\n",
            "/* End PBXProject section */\n",
            "\t};\n",
            "\trootObject = BBBBBBBBBBBBBBBBBBBBBBBB;\n",
            "}\n",
        )
        .to_string()
    }

    #[test]
    fn injects_empty_target_attributes_wrapper_when_missing() {
        let input = pbxproj_without_target_attributes();
        assert!(!input.contains("TargetAttributes"), "fixture must lack wrapper");

        let (out, stats) = sanitize(&input);

        assert!(stats.target_attributes_wrapper_added, "stat must flip");
        assert!(
            out.contains("TargetAttributes = {"),
            "wrapper must be added inside attributes block:\n{out}"
        );
        // Pre-existing keys must still be there:
        assert!(out.contains("BuildIndependentTargetsInParallel = YES"));
        assert!(out.contains("LastUpgradeCheck = 1330"));
    }

    #[test]
    fn target_attributes_lives_inside_pbxproject_attributes_block() {
        let (out, _) = sanitize(&pbxproj_without_target_attributes());

        // The new TargetAttributes line must appear AFTER `attributes = {`
        // and BEFORE its closing `};`, inside the PBXProject block.
        let after_attrs_open = out
            .find("attributes = {")
            .expect("attributes block must exist");
        let target_attrs_pos = out
            .find("TargetAttributes = {")
            .expect("wrapper must be inserted");
        assert!(
            target_attrs_pos > after_attrs_open,
            "TargetAttributes must come after `attributes = {{`"
        );
        // And before buildConfigurationList (which is the next sibling key
        // in PBXProject after attributes).
        let bcl_pos = out
            .find("buildConfigurationList")
            .expect("buildConfigurationList must remain");
        assert!(
            target_attrs_pos < bcl_pos,
            "TargetAttributes must be inside attributes, before buildConfigurationList"
        );
    }

    #[test]
    fn does_not_inject_when_target_attributes_already_exists() {
        let (out, stats) = sanitize(&pbxproj_with_target_attributes());
        assert!(
            !stats.target_attributes_wrapper_added,
            "must not inject when wrapper is already present"
        );
        // Entry from the fixture must remain (tests preservation + no double).
        assert_eq!(
            out.matches("TargetAttributes = {").count(),
            1,
            "must not duplicate existing wrapper"
        );
    }

    #[test]
    fn does_not_inject_when_empty_wrapper_already_exists() {
        // Edge case: wrapper exists but is empty.  fastlane would already
        // pass the guard.  Don't add a second wrapper.
        let input = concat!(
            "// !$*UTF8*$!\n",
            "{\n",
            "\tarchiveVersion = 1;\n",
            "\tobjectVersion = 77;\n",
            "\tobjects = {\n",
            "/* Begin PBXProject section */\n",
            "\t\tBBBBBBBBBBBBBBBBBBBBBBBB /* Project object */ = {\n",
            "\t\t\tisa = PBXProject;\n",
            "\t\t\tattributes = {\n",
            "\t\t\t\tTargetAttributes = {\n",
            "\t\t\t\t};\n",
            "\t\t\t};\n",
            "\t\t};\n",
            "/* End PBXProject section */\n",
            "\t};\n",
            "\trootObject = BBBBBBBBBBBBBBBBBBBBBBBB;\n",
            "}\n",
        );
        let (out, stats) = sanitize(input);
        assert!(!stats.target_attributes_wrapper_added);
        assert_eq!(out.matches("TargetAttributes = {").count(), 1);
    }

    #[test]
    fn injection_is_idempotent() {
        let once = sanitize(&pbxproj_without_target_attributes()).0;
        let (twice, stats) = sanitize(&once);
        assert_eq!(once, twice, "second pass must be a no-op");
        assert!(
            !stats.target_attributes_wrapper_added,
            "second pass must not re-inject"
        );
    }
}
