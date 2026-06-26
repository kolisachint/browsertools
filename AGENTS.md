# Development Rules

## Repo Map

`browsertools` is a single Rust binary crate for browser automation and
scraping. It uses Chrome DevTools Protocol (CDP) via `chromiumoxide`.

Docs (keep these current when behavior changes):

- `README.md` — minimal entry point
- `DESIGN.md` — architecture and design decisions
- `ci/README.md` — CI/release workflow reference

Code:

- `src/main.rs` — CLI entry point with subcommands
- `src/driver.rs` — Chrome DevTools Protocol driver
- `src/contract.rs` — Command/response contract types
- `src/flow.rs` — Workflow orchestration
- `src/discover.rs` — Page discovery and navigation
- `src/observe.rs` — Observation and monitoring
- `src/replay.rs` — Session replay functionality
- `src/serve.rs` — HTTP/WebSocket server for live view
- `src/liveview.rs` — Live view implementation
- `tests/` — Integration tests
- `.github/workflows/` — `ci.yml`, `release.yml`
- `.agents/commands/` — slash-command definitions (`pr.md`)

## Conversational Style

- Keep answers short and concise
- No emojis in commits, issues, PR comments, or code
- No fluff or cheerful filler text
- Technical prose only, be kind but direct

## Code Quality

- Read files in full before making wide-ranging changes, before editing files
  you have not already fully inspected, and when asked to investigate or audit.
  Do not rely only on search snippets for broad changes.
- Match the surrounding style: import order, naming, error handling (`anyhow`
  for error propagation)
- Avoid `unwrap()`/`expect()` outside tests; thread errors with `?`
- Do not preserve backward compatibility unless the user explicitly asks
- Always ask before removing functionality that appears intentional

## Commands

- After code changes (not doc-only changes), run all three and fix everything
  before committing:
  ```bash
  cargo fmt --all --check
  cargo clippy --all-targets -- -D warnings
  cargo test
  ```
- If you create or modify a test, run it and iterate until it passes
- NEVER commit unless the user asks

## Slash Commands

- `/pr [patch|minor|major]` — opens a release PR on a feature branch and labels
  it `cargo:<bump>` so `release.yml` bumps the version, publishes, and builds on merge.
  Defined in `.agents/commands/pr.md`. Defaults to `patch`.
- Slash-command definitions live in `.agents/commands/`.

## Releasing

**Version semantics**:

- `patch` — bug fixes and additions
- `minor` — API changes
- `major` — large breaking changes

### Flow (do NOT bump versions or tag by hand)

**Never edit `version = "…"` in `Cargo.toml` inside a feature PR.** The release
workflow is the sole owner of the version: it computes the next version from the
latest `v*` git tag plus the PR's `cargo:<bump>` label, then rewrites the
manifest. A manual bump is at best ignored and at worst confusing. Leave
versions untouched and just apply the label.

1. `/pr <bump>` opens a PR labeled `cargo:<bump>`.
2. On merge, `release.yml` derives the next version from the latest `v*` tag,
   bumps the manifest, updates `Cargo.lock`, commits `release: v<version>`,
   tags `v<version>`, and pushes `main`.
3. The tag triggers `release.yml`, which creates the GitHub release and builds
   + attaches Linux (`x86_64-unknown-linux-gnu`) and macOS
   (`aarch64-apple-darwin`) binaries to the GitHub release.

Manual fallback (only if asked): `git tag vX.Y.Z && git push origin vX.Y.Z`.

## **CRITICAL** Git Rules for Parallel Agents **CRITICAL**

Multiple agents may work on different files in the same worktree simultaneously.

### Committing

- ONLY commit files YOU changed in THIS session
- Include `fixes #<number>` / `closes #<number>` when there is a related issue/PR
- NEVER use `git add -A` or `git add .` — these sweep up other agents' changes
- ALWAYS `git add <specific-file-paths>` listing only files you modified
- Run `git status` before committing and verify you are staging only YOUR files

### Forbidden Git Operations

These can destroy other agents' work and are never allowed:

- `git reset --hard`
- `git checkout .`
- `git clean -fd`
- `git stash`
- `git add -A` / `git add .`
- `git commit --no-verify`

### Safe Workflow

```bash
git status                      # 1. check first
git add src/driver.rs           # 2. stage only your files
git commit -m "fix(driver): ..." # 3. commit
git pull --rebase && git push   # 4. push (never reset/checkout)
```

### If Rebase Conflicts Occur

- Resolve conflicts in YOUR files only
- If a conflict is in a file you did not modify, abort and ask the user
- NEVER force push over shared history

### User Override

If the user's instructions conflict with these rules, ask for confirmation that
they want to override. Only then proceed.