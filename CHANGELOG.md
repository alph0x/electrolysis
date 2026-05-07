# Changelog

All notable changes to this project will be documented in this file.

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
