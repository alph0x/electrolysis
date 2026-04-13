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
| **Sort** | Default | Sorts PBX sections, `files = (…)` and `children = (…)` lists alphabetically by element name, drops duplicate list entries |
| **Map** | `map` subcommand | Emits a JSON snapshot of the project structure: file tree, targets, build phases, UUID table |
| **Diff** | `diff` subcommand | Compares the current project against a reference map; exits 1 if structural changes are detected |

### Benefits

- **Stable diffs** — deterministic UUIDs mean adding the same file always produces the same UUID regardless of who does it or when
- **Merge conflict recovery** — the sanitize pass repairs the file before parsing, so a conflicted pbxproj is never fatal
- **CI-friendly** — use `-c` as a pre-commit hook; it exits non-zero if the file was modified, enforcing a clean-before-commit policy
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

### From a release binary (macOS)

Download the latest binary from the [Releases](https://github.com/alph0x/electrolysis/releases) page and place it somewhere on your `$PATH`:

```bash
curl -L https://github.com/alph0x/electrolysis/releases/latest/download/electrolysis-aarch64-apple-darwin \
  -o /usr/local/bin/electrolysis
chmod +x /usr/local/bin/electrolysis
```

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
  -v, --verbose         Print detailed diagnostic output
      --sanitize-only   Only repair corruption; skip uniquify and sort
  -u, --unique          Uniquify UUIDs only (skip sort)
  -s, --sort            Sort only (skip uniquify)
  -c, --combine-commit  Exit non-zero if the file was modified (git pre-commit hook)
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
```

Useful as a CI step to catch accidental target or file removals.

### As a git pre-commit hook

Create or append to `.git/hooks/pre-commit`:

```bash
#!/bin/sh
electrolysis MyApp.xcodeproj -c
```

If the file needed changes, the commit is blocked and the repaired file is staged for re-commit.

---

## How it works

Electrolysis processes the pbxproj in four sequential passes:

```
raw text → sanitize → parse → uniquify → sort → write
```

1. **Sanitize** (text-level, regex-based)
   - Resolves `<<<<<<<` / `=======` / `>>>>>>>` merge conflict blocks
   - Deduplicates object declarations (`UUID /* name */ = { … }`)
   - Deduplicates list entries within `( … )` arrays
   - Removes orphan section headers with no body
   - Removes orphan object body lines with no matching declaration

2. **Parse** — custom OpenStep ASCII plist parser, handling all Xcode output formats including modern Xcode 15+ (`objectVersion = 77`) and Swift Package Manager integration types

3. **Uniquify** — traverses the project graph from the root object (targets → mainGroup → build phases → files) and builds a deterministic old→new UUID map. Keys are derived from `MD5(<ProjectName>/<path>)`. Objects unreachable from the root are treated as orphans and removed.

4. **Sort** — sorts `files = (…)` and `children = (…)` lists alphabetically by element name, sorts PBXBuildFile and PBXFileReference sections by filename, removes duplicate list entries.

---

## License

Apache 2.0 — see [LICENSE](LICENSE).
