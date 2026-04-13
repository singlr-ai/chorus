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
  - Runs on pushes to `main`
  - Runs manually through `workflow_dispatch`
  - Runs for pull requests labeled `build-artifacts`
  - Uploads a Linux bundle and an Apple Silicon macOS app bundle

## Testing on a MacBook Pro

1. Push your branch to `origin`
2. Open the `Chorus Artifacts` workflow in GitHub Actions
3. Run it against the branch you want to test, or label the PR with `build-artifacts`
4. Download the `chorus-macos-aarch64-app` artifact
5. Unzip it on the MacBook Pro
6. Launch `Zed.app`

## Current Limits

- The macOS artifact is an unsigned debug app bundle
- Gatekeeper may require removing the quarantine attribute before launch
- The app bundle is still named `Zed.app` until the Chorus branding spec lands
