# Contributing to yeet

Thanks for helping! Bug reports, benchmarks from real networks, and pull requests are all welcome.

## Getting started

```bash
cargo build                 # app + CLI (debug)
cargo test --workspace      # unit and integration tests
cargo run -p yeet-app    # open the app
```

[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) explains where things live. Most changes touch only one of the following:

- **Engine behavior** (speed, protocol, resume): `crates/yeet-core`. Add a test in `tests/transfer.rs` or next to the code.
- **What the app does**: `crates/yeet-app/src/actions.rs`, `receiver.rs`, `view.rs`.
- **How the app looks**: `crates/yeet-app/ui/`. The UI follows [Albedo](https://github.com/neerajsunil/albedo-theme): use `Albedo.*` tokens instead of raw colors and sizes, keep it flat (1px `line` borders, no shadows), and use one primary button per screen. Don't edit `ui/albedo.slint` here. Change it upstream and copy the new `dist/albedo.slint` in.

## Before opening a pull request

CI runs these on Windows, macOS and Linux:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

For UI changes, include before and after screenshots. To render every screen:

```bash
YEET_SCREENSHOT=shots cargo run -p yeet-app
```

For changes that could affect speed, include numbers. Run a loopback transfer before and after, rate-limited to a realistic link:

```bash
cargo build --release -p yeet-cli
scripts/run.sh /tmp/dest 1g --verify <some big folder>
```

## Testing two app instances on one machine

Set `YEET_PROFILE` so that each instance gets its own settings and port:

```bash
YEET_PROFILE=b cargo run -p yeet-app
```

There is also a headless end-to-end check. Build with `--features e2e`, start one instance with `YEET_AUTO_ACCEPT=1`, then run another with `YEET_TEST_SEND=<path>`. The second one sends the path to the first device it finds and exits with 0 on success.

## Code style

- Keep the engine free of UI concerns, and keep the app free of protocol details.
- Return `yeet_core::Error` variants for outcomes callers act on. Never match on error message text.
- Name constants instead of using bare numbers for timeouts and sizes, and say why in a comment.
- `unsafe` is denied workspace-wide. If you need FFI, allow it on the smallest scope and add a `// SAFETY:` comment.
- Changing the wire format means bumping `MAGIC` and updating `docs/PROTOCOL.md`.

## License

By contributing, you agree that your contributions will be dual licensed under MIT OR Apache-2.0, as described in the README.
