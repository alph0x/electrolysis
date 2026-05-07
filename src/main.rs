use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{bail, Context, Result};
use clap::{Parser as ClapParser, Subcommand};
use colored::Colorize;

mod config;
mod error;
mod logger;
mod mapper;
mod merge_driver;
mod parser;
mod pipeline;
mod sanitizer;
mod sorter;
mod uniquifier;

use config::Config;
use logger::{ConsoleLogger, LogLevel, Logger};
use pipeline::{FileSystem, Pipeline, PipelineOutcome, RealFileSystem};

// Re-exports for sibling modules that still reference them via crate::
pub(crate) use pipeline::infer_project_name;
pub(crate) use pipeline::write_atomic;

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

    /// Path to the `.xcodeproj` directory or `project.pbxproj` file.
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,

    /// Suppress non-error output.
    #[arg(short, long, global = true, num_args = 0..=1, default_missing_value = "true")]
    quiet: Option<bool>,

    /// Enable verbose diagnostic output.
    #[arg(short, long, global = true, num_args = 0..=1, default_missing_value = "true")]
    verbose: Option<bool>,

    /// Only sanitize (fix corruption). Skip uniquify and sort.
    #[arg(long, conflicts_with_all = ["unique", "sort"], num_args = 0..=1, default_missing_value = "true")]
    sanitize_only: Option<bool>,

    /// Uniquify UUIDs only (skips sort).
    #[arg(short, long, conflicts_with = "sort", num_args = 0..=1, default_missing_value = "true")]
    unique: Option<bool>,

    /// Sort only (skips uniquify).
    #[arg(short, long, conflicts_with = "unique", num_args = 0..=1, default_missing_value = "true")]
    sort: Option<bool>,

    /// Exit with non-zero status if the file was modified.
    /// Useful as a git pre-commit hook.
    #[arg(short = 'c', long, num_args = 0..=1, default_missing_value = "true")]
    combine_commit: Option<bool>,

    /// Write processed output to this path instead of modifying in place.
    #[arg(short = 'o', long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Report whether the file needs changes without modifying it.
    /// Exit 0 if clean, exit 1 if changes would be made.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    check: Option<bool>,

    /// Create a backup before modifying in place.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    backup: Option<bool>,

    /// Custom backup path (requires --backup).
    #[arg(long, value_name = "PATH")]
    backup_path: Option<PathBuf>,

    /// Sort the main group's children list too.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    sort_main_group: Option<bool>,

    /// Watch the pbxproj file and re-run automatically when it changes.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    watch: Option<bool>,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate a JSON map of the project structure.
    Map {
        path: PathBuf,
        #[arg(short = 'o', long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Compare the current project structure against a reference map.
    Diff {
        path: PathBuf,
        reference: PathBuf,
        #[arg(short = 'o', long, value_name = "FILE")]
        output: Option<PathBuf>,
        /// Print a human-readable colored diff instead of JSON.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        color: Option<bool>,
    },
    /// Git merge driver for `project.pbxproj` files.
    MergeDriver {
        base: PathBuf,
        current: PathBuf,
        other: PathBuf,
    },

    /// Restore the most recent backup of a pbxproj file.
    Restore {
        /// Path to the `.xcodeproj` directory or `project.pbxproj` file.
        path: PathBuf,
    },

    /// Install git hooks and merge driver for the current repository.
    InstallGitHooks,
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();
    let toml_config = load_electrolysis_config();
    let config = config_from_cli(&cli, toml_config);
    let level = if config.quiet {
        LogLevel::Quiet
    } else if config.verbose {
        LogLevel::Verbose
    } else {
        LogLevel::Normal
    };

    let result = match cli.command {
        Some(Commands::Map { path, output }) => {
            let pbxproj = resolve_pbxproj_path(&path);
            match pbxproj {
                Ok(p) => {
                    let fs = RealFileSystem;
                    let logger = ConsoleLogger::new(level);
                    let pipeline = Pipeline::new(&config, &fs, &logger);
                    pipeline.map(&p, output.as_deref())
                }
                Err(e) => Err(e),
            }
        }
        Some(Commands::Diff { path, reference, output, color }) => {
            run_diff(&config, level, &path, &reference, output.as_deref(), color.unwrap_or(false))
        }
        Some(Commands::MergeDriver { base, current, other }) => {
            merge_driver::run(&base, &current, &other, cli.verbose.unwrap_or(false))
        }
        Some(Commands::Restore { path }) => {
            run_restore(&path)
        }
        Some(Commands::InstallGitHooks) => {
            run_install_git_hooks()
        }
        None => run_process(config, cli),
    };

    match result {
        Ok(()) => {}
        Err(e) => {
            let logger = ConsoleLogger::new(LogLevel::Normal);
            logger.error(&e.to_string());
            let mut source = e.source();
            while let Some(cause) = source {
                eprintln!("  {} {}", "caused by:".dimmed(), cause);
                source = cause.source();
            }
            process::exit(1);
        }
    }
}

// ── Config builder ────────────────────────────────────────────────────────────

fn run_diff(
    config: &Config,
    level: LogLevel,
    path: &Path,
    reference: &Path,
    output: Option<&Path>,
    color: bool,
) -> Result<()> {
    let p = resolve_pbxproj_path(path)?;
    let fs = RealFileSystem;
    let logger = ConsoleLogger::new(level);
    let pipeline = Pipeline::new(config, &fs, &logger);
    let status = pipeline.diff(&p, reference, output, color)?;
    if status == mapper::DiffStatus::HasChanges {
        process::exit(1);
    }
    Ok(())
}

fn run_install_git_hooks() -> Result<()> {
    let cwd = std::env::current_dir().context("cannot get current directory")?;
    let git_dir = find_git_dir(&cwd).ok_or_else(|| anyhow::anyhow!("not inside a git repository"))?;
    let hook_path = git_dir.join("hooks").join("pre-commit");
    let attributes_path = cwd.join(".gitattributes");

    let hook_content = r#"#!/bin/sh
# Auto-generated by electrolysis install-git-hooks
exec electrolysis -c
"#;

    if hook_path.exists() {
        let existing = std::fs::read_to_string(&hook_path).unwrap_or_default();
        if existing.contains("electrolysis") {
            eprintln!("{} pre-commit hook already contains electrolysis — skipping", "→".cyan());
        } else {
            std::fs::write(&hook_path, format!("{}\n{}", existing.trim_end(), hook_content))
                .with_context(|| format!("cannot append to {}", hook_path.display()))?;
            eprintln!("{} appended electrolysis to existing pre-commit hook", "✓".green().bold());
        }
    } else {
        std::fs::write(&hook_path, hook_content)
            .with_context(|| format!("cannot write {}", hook_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&hook_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&hook_path, perms)?;
        }
        eprintln!("{} created pre-commit hook: {}", "✓".green().bold(), hook_path.display());
    }

    let output = std::process::Command::new("git")
        .args(["config", "merge.electrolysis.name", "Electrolysis pbxproj merge driver"])
        .output()
        .context("failed to run `git config`")?;
    if !output.status.success() {
        bail!("git config failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let output = std::process::Command::new("git")
        .args(["config", "merge.electrolysis.driver", "electrolysis merge-driver %O %A %B"])
        .output()
        .context("failed to run `git config`")?;
    if !output.status.success() {
        bail!("git config failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    eprintln!("{} configured merge driver", "✓".green().bold());

    let attr_line = "*.pbxproj merge=electrolysis";
    if attributes_path.exists() {
        let existing = std::fs::read_to_string(&attributes_path).unwrap_or_default();
        if existing.contains("merge=electrolysis") {
            eprintln!("{} .gitattributes already configured — skipping", "→".cyan());
        } else {
            std::fs::write(&attributes_path, format!("{}\n{}\n", existing.trim_end(), attr_line))
                .with_context(|| format!("cannot write {}", attributes_path.display()))?;
            eprintln!("{} appended to .gitattributes", "✓".green().bold());
        }
    } else {
        std::fs::write(&attributes_path, format!("{}\n", attr_line))
            .with_context(|| format!("cannot write {}", attributes_path.display()))?;
        eprintln!("{} created .gitattributes", "✓".green().bold());
    }

    eprintln!("\n{}", "Git hooks installed successfully.".bold());
    eprintln!("  Commit the .gitattributes file so the whole team gets the merge driver.");
    Ok(())
}

fn find_git_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let git = d.join(".git");
        if git.is_dir() {
            return Some(git);
        }
        dir = d.parent();
    }
    None
}

fn run_restore(path: &Path) -> Result<()> {
    let pbxproj = resolve_pbxproj_path(path)?;
    let backup = pbxproj.with_extension("pbxproj.bak");
    if !backup.exists() {
        bail!("no backup found: {}", backup.display());
    }
    let fs = RealFileSystem;
    fs.copy(&backup, &pbxproj)
        .with_context(|| format!("cannot restore {} to {}", backup.display(), pbxproj.display()))?;
    eprintln!("{} restored {} → {}", "✓".green().bold(), backup.display(), pbxproj.display());
    Ok(())
}

fn load_electrolysis_config() -> Option<Config> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let path = dir.join(".electrolysis.toml");
        if path.exists() {
            let content = std::fs::read_to_string(&path).ok()?;
            let toml: config::TomlConfig = toml::from_str(&content).ok()?;
            return Some(toml.into_config());
        }
        dir = dir.parent()?.to_path_buf();
    }
}

fn config_from_cli(cli: &Cli, base: Option<Config>) -> Config {
    let mut config = base.unwrap_or_default();
    if let Some(v) = cli.quiet { config.quiet = v; }
    if let Some(v) = cli.verbose { config.verbose = v; }
    if let Some(v) = cli.sanitize_only { config.sanitize_only = v; }
    if let Some(v) = cli.unique { config.unique_only = v; }
    if let Some(v) = cli.sort { config.sort_only = v; }
    if let Some(v) = cli.combine_commit { config.combine_commit = v; }
    if let Some(v) = cli.check { config.check = v; }
    if let Some(v) = cli.backup { config.backup = v; }
    if let Some(v) = cli.sort_main_group { config.sort_main_group = v; }
    if let Some(v) = cli.watch { config.watch = v; }
    if cli.output.is_some() { config.output = cli.output.clone(); }
    if cli.backup_path.is_some() { config.backup_path = cli.backup_path.clone(); }
    config
}

// ── Watch mode ────────────────────────────────────────────────────────────────

fn run_watch(config: Config, pbxproj_path: PathBuf) -> Result<()> {
    use notify::{Config as NotifyConfig, Event, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc::channel;
    use std::time::{Duration, SystemTime};
    use std::thread::sleep;

    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(tx, NotifyConfig::default())
        .context("failed to create file watcher")?;
    let watch_dir = pbxproj_path.parent().unwrap_or(&pbxproj_path);
    watcher.watch(watch_dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("cannot watch {}", watch_dir.display()))?;

    let level = if config.quiet {
        LogLevel::Quiet
    } else if config.verbose {
        LogLevel::Verbose
    } else {
        LogLevel::Normal
    };
    let fs = RealFileSystem;
    let logger = ConsoleLogger::new(level);
    let pipeline = Pipeline::new(&config, &fs, &logger);

    // Run once immediately.
    pipeline.process(&pbxproj_path)?;

    eprintln!("{} watching {} for changes… (Ctrl-C to stop)", "→".cyan(), pbxproj_path.display());

    let mut last_mtime = SystemTime::UNIX_EPOCH;

    loop {
        match rx.recv() {
            Ok(Ok(Event { paths, kind, .. })) => {
                if paths.iter().any(|p| p == &pbxproj_path) {
                    // Only react to modify events to avoid noise.
                    if !matches!(kind, notify::EventKind::Modify(_)) {
                        continue;
                    }
                    // Debounce: wait for write to settle.
                    sleep(Duration::from_millis(200));
                    let meta = std::fs::metadata(&pbxproj_path)
                        .with_context(|| format!("cannot stat {}", pbxproj_path.display()))?;
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    if mtime > last_mtime {
                        last_mtime = mtime;
                        pipeline.process(&pbxproj_path)?;
                    }
                }
            }
            Ok(Err(e)) => {
                eprintln!("{} watch error: {}", "warn:".yellow(), e);
            }
            Err(e) => {
                eprintln!("{} watch channel closed: {}", "warn:".yellow(), e);
                break;
            }
        }
    }

    Ok(())
}

// ── Process (default) ─────────────────────────────────────────────────────────

fn run_process(config: Config, cli: Cli) -> Result<()> {
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

    let level = if config.quiet {
        LogLevel::Quiet
    } else if config.verbose {
        LogLevel::Verbose
    } else {
        LogLevel::Normal
    };
    if config.watch {
        if paths.len() != 1 {
            bail!("--watch requires exactly one project path");
        }
        let path = if paths[0].is_absolute() {
            paths[0].clone()
        } else {
            resolve_pbxproj_path(&paths[0])?
        };
        return run_watch(config, path);
    }

    let fs = RealFileSystem;
    let logger = ConsoleLogger::new(level);
    let pipeline = Pipeline::new(&config, &fs, &logger);

    for path in &paths {
        let path = if path.is_absolute() {
            path.clone()
        } else {
            resolve_pbxproj_path(path)?
        };

        match pipeline.process(&path)? {
            PipelineOutcome::Clean => {}
            PipelineOutcome::Modified if config.combine_commit => {
                bail!(
                    "project.pbxproj was modified — please `git add {}` and commit again",
                    path.display()
                );
            }
            PipelineOutcome::Modified => {}
            PipelineOutcome::WouldChange => {
                process::exit(1);
            }
        }
    }

    if !config.check {
        eprintln!(
            "\n{}",
            "Successfully removed the rust on this project with electrolysis 🔬".bold()
        );
        eprintln!("  https://github.com/alph0x/electrolysis — thanks for using it!");
    }

    Ok(())
}

// ── Discovery helpers ─────────────────────────────────────────────────────────

/// Find all `*.xcodeproj` directories directly inside `dir` (non-recursive).
fn discover_xcodeprojs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
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
        bail!("invalid selection: {}", trimmed)
    }
}

// ── Re-exported helpers for merge_driver ──────────────────────────────────────

pub(crate) fn print_sanitize_stats(s: &sanitizer::SanitizeStats) {
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

// ── Path resolution ───────────────────────────────────────────────────────────

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

    bail!("'{}' is neither a file nor a directory", abs.display())
}
