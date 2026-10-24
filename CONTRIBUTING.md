# Contributing to open-u60-pro

Thanks for your interest in contributing! This guide will help you get started.

## Prerequisites

- **Rust toolchain** (stable, latest) — install via [rustup](https://rustup.rs/)
- **Cross-compilation target**: `aarch64-unknown-linux-musl`
- **ADB** (Android Debug Bridge) for deploying to the device
- **A ZTE U60 Pro** (MU5250) with ADB access enabled

## Architecture

| Component | Path | Description |
|---|---|---|
| zte-agent | `zte-agent/` | Rust HTTP agent (Axum) running on the device: REST API, live updates (`/api/events`) and a browser UI on port 9090 |
| iOS app | `mobile/ios/` | SwiftUI companion app |
| Android app | `mobile/android/` | Jetpack Compose companion app |

## Development Setup

```bash
# Clone the repo
git clone https://github.com/jesther-ai/open-u60-pro.git
cd open-u60-pro

# Build the agent
cd zte-agent
cargo build --release --target aarch64-unknown-linux-musl

# Deploy to device via ADB
adb push target/aarch64-unknown-linux-musl/release/zte-agent /data/local/tmp/
adb shell chmod +x /data/local/tmp/zte-agent
```

## Code Style

- Run `cargo fmt` before committing
- Run `cargo clippy` and fix all warnings
- Follow standard Rust naming conventions

## Submitting Issues

- Use the provided issue templates (bug report or feature request)
- Include device firmware version and zte-agent version when reporting bugs
- Attach relevant logs from the agent or companion app

## Submitting Pull Requests

1. Fork the repository and create a feature branch from `main`
2. Make your changes with clear, focused commits
3. Ensure `cargo fmt` and `cargo clippy` pass
4. Test on a real device if possible
5. Open a PR using the pull request template
6. Describe what changed and how you tested it

## Code of Conduct

This project follows the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md). Please read it before participating.
