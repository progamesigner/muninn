# CLAUDE.md

Guidance for Claude Code when working in this repository.

## Git & releases

- **Sign both commits and tags.** Every commit must be GPG-signed (`git commit -S`,
  or rely on `commit.gpgsign=true`) and every tag must be a signed annotated tag
  (`git tag -s`, not `-a`). Unsigned commits or tags show as "Unverified" on
  GitHub and are not acceptable — verify with `git log --show-signature` and
  `git tag -v <tag>` before pushing.
- **Release flow:** bump `version` in `Cargo.toml`, refresh `Cargo.lock`
  (`cargo check`), commit as `Release vX.Y.Z`, then create a signed tag
  `vX.Y.Z` with message `muninn X.Y.Z`. Pushing the tag triggers the CI
  release jobs.
- Run `cargo fmt --check`, `cargo clippy --all-targets`, and `cargo test`
  before committing release-bound work — CI rejects unformatted code.
