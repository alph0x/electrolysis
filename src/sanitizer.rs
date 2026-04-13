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

    (after_bodies, stats)
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
}
