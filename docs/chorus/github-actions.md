# Chorus GitHub Actions

## Remote Layout

- `origin`: `https://github.com/singlr-ai/chorus.git`
- `upstream`: `https://github.com/zed-industries/zed.git`

## Public Workflows

- `Chorus CI`
  - Runs on pull requests, pushes to `main`, and manual dispatch
  - Verifies Linux formatting and workflow definitions
  - Checks that Chorus builds on Linux and macOS
  - Runs `cargo test -p sing_bridge`

- `Chorus Artifacts`
  - Runs manually through `workflow_dispatch`
  - Manual dispatch can build `all`, `macos`, or `linux`
  - Runs for pull requests labeled `build-artifacts`
  - Uploads only the Chorus app bundles needed for local testing

## Disabled Upstream Workflows

- Chorus keeps only the workflows that apply to the public fork today.
- Upstream Zed workflows for release automation, documentation suggestions, reviewer assignment, community bots, and private infrastructure are intentionally removed from `.github/workflows`.
- If Chorus later needs one of those capabilities, add back a Chorus-owned workflow instead of re-enabling the upstream file unchanged.

## Testing on a MacBook Pro

1. Push your branch to `origin`
2. Open the `Chorus Artifacts` workflow in GitHub Actions
3. Run it against the branch you want to test with `platform=macos`, or label the PR with `build-artifacts`
4. Download the `chorus-macos-aarch64-app` artifact
5. Unzip it on the MacBook Pro
6. Launch `Chorus Dev.app`

## Current Limits

- The macOS artifact is an unsigned release app bundle
- Gatekeeper may require removing the quarantine attribute before launch
- The artifact is intended for local testing, not distribution
