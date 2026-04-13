# Chorus Fork Bootstrap

## Upstream Baseline

- Upstream repository: `zed-industries/zed`
- Synced upstream revision: `cbd856ff3e`
- Sync date: `2026-04-13`
- Active Chorus bootstrap branch: `feat/chorus-fork-bootstrap`

## Supported Build Paths

### Linux

- Install system libraries with `script/linux`
- Verify the editor build with `cargo check -p zed`
- Install a local development build with `./script/install-linux`

### macOS

- Install Xcode and the Xcode command line tools
- Point the command line tools at the full Xcode install with `xcode-select`
- Install `cmake` with Homebrew
- Use `cargo run` for a debug build
- Use `cargo run --release` for a release build
- Use `cargo test --workspace` for the full test suite

## Verification

- Linux host: `Ubuntu 24.04.4 LTS`
- Rust toolchain: `rustup` minimal profile with `cargo 1.94.1` and `rustc 1.94.1`
- Verified command: `cargo check -p sing_bridge -p sing_dispatch -p sing_orchestrator -p sing_project -p sing_spec -p zed`
- Result: success on `2026-04-13`

## Reserved Fork Touch Points

- `Cargo.toml` for workspace members and shared crate paths
- `crates/zed` for app-level initialization and binary ownership
- `crates/workspace` for Chorus panel registration seams
- App metadata and packaging scripts when branding work starts

## Chorus-Owned Files In This Bootstrap

- `Cargo.toml`
- `crates/sing_bridge/**`
- `crates/sing_dispatch/**`
- `crates/sing_orchestrator/**`
- `crates/sing_project/**`
- `crates/sing_spec/**`
- `docs/chorus/fork-bootstrap.md`

## Planned Crate Ownership

- `sing_bridge`: SSH and `sing --json` integration boundary
- `sing_project`: project panel and remote-open coordination
- `sing_spec`: spec domain and board model
- `sing_dispatch`: dispatch actions and agent lifecycle tracking
- `sing_orchestrator`: orchestrator agent integration

## Guardrails

- Keep `default-members = ["crates/zed"]`
- Land Chorus feature work in `crates/sing_*` first
- Treat edits outside the reserved fork touch points as exceptions that need explicit justification
