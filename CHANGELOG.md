# Changelog

All notable changes to this project will be documented in this file.

## [1.4.3] - 2026-05-08

### Changed

- **Code quality pass** — applied `rust-patterns` and `clean-code-architecture` skills across the entire codebase.
  - Extracted helper functions to eliminate duplication (`log_level_from_config`, `resolve_path_if_relative`).
  - Simplified TOML→Config mapping with a DRY `set_if_some!` macro.
  - Split `run_install_git_hooks` into 3 single-responsibility functions plus an orchestrator.
  - Removed unnecessary `String` clones in `uniquifier` and `sanitizer`.
  - Replaced `unwrap()` with safer error handling in `merge_driver` and `scheme_updater`.
  - Applied clippy pedantic/nursery lints: redundant closures, `Self` keyword, `map_or_else`, inlined format args, needless raw string hashes.
  - Converted unused-self methods to static functions in `pipeline`.

### Internal

- **New tests** — added 14 unit tests for previously uncovered modules: `logger` (8 tests) and `error` (6 tests).
- **Total test count: 98** (83 unit + 15 integration); all pass.
- **Verified idempotency** on the 22MB real-world fixture (`test-subject.xcodeproj.bak`).

## [1.4.2] - 2026-05-07

### Fixed

- **`PBXProject.attributes.TargetAttributes` preservation** — the dedup pass walks every UUID-keyed `UUID = { … }` line inside `objects = {…}` regardless of nesting depth. When a UUID appears as a *key* of `attributes.TargetAttributes` (signing metadata indexed by target UUID) **and** the same UUID is also the declaration of a top-level `PBXNativeTarget`, the dedup pass treated the inner entry as a duplicate and stripped its body, leaving the wrapper empty. Now bounded by depth: only top-level `objects = {…}` children are deduplicated.
- **`PBXProject.attributes.TargetAttributes` rehydration** — when the wrapper is fully absent (older electrolysis runs stripped it; modern Xcode 16+ doesn't write it back unless explicit per-target signing is set), electrolysis now adds an empty `TargetAttributes = { };` inside `PBXProject.attributes`. fastlane's `automatic_code_signing` / `update_code_signing_settings` actions auto-populate per-target entries on demand, so an empty wrapper is sufficient and unblocks CI builds that were failing with the misleading `"Seems to be a very old project file format"` error.

### Internal

- New `sanitizer::ensure_pbxproject_target_attributes` (Pass 6) and corresponding `target_attributes_wrapper_added` stat. All paths covered by unit tests; idempotent.

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
