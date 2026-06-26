# CI / Release workflows

The GitHub Actions workflows live in `.github/workflows/` (`ci.yml` and
`release.yml`). There is no manual activation step.

## Workflow details

### `ci.yml`

Runs on pushes to `main` and on PRs:
- `cargo fmt --all --check` — formatting
- `cargo clippy --all-targets -- -D warnings` — lints
- `cargo test` — all tests

### `release.yml`

A single workflow triggered when a PR with a `cargo:patch`, `cargo:minor`, or
`cargo:major` label is merged. Runs four jobs:

1. **bump-and-tag** — reads the current version, bumps it based on the label,
   commits to `main`, pushes, and creates an annotated `v*` tag
2. **create-release** — creates the GitHub release with auto-generated notes
3. **build** — builds `browsertools` for seven targets (Linux gnu/musl x86_64 +
   aarch64, macOS x86_64 + aarch64, Windows x86_64) and attaches each archive
   plus a per-asset `.sha256`
4. **checksums** — aggregates a combined `SHA256SUMS` manifest for downloaders

## PR-based release flow

The recommended release process uses the `/pr` command (see `.agents/commands/pr.md`):

1. **Agent runs `/pr patch`** (or `minor`/`major`) → Creates PR with `cargo:<bump>` label
2. **PR gets merged** → Triggers `release.yml`
3. **Release workflow** → Bumps version, tags, builds cross-platform binaries,
   and uploads checksums — all in one workflow

This ensures version bumps are reviewable and tied to specific changes.