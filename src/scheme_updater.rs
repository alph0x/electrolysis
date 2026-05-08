//! Phase 6 — propagate UUID remappings to `.xcscheme` files.
//!
//! The uniquifier rewrites every UUID inside `project.pbxproj` but Xcode
//! schemes live outside that file (in `xcshareddata/xcschemes/*.xcscheme`).
//! Each `BuildableReference` inside a scheme references its target via a
//! `BlueprintIdentifier="<uuid>"` attribute that is **not** inside the
//! pbxproj — so without this pass the schemes orphan as soon as the
//! uniquifier runs.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use regex::Regex;

use crate::logger::Logger;
use crate::pipeline::FileSystem;

// Matches the value of a `BlueprintIdentifier` attribute.  Tolerates both
// `BlueprintIdentifier = "…"` (Xcode default) and the unspaced form, plus
// 24- or 32-char UUIDs.  The 32-char alternative is listed first so it wins
// over a 24-char prefix.
static RE_BLUEPRINT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"BlueprintIdentifier\s*=\s*"([0-9A-Fa-f]{32}|[0-9A-Fa-f]{24})""#)
        .expect("BlueprintIdentifier regex must compile")
});

// Captures one whole `<BuildableReference …>` opening element (attributes
// span multiple lines, so `(?s)` lets `.` match newlines).
static RE_BUILDABLE_REF: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)<BuildableReference\b[^>]*>")
        .expect("BuildableReference regex must compile")
});

// Generic `Attr = "value"` matcher used to read attributes out of a
// captured `<BuildableReference>` block.
static RE_SCHEME_ATTR: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(\w+)\s*=\s*"([^"]*)""#).expect("scheme attribute regex must compile")
});

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UpdateStats {
    pub files_scanned: usize,
    pub files_modified: usize,
    pub identifiers_replaced: usize,
}

/// Rewrite every `BlueprintIdentifier="<uuid>"` whose UUID has a mapping in
/// `map`.  Unmapped UUIDs are left untouched.  Returns the new XML and the
/// number of replacements made.
pub fn remap_blueprint_identifiers(
    xml: &str,
    map: &HashMap<String, String>,
) -> (String, usize) {
    if map.is_empty() {
        return (xml.to_string(), 0);
    }

    let mut count = 0usize;
    let new_xml = RE_BLUEPRINT.replace_all(xml, |caps: &regex::Captures| {
        let key = caps[1].to_uppercase();
        match map.get(&key) {
            Some(new_uuid) => {
                count += 1;
                format!(r#"BlueprintIdentifier = "{}""#, new_uuid)
            }
            None => caps[0].to_string(),
        }
    });

    (new_xml.into_owned(), count)
}

/// Walk `<bundle>.xcodeproj/xcshareddata/xcschemes` and apply the UUID map
/// to every `.xcscheme` file.  Schemes already on the new UUIDs are left
/// untouched (idempotent).  `xcuserdata` is intentionally not visited —
/// those schemes are per-developer and typically gitignored.
pub fn update_shared_schemes(
    xcodeproj_dir: &Path,
    map: &HashMap<String, String>,
    fs: &dyn FileSystem,
    logger: &dyn Logger,
) -> Result<UpdateStats> {
    let mut stats = UpdateStats::default();
    if map.is_empty() {
        return Ok(stats);
    }

    let schemes_dir = xcodeproj_dir.join("xcshareddata").join("xcschemes");
    let entries = match fs.list_dir(&schemes_dir) {
        Ok(e) => e,
        Err(_) => return Ok(stats),
    };

    for path in entries {
        if path.extension().and_then(|e| e.to_str()) != Some("xcscheme") {
            continue;
        }
        stats.files_scanned += 1;

        let content = fs
            .read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let (new_content, replaced) = remap_blueprint_identifiers(&content, map);

        if replaced == 0 || new_content == content {
            continue;
        }

        fs.write_atomic(&path, &new_content)
            .with_context(|| format!("cannot write {}", path.display()))?;
        stats.files_modified += 1;
        stats.identifiers_replaced += replaced;
        logger.verbose(&format!(
            "  ✓ updated {} ({} BlueprintIdentifier(s))",
            path.display(),
            replaced
        ));
    }

    Ok(stats)
}

/// Repair `BlueprintIdentifier` values whose UUID no longer corresponds to
/// any target in the pbxproj, by resolving them via `BlueprintName` against
/// the project's current target name → uuid index.
///
/// A `<BuildableReference>` block is repaired only when **all** of the
/// following hold:
///
/// 1. its current `BlueprintIdentifier` is **not** present in `valid_uuids`
///    (i.e. genuinely orphaned — not a cross-project reference whose UUID
///    is correctly resolved by some other pbxproj),
/// 2. its `BlueprintName` resolves to a single UUID in `name_index`, and
/// 3. its `ReferencedContainer` equals `container:<expected_container>`.
///
/// References that fail any of these guards are left untouched, keeping the
/// pass conservative — we only ever repair what we can prove belongs to us.
///
/// Returns the new XML and the number of identifiers repaired.
pub fn repair_orphan_blueprint_identifiers(
    xml: &str,
    name_index: &HashMap<String, String>,
    valid_uuids: &HashSet<String>,
    expected_container: &str,
) -> (String, usize) {
    if name_index.is_empty() {
        return (xml.to_string(), 0);
    }

    let expected_ref = format!("container:{}", expected_container);
    let mut count = 0usize;

    let new_xml = RE_BUILDABLE_REF.replace_all(xml, |caps: &regex::Captures| {
        let block = &caps[0];
        let attrs = parse_scheme_attrs(block);

        let Some(current_id) = attrs.get("BlueprintIdentifier") else {
            return block.to_string();
        };
        let Some(blueprint_name) = attrs.get("BlueprintName") else {
            return block.to_string();
        };
        let Some(container) = attrs.get("ReferencedContainer") else {
            return block.to_string();
        };

        if container != &expected_ref {
            return block.to_string();
        }
        if valid_uuids.contains(&current_id.to_uppercase())
            || valid_uuids.contains(*current_id)
        {
            return block.to_string();
        }
        let Some(canonical) = name_index.get(*blueprint_name) else {
            return block.to_string();
        };

        count += 1;
        let needle = format!(r#"BlueprintIdentifier = "{}""#, current_id);
        let alt_needle = format!(r#"BlueprintIdentifier="{}""#, current_id);
        let replacement = format!(r#"BlueprintIdentifier = "{}""#, canonical);
        block
            .replace(&needle, &replacement)
            .replace(&alt_needle, &replacement)
    });

    (new_xml.into_owned(), count)
}

/// Walk `<bundle>.xcodeproj/xcshareddata/xcschemes` and apply the orphan
/// repair pass to every `.xcscheme` file.  Idempotent.
pub fn repair_shared_schemes(
    xcodeproj_dir: &Path,
    name_index: &HashMap<String, String>,
    valid_uuids: &HashSet<String>,
    expected_container: &str,
    fs: &dyn FileSystem,
    logger: &dyn Logger,
) -> Result<UpdateStats> {
    let mut stats = UpdateStats::default();
    if name_index.is_empty() {
        return Ok(stats);
    }

    let schemes_dir = xcodeproj_dir.join("xcshareddata").join("xcschemes");
    let entries = match fs.list_dir(&schemes_dir) {
        Ok(e) => e,
        Err(_) => return Ok(stats),
    };

    for path in entries {
        if path.extension().and_then(|e| e.to_str()) != Some("xcscheme") {
            continue;
        }
        stats.files_scanned += 1;

        let content = fs
            .read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let (new_content, repaired) = repair_orphan_blueprint_identifiers(
            &content,
            name_index,
            valid_uuids,
            expected_container,
        );

        if repaired == 0 || new_content == content {
            continue;
        }

        fs.write_atomic(&path, &new_content)
            .with_context(|| format!("cannot write {}", path.display()))?;
        stats.files_modified += 1;
        stats.identifiers_replaced += repaired;
        logger.verbose(&format!(
            "  ✓ repaired {} ({} orphan BlueprintIdentifier(s))",
            path.display(),
            repaired
        ));
    }

    Ok(stats)
}

fn parse_scheme_attrs(block: &str) -> HashMap<&str, &str> {
    let mut out = HashMap::new();
    for caps in RE_SCHEME_ATTR.captures_iter(block) {
        let (Some(key), Some(value)) = (caps.get(1), caps.get(2)) else {
            continue;
        };
        out.entry(key.as_str()).or_insert(value.as_str());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_to_one(old: &str, new: &str) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert(old.to_string(), new.to_string());
        m
    }

    // ── remap_blueprint_identifiers ──────────────────────────────────────

    #[test]
    fn replaces_32_char_blueprint_identifier() {
        let xml = r#"<BuildableReference
            BlueprintIdentifier = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
            BlueprintName = "App">"#;
        let map = one_to_one(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        );
        let (out, n) = remap_blueprint_identifiers(xml, &map);
        assert_eq!(n, 1);
        assert!(out.contains(r#"BlueprintIdentifier = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB""#));
        assert!(!out.contains("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"));
    }

    #[test]
    fn replaces_24_char_blueprint_identifier() {
        let xml = r#"BlueprintIdentifier = "ABCD123456789012ABCD1234""#;
        let map = one_to_one(
            "ABCD123456789012ABCD1234",
            "00000000000000000000000000000001",
        );
        let (out, n) = remap_blueprint_identifiers(xml, &map);
        assert_eq!(n, 1);
        assert!(out.contains(r#"BlueprintIdentifier = "00000000000000000000000000000001""#));
    }

    #[test]
    fn leaves_unmapped_uuids_untouched() {
        let xml = r#"BlueprintIdentifier = "DEADBEEFDEADBEEFDEADBEEFDEADBEEF""#;
        let map: HashMap<String, String> = HashMap::new();
        let (out, n) = remap_blueprint_identifiers(xml, &map);
        assert_eq!(n, 0);
        assert_eq!(out, xml);
    }

    #[test]
    fn empty_map_short_circuits() {
        let xml = r#"BlueprintIdentifier = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA""#;
        let (out, n) = remap_blueprint_identifiers(xml, &HashMap::new());
        assert_eq!(out, xml);
        assert_eq!(n, 0);
    }

    #[test]
    fn lookup_is_case_insensitive() {
        // Schemes occasionally contain lowercase hex.
        let xml = r#"BlueprintIdentifier = "abcdef0123456789abcdef0123456789""#;
        let map = one_to_one(
            "ABCDEF0123456789ABCDEF0123456789",
            "11111111111111111111111111111111",
        );
        let (out, n) = remap_blueprint_identifiers(xml, &map);
        assert_eq!(n, 1);
        assert!(out.contains(r#"BlueprintIdentifier = "11111111111111111111111111111111""#));
    }

    #[test]
    fn multiple_blueprint_identifiers_in_one_xml() {
        let xml = r#"
            <BuildableReference BlueprintIdentifier = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" />
            <BuildableReference BlueprintIdentifier = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" />
            <BuildableReference BlueprintIdentifier = "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC" />
        "#;
        let mut map = HashMap::new();
        map.insert(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
            "11111111111111111111111111111111".to_string(),
        );
        map.insert(
            "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC".to_string(),
            "22222222222222222222222222222222".to_string(),
        );

        let (out, n) = remap_blueprint_identifiers(xml, &map);
        assert_eq!(n, 3);
        assert_eq!(out.matches("11111111111111111111111111111111").count(), 2);
        assert_eq!(out.matches("22222222222222222222222222222222").count(), 1);
        assert!(!out.contains("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"));
        assert!(!out.contains("CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC"));
    }

    #[test]
    fn does_not_match_other_attributes_with_uuid_values() {
        // Only `BlueprintIdentifier` should be remapped.  A scheme can
        // contain other attributes whose value happens to look like a UUID
        // (e.g. plugin identifiers); those must be left alone.
        let xml = r#"
            <Foo OtherIdentifier = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" />
            <BuildableReference BlueprintIdentifier = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" />
        "#;
        let map = one_to_one(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "11111111111111111111111111111111",
        );
        let (out, n) = remap_blueprint_identifiers(xml, &map);
        assert_eq!(n, 1, "only the BlueprintIdentifier should be replaced");
        assert!(out.contains(r#"OtherIdentifier = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA""#));
    }

    #[test]
    fn idempotent_on_already_remapped_xml() {
        let xml = r#"BlueprintIdentifier = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA""#;
        let map = one_to_one(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        );
        let (once, _) = remap_blueprint_identifiers(xml, &map);
        let (twice, n) = remap_blueprint_identifiers(&once, &map);
        assert_eq!(once, twice, "second pass must be a no-op");
        assert_eq!(n, 0, "second pass must replace nothing");
    }

    // ── update_shared_schemes ────────────────────────────────────────────

    use crate::logger::NullLogger;
    use crate::pipeline::test_double::InMemoryFileSystem;
    use std::path::PathBuf;

    fn scheme_with(uuid: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Scheme>
   <BuildAction>
      <BuildActionEntries>
         <BuildActionEntry>
            <BuildableReference
               BuildableIdentifier = "primary"
               BlueprintIdentifier = "{uuid}"
               BuildableName = "App.app"
               BlueprintName = "App"
               ReferencedContainer = "container:App.xcodeproj">
            </BuildableReference>
         </BuildActionEntry>
      </BuildActionEntries>
   </BuildAction>
</Scheme>
"#
        )
    }

    #[test]
    fn updates_a_scheme_with_a_stale_blueprint() {
        let fs = InMemoryFileSystem::new();
        let bundle = PathBuf::from("/repo/App.xcodeproj");
        let scheme = bundle.join("xcshareddata/xcschemes/App.xcscheme");
        fs.insert(
            scheme.clone(),
            scheme_with("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        );

        let map = one_to_one(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        );

        let stats = update_shared_schemes(&bundle, &map, &fs, &NullLogger).unwrap();
        assert_eq!(stats.files_scanned, 1);
        assert_eq!(stats.files_modified, 1);
        assert_eq!(stats.identifiers_replaced, 1);

        let written = fs.get(&scheme).unwrap();
        assert!(written.contains("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"));
        assert!(!written.contains("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"));
    }

    #[test]
    fn skips_schemes_whose_blueprints_are_already_current() {
        let fs = InMemoryFileSystem::new();
        let bundle = PathBuf::from("/repo/App.xcodeproj");
        let scheme = bundle.join("xcshareddata/xcschemes/App.xcscheme");
        let original = scheme_with("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        fs.insert(scheme.clone(), original.clone());

        // Map only contains an unrelated old UUID.
        let map = one_to_one(
            "DEADBEEFDEADBEEFDEADBEEFDEADBEEF",
            "00000000000000000000000000000000",
        );

        let stats = update_shared_schemes(&bundle, &map, &fs, &NullLogger).unwrap();
        assert_eq!(stats.files_scanned, 1);
        assert_eq!(stats.files_modified, 0);
        assert_eq!(stats.identifiers_replaced, 0);
        assert_eq!(fs.get(&scheme).unwrap(), original);
    }

    #[test]
    fn does_not_visit_xcuserdata_schemes() {
        let fs = InMemoryFileSystem::new();
        let bundle = PathBuf::from("/repo/App.xcodeproj");
        let user_scheme = bundle.join("xcuserdata/alice.xcuserdatad/xcschemes/App.xcscheme");
        let original = scheme_with("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        fs.insert(user_scheme.clone(), original.clone());

        let map = one_to_one(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        );

        let stats = update_shared_schemes(&bundle, &map, &fs, &NullLogger).unwrap();
        assert_eq!(stats.files_scanned, 0, "xcuserdata must be ignored");
        assert_eq!(fs.get(&user_scheme).unwrap(), original);
    }

    #[test]
    fn missing_schemes_directory_is_not_an_error() {
        let fs = InMemoryFileSystem::new();
        let bundle = PathBuf::from("/repo/Empty.xcodeproj");
        let map = one_to_one(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        );
        let stats = update_shared_schemes(&bundle, &map, &fs, &NullLogger).unwrap();
        assert_eq!(stats, UpdateStats::default());
    }

    #[test]
    fn empty_map_short_circuits_orchestrator() {
        let fs = InMemoryFileSystem::new();
        let bundle = PathBuf::from("/repo/App.xcodeproj");
        let scheme = bundle.join("xcshareddata/xcschemes/App.xcscheme");
        let original = scheme_with("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        fs.insert(scheme.clone(), original.clone());

        let stats =
            update_shared_schemes(&bundle, &HashMap::new(), &fs, &NullLogger).unwrap();
        assert_eq!(stats, UpdateStats::default());
        assert_eq!(fs.get(&scheme).unwrap(), original);
    }

    #[test]
    fn updates_every_scheme_in_the_directory() {
        let fs = InMemoryFileSystem::new();
        let bundle = PathBuf::from("/repo/App.xcodeproj");
        let dir = bundle.join("xcshareddata/xcschemes");
        fs.insert(
            dir.join("WAPA.xcscheme"),
            scheme_with("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        );
        fs.insert(
            dir.join("Tests.xcscheme"),
            scheme_with("CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC"),
        );
        // A non-scheme file in the same dir must not be touched.
        fs.insert(
            dir.join("xcschememanagement.plist"),
            "<plist></plist>".to_string(),
        );

        let mut map = HashMap::new();
        map.insert(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
            "11111111111111111111111111111111".to_string(),
        );
        map.insert(
            "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC".to_string(),
            "22222222222222222222222222222222".to_string(),
        );

        let stats = update_shared_schemes(&bundle, &map, &fs, &NullLogger).unwrap();
        assert_eq!(stats.files_scanned, 2);
        assert_eq!(stats.files_modified, 2);
        assert_eq!(stats.identifiers_replaced, 2);
        assert_eq!(
            fs.get(&dir.join("xcschememanagement.plist")).unwrap(),
            "<plist></plist>"
        );
    }

    #[test]
    fn orchestrator_is_idempotent() {
        let fs = InMemoryFileSystem::new();
        let bundle = PathBuf::from("/repo/App.xcodeproj");
        let scheme = bundle.join("xcshareddata/xcschemes/App.xcscheme");
        fs.insert(
            scheme.clone(),
            scheme_with("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        );
        let map = one_to_one(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        );

        let first = update_shared_schemes(&bundle, &map, &fs, &NullLogger).unwrap();
        let second = update_shared_schemes(&bundle, &map, &fs, &NullLogger).unwrap();

        assert_eq!(first.files_modified, 1);
        assert_eq!(second.files_modified, 0);
        assert_eq!(second.identifiers_replaced, 0);
    }

    // ── repair_orphan_blueprint_identifiers ──────────────────────────────────
    //
    // Orphan repair complements the rename-based propagation: when a scheme
    // contains a `BlueprintIdentifier` UUID that no longer exists in the
    // pbxproj — and was never part of a same-pass uniquify rename — the only
    // way to recover the link is to resolve it by `BlueprintName` against the
    // pbxproj's current target name → uuid index.

    fn name_index(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(n, u)| ((*n).to_string(), (*u).to_string()))
            .collect()
    }

    fn valid(uuids: &[&str]) -> HashSet<String> {
        uuids.iter().map(|u| (*u).to_string()).collect()
    }

    fn buildable_ref(uuid: &str, name: &str, container: &str) -> String {
        format!(
            r#"<BuildableReference
                  BuildableIdentifier = "primary"
                  BlueprintIdentifier = "{uuid}"
                  BuildableName = "{name}.app"
                  BlueprintName = "{name}"
                  ReferencedContainer = "container:{container}">
               </BuildableReference>"#
        )
    }

    #[test]
    fn repairs_orphan_blueprint_id_by_name() {
        let xml = buildable_ref("00000000000000000000000000000000", "WAPA", "App.xcodeproj");
        let names = name_index(&[("WAPA", "BE4D3D56F59CC4FC5CEC03367E182C87")]);
        let valid = valid(&["BE4D3D56F59CC4FC5CEC03367E182C87"]);

        let (out, n) = repair_orphan_blueprint_identifiers(
            &xml,
            &names,
            &valid,
            "App.xcodeproj",
        );
        assert_eq!(n, 1);
        assert!(out.contains(r#"BlueprintIdentifier = "BE4D3D56F59CC4FC5CEC03367E182C87""#));
        assert!(!out.contains("00000000000000000000000000000000"));
    }

    #[test]
    fn leaves_already_valid_blueprint_id_alone() {
        let xml = buildable_ref("BE4D3D56F59CC4FC5CEC03367E182C87", "WAPA", "App.xcodeproj");
        let names = name_index(&[("WAPA", "BE4D3D56F59CC4FC5CEC03367E182C87")]);
        let valid = valid(&["BE4D3D56F59CC4FC5CEC03367E182C87"]);

        let (out, n) = repair_orphan_blueprint_identifiers(
            &xml,
            &names,
            &valid,
            "App.xcodeproj",
        );
        assert_eq!(n, 0, "valid identifiers must not be repaired");
        assert_eq!(out, xml);
    }

    #[test]
    fn does_not_repair_cross_project_references() {
        // ReferencedContainer points to a different project — the orphan UUID
        // is correctly resolved over there, even though the BlueprintName
        // happens to collide with one of our targets.
        let xml = buildable_ref(
            "00000000000000000000000000000000",
            "WAPA",
            "Other.xcodeproj",
        );
        let names = name_index(&[("WAPA", "BE4D3D56F59CC4FC5CEC03367E182C87")]);
        let valid = valid(&["BE4D3D56F59CC4FC5CEC03367E182C87"]);

        let (out, n) = repair_orphan_blueprint_identifiers(
            &xml,
            &names,
            &valid,
            "App.xcodeproj",
        );
        assert_eq!(n, 0, "cross-project refs must not be touched");
        assert_eq!(out, xml);
    }

    #[test]
    fn leaves_orphan_alone_when_name_is_unknown() {
        let xml = buildable_ref(
            "00000000000000000000000000000000",
            "Ghost",
            "App.xcodeproj",
        );
        let names = name_index(&[("WAPA", "BE4D3D56F59CC4FC5CEC03367E182C87")]);
        let valid = valid(&["BE4D3D56F59CC4FC5CEC03367E182C87"]);

        let (out, n) = repair_orphan_blueprint_identifiers(
            &xml,
            &names,
            &valid,
            "App.xcodeproj",
        );
        assert_eq!(n, 0, "unknown names cannot be resolved");
        assert_eq!(out, xml);
    }

    #[test]
    fn repair_is_idempotent() {
        let xml = buildable_ref("00000000000000000000000000000000", "WAPA", "App.xcodeproj");
        let names = name_index(&[("WAPA", "BE4D3D56F59CC4FC5CEC03367E182C87")]);
        let valid = valid(&["BE4D3D56F59CC4FC5CEC03367E182C87"]);

        let (once, _) =
            repair_orphan_blueprint_identifiers(&xml, &names, &valid, "App.xcodeproj");
        let (twice, n) =
            repair_orphan_blueprint_identifiers(&once, &names, &valid, "App.xcodeproj");
        assert_eq!(once, twice, "second pass must be a no-op");
        assert_eq!(n, 0, "second pass must repair nothing");
    }

    #[test]
    fn repairs_multiple_orphans_in_one_xml() {
        let xml = format!(
            "{}\n{}\n{}",
            buildable_ref("00000000000000000000000000000000", "WAPA", "App.xcodeproj"),
            buildable_ref("11111111111111111111111111111111", "Tests", "App.xcodeproj"),
            buildable_ref(
                "BE4D3D56F59CC4FC5CEC03367E182C87",
                "WAPA",
                "App.xcodeproj"
            ), // already valid
        );
        let names = name_index(&[
            ("WAPA", "BE4D3D56F59CC4FC5CEC03367E182C87"),
            ("Tests", "513196272F46623400E4153F"),
        ]);
        let valid = valid(&[
            "BE4D3D56F59CC4FC5CEC03367E182C87",
            "513196272F46623400E4153F",
        ]);

        let (out, n) = repair_orphan_blueprint_identifiers(
            &xml,
            &names,
            &valid,
            "App.xcodeproj",
        );
        assert_eq!(n, 2);
        assert!(!out.contains("00000000000000000000000000000000"));
        assert!(!out.contains("11111111111111111111111111111111"));
        assert_eq!(
            out.matches("BE4D3D56F59CC4FC5CEC03367E182C87").count(),
            2,
            "WAPA target id should appear in both WAPA buildable refs"
        );
        assert_eq!(out.matches("513196272F46623400E4153F").count(), 1);
    }

    // ── repair_shared_schemes (orchestrator) ─────────────────────────────────

    #[test]
    fn orchestrator_repairs_orphans_across_scheme_dir() {
        let fs = InMemoryFileSystem::new();
        let bundle = PathBuf::from("/repo/App.xcodeproj");
        let dir = bundle.join("xcshareddata/xcschemes");
        fs.insert(
            dir.join("WAPA.xcscheme"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Scheme>{}</Scheme>"#,
                buildable_ref("00000000000000000000000000000000", "WAPA", "App.xcodeproj")
            ),
        );

        let names = name_index(&[("WAPA", "BE4D3D56F59CC4FC5CEC03367E182C87")]);
        let valid = valid(&["BE4D3D56F59CC4FC5CEC03367E182C87"]);

        let stats = repair_shared_schemes(
            &bundle,
            &names,
            &valid,
            "App.xcodeproj",
            &fs,
            &NullLogger,
        )
        .unwrap();
        assert_eq!(stats.files_scanned, 1);
        assert_eq!(stats.files_modified, 1);
        assert_eq!(stats.identifiers_replaced, 1);

        let written = fs.get(&dir.join("WAPA.xcscheme")).unwrap();
        assert!(written.contains("BE4D3D56F59CC4FC5CEC03367E182C87"));
    }
}
