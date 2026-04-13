use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{bail, Context, Result};
use clap::{Parser as ClapParser, Subcommand};
use colored::Colorize;

mod error;
mod mapper;
mod parser;
mod sanitizer;
mod sorter;
mod uniquifier;

// ── CLI ───────────────────────────────────────────────────────────────────────

/// Electrolysis — pbxproj sanitizer, uniquifier, sorter, and mapper.
///
/// Repairs corruption caused by merge conflicts or AI code-agent manipulation,
/// assigns deterministic MD5-based UUIDs, and sorts the file structurally —
/// producing a stable, diff-friendly output that Xcode can read.
///
/// When PATH is omitted, electrolysis searches the current directory for
/// .xcodeproj bundles and prompts you to choose if more than one is found.
#[derive(ClapParser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    // ── Process arguments (no subcommand) ─────────────────────────────────────

    /// Path to the `.xcodeproj` directory or `project.pbxproj` file.
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,

    /// Enable verbose diagnostic output.
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Only sanitize (fix corruption). Skip uniquify and sort.
    #[arg(long, conflicts_with_all = ["unique", "sort"])]
    sanitize_only: bool,

    /// Uniquify UUIDs only (skips sort).
    #[arg(short, long, conflicts_with = "sort")]
    unique: bool,

    /// Sort only (skips uniquify).
    #[arg(short, long, conflicts_with = "unique")]
    sort: bool,

    /// Exit with non-zero status if the file was modified.
    /// Useful as a git pre-commit hook.
    #[arg(short = 'c', long)]
    combine_commit: bool,

    /// Write processed output to this path instead of modifying in place.
    #[arg(short = 'o', long, value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate a JSON map of the project structure.
    ///
    /// The map captures the file tree, targets, build phases, and a complete
    /// UUID table.  It is human-readable and can be diffed against a future
    /// state to detect structural changes.
    ///
    /// The project is sanitized and uniquified before the map is built so all
    /// UUIDs in the map are the deterministic MD5 values.
    Map {
        /// Path to the `.xcodeproj` directory or `project.pbxproj` file.
        path: PathBuf,

        /// Output path for the map file.
        /// Defaults to `<ProjectName>.electrolysis-map.json` next to the xcodeproj.
        #[arg(short = 'o', long, value_name = "FILE")]
        output: Option<PathBuf>,
    },

    /// Compare the current project structure against a reference map.
    ///
    /// Reads the project (sanitize + uniquify), builds a fresh map, and diffs
    /// it against the reference JSON file produced by a previous `map` run.
    /// Reports added / removed files, groups, targets, and build-phase entries.
    Diff {
        /// Path to the `.xcodeproj` directory or `project.pbxproj` file.
        path: PathBuf,

        /// Reference map file to compare against (from a previous `map` run).
        reference: PathBuf,

        /// Write the diff result as JSON to this file (prints to stdout by default).
        #[arg(short = 'o', long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Print `msg` to stderr dimmed when `verbose` is true.
fn vlog(verbose: bool, msg: &str) {
    if verbose {
        eprintln!("{}", msg.dimmed());
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();
    let verbose = cli.verbose;
    let result = match cli.command {
        Some(Commands::Map { path, output }) => run_map(path, output, verbose),
        Some(Commands::Diff { path, reference, output }) => {
            run_diff(path, reference, output, verbose)
        }
        None => run_process(cli),
    };

    match result {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{} {}", "error:".red().bold(), e);
            // Print the full error chain so parse errors with line numbers are visible.
            let mut source = e.source();
            while let Some(cause) = source {
                eprintln!("  {} {}", "caused by:".dimmed(), cause);
                source = cause.source();
            }
            process::exit(1);
        }
    }
}

// ── `process` (default) ───────────────────────────────────────────────────────

fn run_process(cli: Cli) -> Result<()> {
    // Resolve the target path(s): explicit arg or auto-discover in cwd.
    let paths: Vec<PathBuf> = match &cli.path {
        Some(p) => vec![resolve_pbxproj_path(p)?],
        None => {
            let found = discover_xcodeprojs(&std::env::current_dir()?);
            match found.len() {
                0 => bail!("no .xcodeproj found in the current directory"),
                1 => {
                    eprintln!("{} {}", "→ found:".cyan(), found[0].display());
                    found.into_iter().map(|p| p.join("project.pbxproj")).collect()
                }
                _ => pick_xcodeprojs(found)?,
            }
        }
    };

    for path in &paths {
        run_process_single(path, &cli)?;
    }

    eprintln!(
        "\n{}",
        "Successfully removed the rust on this project with electrolysis 🔬".bold()
    );
    eprintln!("  https://github.com/alph0x/electrolysis — thanks for using it!");

    Ok(())
}

fn run_process_single(pbxproj_path: &Path, cli: &Cli) -> Result<()> {
    let pbxproj_path = pbxproj_path.to_path_buf();
    // (path was already resolved; for the explicit-arg branch we need to resolve here)
    let pbxproj_path = if pbxproj_path.is_absolute() {
        pbxproj_path
    } else {
        resolve_pbxproj_path(&pbxproj_path)?
    };
    let proj_root_name = infer_project_name(&pbxproj_path);
    let verbose = cli.verbose;

    let original = fs::read_to_string(&pbxproj_path)
        .with_context(|| format!("cannot read {}", pbxproj_path.display()))?;
    let mut current = original.clone();

    // ── Sanitize ──────────────────────────────────────────────────────────────

    eprintln!("{}", "→ sanitizing…".cyan());
    let (sanitized, san_stats) = sanitizer::sanitize(&current);
    current = sanitized;
    print_sanitize_stats(&san_stats);

    if cli.sanitize_only {
        return process_finish(&pbxproj_path, cli.output.as_deref(), cli.combine_commit, &original, &current);
    }

    let run_unique = !cli.sort;
    let run_sort   = !cli.unique;

    // ── Uniquify ──────────────────────────────────────────────────────────────

    if run_unique {
        eprintln!("{}", "→ uniquifying…".cyan());
        vlog(verbose, "parsing project structure");
        let proj = parser::parse_project(&current).map_err(|e| {
            // In verbose mode print surrounding lines so the bad content is visible.
            if verbose {
                if let crate::error::ElectrolysisError::Parse { line, .. } = &e {
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
            anyhow::Error::from(e)
                .context("failed to parse — run --sanitize-only first if the file is corrupt")
        })?;
        vlog(verbose, &format!("root: {}  objects: {}", proj.root_object, proj.objects.len()));

        let unique_map = uniquifier::build_unique_map(&proj, &proj_root_name);
        if verbose {
            eprintln!("  {} UUID mappings built", unique_map.map.len());
            for w in &unique_map.warnings {
                eprintln!("{} {}", "  warn:".yellow(), w);
            }
        }
        let (uniquified, removed) = unique_map.apply(&current)?;
        current = uniquified;
        eprintln!("{} UUIDs remapped ({} orphan line(s) removed)", "  ✓".green(), removed);

        // ── Map (always alongside full pipeline) ──────────────────────────────

        if run_sort {
            eprintln!("{}", "→ generating project map…".cyan());
            let project_map = mapper::build_map(&proj, &unique_map, &proj_root_name);
            let xcodeproj_dir = pbxproj_path.parent().unwrap_or(&pbxproj_path);
            let map_path = mapper::default_map_path(xcodeproj_dir);
            let json = serde_json::to_string_pretty(&project_map)
                .context("failed to serialise project map")?;
            fs::write(&map_path, &json)
                .with_context(|| format!("cannot write map {}", map_path.display()))?;
            eprintln!(
                "{} map written: {}  ({} targets, {} uuid entries)",
                "  ✓".green(),
                map_path.display(),
                project_map.targets.len(),
                project_map.uuid_table.len(),
            );
        }
    }

    // ── Sort ──────────────────────────────────────────────────────────────────

    if run_sort {
        eprintln!("{}", "→ sorting…".cyan());
        let (sorted, sort_stats) = sorter::sort(&current);
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
        if verbose || any_sorted > 0 {
            eprintln!(
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
            );
        }
    }

    process_finish(&pbxproj_path, cli.output.as_deref(), cli.combine_commit, &original, &current)
}

// ── `map` subcommand ──────────────────────────────────────────────────────────

fn run_map(path: PathBuf, output: Option<PathBuf>, verbose: bool) -> Result<()> {
    let pbxproj_path = resolve_pbxproj_path(&path)?;
    let proj_root_name = infer_project_name(&pbxproj_path);

    eprintln!("{}", "→ reading and sanitizing…".cyan());
    let raw = fs::read_to_string(&pbxproj_path)
        .with_context(|| format!("cannot read {}", pbxproj_path.display()))?;
    let (sanitized, _) = sanitizer::sanitize(&raw);

    eprintln!("{}", "→ parsing…".cyan());
    let proj = parser::parse_project(&sanitized)
        .context("failed to parse — try running `electrolysis <path>` first to repair the file")?;

    vlog(verbose, &format!("objects: {}", proj.objects.len()));

    eprintln!("{}", "→ building UUID map…".cyan());
    let unique_map = uniquifier::build_unique_map(&proj, &proj_root_name);
    vlog(verbose, &format!("mapped {} UUIDs", unique_map.map.len()));

    eprintln!("{}", "→ generating project map…".cyan());
    let project_map = mapper::build_map(&proj, &unique_map, &proj_root_name);

    // Determine output path.
    let xcodeproj_dir = pbxproj_path.parent().unwrap_or(&pbxproj_path);
    let out_path = output.unwrap_or_else(|| mapper::default_map_path(xcodeproj_dir));

    let json = serde_json::to_string_pretty(&project_map)
        .context("failed to serialise project map")?;
    fs::write(&out_path, &json)
        .with_context(|| format!("cannot write {}", out_path.display()))?;

    eprintln!(
        "{} map written: {}  ({} targets, {} uuid entries)",
        "✓".green().bold(),
        out_path.display(),
        project_map.targets.len(),
        project_map.uuid_table.len(),
    );
    Ok(())
}

// ── `diff` subcommand ─────────────────────────────────────────────────────────

fn run_diff(
    path: PathBuf,
    reference_path: PathBuf,
    output: Option<PathBuf>,
    verbose: bool,
) -> Result<()> {
    let pbxproj_path = resolve_pbxproj_path(&path)?;
    let proj_root_name = infer_project_name(&pbxproj_path);

    // Load reference map.
    let ref_json = fs::read_to_string(&reference_path)
        .with_context(|| format!("cannot read reference map: {}", reference_path.display()))?;
    let reference: mapper::ProjectMap =
        serde_json::from_str(&ref_json).context("failed to parse reference map JSON")?;

    // Build current map.
    if verbose {
        eprintln!("{}", "→ building current map…".cyan().dimmed());
    }
    let raw = fs::read_to_string(&pbxproj_path)
        .with_context(|| format!("cannot read {}", pbxproj_path.display()))?;
    let (sanitized, _) = sanitizer::sanitize(&raw);
    let proj = parser::parse_project(&sanitized)
        .context("failed to parse current project")?;
    let unique_map = uniquifier::build_unique_map(&proj, &proj_root_name);
    let current_map = mapper::build_map(&proj, &unique_map, &proj_root_name);

    // Diff.
    let diff = mapper::diff_maps(&reference, &current_map);
    let diff_json = serde_json::to_string_pretty(&diff).context("failed to serialise diff")?;

    match output {
        Some(out_path) => {
            fs::write(&out_path, &diff_json)
                .with_context(|| format!("cannot write {}", out_path.display()))?;
            eprintln!("{} diff written: {}", "✓".green().bold(), out_path.display());
        }
        None => {
            println!("{}", diff_json);
        }
    }

    // Print a human-readable summary to stderr regardless.
    print_diff_summary(&diff, &reference_path);

    if diff.status == mapper::DiffStatus::HasChanges {
        process::exit(1);
    }
    Ok(())
}

// ── discovery helpers ─────────────────────────────────────────────────────────

/// Find all `*.xcodeproj` directories directly inside `dir` (non-recursive).
fn discover_xcodeprojs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return vec![] };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.extension().and_then(|x| x.to_str()) == Some("xcodeproj"))
        .filter(|p| p.join("project.pbxproj").exists())
        .collect();
    found.sort();
    found
}

/// Present a numbered menu of found `.xcodeproj` bundles and return the
/// `project.pbxproj` paths for the user's selection (one or all).
fn pick_xcodeprojs(found: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    eprintln!("{}", "→ multiple .xcodeproj found:".cyan());
    for (i, p) in found.iter().enumerate() {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        eprintln!("  {}. {}", i + 1, name);
    }
    let all_idx = found.len() + 1;
    eprintln!("  {}. {} (all)", all_idx, "all".bold());
    eprint!("Choose [1]: ");
    io::stderr().flush().ok();

    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let trimmed = line.trim();
    let choice: usize = if trimmed.is_empty() {
        1
    } else {
        trimmed.parse().unwrap_or(0)
    };

    if choice == all_idx {
        Ok(found.iter().map(|p| p.join("project.pbxproj")).collect())
    } else if choice >= 1 && choice <= found.len() {
        Ok(vec![found[choice - 1].join("project.pbxproj")])
    } else {
        bail!("invalid selection: {}", trimmed);
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn print_sanitize_stats(s: &sanitizer::SanitizeStats) {
    if s.conflict_hunks_resolved > 0 {
        eprintln!("  {} resolved {} merge conflict hunk(s)", "✓".green(), s.conflict_hunks_resolved);
    }
    if s.duplicate_objects_removed > 0 {
        eprintln!("  {} removed {} duplicate object(s)", "✓".green(), s.duplicate_objects_removed);
    }
    if s.duplicate_list_items_removed > 0 {
        eprintln!("  {} removed {} duplicate list item(s)", "✓".green(), s.duplicate_list_items_removed);
    }
    if s.orphan_sections_removed > 0 {
        eprintln!("  {} removed {} orphan section(s)", "✓".green(), s.orphan_sections_removed);
    }
    if s.orphan_object_bodies_removed > 0 {
        eprintln!("  {} removed {} orphan object body line(s)", "✓".green(), s.orphan_object_bodies_removed);
    }
}

fn print_diff_summary(diff: &mapper::MapDiff, reference_path: &Path) {
    eprintln!(
        "\n{} vs reference: {}",
        "diff".cyan().bold(),
        reference_path.display()
    );
    if diff.status == mapper::DiffStatus::Identical {
        eprintln!("{}", "  identical — no structural changes".green());
        return;
    }

    let print_list = |label: &str, items: &[String]| {
        if !items.is_empty() {
            eprintln!("  {}:", label.bold());
            for item in items {
                eprintln!("    {}", item);
            }
        }
    };

    print_list("added targets",         &diff.added.targets);
    print_list("removed targets",       &diff.removed.targets);
    print_list("added groups",          &diff.added.groups);
    print_list("removed groups",        &diff.removed.groups);
    print_list("added files",           &diff.added.files);
    print_list("removed files",         &diff.removed.files);
    print_list("added build entries",   &diff.added.build_phase_entries);
    print_list("removed build entries", &diff.removed.build_phase_entries);

    if !diff.uuid_changes.is_empty() {
        eprintln!("  {}:", "UUID changes".bold());
        for c in &diff.uuid_changes {
            eprintln!("    {} → {}  ({})", c.old_uuid, c.new_uuid, c.path);
        }
    }
}

fn process_finish(
    pbxproj_path: &Path,
    output: Option<&Path>,
    combine_commit: bool,
    original: &str,
    result: &str,
) -> Result<()> {
    let modified = result != original;
    let dest = output.unwrap_or(pbxproj_path);

    if modified {
        write_atomic(dest, result)
            .with_context(|| format!("cannot write {}", dest.display()))?;
        eprintln!("{}", format!("✓ written: {}", dest.display()).green().bold());

        if combine_commit {
            bail!(
                "project.pbxproj was modified — please `git add {}` and commit again",
                dest.display()
            );
        }
    } else {
        eprintln!("{}", "✓ no changes — file already clean".green());
    }
    Ok(())
}

/// Write to a temp file then atomically rename.
fn write_atomic(dest: &Path, content: &str) -> Result<()> {
    let tmp = dest.with_extension("pbxproj.tmp");
    fs::write(&tmp, content)?;
    fs::rename(&tmp, dest)?;
    Ok(())
}

fn resolve_pbxproj_path(input: &Path) -> Result<PathBuf> {
    let abs = input
        .canonicalize()
        .with_context(|| format!("path not found: {}", input.display()))?;

    if abs.is_file() {
        if abs.extension().and_then(|e| e.to_str()) == Some("pbxproj") {
            return Ok(abs);
        }
        bail!("'{}' is not a project.pbxproj file", abs.display());
    }

    if abs.is_dir() {
        if abs.extension().and_then(|e| e.to_str()) == Some("xcodeproj") {
            let pbxproj = abs.join("project.pbxproj");
            if pbxproj.exists() {
                return Ok(pbxproj);
            }
            bail!("no project.pbxproj found inside {}", abs.display());
        }
        bail!("directory '{}' is not a .xcodeproj bundle", abs.display());
    }

    bail!("'{}' is neither a file nor a directory", abs.display());
}

fn infer_project_name(pbxproj: &Path) -> String {
    pbxproj
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("Project.xcodeproj")
        .to_string()
}
