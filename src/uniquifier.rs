//! Phase 3 — deterministic UUID uniquification.
//!
//! Each object in the pbxproj tree gets a new 32-char uppercase MD5-derived UUID
//! computed from its "absolute path" in the tree (matching xUnique's algorithm).
//!
//! After building the old→new mapping the text is substituted in place, and
//! lines referencing unknown UUIDs are dropped.

use std::collections::{HashMap, HashSet};

use once_cell::sync::Lazy;
use regex::Regex;

use crate::error::ElectrolysisError;
use crate::parser::{PbxProject, PbxValue};

// Matches a 24-char Xcode UUID or a 32-char xUnique UUID as a whole word.
// Word boundaries (\b) work because UUID chars are [0-9A-Fa-f] ⊆ \w.
// The 32-char alternative is listed first so it takes priority over a 24-char prefix.
static RE_UUID: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b([0-9A-Fa-f]{32}|[0-9A-Fa-f]{24})\b").unwrap());

// ── Hash helper ───────────────────────────────────────────────────────────────

fn md5_uuid(path: &str) -> String {
    let digest = md5::compute(path.as_bytes());
    format!("{:X}", digest) // 32-char uppercase hex
}

// ── Result map ────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct ResultMap {
    /// uuid → (absolute_path, new_uuid)
    entries: HashMap<String, (String, String)>,
    /// UUIDs that should be stripped from the output.
    to_remove: HashSet<String>,
    /// Non-fatal warnings accumulated during traversal.
    warnings: Vec<String>,
}

impl ResultMap {
    fn set(
        &mut self,
        parent_uuid: &str,
        current_uuid: &str,
        path_component: &str,
        isa: &str,
    ) -> String {
        let parent_path = self
            .entries
            .get(parent_uuid)
            .map(|(p, _)| p.as_str())
            .unwrap_or("");
        let abs_path = format!("{}/{}", parent_path, path_component);
        let typed_path = format!("{}[{}]", isa, abs_path);
        let new_uuid = md5_uuid(&typed_path);
        self.entries
            .insert(current_uuid.to_string(), (typed_path, new_uuid.clone()));
        new_uuid
    }

    fn abs_path(&self, uuid: &str) -> Option<&str> {
        self.entries.get(uuid).map(|(p, _)| p.as_str())
    }
}

// ── Main traversal ────────────────────────────────────────────────────────────

pub struct Uniquifier<'a> {
    proj: &'a PbxProject,
    result: ResultMap,
}

impl<'a> Uniquifier<'a> {
    pub fn new(proj: &'a PbxProject) -> Self {
        Uniquifier { proj, result: ResultMap::default() }
    }

    /// Build the full old→new UUID map.
    pub fn build_map(mut self, proj_root_name: &str) -> UniqueMap {
        let root_uuid = &self.proj.root_object;

        // Seed the root entry (PBXProject node).
        let root_new = md5_uuid(proj_root_name);
        self.result.entries.insert(
            root_uuid.clone(),
            (proj_root_name.to_string(), root_new),
        );

        // Main group.
        if let Some(main_group) = self.proj.str_field(root_uuid, "mainGroup") {
            let main_group = main_group.to_string();
            self.unique_group_or_ref(root_uuid, &main_group);
        }

        // Project-level build configuration list.
        if let Some(bcl) = self.proj.str_field(root_uuid, "buildConfigurationList") {
            let bcl = bcl.to_string();
            self.unique_build_configuration_list(root_uuid, &bcl);
        }

        // Subprojects — projectReferences is an array of dicts:
        // ( { ProductGroup = UUID; ProjectRef = UUID; }, … )
        if let Some(PbxValue::Array(sub_array)) =
            self.proj.raw_field(root_uuid, "projectReferences")
        {
            for item in sub_array.iter() {
                if let PbxValue::Dict(sub_dict) = item {
                    let pg = sub_dict.get("ProductGroup").and_then(|v| v.as_str());
                    let pr = sub_dict.get("ProjectRef").and_then(|v| v.as_str());
                    if let (Some(pg), Some(pr)) = (pg, pr) {
                        let pg = pg.to_string();
                        let pr = pr.to_string();
                        self.unique_group_or_ref(&pr, &pg);
                    }
                }
            }
        }

        // SPM package references at the project level.
        let pkg_refs = self.proj.array_field(root_uuid, "packageReferences").unwrap_or_default();
        for pkg in &pkg_refs {
            let pkg = pkg.clone();
            self.unique_package_ref(root_uuid, &pkg);
        }

        // Pre-register all targets so cross-references resolve.
        let targets = self.proj.array_field(root_uuid, "targets").unwrap_or_default();
        for t in &targets {
            self.pre_register_target(root_uuid, t);
        }
        for t in &targets {
            let t = t.clone();
            self.unique_target(&t);
        }

        // Build final old→new map.
        let mut map: HashMap<String, String> = HashMap::with_capacity(self.result.entries.len());
        for (old, (_, new)) in &self.result.entries {
            map.insert(old.clone(), new.clone());
        }

        let entries = self.result.entries;

        UniqueMap {
            map,
            entries,
            to_remove: self.result.to_remove,
            warnings: self.result.warnings,
        }
    }

    // ── helpers ───────────────────────────────────────────────────────────

    fn pre_register_target(&mut self, parent_uuid: &str, target_uuid: &str) {
        if self.result.entries.contains_key(target_uuid) { return; }
        let product_name = self
            .proj
            .str_field(target_uuid, "productName")
            .or_else(|| self.proj.str_field(target_uuid, "name"))
            .unwrap_or(target_uuid)
            .to_string();
        let isa = self.proj.isa(target_uuid).unwrap_or("PBXTarget").to_string();
        let component = format!(
            "{}/{}",
            product_name,
            self.proj.str_field(target_uuid, "name").unwrap_or(target_uuid)
        );
        self.result.set(parent_uuid, target_uuid, &component, &isa);
    }

    fn unique_group_or_ref(&mut self, parent_uuid: &str, node_uuid: &str) {
        if self.proj.get_object(node_uuid).is_none() {
            self.result.to_remove.insert(node_uuid.to_string());
            return;
        }
        // Path component: prefer name, then path, then "PBXRootGroup".
        let component = self
            .proj
            .str_field(node_uuid, "name")
            .or_else(|| self.proj.str_field(node_uuid, "path"))
            .unwrap_or("PBXRootGroup")
            .to_string();
        let isa = self.proj.isa(node_uuid).unwrap_or("PBXGroup").to_string();
        self.result.set(parent_uuid, node_uuid, &component, &isa);

        // Recurse into children.
        let children = self.proj.array_field(node_uuid, "children").unwrap_or_default();
        for child in children {
            self.unique_group_or_ref(node_uuid, &child);
        }

        // PBXReferenceProxy also has a remoteRef.
        if isa == "PBXReferenceProxy" {
            if let Some(remote_ref) = self.proj.str_field(node_uuid, "remoteRef") {
                let remote_ref = remote_ref.to_string();
                self.unique_container_item_proxy(node_uuid, &remote_ref);
            }
        }
    }

    fn unique_build_configuration_list(&mut self, parent_uuid: &str, bcl_uuid: &str) {
        if self.proj.get_object(bcl_uuid).is_none() { return; }
        let default_config = self
            .proj
            .str_field(bcl_uuid, "defaultConfigurationName")
            .unwrap_or("Release")
            .to_string();
        let isa = "XCConfigurationList";
        self.result.set(parent_uuid, bcl_uuid, &default_config, isa);

        let configs = self.proj.array_field(bcl_uuid, "buildConfigurations").unwrap_or_default();
        for cfg_uuid in configs {
            self.unique_build_configuration(bcl_uuid, &cfg_uuid);
        }
    }

    fn unique_build_configuration(&mut self, parent_uuid: &str, cfg_uuid: &str) {
        if self.proj.get_object(cfg_uuid).is_none() { return; }
        let name = self.proj.str_field(cfg_uuid, "name").unwrap_or("Release").to_string();
        self.result.set(parent_uuid, cfg_uuid, &name, "XCBuildConfiguration");
    }

    fn unique_target(&mut self, target_uuid: &str) {
        if self.proj.get_object(target_uuid).is_none() { return; }

        // Build config list.
        if let Some(bcl) = self.proj.str_field(target_uuid, "buildConfigurationList") {
            let bcl = bcl.to_string();
            self.unique_build_configuration_list(target_uuid, &bcl);
        }

        // Dependencies.
        let deps = self.proj.array_field(target_uuid, "dependencies").unwrap_or_default();
        for dep in deps {
            self.unique_target_dependency(target_uuid, &dep);
        }

        // Build phases.
        let phases = self.proj.array_field(target_uuid, "buildPhases").unwrap_or_default();
        for phase in phases {
            let phase = phase.clone();
            self.unique_build_phase(target_uuid, &phase);
        }

        // Build rules.
        let rules = self.proj.array_field(target_uuid, "buildRules").unwrap_or_default();
        for rule in rules {
            self.unique_build_rule(target_uuid, &rule);
        }

        // SPM product dependencies.
        let pkg_deps = self.proj.array_field(target_uuid, "packageProductDependencies").unwrap_or_default();
        for dep in pkg_deps {
            let dep = dep.clone();
            self.unique_package_product_dep(target_uuid, &dep);
        }
    }

    fn unique_target_dependency(&mut self, parent_uuid: &str, dep_uuid: &str) {
        if self.proj.get_object(dep_uuid).is_none() { return; }

        let path_component = if let Some(target_uuid) = self.proj.str_field(dep_uuid, "target") {
            self.result.abs_path(target_uuid).unwrap_or(target_uuid).to_string()
        } else {
            self.proj.str_field(dep_uuid, "name").unwrap_or(dep_uuid).to_string()
        };
        self.result.set(parent_uuid, dep_uuid, &path_component, "PBXTargetDependency");

        if let Some(proxy_uuid) = self.proj.str_field(dep_uuid, "targetProxy") {
            let proxy_uuid = proxy_uuid.to_string();
            self.unique_container_item_proxy(dep_uuid, &proxy_uuid);
        } else {
            self.result.warnings.push(format!(
                "PBXTargetDependency '{}' has no targetProxy — may be stale",
                dep_uuid
            ));
        }
    }

    fn unique_container_item_proxy(&mut self, parent_uuid: &str, proxy_uuid: &str) {
        if self.proj.get_object(proxy_uuid).is_none() { return; }

        let remote_info = self
            .proj
            .str_field(proxy_uuid, "remoteInfo")
            .unwrap_or("")
            .to_string();
        let component = format!("PBXContainerItemProxy/{}", remote_info);
        let new_proxy_uuid = self.result.set(parent_uuid, proxy_uuid, &component, "PBXContainerItemProxy");
        let proxy_path = self.result.abs_path(proxy_uuid).unwrap_or("").to_string();

        if let Some(rg_uuid) = self.proj.str_field(proxy_uuid, "remoteGlobalIDString") {
            let rg_uuid = rg_uuid.to_string();
            if !self.result.entries.contains_key(&rg_uuid) {
                if let Some(portal_uuid) = self.proj.str_field(proxy_uuid, "containerPortal") {
                    let portal_path = self
                        .result
                        .abs_path(portal_uuid)
                        .unwrap_or(portal_uuid)
                        .to_string();
                    let rg_path = format!("{}+{}", proxy_path, portal_path);
                    let rg_new = md5_uuid(&rg_path);
                    self.result.entries.insert(
                        rg_uuid,
                        (format!("PBXContainerItemProxy#remoteGlobalIDString[{}]", rg_path), rg_new),
                    );
                } else {
                    self.result.warnings.push(format!(
                        "PBXContainerItemProxy '{}' (new: '{}') has no containerPortal — may be stale",
                        proxy_uuid, new_proxy_uuid
                    ));
                }
            }
        }
    }

    fn unique_build_phase(&mut self, parent_uuid: &str, phase_uuid: &str) {
        if self.proj.get_object(phase_uuid).is_none() { return; }

        let bp_isa = self.proj.isa(phase_uuid).unwrap_or("PBXBuildPhase").to_string();
        let component: String = match bp_isa.as_str() {
            "PBXShellScriptBuildPhase" => {
                self.proj
                    .str_field(phase_uuid, "shellScript")
                    .unwrap_or(&bp_isa)
                    .to_string()
            }
            "PBXCopyFilesBuildPhase" => {
                let name = self.proj.str_field(phase_uuid, "name");
                let spec = self
                    .proj
                    .str_field(phase_uuid, "dstSubfolderSpec")
                    .unwrap_or("");
                let path = self.proj.str_field(phase_uuid, "dstPath").unwrap_or("");
                match name {
                    Some(n) => format!("{}/{}/{}", n, spec, path),
                    None    => format!("{}/{}", spec, path),
                }
            }
            _ => bp_isa.clone(),
        };
        self.result.set(parent_uuid, phase_uuid, &component, &bp_isa);

        let files = self.proj.array_field(phase_uuid, "files").unwrap_or_default();
        for file_uuid in files {
            self.unique_build_file(phase_uuid, &file_uuid);
        }
    }

    fn unique_build_file(&mut self, parent_uuid: &str, bf_uuid: &str) {
        let obj = match self.proj.get_object(bf_uuid) {
            Some(o) => o,
            None => {
                self.result.to_remove.insert(bf_uuid.to_string());
                return;
            }
        };

        // Regular files use `fileRef`; SPM-generated build files use `productRef`.
        let ref_uuid = obj
            .get("fileRef")
            .or_else(|| obj.get("productRef"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let ref_uuid = match ref_uuid {
            Some(r) => r,
            None => {
                self.result.to_remove.insert(bf_uuid.to_string());
                return;
            }
        };

        let path_component = if let Some(abs) = self.result.abs_path(&ref_uuid) {
            abs.to_string()
        } else {
            self.result.to_remove.insert(bf_uuid.to_string());
            self.result.to_remove.insert(ref_uuid);
            return;
        };
        self.result.set(parent_uuid, bf_uuid, &path_component, "PBXBuildFile");
    }

    fn unique_package_ref(&mut self, parent_uuid: &str, pkg_uuid: &str) {
        if self.result.entries.contains_key(pkg_uuid) { return; }
        if self.proj.get_object(pkg_uuid).is_none() { return; }
        // Key on repositoryURL so two packages with the same URL share the same UUID.
        let url = self
            .proj
            .str_field(pkg_uuid, "repositoryURL")
            .unwrap_or(pkg_uuid)
            .to_string();
        self.result.set(parent_uuid, pkg_uuid, &url, "XCRemoteSwiftPackageReference");
    }

    fn unique_package_product_dep(&mut self, parent_uuid: &str, dep_uuid: &str) {
        if self.result.entries.contains_key(dep_uuid) { return; }
        if self.proj.get_object(dep_uuid).is_none() { return; }
        let product_name = self
            .proj
            .str_field(dep_uuid, "productName")
            .unwrap_or(dep_uuid)
            .to_string();
        self.result.set(parent_uuid, dep_uuid, &product_name, "XCSwiftPackageProductDependency");

        // Ensure the referenced package is also registered.
        if let Some(pkg_uuid) = self.proj.str_field(dep_uuid, "package") {
            let pkg_uuid = pkg_uuid.to_string();
            if !self.result.entries.contains_key(&pkg_uuid) {
                let root_uuid = self.proj.root_object.clone();
                self.unique_package_ref(&root_uuid, &pkg_uuid);
            }
        }
    }

    fn unique_build_rule(&mut self, parent_uuid: &str, rule_uuid: &str) {
        if self.proj.get_object(rule_uuid).is_none() {
            self.result.to_remove.insert(rule_uuid.to_string());
            return;
        }
        let file_type = self.proj.str_field(rule_uuid, "fileType").unwrap_or("unknown");
        let component = if file_type == "pattern.proxy" {
            let patterns = self
                .proj
                .str_field(rule_uuid, "filePatterns")
                .unwrap_or("");
            format!("{}/{}", file_type, patterns)
        } else {
            file_type.to_string()
        };
        self.result.set(parent_uuid, rule_uuid, &component, "PBXBuildRule");
    }
}

// ── UniqueMap + text substitution ────────────────────────────────────────────

pub struct UniqueMap {
    /// old_uuid → new_uuid  (for text substitution)
    pub map: HashMap<String, String>,
    /// old_uuid → (typed_path, new_uuid)  (for map generation)
    ///
    /// typed_path format: `ISA[project_root/component/…]`
    pub entries: HashMap<String, (String, String)>,
    pub(crate) to_remove: HashSet<String>,
    pub warnings: Vec<String>,
}

impl UniqueMap {
    /// Apply the UUID remapping to a pbxproj text, removing orphaned lines.
    /// Returns (new_text, lines_removed).
    pub fn apply(&self, input: &str) -> Result<(String, usize), ElectrolysisError> {
        // Normalise all UUIDs to uppercase for lookup.
        let normalised_map: HashMap<String, &String> = self
            .map
            .iter()
            .map(|(k, v)| (k.to_uppercase(), v))
            .collect();
        let normalised_remove: HashSet<String> =
            self.to_remove.iter().map(|s| s.to_uppercase()).collect();

        let mut out = String::with_capacity(input.len());
        let mut removed = 0usize;

        // When Some(target_depth), we are skipping the body of a dropped object
        // declaration until the brace depth returns to target_depth.
        // This prevents orphaned body lines (isa = …, fileRef = …, etc.) from
        // leaking into the output after their UUID header was dropped.
        let mut skip_body_to_depth: Option<i32> = None;
        let mut depth: i32 = 0;

        for line in input.lines() {
            let delta = crate::sanitizer::brace_delta(line);
            depth += delta;

            // Currently skipping a dropped object's body.
            if let Some(target) = skip_body_to_depth {
                removed += 1;
                if depth <= target {
                    skip_body_to_depth = None;
                }
                continue;
            }

            let uuids = collect_uuids_from_line(line);
            if uuids.is_empty() {
                out.push_str(line);
                out.push('\n');
                continue;
            }

            let should_drop =
                // Drop lines that reference a UUID explicitly marked for removal.
                uuids.iter().any(|u| normalised_remove.contains(u.as_str()))
                // Drop lines with UUIDs that were never registered (orphans).
                || !uuids.iter().all(|u| normalised_map.contains_key(u.as_str()));

            if should_drop {
                removed += 1;
                // If this line opened a brace (object declaration), skip the
                // body so we don't leave behind dangling `isa = …; };` lines.
                let pre_delta_depth = depth - delta;
                if depth > pre_delta_depth {
                    skip_body_to_depth = Some(pre_delta_depth);
                }
                continue;
            }

            // Replace every UUID in the line.
            let new_line = RE_UUID.replace_all(line, |caps: &regex::Captures| {
                let uuid_upper = caps[1].to_uppercase();
                normalised_map
                    .get(&uuid_upper)
                    .map(|s| s.as_str())
                    .unwrap_or(&caps[1])
                    .to_string()
            });
            out.push_str(&new_line);
            out.push('\n');
        }

        Ok((out, removed))
    }
}

/// Collect all UUID strings from a line (normalised to uppercase).
fn collect_uuids_from_line(line: &str) -> Vec<String> {
    RE_UUID
        .captures_iter(line)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_uppercase()))
        .collect()
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Build the uniquification map for `proj`.
/// `proj_root_name` is e.g. `"MyApp.xcodeproj"`.
pub fn build_unique_map(proj: &PbxProject, proj_root_name: &str) -> UniqueMap {
    Uniquifier::new(proj).build_map(proj_root_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_uuid_is_uppercase_32_chars() {
        let u = md5_uuid("test/path");
        assert_eq!(u.len(), 32);
        assert!(u.chars().all(|c| c.is_ascii_hexdigit() && !c.is_lowercase()));
    }

    #[test]
    fn apply_replaces_and_drops_orphans() {
        let mut map = HashMap::new();
        // 24-char old UUID  →  32-char new UUID
        map.insert(
            "AABBCCDDEEFF001122334455".to_string(),
            "NEWHEXNEWHEXNEWHEXNEWHEX12345678".to_string(),
        );
        let um = UniqueMap {
            map,
            entries: HashMap::new(),
            to_remove: HashSet::new(),
            warnings: vec![],
        };

        let input = concat!(
            "\t\tAABBCCDDEEFF001122334455 /* file */ = {};\n",
            // 24-char orphan UUID that is not in the map
            "\t\tDEADBEEF00000000DEADBEEF /* orphan */ = {};\n",
        );
        let (out, removed) = um.apply(input).unwrap();
        assert!(out.contains("NEWHEXNEWHEXNEWHEXNEWHEX12345678"), "new UUID should appear");
        assert!(!out.contains("AABBCCDDEEFF001122334455"), "old UUID should be gone");
        assert_eq!(removed, 1, "orphan line should be dropped");
    }
}
