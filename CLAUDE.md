# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

Rawrr is a Rust daemon that watches Docker containers for image updates and applies them based on per-container label policies. It compares registry digests rather than tags, and enforces a configurable release-age delay before upgrading to avoid chasing unstable releases.

## Commands

```bash
cargo build                # debug build
cargo build --release      # release build
cargo run                  # run (reads .env if present)
cargo test                 # all tests
cargo test test_name       # single test by name (substring match)
cargo clippy               # lint
cargo fmt                  # format
```

Logging is controlled via `RUST_LOG`. The default directive is `rawrr=debug`. To change verbosity:

```bash
RUST_LOG=rawrr=info cargo run
```

Copy `.env.example` to `.env` for local config — the app calls `dotenv::dotenv().ok()` at startup.

## Architecture

The main `Rawrr` struct in `src/main.rs` owns all subsystems and drives the poll loop. Each poll cycle:

1. Checks rate limiter (`src/rate_limiter.rs`) — sliding window, tracks timestamps in a `VecDeque`
2. Lists containers via `DockerClient` (`src/docker.rs`)
3. For each container, fetches the registry digest (Docker Hub, GHCR, or generic OCI)
4. Compares against persisted state (`src/state.rs`) — if the digest is new, `first_seen` is set to now
5. Calls `state.should_upgrade()` — returns true only after `release_delay` has elapsed since `first_seen`
6. If upgrading, sends a notification via `Notifier` (`src/notifier.rs`) and (TODO) restarts the container

**Config** (`src/config.rs`) is entirely environment-driven — see `.env.example` for all variables. `NotifierConfig` is an enum with `Gotify`, `Ntfy`, and `None` variants.

## Known Stubs / TODOs

- `DockerClient::list_containers()` always returns an empty `Vec` — Unix socket communication is not yet implemented. The real implementation needs either the `docker-rs` crate or manual HTTP-over-Unix-socket handling.
- `DockerClient::inspect_container()` returns `Err("Not yet implemented")`.
- The actual container upgrade (pull + stop + restart) in `handle_upgrade()` is a TODO comment block.

These are the primary areas where the application is not yet functional end-to-end.
