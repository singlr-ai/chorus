# Chorus Upstream Sync

## Current Baseline

- Sync branch: `feat/chorus-upstream-zed-sync`
- Upstream repository: `zed-industries/zed`
- Upstream revision: `832c17e819`
- Sync date: `2026-04-27`
- Validation command: `cargo check -p sing_bridge -p sing_project -p sing_spec`

## Sync Policy

- Merge upstream Zed into Chorus on a dedicated sync branch before building more agent, thread, worktree, or dispatch features.
- Prefer upstream behavior in Zed-owned crates when resolving conflicts.
- Reattach Chorus behavior through narrow registration points after upstream code is preserved.
- Keep conflict resolution mechanical for lockfiles, generated assets, and packaging files.

## Chorus-Owned Extension Crates

- `crates/sing_bridge`: boundary for `sing` CLI, SSH, validation, and typed command models.
- `crates/sing_project`: project lifecycle panel and remote-open coordination.
- `crates/sing_spec`: spec store, spec board, and spec file interactions.
- `crates/sing_dispatch`: future dispatch bridge from specs to agent sessions.
- `crates/sing_orchestrator`: future local orchestrator integration.

## Allowed Zed-Owned Touch Points

- `Cargo.toml` and `Cargo.lock` for workspace membership and dependency resolution.
- `crates/zed` for app startup, binary naming, and registration of Chorus panels.
- `crates/workspace` for small UI seams that cannot yet be expressed through panel registration alone.
- `assets/settings/default.json` for schema and defaults that cannot yet be layered elsewhere.
- Packaging scripts and resources only when required for Chorus builds or distribution.

## Conflict Hotspots

- Agent and thread crates change often upstream; avoid direct Chorus edits there.
- Worktree and project-panel internals change often upstream; prefer adapters over patches.
- `crates/zed/src/zed.rs` is currently the main registration seam for `sing_project` and `sing_spec`.
- Branding resources create broad noisy diffs; defer icon or resource changes unless they are required.
- Upstream GitHub workflows should stay removed unless Chorus intentionally adopts an equivalent workflow.

## Future Isolation Work

- Introduce a single Chorus integration module for startup, action registration, settings defaults, and panel registration.
- Move remaining Chorus welcome/onboarding branding into release-channel or product-identity APIs where possible.
- Keep new feature work in `sing_*` crates first, then add the smallest possible hook in Zed-owned code.
- Document every unavoidable fork patch with its owner and reason before adding more patches nearby.
