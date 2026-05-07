# Electrolysis Roadmap

This document tracks completed and proposed improvements for the project. Items are grouped by priority and include context on why they matter and rough complexity estimates.

---

## ✅ Recently Completed

### Scheme propagation (1.4.0)
- **What:** After uniquify, rewrites `BlueprintIdentifier` values in every `<bundle>.xcodeproj/xcshareddata/xcschemes/*.xcscheme` so schemes keep resolving to their targets. Opt out with `--no-update-schemes` or `update-schemes = false` in TOML.
- **Why:** Uniquifying without this pass orphaned every shared scheme — Xcode showed "missing target" and tools that resolve targets by `BlueprintIdentifier` (e.g. fastlane's `get_product_bundle_id`) failed at the first step. Real-world impact: a routine `electrolysis` run on a 30+ target project broke 37/37 schemes simultaneously.
- **Impact:** High — closes the load-bearing gap between deterministic pbxproj UUIDs and the rest of the bundle that references them.

### Git merge driver (`merge-driver` subcommand)
- **What:** Custom git merge driver for `project.pbxproj` files. Normalizes base/ours/theirs independently (sanitize → uniquify → sort), then runs `git merge-file --union`.
- **Why:** Eliminates manual conflict resolution for pbxproj merges. Because all sides use deterministic UUIDs and sorted lists, most changes merge cleanly without conflicts.
- **Impact:** High — solves the #1 pain point for teams with multiple developers.

### Sanitize post-uniquify
- **What:** Added a second `sanitize` pass after `uniquify` in the pipeline.
- **Why:** The uniquifier can cause different original UUIDs to collide into the same deterministic UUID (e.g. two `XCLocalSwiftPackageReference` objects pointing to the same local package). The sanitizer removes those duplicate declarations.
- **Impact:** High — fixes idempotency and prevents invalid pbxproj output.

### Post-process validation (#1)
- **What:** After the full pipeline produces the final output, re-parses it with `parser::parse_project()` before writing to disk. If it fails, aborts with a clear error.
- **Why:** Guarantees that a bug in the sorter or uniquifier can never produce an invalid pbxproj that Xcode rejects.

### `--check` / `--dry-run` mode (#2)
- **What:** Reports whether the file needs changes but does **not** modify it. Exit 0 if clean, exit 1 if changes would be made.
- **Why:** CI-friendly. Teams can enforce clean pbxproj files in CI without side effects.

### Backup flag (`--backup`) (#3)
- **What:** Before modifying in place, copies the original to `.pbxproj.bak` (or a user-specified path via `--backup-path`).
- **Why:** If electrolysis has an undiscovered bug, users can recover instantly.

### Config file support (`.electrolysis.toml`) (#4)
- **What:** Per-project configuration file to set flags like `sort-main-group = true`, `verbose = true`, etc. CLI arguments override TOML values.
- **Why:** Eliminates the need to pass the same CLI flags every time. Teams can version-control their preferences.

### Configurable main-group sorting (`--sort-main-group`) (#6)
- **What:** Flag to sort the main group's `children` list too for teams that prefer full determinism.
- **Why:** Some teams don't care about Xcode navigator order and want 100% stable diffs.

### Human-readable colored diff (#7)
- **What:** `diff --color` prints a `git diff --color-words` style output with red/green highlighting instead of JSON.
- **Why:** Much faster to scan visually in day-to-day usage.

### Replace manual timestamp implementation (#8)
- **What:** Replaced hand-rolled `utc_timestamp()` in `mapper.rs` with `chrono::Utc::now()`.
- **Why:** Less code to maintain, fewer bugs, standard library handles edge cases.

### MD5 collision detection (#10)
- **What:** Debug-only assertion in the uniquifier that panics if two different semantic paths produce the same MD5 hash.
- **Why:** Catches the extremely unlikely event of silent data corruption.

### Richer logging system (#11)
- **What:** Replaced ad-hoc `eprintln!` with a `Logger` trait supporting `--quiet`, default, and `--verbose` levels.
- **Why:** Better control over output in CI and scripts.

### `restore` subcommand (#12)
- **What:** `electrolysis restore MyApp.xcodeproj` copies the most recent `.pbxproj.bak` back to the original file.
- **Why:** Quick recovery if a run produced unexpected results.

### Watch mode (`--watch`) (#13)
- **What:** Monitors the pbxproj file and auto-runs electrolysis when it changes.
- **Why:** Useful during heavy AI-assisted refactoring where the pbxproj is touched repeatedly.

### Installer for git hooks (#14)
- **What:** `electrolysis install-git-hooks` automatically configures the pre-commit hook and merge driver in the current repo.
- **Why:** Reduces friction for team onboarding. One command instead of manual `.git/config` and `.gitattributes` editing.

---

## 🟡 Medium Priority

### Preserve custom comments (#5)
**What:** The parser currently strips **all** comments (`//` and `/* */`) before processing. Some teams add explanatory comments in the pbxproj (rare but happens). Preserve them through the pipeline.

**Why:** Avoids data loss for teams that document their project structure inline.

**Complexity:** Medium — requires storing comments as metadata in the AST and re-emitting them during serialization. The current parser is lossy by design.

---

### Memory optimization in parser (#9)
**What:** In `parse_project()`, every object dict is cloned into a new `IndexMap`:
```rust
for (uuid, val) in objects_val {
    if let Some(obj_dict) = val.as_dict() {
        objects.insert(uuid.clone(), obj_dict.clone());
    }
}
```

**Why:** For very large pbxproj files this doubles memory usage unnecessarily. Could reuse or reference the already-parsed data.

**Complexity:** Medium — may require lifetime changes in `PbxProject`.
