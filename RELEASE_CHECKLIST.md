# Release Checklist

> Use this checklist before publishing a new version of electrolysis.

## Pre-release

- [ ] All tests pass (`cargo test`)
- [ ] `cargo check` is clean (no errors)
- [ ] `cargo clippy` passes (if enabled)
- [ ] Version bumped in `Cargo.toml`
- [ ] `CHANGELOG.md` updated with release notes
- [ ] `README.md` and `ROADMAP.md` are up to date
- [ ] Local release build succeeds (`cargo build --release`)
- [ ] Binary runs correctly on a real fixture

## Tag & Release

- [ ] Changes committed (`git add -A && git commit -m "chore: release vX.Y.Z"`)
- [ ] Git tag created (`git tag -a vX.Y.Z -m "Release vX.Y.Z"`)
- [ ] Tag pushed (`git push origin vX.Y.Z`)
- [ ] GitHub Actions `release.yml` completes successfully
- [ ] Release artifacts downloaded and smoke-tested
- [ ] GitHub release notes published (copy from CHANGELOG)

## Post-release

- [ ] Close completed roadmap items
- [ ] Announce in relevant channels (if applicable)

---

## Automated workflow

This repo uses [`.github/workflows/release.yml`](.github/workflows/release.yml):

- Triggered automatically on `git push origin v*`
- Builds for `aarch64-apple-darwin` and `x86_64-apple-darwin`
- Creates a GitHub release with attached `.tar.gz` binaries

So the manual steps are essentially: **test → bump → changelog → commit → tag → push**.
