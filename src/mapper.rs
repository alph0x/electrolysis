//! Project map generation and diff.
//!
//! A `ProjectMap` is a deterministic, human + machine-readable JSON snapshot of a
//! pbxproj's semantic structure.  It captures:
//!
//! - The file/group tree (project navigator view)
//! - Targets with their build phases and compile/link/resource files
//! - A complete UUID table: new_uuid → {isa, semantic path}
//!
//! Because all UUIDs in the map are the deterministic MD5 values produced by the
//! uniquifier, the map is stable across runs as long as the project structure
//! doesn't change.  This makes it suitable as a version-controlled snapshot.
//!
//! ## Diff
//!
//! `diff_maps(reference, current)` compares two maps and returns a `MapDiff` that
//! lists every structural change: added/removed files, groups, targets, and build
//! phase entries.  The diff is also serialisable to JSON.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::parser::PbxProject;
use crate::uniquifier::UniqueMap;

// ── Map format version ────────────────────────────────────────────────────────

const MAP_VERSION: &str = "1";

// ── Top-level map ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMap {
    /// Format version — bump when the schema changes.
    pub version: String,
    /// ISO-8601 UTC timestamp of generation.
    pub generated: String,
    pub project: ProjectMeta,
    pub targets: Vec<TargetMap>,
    pub file_tree: FileNode,
    /// new_uuid → record.  Sorted by UUID for stable diffs.
    pub uuid_table: BTreeMap<String, UuidRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub name: String,
    pub configurations: Vec<String>,
}

/// One row in the uuid_table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UuidRecord {
    pub isa: String,
    /// Full semantic path used to derive the UUID, e.g.
    /// `"PBXFileReference[MyApp.xcodeproj/Sources/AppDelegate.swift]"`
    pub path: String,
}

// ── File tree ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileNode {
    pub uuid: String,
    pub isa: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_tree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<FileNode>,
}

// ── Targets ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetMap {
    pub uuid: String,
    pub name: String,
    pub product_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_type: Option<String>,
    pub configurations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    pub build_phases: Vec<BuildPhaseMap>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildPhaseMap {
    pub uuid: String,
    #[serde(rename = "type")]
    pub phase_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub files: Vec<BuildFileMap>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildFileMap {
    pub uuid: String,
    /// Resolved path of the referenced file (from file_ref's path/name field).
    pub path: String,
    /// Compiler flags, attributes, etc. — kept as raw string for readability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<String>,
}

// ── Diff types ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct MapDiff {
    pub status: DiffStatus,
    pub added: DiffSection,
    pub removed: DiffSection,
    /// UUID changes for the same semantic path (rename/move).
    pub uuid_changes: Vec<UuidChange>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffStatus {
    Identical,
    HasChanges,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DiffSection {
    pub targets: Vec<String>,
    pub groups: Vec<String>,
    pub files: Vec<String>,
    /// Entries removed/added from build phases: `"TargetName/PhaseType/file.swift"`
    pub build_phase_entries: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UuidChange {
    pub path: String,
    pub old_uuid: String,
    pub new_uuid: String,
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Build a `ProjectMap` from a parsed project and its uniquification result.
pub fn build_map(proj: &PbxProject, unique_map: &UniqueMap, project_name: &str) -> ProjectMap {
    let root_uuid = &proj.root_object;

    // ── UUID table ────────────────────────────────────────────────────────────

    let mut uuid_table: BTreeMap<String, UuidRecord> = BTreeMap::new();
    for (_old, (typed_path, new_uuid)) in &unique_map.entries {
        let (isa, _path) = split_typed_path(typed_path);
        uuid_table.insert(
            new_uuid.clone(),
            UuidRecord { isa: isa.to_string(), path: typed_path.clone() },
        );
    }

    // ── Project configurations ────────────────────────────────────────────────

    let configurations = project_configurations(proj, root_uuid);

    // ── File tree ─────────────────────────────────────────────────────────────

    let main_group = proj.str_field(root_uuid, "mainGroup").unwrap_or("").to_string();
    let file_tree = build_file_node(proj, &main_group, unique_map)
        .unwrap_or_else(|| FileNode {
            uuid: resolve_new_uuid(unique_map, &main_group),
            isa: "PBXGroup".to_string(),
            name: project_name.to_string(),
            path: None,
            source_tree: None,
            file_type: None,
            children: vec![],
        });

    // ── Targets ───────────────────────────────────────────────────────────────

    let target_uuids = proj.array_field(root_uuid, "targets").unwrap_or_default();
    let targets: Vec<TargetMap> = target_uuids
        .iter()
        .filter_map(|t| build_target_map(proj, t, unique_map))
        .collect();

    ProjectMap {
        version: MAP_VERSION.to_string(),
        generated: utc_timestamp(),
        project: ProjectMeta { name: project_name.to_string(), configurations },
        targets,
        file_tree,
        uuid_table,
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Resolve a new (deterministic) UUID for the given old UUID.
fn resolve_new_uuid(unique_map: &UniqueMap, old_uuid: &str) -> String {
    unique_map
        .map
        .get(old_uuid)
        .cloned()
        .unwrap_or_else(|| old_uuid.to_string())
}

/// Split `"ISA[path]"` → `("ISA", "path")`.
fn split_typed_path(typed: &str) -> (&str, &str) {
    if let Some(bracket) = typed.find('[') {
        let isa = &typed[..bracket];
        let path = typed[bracket + 1..].trim_end_matches(']');
        (isa, path)
    } else {
        (typed, "")
    }
}

fn project_configurations(proj: &PbxProject, root_uuid: &str) -> Vec<String> {
    let bcl = match proj.str_field(root_uuid, "buildConfigurationList") {
        Some(u) => u.to_string(),
        None => return vec![],
    };
    let config_uuids = proj.array_field(&bcl, "buildConfigurations").unwrap_or_default();
    config_uuids
        .iter()
        .filter_map(|u| proj.str_field(u, "name").map(|s| s.to_string()))
        .collect()
}

fn build_file_node(proj: &PbxProject, uuid: &str, um: &UniqueMap) -> Option<FileNode> {
    let obj = proj.get_object(uuid)?;
    let isa = obj.get("isa").and_then(|v| v.as_str()).unwrap_or("PBXGroup");

    let name = obj
        .get("name")
        .and_then(|v| v.as_str())
        .or_else(|| obj.get("path").and_then(|v| v.as_str()))
        .unwrap_or("(unnamed)")
        .to_string();

    let path = obj.get("path").and_then(|v| v.as_str()).map(|s| s.to_string());
    let source_tree = obj.get("sourceTree").and_then(|v| v.as_str()).map(|s| s.to_string());
    let file_type = obj
        .get("lastKnownFileType")
        .or_else(|| obj.get("explicitFileType"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let child_uuids = proj.array_field(uuid, "children").unwrap_or_default();
    let children: Vec<FileNode> = child_uuids
        .iter()
        .filter_map(|c| build_file_node(proj, c, um))
        .collect();

    Some(FileNode {
        uuid: resolve_new_uuid(um, uuid),
        isa: isa.to_string(),
        name,
        path,
        source_tree,
        file_type,
        children,
    })
}

fn build_target_map(proj: &PbxProject, target_uuid: &str, um: &UniqueMap) -> Option<TargetMap> {
    let obj = proj.get_object(target_uuid)?;

    let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or(target_uuid).to_string();
    let product_name = obj
        .get("productName")
        .and_then(|v| v.as_str())
        .unwrap_or(&name)
        .to_string();
    let product_type = obj.get("productType").and_then(|v| v.as_str()).map(|s| s.to_string());

    let configurations = {
        let bcl = obj
            .get("buildConfigurationList")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cfg_uuids = proj.array_field(&bcl, "buildConfigurations").unwrap_or_default();
        cfg_uuids
            .iter()
            .filter_map(|u| proj.str_field(u, "name").map(|s| s.to_string()))
            .collect()
    };

    let dependencies = {
        let dep_uuids = proj.array_field(target_uuid, "dependencies").unwrap_or_default();
        dep_uuids
            .iter()
            .filter_map(|dep_uuid| {
                // Dependency → target → name
                proj.str_field(dep_uuid, "target")
                    .and_then(|t| proj.str_field(t, "name"))
                    .map(|s| s.to_string())
            })
            .collect()
    };

    let phase_uuids = proj.array_field(target_uuid, "buildPhases").unwrap_or_default();
    let build_phases: Vec<BuildPhaseMap> = phase_uuids
        .iter()
        .filter_map(|p| build_phase_map(proj, p, um))
        .collect();

    Some(TargetMap {
        uuid: resolve_new_uuid(um, target_uuid),
        name,
        product_name,
        product_type,
        configurations,
        dependencies,
        build_phases,
    })
}

fn build_phase_map(proj: &PbxProject, phase_uuid: &str, um: &UniqueMap) -> Option<BuildPhaseMap> {
    let obj = proj.get_object(phase_uuid)?;
    let phase_type = obj.get("isa").and_then(|v| v.as_str()).unwrap_or("PBXBuildPhase").to_string();
    let name = obj.get("name").and_then(|v| v.as_str()).map(|s| s.to_string());

    let file_uuids = proj.array_field(phase_uuid, "files").unwrap_or_default();
    let files: Vec<BuildFileMap> = file_uuids
        .iter()
        .filter_map(|f| build_file_map(proj, f, um))
        .collect();

    Some(BuildPhaseMap {
        uuid: resolve_new_uuid(um, phase_uuid),
        phase_type,
        name,
        files,
    })
}

fn build_file_map(proj: &PbxProject, bf_uuid: &str, um: &UniqueMap) -> Option<BuildFileMap> {
    let obj = proj.get_object(bf_uuid)?;
    let file_ref = obj.get("fileRef").and_then(|v| v.as_str())?;

    // Resolve a human-readable path from the file reference.
    let ref_obj = proj.get_object(file_ref)?;
    let path = ref_obj
        .get("path")
        .and_then(|v| v.as_str())
        .or_else(|| ref_obj.get("name").and_then(|v| v.as_str()))
        .unwrap_or(file_ref)
        .to_string();

    // Capture settings (compiler flags, attributes) as a compact string.
    let settings = obj.get("settings").and_then(|v| {
        // Only include settings if non-empty.
        if let crate::parser::PbxValue::Dict(d) = v {
            if d.is_empty() { return None; }
        }
        Some(format!("{v:?}"))
    });

    Some(BuildFileMap {
        uuid: resolve_new_uuid(um, bf_uuid),
        path,
        settings,
    })
}

// ── Diff ──────────────────────────────────────────────────────────────────────

pub fn diff_maps(reference: &ProjectMap, current: &ProjectMap) -> MapDiff {
    // ── file tree ─────────────────────────────────────────────────────────────

    let ref_files   = collect_tree_entries(&reference.file_tree, "");
    let cur_files   = collect_tree_entries(&current.file_tree, "");

    let ref_file_set: BTreeSet<_> = ref_files.iter().map(|(p, i)| format!("{i}:{p}")).collect();
    let cur_file_set: BTreeSet<_> = cur_files.iter().map(|(p, i)| format!("{i}:{p}")).collect();

    let mut added_groups  = vec![];
    let mut added_files   = vec![];
    let mut removed_groups = vec![];
    let mut removed_files  = vec![];

    for entry in cur_file_set.difference(&ref_file_set) {
        if entry.starts_with("PBXGroup:") || entry.starts_with("PBXVariantGroup:") {
            added_groups.push(entry.clone());
        } else {
            added_files.push(entry.clone());
        }
    }
    for entry in ref_file_set.difference(&cur_file_set) {
        if entry.starts_with("PBXGroup:") || entry.starts_with("PBXVariantGroup:") {
            removed_groups.push(entry.clone());
        } else {
            removed_files.push(entry.clone());
        }
    }

    // ── targets ───────────────────────────────────────────────────────────────

    let ref_target_names: BTreeSet<_> = reference.targets.iter().map(|t| t.name.clone()).collect();
    let cur_target_names: BTreeSet<_> = current.targets.iter().map(|t| t.name.clone()).collect();

    let added_targets: Vec<String> = cur_target_names.difference(&ref_target_names).cloned().collect();
    let removed_targets: Vec<String> = ref_target_names.difference(&cur_target_names).cloned().collect();

    // ── build phase entries ───────────────────────────────────────────────────

    let ref_bpf = collect_build_phase_files(reference);
    let cur_bpf = collect_build_phase_files(current);

    let added_bpf: Vec<String>   = cur_bpf.difference(&ref_bpf).cloned().collect();
    let removed_bpf: Vec<String> = ref_bpf.difference(&cur_bpf).cloned().collect();

    // ── UUID changes (same semantic path, different UUID) ─────────────────────

    let ref_path_to_uuid: BTreeMap<&str, &str> = reference
        .uuid_table
        .iter()
        .map(|(uuid, rec)| (rec.path.as_str(), uuid.as_str()))
        .collect();
    let cur_path_to_uuid: BTreeMap<&str, &str> = current
        .uuid_table
        .iter()
        .map(|(uuid, rec)| (rec.path.as_str(), uuid.as_str()))
        .collect();

    let mut uuid_changes: Vec<UuidChange> = vec![];
    for (path, ref_uuid) in &ref_path_to_uuid {
        if let Some(cur_uuid) = cur_path_to_uuid.get(path) {
            if cur_uuid != ref_uuid {
                uuid_changes.push(UuidChange {
                    path: path.to_string(),
                    old_uuid: ref_uuid.to_string(),
                    new_uuid: cur_uuid.to_string(),
                });
            }
        }
    }

    let has_changes = !added_targets.is_empty()
        || !removed_targets.is_empty()
        || !added_groups.is_empty()
        || !removed_groups.is_empty()
        || !added_files.is_empty()
        || !removed_files.is_empty()
        || !added_bpf.is_empty()
        || !removed_bpf.is_empty()
        || !uuid_changes.is_empty();

    MapDiff {
        status: if has_changes { DiffStatus::HasChanges } else { DiffStatus::Identical },
        added: DiffSection {
            targets: added_targets,
            groups: added_groups,
            files: added_files,
            build_phase_entries: added_bpf,
        },
        removed: DiffSection {
            targets: removed_targets,
            groups: removed_groups,
            files: removed_files,
            build_phase_entries: removed_bpf,
        },
        uuid_changes,
    }
}

/// Collect all `(tree_path, isa)` tuples from the file tree, depth-first.
fn collect_tree_entries(node: &FileNode, parent_path: &str) -> Vec<(String, String)> {
    let this_path = if parent_path.is_empty() {
        node.name.clone()
    } else {
        format!("{}/{}", parent_path, node.name)
    };

    let mut result = vec![(this_path.clone(), node.isa.clone())];
    for child in &node.children {
        result.extend(collect_tree_entries(child, &this_path));
    }
    result
}

/// Collect `"target/phase_type/file_path"` fingerprints for every build file.
fn collect_build_phase_files(map: &ProjectMap) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for target in &map.targets {
        for phase in &target.build_phases {
            for file in &phase.files {
                set.insert(format!("{}/{}/{}", target.name, phase.phase_type, file.path));
            }
        }
    }
    set
}

// ── Timestamp ─────────────────────────────────────────────────────────────────

fn utc_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut rem = secs;
    let s = rem % 60; rem /= 60;
    let min = rem % 60; rem /= 60;
    let h = rem % 24; rem /= 24;

    let mut year = 1970u32;
    loop {
        let dy = if is_leap(year) { 366u64 } else { 365 };
        if rem < dy { break; }
        rem -= dy;
        year += 1;
    }
    let months: [u64; 12] = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 0usize;
    for &dm in &months {
        if rem < dm { break; }
        rem -= dm;
        month += 1;
    }

    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", year, month + 1, rem + 1, h, min, s)
}

fn is_leap(y: u32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

// ── Default output path ───────────────────────────────────────────────────────

/// Given a `.xcodeproj` path, produce the default map output path.
pub fn default_map_path(xcodeproj: &std::path::Path) -> std::path::PathBuf {
    let stem = xcodeproj
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    xcodeproj
        .parent()
        .unwrap_or(xcodeproj)
        .join(format!("{}.electrolysis-map.json", stem))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_timestamp_format() {
        let ts = utc_timestamp();
        // Basic shape: "YYYY-MM-DDTHH:MM:SSZ"
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
    }

    #[test]
    fn split_typed_path_works() {
        let (isa, path) = split_typed_path("PBXFileReference[MyApp.xcodeproj/Sources/Foo.swift]");
        assert_eq!(isa, "PBXFileReference");
        assert_eq!(path, "MyApp.xcodeproj/Sources/Foo.swift");
    }

    #[test]
    fn diff_identical_maps_reports_no_changes() {
        // Minimal maps with one file each.
        let node = FileNode {
            uuid: "AA".into(), isa: "PBXGroup".into(), name: "Root".into(),
            path: None, source_tree: None, file_type: None, children: vec![],
        };
        let map = ProjectMap {
            version: "1".into(),
            generated: "2026-01-01T00:00:00Z".into(),
            project: ProjectMeta { name: "Test".into(), configurations: vec![] },
            targets: vec![],
            file_tree: node.clone(),
            uuid_table: BTreeMap::new(),
        };
        let diff = diff_maps(&map, &map);
        assert_eq!(diff.status, DiffStatus::Identical);
    }
}
