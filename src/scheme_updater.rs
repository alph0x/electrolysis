//! Phase 6 — propagate UUID remappings to `.xcscheme` files.
//!
//! The uniquifier rewrites every UUID inside `project.pbxproj` but Xcode
//! schemes live outside that file (in `xcshareddata/xcschemes/*.xcscheme`).
//! Each `BuildableReference` inside a scheme references its target via a
//! `BlueprintIdentifier="<uuid>"` attribute that is **not** inside the
//! pbxproj — so without this pass the schemes orphan as soon as the
//! uniquifier runs.

use std::collections::HashMap;
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
}
