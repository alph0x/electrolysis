use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;

use crate::config::Config;
use crate::error::ElectrolysisError;
use crate::logger::Logger;
use crate::mapper;
use crate::parser;
use crate::sanitizer;
use crate::sorter;
use crate::uniquifier;

// ── FileSystem trait ──────────────────────────────────────────────────────────

pub trait FileSystem {
    fn read_to_string(&self, path: &Path) -> Result<String>;
    fn write_atomic(&self, path: &Path, content: &str) -> Result<()>;
    fn copy(&self, from: &Path, to: &Path) -> Result<()>;
}

pub struct RealFileSystem;

impl FileSystem for RealFileSystem {
    fn read_to_string(&self, path: &Path) -> Result<String> {
        std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))
    }

    fn write_atomic(&self, path: &Path, content: &str) -> Result<()> {
        write_atomic(path, content)
    }

    fn copy(&self, from: &Path, to: &Path) -> Result<()> {
        std::fs::copy(from, to)
            .with_context(|| format!("cannot copy {} to {}", from.display(), to.display()))?;
        Ok(())
    }
}

/// Write to a temp file then atomically rename.
pub fn write_atomic(dest: &Path, content: &str) -> Result<()> {
    let tmp = dest.with_extension("pbxproj.tmp");
    std::fs::write(&tmp, content)
        .with_context(|| format!("cannot write temp file {}", tmp.display()))?;
    std::fs::rename(&tmp, dest)
        .with_context(|| format!("cannot rename to {}", dest.display()))?;
    Ok(())
}

// ── Pipeline outcome ──────────────────────────────────────────────────────────

pub enum PipelineOutcome {
    Clean,
    Modified,
    WouldChange,
}

// ── Pipeline ──────────────────────────────────────────────────────────────────

pub struct Pipeline<'a> {
    config: &'a Config,
    fs: &'a dyn FileSystem,
    logger: &'a dyn Logger,
}

impl<'a> Pipeline<'a> {
    pub fn new(config: &'a Config, fs: &'a dyn FileSystem, logger: &'a dyn Logger) -> Self {
        Self { config, fs, logger }
    }

    // ── process (default subcommand) ─────────────────────────────────────────

    pub fn process(&self, pbxproj_path: &Path) -> Result<PipelineOutcome> {
        let proj_root_name = infer_project_name(pbxproj_path);
        let original = self.fs.read_to_string(pbxproj_path)?;
        let mut current = original.clone();

        self.logger.info(&"→ sanitizing…".cyan().to_string());
        let (sanitized, san_stats) = sanitizer::sanitize(&current);
        current = sanitized;
        self.print_sanitize_stats(&san_stats);

        if self.config.sanitize_only {
            return self.finish(&original, &current, pbxproj_path);
        }

        if self.config.run_unique() {
            self.logger.info(&"→ uniquifying…".cyan().to_string());
            self.logger.verbose("parsing project structure");

            let proj = parser::parse_project(&current).map_err(|e| {
                if self.config.verbose {
                    self.print_parse_context(&e, &current);
                }
                anyhow::Error::from(e)
                    .context("failed to parse — run --sanitize-only first if the file is corrupt")
            })?;

            self.logger.verbose(&format!(
                "root: {}  objects: {}",
                proj.root_object,
                proj.objects.len()
            ));

            let unique_map = uniquifier::build_unique_map(&proj, &proj_root_name);
            if self.config.verbose {
                self.logger
                    .verbose(&format!("{} UUID mappings built", unique_map.map.len()));
                for w in &unique_map.warnings {
                    self.logger.warn(w);
                }
            }

            let (uniquified, removed) = unique_map.apply(&current)?;
            current = uniquified;
            self.logger.info(&format!(
                "{} UUIDs remapped ({} orphan line(s) removed)",
                "  ✓".green(),
                removed
            ));

            let (post_sanitized, post_san_stats) = sanitizer::sanitize(&current);
            current = post_sanitized;
            if self.config.verbose {
                self.print_sanitize_stats(&post_san_stats);
            }

            if self.config.run_sort() && !self.config.check {
                self.logger
                    .info(&"→ generating project map…".cyan().to_string());
                let project_map = mapper::build_map(&proj, &unique_map, &proj_root_name);
                let xcodeproj_dir = pbxproj_path.parent().unwrap_or(pbxproj_path);
                let map_path = mapper::default_map_path(xcodeproj_dir);
                let json = serde_json::to_string_pretty(&project_map)
                    .context("failed to serialise project map")?;
                self.fs
                    .write_atomic(&map_path, &json)
                    .with_context(|| format!("cannot write map {}", map_path.display()))?;
                self.logger.info(&format!(
                    "{} map written: {}  ({} targets, {} uuid entries)",
                    "  ✓".green(),
                    map_path.display(),
                    project_map.targets.len(),
                    project_map.uuid_table.len(),
                ));
            }
        }

        if self.config.run_sort() {
            self.logger.info(&"→ sorting…".cyan().to_string());
            let (sorted, sort_stats) = sorter::sort(&current, self.config.sort_main_group);
            current = sorted;
            let any_sorted = sort_stats.files_lists_sorted
                + sort_stats.children_lists_sorted
                + sort_stats.pbx_sections_sorted
                + sort_stats.xc_build_configs_sorted
                + sort_stats.pbx_variant_groups_sorted
                + sort_stats.xc_config_lists_sorted
                + sort_stats.pbx_native_targets_sorted
                + sort_stats.pbx_aggregate_targets_sorted
                + sort_stats.pbx_groups_sorted
                + sort_stats.pbx_target_dependencies_sorted
                + sort_stats.xc_remote_package_refs_sorted
                + sort_stats.xc_package_product_deps_sorted;
            if self.config.verbose || any_sorted > 0 {
                self.logger.info(&format!(
                    "  {} files lists, {} children lists, {} PBX sections, \
                     {} XCBuildConfig, {} PBXVariantGroup, {} XCConfigurationList, \
                     {} PBXNativeTarget, {} PBXAggregateTarget, {} PBXGroup, \
                     {} PBXTargetDependency, {} XCRemoteSwiftPackageRef, \
                     {} XCSwiftPackageProductDep sorted; {} duplicate(s) dropped",
                    sort_stats.files_lists_sorted,
                    sort_stats.children_lists_sorted,
                    sort_stats.pbx_sections_sorted,
                    sort_stats.xc_build_configs_sorted,
                    sort_stats.pbx_variant_groups_sorted,
                    sort_stats.xc_config_lists_sorted,
                    sort_stats.pbx_native_targets_sorted,
                    sort_stats.pbx_aggregate_targets_sorted,
                    sort_stats.pbx_groups_sorted,
                    sort_stats.pbx_target_dependencies_sorted,
                    sort_stats.xc_remote_package_refs_sorted,
                    sort_stats.xc_package_product_deps_sorted,
                    sort_stats.duplicate_entries_removed,
                ));
            }
        }

        self.validate(&current)?;
        self.finish(&original, &current, pbxproj_path)
    }

    fn validate(&self, content: &str) -> Result<()> {
        self.logger.verbose("validating output…");
        validate_pbxproj(content)
    }

    fn finish(
        &self,
        original: &str,
        result: &str,
        pbxproj_path: &Path,
    ) -> Result<PipelineOutcome> {
        let modified = result != original;
        let dest = self.config.output.as_deref().unwrap_or(pbxproj_path);

        if !modified {
            self.logger
                .info(&"✓ no changes — file already clean".green().to_string());
            return Ok(PipelineOutcome::Clean);
        }

        if self.config.check {
            return Ok(PipelineOutcome::WouldChange);
        }

        if self.config.backup {
            let backup_path = self
                .config
                .backup_path
                .as_deref()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| dest.with_extension("pbxproj.bak"));
            self.fs
                .copy(pbxproj_path, &backup_path)
                .with_context(|| format!("cannot create backup {}", backup_path.display()))?;
            self.logger
                .verbose(&format!("backup created: {}", backup_path.display()));
        }

        self.fs
            .write_atomic(dest, result)
            .with_context(|| format!("cannot write {}", dest.display()))?;
        self.logger
            .info(&format!("{} written: {}", "✓".green().bold(), dest.display()));

        Ok(PipelineOutcome::Modified)
    }

    fn print_sanitize_stats(&self, s: &sanitizer::SanitizeStats) {
        if s.conflict_hunks_resolved > 0 {
            self.logger.info(&format!(
                "  {} resolved {} merge conflict hunk(s)",
                "✓".green(),
                s.conflict_hunks_resolved
            ));
        }
        if s.duplicate_objects_removed > 0 {
            self.logger.info(&format!(
                "  {} removed {} duplicate object(s)",
                "✓".green(),
                s.duplicate_objects_removed
            ));
        }
        if s.duplicate_list_items_removed > 0 {
            self.logger.info(&format!(
                "  {} removed {} duplicate list item(s)",
                "✓".green(),
                s.duplicate_list_items_removed
            ));
        }
        if s.orphan_sections_removed > 0 {
            self.logger.info(&format!(
                "  {} removed {} orphan section(s)",
                "✓".green(),
                s.orphan_sections_removed
            ));
        }
        if s.orphan_object_bodies_removed > 0 {
            self.logger.info(&format!(
                "  {} removed {} orphan object body line(s)",
                "✓".green(),
                s.orphan_object_bodies_removed
            ));
        }
    }

    fn print_parse_context(&self, e: &ElectrolysisError, current: &str) {
        if let ElectrolysisError::Parse { line, .. } = e {
            let ctx = 5usize;
            let start = line.saturating_sub(ctx);
            eprintln!("{}", "  context around parse error:".yellow());
            for (i, l) in current.lines().enumerate().skip(start).take(ctx * 2 + 1) {
                let lineno = i + 1;
                if lineno == *line {
                    eprintln!("  {}", format!("{:>6} ▶ {}", lineno, l).red());
                } else {
                    eprintln!("  {:>6}   {}", lineno, l);
                }
            }
        }
    }

    // ── map subcommand ───────────────────────────────────────────────────────

    pub fn map(&self, pbxproj_path: &Path, output: Option<&Path>) -> Result<()> {
        let proj_root_name = infer_project_name(pbxproj_path);

        self.logger
            .info(&"→ reading and sanitizing…".cyan().to_string());
        let raw = self.fs.read_to_string(pbxproj_path)?;
        let (sanitized, _) = sanitizer::sanitize(&raw);

        self.logger.info(&"→ parsing…".cyan().to_string());
        let proj = parser::parse_project(&sanitized)
            .context("failed to parse — try running `electrolysis <path>` first to repair the file")?;

        self.logger.verbose(&format!("objects: {}", proj.objects.len()));

        self.logger
            .info(&"→ building UUID map…".cyan().to_string());
        let unique_map = uniquifier::build_unique_map(&proj, &proj_root_name);
        self.logger
            .verbose(&format!("mapped {} UUIDs", unique_map.map.len()));

        self.logger
            .info(&"→ generating project map…".cyan().to_string());
        let project_map = mapper::build_map(&proj, &unique_map, &proj_root_name);

        let xcodeproj_dir = pbxproj_path.parent().unwrap_or(pbxproj_path);
        let out_path = output
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| mapper::default_map_path(xcodeproj_dir));

        let json = serde_json::to_string_pretty(&project_map)
            .context("failed to serialise project map")?;
        self.fs
            .write_atomic(&out_path, &json)
            .with_context(|| format!("cannot write {}", out_path.display()))?;

        self.logger.info(&format!(
            "{} map written: {}  ({} targets, {} uuid entries)",
            "✓".green().bold(),
            out_path.display(),
            project_map.targets.len(),
            project_map.uuid_table.len(),
        ));
        Ok(())
    }

    // ── diff subcommand ──────────────────────────────────────────────────────

    pub fn diff(
        &self,
        pbxproj_path: &Path,
        reference_path: &Path,
        output: Option<&Path>,
        color: bool,
    ) -> Result<mapper::DiffStatus> {
        let proj_root_name = infer_project_name(pbxproj_path);

        let ref_json = self
            .fs
            .read_to_string(reference_path)
            .with_context(|| format!("cannot read reference map: {}", reference_path.display()))?;
        let reference: mapper::ProjectMap = serde_json::from_str(&ref_json)
            .context("failed to parse reference map JSON")?;

        self.logger.verbose("building current map…");
        let raw = self
            .fs
            .read_to_string(pbxproj_path)
            .with_context(|| format!("cannot read {}", pbxproj_path.display()))?;
        let (sanitized, _) = sanitizer::sanitize(&raw);
        let proj = parser::parse_project(&sanitized).context("failed to parse current project")?;
        let unique_map = uniquifier::build_unique_map(&proj, &proj_root_name);
        let current_map = mapper::build_map(&proj, &unique_map, &proj_root_name);

        let diff = mapper::diff_maps(&reference, &current_map);
        let diff_json = serde_json::to_string_pretty(&diff).context("failed to serialise diff")?;

        match output {
            Some(out_path) => {
                self.fs
                    .write_atomic(out_path, &diff_json)
                    .with_context(|| format!("cannot write {}", out_path.display()))?;
                self.logger.info(&format!(
                    "{} diff written: {}",
                    "✓".green().bold(),
                    out_path.display()
                ));
            }
            None if color => {
                self.print_colored_diff(&diff);
            }
            None => {
                println!("{}", diff_json);
            }
        }

        self.print_diff_summary(&diff, reference_path);

        Ok(diff.status)
    }

    fn print_colored_diff(&self, diff: &mapper::MapDiff) {
        if diff.status == mapper::DiffStatus::Identical {
            println!("{}", "✓ identical — no structural changes".green());
            return;
        }

        let print_section = |label: &str, items: &[String], color: colored::Color, sign: &str| {
            if !items.is_empty() {
                println!("{}:", label.color(color).bold());
                for item in items {
                    println!("  {} {}", sign.color(color), item.color(color));
                }
            }
        };

        print_section("added targets", &diff.added.targets, colored::Color::Green, "+");
        print_section("removed targets", &diff.removed.targets, colored::Color::Red, "-");
        print_section("added groups", &diff.added.groups, colored::Color::Green, "+");
        print_section("removed groups", &diff.removed.groups, colored::Color::Red, "-");
        print_section("added files", &diff.added.files, colored::Color::Green, "+");
        print_section("removed files", &diff.removed.files, colored::Color::Red, "-");
        print_section("added build entries", &diff.added.build_phase_entries, colored::Color::Green, "+");
        print_section("removed build entries", &diff.removed.build_phase_entries, colored::Color::Red, "-");

        if !diff.uuid_changes.is_empty() {
            println!("{}", "UUID changes:".yellow().bold());
            for c in &diff.uuid_changes {
                println!(
                    "  {} → {}  ({})",
                    c.old_uuid.yellow(),
                    c.new_uuid.yellow(),
                    c.path.dimmed()
                );
            }
        }
    }

    fn print_diff_summary(&self, diff: &mapper::MapDiff, reference_path: &Path) {
        self.logger.info(&format!(
            "\n{} vs reference: {}",
            "diff".cyan().bold(),
            reference_path.display()
        ));
        if diff.status == mapper::DiffStatus::Identical {
            self.logger
                .info(&"  identical — no structural changes".green().to_string());
            return;
        }

        let print_list = |label: &str, items: &[String]| {
            if !items.is_empty() {
                self.logger.info(&format!("  {}:", label.bold()));
                for item in items {
                    self.logger.info(&format!("    {}", item));
                }
            }
        };

        print_list("added targets", &diff.added.targets);
        print_list("removed targets", &diff.removed.targets);
        print_list("added groups", &diff.added.groups);
        print_list("removed groups", &diff.removed.groups);
        print_list("added files", &diff.added.files);
        print_list("removed files", &diff.removed.files);
        print_list("added build entries", &diff.added.build_phase_entries);
        print_list("removed build entries", &diff.removed.build_phase_entries);

        if !diff.uuid_changes.is_empty() {
            self.logger.info(&"  UUID changes:".bold().to_string());
            for c in &diff.uuid_changes {
                self.logger.info(&format!(
                    "    {} → {}  ({})",
                    c.old_uuid, c.new_uuid, c.path
                ));
            }
        }
    }
}

pub(crate) fn validate_pbxproj(content: &str) -> Result<()> {
    parser::parse_project(content).context(
        "post-process validation failed: the pipeline produced an invalid pbxproj. \
         This is a bug — please report it",
    )?;
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn infer_project_name(pbxproj: &Path) -> String {
    pbxproj
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("Project.xcodeproj")
        .to_string()
}

// ── In-memory double for unit tests ───────────────────────────────────────────

#[cfg(test)]
pub mod test_double {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;

    pub struct InMemoryFileSystem {
        files: RefCell<HashMap<PathBuf, String>>,
    }

    impl InMemoryFileSystem {
        pub fn new() -> Self {
            Self {
                files: RefCell::new(HashMap::new()),
            }
        }

        pub fn insert(&self, path: PathBuf, content: String) {
            self.files.borrow_mut().insert(path, content);
        }

        pub fn get(&self, path: &Path) -> Option<String> {
            self.files.borrow().get(path).cloned()
        }
    }

    impl FileSystem for InMemoryFileSystem {
        fn read_to_string(&self, path: &Path) -> Result<String> {
            self.files
                .borrow()
                .get(path)
                .cloned()
                .with_context(|| format!("file not found: {}", path.display()))
        }

        fn write_atomic(&self, path: &Path, content: &str) -> Result<()> {
            self.files
                .borrow_mut()
                .insert(path.to_path_buf(), content.to_string());
            Ok(())
        }

        fn copy(&self, from: &Path, to: &Path) -> Result<()> {
            let content = self.read_to_string(from)?;
            self.write_atomic(to, &content)
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::test_double::*;
    use super::*;
    use crate::config::Config;
    use crate::logger::NullLogger;
    use std::path::PathBuf;

    fn minimal_pbxproj() -> String {
        r#"// !$*UTF8*$!
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
            );
            sourceTree = "<group>";
        };
    };
    rootObject = BBBBBBBBBBBBBBBBBBBBBBBB;
}
"#
        .to_string()
    }

    #[test]
    fn pipeline_sanitizes_and_uniquifies_and_sorts() {
        let fs = InMemoryFileSystem::new();
        let path = PathBuf::from("/Test.xcodeproj/project.pbxproj");
        fs.insert(path.clone(), minimal_pbxproj());

        let config = Config::default();
        let logger = NullLogger;
        let pipeline = Pipeline::new(&config, &fs, &logger);

        let outcome = pipeline.process(&path).unwrap();
        assert!(
            matches!(outcome, PipelineOutcome::Modified),
            "expected Modified, got {:?}",
            std::mem::discriminant(&outcome)
        );

        let result = fs.get(&path).unwrap();
        assert!(!result.contains("BBBBBBBBBBBBBBBBBBBBBBBB"));
        assert!(!result.contains("CCCCCCCCCCCCCCCCCCCCCCCC"));
    }

    #[test]
    fn pipeline_sanitize_only_does_not_uniquify() {
        let fs = InMemoryFileSystem::new();
        let path = PathBuf::from("/Test.xcodeproj/project.pbxproj");
        fs.insert(path.clone(), minimal_pbxproj());

        let config = Config {
            sanitize_only: true,
            ..Config::default()
        };
        let logger = NullLogger;
        let pipeline = Pipeline::new(&config, &fs, &logger);

        let outcome = pipeline.process(&path).unwrap();
        assert!(
            matches!(outcome, PipelineOutcome::Clean | PipelineOutcome::Modified),
            "unexpected outcome"
        );

        let result = fs.get(&path).unwrap();
        assert!(result.contains("BBBBBBBBBBBBBBBBBBBBBBBB"));
        assert!(result.contains("CCCCCCCCCCCCCCCCCCCCCCCC"));
    }

    #[test]
    fn pipeline_check_reports_would_change() {
        let fs = InMemoryFileSystem::new();
        let path = PathBuf::from("/Test.xcodeproj/project.pbxproj");
        fs.insert(path.clone(), minimal_pbxproj());

        let config = Config {
            check: true,
            ..Config::default()
        };
        let logger = NullLogger;
        let pipeline = Pipeline::new(&config, &fs, &logger);

        let outcome = pipeline.process(&path).unwrap();
        assert!(matches!(outcome, PipelineOutcome::WouldChange));
    }

    #[test]
    fn validation_accepts_valid_project() {
        assert!(validate_pbxproj(&minimal_pbxproj()).is_ok());
    }

    #[test]
    fn validation_rejects_broken_project() {
        assert!(validate_pbxproj("{ archiveVersion = 1; /* missing close").is_err());
    }

    #[test]
    fn pipeline_creates_backup_when_requested() {
        let fs = InMemoryFileSystem::new();
        let path = PathBuf::from("/Test.xcodeproj/project.pbxproj");
        fs.insert(path.clone(), minimal_pbxproj());

        let config = Config {
            backup: true,
            ..Config::default()
        };
        let logger = NullLogger;
        let pipeline = Pipeline::new(&config, &fs, &logger);

        pipeline.process(&path).unwrap();

        let backup_path = PathBuf::from("/Test.xcodeproj/project.pbxproj.bak");
        assert!(
            fs.get(&backup_path).is_some(),
            "backup should be created"
        );
    }

    #[test]
    fn pipeline_check_on_clean_reports_clean() {
        let fs = InMemoryFileSystem::new();
        let path = PathBuf::from("/Test.xcodeproj/project.pbxproj");
        fs.insert(path.clone(), minimal_pbxproj());

        // Run once to get clean output.
        {
            let config = Config::default();
            let logger = NullLogger;
            let pipeline = Pipeline::new(&config, &fs, &logger);
            pipeline.process(&path).unwrap();
        }

        let clean = fs.get(&path).unwrap();
        fs.insert(path.clone(), clean);

        let config = Config {
            check: true,
            ..Config::default()
        };
        let logger = NullLogger;
        let pipeline = Pipeline::new(&config, &fs, &logger);
        let outcome = pipeline.process(&path).unwrap();
        assert!(matches!(outcome, PipelineOutcome::Clean));
    }
}
