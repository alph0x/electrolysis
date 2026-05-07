# electrolysis

> A Rust tool for keeping Xcode `.pbxproj` files clean, deterministic, and merge-safe.

Electrolysis repairs corruption in `project.pbxproj` caused by merge conflicts or AI code-agent manipulation, assigns deterministic MD5-based UUIDs, and sorts the file structurally — producing a stable, diff-friendly output that Xcode can read without issues.

---

## The problem this solves

This tool was born out of a real-world need on a large-scale iOS project: **30+ targets, exclusive internal dependencies per target, multiple developers, and frequent AI-assisted code changes**.

In that environment, `project.pbxproj` becomes a source of constant pain:

- Every `git merge` on the xcodeproj produces conflict markers that standard parsers cannot handle
- Xcode assigns non-deterministic random UUIDs on every project modification — two developers adding the same file end up with different UUIDs for it, making diffs noisy and merges explosive
- AI code agents (GitHub Copilot, Claude, Cursor, etc.) frequently rewrite sections of the pbxproj in ways that drift from the original structure
- With 30+ targets and their own exclusive build configurations, schemes, and SPM dependencies, a single bad merge can silently remove an entire target from the build

`electrolysis` solves all of this.

---

## Features

| Step | When | What it does |
|------|------|--------------|
| **Sanitize** | Always | Resolves merge-conflict markers, removes duplicate objects and list items, drops orphan section bodies |
| **Uniquify** | Default | Replaces random Xcode UUIDs with deterministic MD5-based hex UUIDs keyed on `<ProjectName>/<path/to/file>` |
| **Scheme propagation** | After uniquify | Rewrites `BlueprintIdentifier` values in `xcshareddata/xcschemes/*.xcscheme` so schemes keep resolving to their targets after the UUID rename. Disable with `--no-update-schemes`. |
| **Sort** | Default | Sorts PBX sections, `files = (…)` and `children = (…)` lists alphabetically by element name, drops duplicate list entries |
| **Validate** | Always | Re-parses the output before writing to guarantee Xcode can read it |
| **Map** | `map` subcommand | Emits a JSON snapshot of the project structure: file tree, targets, build phases, UUID table |
| **Diff** | `diff` subcommand | Compares the current project against a reference map; exits 1 if structural changes are detected |
| **Merge driver** | `merge-driver` subcommand | Registers as a custom git merge driver for `*.pbxproj` — resolves merge conflicts automatically via union merge + electrolysis pipeline |
| **Restore** | `restore` subcommand | Copies the most recent `.pbxproj.bak` back to the original file |
| **Watch** | `--watch` flag | Monitors the pbxproj file and re-runs automatically when it changes |
| **Git hooks** | `install-git-hooks` subcommand | One-command setup for pre-commit hook and merge driver |

### Benefits

- **Stable diffs** — deterministic UUIDs mean adding the same file always produces the same UUID regardless of who does it or when
- **Merge conflict recovery** — the sanitize pass repairs the file before parsing, so a conflicted pbxproj is never fatal
- **CI-friendly** — use `-c` as a pre-commit hook or `--check` in CI; both exit non-zero if the file needs changes
- **Structural auditing** — `map` + `diff` let you catch when a target, file, or build phase was accidentally removed between commits
- **Idempotent** — running electrolysis twice produces identical output; the second run reports "no changes"
- **No runtime dependencies** — a single statically-linked binary; no Python, no Ruby, no Xcode tools required

---

## Installation

### From source (requires Rust)

```bash
git clone https://github.com/alph0x/electrolysis
cd electrolysis
cargo install --path .
```

### From a release binary

Download the latest binary from the [Releases](https://github.com/alph0x/electrolysis/releases) page and place it somewhere on your `$PATH`.

**macOS — Apple Silicon**

```bash
curl -L https://github.com/alph0x/electrolysis/releases/latest/download/electrolysis-aarch64-apple-darwin.tar.gz | sudo tar -xz -C /usr/local/bin
```

**macOS — Intel**

```bash
curl -L https://github.com/alph0x/electrolysis/releases/latest/download/electrolysis-x86_64-apple-darwin.tar.gz | sudo tar -xz -C /usr/local/bin
```

> `sudo` is required to write to `/usr/local/bin`. You will be prompted for your password.


---

## Usage

### Basic — process a project

```bash
# Pass the .xcodeproj directory (or the project.pbxproj file directly)
electrolysis MyApp.xcodeproj

# Auto-discover in the current directory
cd /path/to/your/repo
electrolysis
```

When run without arguments, electrolysis searches the current directory for `.xcodeproj` bundles:
- **One found** — runs automatically
- **Multiple found** — presents a numbered list and lets you choose one or all

### Flags

```
electrolysis [OPTIONS] [PATH]

Arguments:
  [PATH]  Path to the .xcodeproj directory or project.pbxproj file

Options:
  -q, --quiet           Suppress non-error output
  -v, --verbose         Print detailed diagnostic output
      --sanitize-only   Only repair corruption; skip uniquify and sort
  -u, --unique          Uniquify UUIDs only (skip sort)
  -s, --sort            Sort only (skip uniquify)
      --sort-main-group Sort the main group's children list too
      --no-update-schemes
                        Skip rewriting BlueprintIdentifier in
                        xcshareddata/xcschemes/*.xcscheme after uniquify
  -c, --combine-commit  Exit non-zero if the file was modified (git pre-commit hook)
      --check           Report whether the file needs changes without modifying it
      --backup          Create a backup before modifying in place
      --backup-path     Custom backup path (requires --backup)
      --watch           Watch the pbxproj file and re-run automatically when it changes
  -o, --output <FILE>   Write output to FILE instead of modifying in place
  -h, --help            Print help
  -V, --version         Print version
```

### Generate a project map (JSON snapshot)

```bash
electrolysis map MyApp.xcodeproj
# writes MyApp.electrolysis-map.json next to the .xcodeproj

electrolysis map MyApp.xcodeproj -o snapshot.json
```

The map captures targets, groups, files, build phases, and a complete UUID table.

### Diff against a reference map

```bash
# Save a baseline after a known-good state
electrolysis map MyApp.xcodeproj -o baseline.json

# Later, check if anything structural changed
electrolysis diff MyApp.xcodeproj baseline.json
# exits 0 if identical, exits 1 if changes detected

# Human-readable colored output
electrolysis diff MyApp.xcodeproj baseline.json --color
```

Useful as a CI step to catch accidental target or file removals.

### Restore a backup

```bash
electrolysis restore MyApp.xcodeproj
```

Copies `MyApp.xcodeproj/project.pbxproj.bak` back to `project.pbxproj`. Useful if the last run produced unexpected results.

### Watch mode

```bash
electrolysis MyApp.xcodeproj --watch
```

Monitors `project.pbxproj` and re-runs the full pipeline automatically when the file changes. Press `Ctrl-C` to stop.

### As a git pre-commit hook

#### Automatic setup (recommended)

```bash
cd /path/to/your/repo
electrolysis install-git-hooks
```

This creates/updates:
- `.git/hooks/pre-commit` — blocks commits if the pbxproj needs changes
- `.gitattributes` — registers the merge driver for all `*.pbxproj` files
- git config — sets up the `merge.electrolysis` driver

Commit the `.gitattributes` file so the whole team gets the merge driver.

#### Manual setup

Create or append to `.git/hooks/pre-commit`:

```bash
#!/bin/sh
electrolysis MyApp.xcodeproj -c
```

If the file needed changes, the commit is blocked and the repaired file is staged for re-commit.

---

## As a git merge driver

Electrolysis can **automatically resolve merge conflicts** in `project.pbxproj` so you never have to touch conflict markers by hand again.

### One-time setup (per repository)

```bash
# 1. Tell git to use electrolysis as the merge driver for pbxproj files
git config merge.electrolysis.name "Electrolysis pbxproj merge driver"
git config merge.electrolysis.driver "electrolysis merge-driver %O %A %B"

# 2. Register the driver for all pbxproj files
echo "*.pbxproj merge=electrolysis" >> .gitattributes

# 3. Commit the .gitattributes so the whole team gets it
git add .gitattributes
git commit -m "chore: use electrolysis merge driver for pbxproj files"
```

Or simply run `electrolysis install-git-hooks` (see above).

### What happens during a merge

When git detects a conflict in `project.pbxproj`:

1. **Normalize each side** — runs the full electrolysis pipeline on **base**, **ours**, and **theirs** independently. This turns random Xcode UUIDs into deterministic MD5-based ones and sorts all lists alphabetically.
2. **Union merge** — runs `git merge-file --union` on the three already-normalized files. Because identical objects now share the exact same lines, most changes merge cleanly without conflicts.
3. **Sanitize** — removes any duplicate objects or list entries that slipped through, plus any leftover conflict markers.
4. **Validation** — re-parses the output to guarantee Xcode can read it.

The resolved file is written back and git treats the conflict as **fully resolved** — no manual intervention required in ~95% of cases.

---

## Configuration file

You can create an `.electrolysis.toml` file in your project root (or any parent directory) to set default flags:

```toml
verbose = true
sort-main-group = true
backup = true
```

CLI arguments always override the config file. This is useful for teams that want to version-control their preferences.

---

## How it works

Electrolysis processes the pbxproj in six sequential passes:

```
raw text → sanitize → parse → uniquify → propagate-to-schemes → sort → validate → write
```

1. **Sanitize** (text-level, regex-based)
   - Resolves `<<<<<<<` / `=======` / `>>>>>>>` merge conflict blocks
   - Deduplicates object declarations (`UUID /* name */ = { … }`)
   - Deduplicates list entries within `( … )` arrays
   - Removes orphan section headers with no body
   - Removes orphan object body lines with no matching declaration

2. **Parse** — custom OpenStep ASCII plist parser, handling all Xcode output formats including modern Xcode 15+ (`objectVersion = 77`) and Swift Package Manager integration types

3. **Uniquify** — traverses the project graph from the root object (targets → mainGroup → build phases → files) and builds a deterministic old→new UUID map. Keys are derived from `MD5(<ProjectName>/<path>)`. Objects unreachable from the root are treated as orphans and removed.

4. **Scheme propagation** — applies the same old→new UUID map to every `BlueprintIdentifier` inside `<bundle>.xcodeproj/xcshareddata/xcschemes/*.xcscheme`. Without this step, every shared scheme would orphan as soon as the uniquifier renames its target. Schemes already on the new UUIDs are left untouched (idempotent). `xcuserdata` is intentionally skipped — those are per-developer state and typically gitignored. Opt out with `--no-update-schemes`.

5. **Sort** — sorts `files = (…)` and `children = (…)` lists alphabetically by element name, sorts PBXBuildFile and PBXFileReference sections by filename, removes duplicate list entries.

6. **Validate** — re-parses the final output to guarantee it is structurally valid before writing to disk.

---

## License

Apache 2.0 — see [LICENSE](LICENSE).
