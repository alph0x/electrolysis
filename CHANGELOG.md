# Changelog

All notable changes to this project will be documented in this file.

## [1.4.1] - 2026-05-07

### Fixed

- **Orphan `BlueprintIdentifier` repair** — schemes whose `BlueprintIdentifier` no longer corresponds to any target in the pbxproj are now repaired by resolving them through `BlueprintName` against the project's current target index. Previously, only UUIDs that changed in the *current* uniquify pass were propagated; orphans that pre-dated the run (e.g. introduced by an older tool, manual edits, or pre-1.4.0 electrolysis runs) stayed broken even after `update_shared_schemes` reported success. The repair pass is conservative: it only acts when the orphan's `ReferencedContainer` matches the project being processed and `BlueprintName` resolves to a single target — duplicate names and cross-project references are left untouched.

### Internal

- New `scheme_updater::repair_orphan_blueprint_identifiers` (pure) and `repair_shared_schemes` (orchestrator) functions, plus a `build_scheme_repair_inputs` helper in `pipeline` that produces the `name → uuid` index and the post-rename UUID set from the parsed `PbxProject`. All paths covered by unit tests.

## [1.4.0] - 2026-05-07

### Added

- **Scheme propagation** — after uniquify, electrolysis now rewrites every `BlueprintIdentifier` value inside `<bundle>.xcodeproj/xcshareddata/xcschemes/*.xcscheme` to match the new UUIDs. Previously, uniquifying a project orphaned every shared scheme because their `BlueprintIdentifier` references kept pointing to the old target UUIDs, leaving Xcode and tooling (e.g. fastlane's `get_product_bundle_id`) unable to resolve the targets.
- **`--no-update-schemes`** — opt out of the scheme propagation pass. Useful if your team manages schemes by hand or from another tool.
- **`update-schemes` TOML key** — same opt-out at the project level via `.electrolysis.toml`.

### Changed

- **Default behavior is opinionated** — scheme propagation runs by default so existing users get the fix automatically with no flag changes. The pass is fully idempotent and skipped in `--check` mode and in `--sort`-only mode (no UUIDs change).
- **`xcuserdata` schemes are intentionally untouched** — those are per-developer state and typically gitignored. A future flag may opt them in.

### Internal

- **`FileSystem::list_dir`** added to the trait so the pipeline can enumerate scheme files through the same abstraction the rest of the I/O uses (keeps the in-memory test double drop-in).

## [1.3.0] - 2026-05-07

### Added

- **`--check` flag** — dry-run mode that validates whether a file would change without writing to disk (exit 0 = clean, 1 = would change).
- **`--backup` / `--backup-path`** — creates a `.pbxproj.bak` copy before overwriting.
- **`restore` subcommand** — restores `.pbxproj.bak` to the original `.pbxproj` file.
- **`.electrolysis.toml` config file** — per-project persistent configuration with CLI override priority.
- **`--sort-main-group`** — optionally sorts the root group children in addition to other sections.
- **`--watch` mode** — uses `notify` crate with debounce to automatically re-process files on change.
- **`install-git-hooks` subcommand** — configures pre-commit hook, merge driver, and `.gitattributes` for transparent integration.
- **Colored diff output** — `diff --color` prints git-diff-style ANSI colors.
- **Structured logging** — `--quiet` / `--verbose` log levels via `Logger` trait abstraction.
- **Collision detection** — debug-only MD5 collision assertion in uniquifier.

### Changed

- **Refactored to testable pipeline** — extracted `Pipeline`, `FileSystem`/`Logger` traits, `Config` struct. `main.rs` is now bootstrap-only.
- **Validation** — all writes are pre-validated by re-parsing output before committing to disk.
- **Timestamps** — replaced manual `utc_timestamp()` with `chrono` for RFC 3339 formatting.
- **Map/diff paths** — fixed path resolution when processing files in `/private/tmp` and other edge locations.

### Fixed

- **Clap flag conflict** — resolved `--verbose` short `-v` collision with built-in `--version` (`-V`).

## [1.2.0] and earlier

See git history for changes prior to 1.3.0.
