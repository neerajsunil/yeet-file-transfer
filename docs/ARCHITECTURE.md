# Architecture

yeet aims to be the fastest way to copy big folders between computers on the same network: game installs, repos, photo libraries. It targets 1 Gbps Ethernet and Wi-Fi, where the network is the bottleneck. On those links, sending fewer bytes beats sending them faster.

## Crates

| crate | what it is |
|---|---|
| `crates/yeet-core` | The engine: protocol, sending, receiving, resume, discovery. No UI and no global state. |
| `crates/yeet-app` | The desktop app (Slint). Binary: `yeet`. |
| `crates/yeet-cli` | Command-line send/receive, plus developer tools (`verify`, `proxy`). Binary: `yeet-cli`. |
| `tools/bench` | Measures which compression strategy wins for a real folder at a given link speed. Not built by default. |

The app and CLI are thin layers over the engine. If a feature isn't about UI, it belongs in `yeet-core`, where it can be tested.

## Sending (`yeet_core::send`)

```
scan ──▶ plan ──▶ packers (N threads) ──▶ writer ──▶ socket
                                                 ◀── ACK reader
```

- **scan** (`send/scan.rs`) walks the sources into a list of entries in a stable order, so file ids stay the same across reconnects.
- **plan** (`send/plan.rs`) emits the DIR and FILE frames, and decides what goes into each chunk. Large files are split into 4 MiB blocks. Small files are bundled into shared chunks, because per-file overhead is what makes many-small-file copies slow.
- **packers** (`send/pack.rs`) read the data and may compress it with zstd level 3. Compression is adaptive per file type (by extension): `ClassMemory` stops trying a type after a few chunks that don't shrink by 5%, and re-checks it now and then. Already-compressed game archives therefore cost almost nothing, and compressible ones go out smaller.
- **writer** (`send/conn.rs`) writes frames in order, rate-limited when the CLI asks for `--limit`.
- `send()` (`send/mod.rs`) wraps all of this in a reconnect loop. Every attempt re-resolves the peer's address, and the receiver's HAVE list decides what still needs sending.

## Receiving (`yeet_core::recv`)

- `serve()` accepts connections. Each connection reads the OFFER, then either reattaches to an existing session (a reconnect) or asks the `Handler` (the UI) whether to accept.
- **Session** (`recv/session.rs`) is a transfer that outlives its connections. It keeps open files and progress for 10 minutes after a drop.
- **pipeline** (`recv/pipeline.rs`) has one reader that decodes frames, several writer threads, and an ACK thread. Each file is assigned to one writer, so a file is always written in order while different files are written in parallel.
- **paths** (`recv/paths.rs`) makes sure nothing escapes the destination folder. It also renames clashing top-level names, so an existing `Game` makes the new copy arrive as `Game (2)` instead of being overwritten.
- **files** (`recv/files.rs`) tracks how much of each file has been written. That data becomes the HAVE list on resume, and on cancel it says which partial files to remove.

## Errors

Everything in the engine returns `yeet_core::Result<T>`. The `Error` enum separates the outcomes people care about (declined, cancelled, lost contact, unsafe input) from plain I/O errors. The app turns each variant into a sentence in `crates/yeet-app/src/text.rs`. Never match on error message strings.

## The app (`crates/yeet-app`)

```
background threads ──change──▶ State (Mutex) ◀──read── view::refresh (UI thread, every 150 ms)
UI callbacks       ──call────▶ actions
```

- `state.rs` holds everything the app knows. Background threads (discovery, receiving, sending) change only this.
- `view.rs` copies the state into the window on a timer. It updates only the rows that changed, so hover states and animations survive.
- `actions.rs` holds what each button does. `callbacks.rs` connects them to the window.
- `receiver.rs` is the engine's `Handler`: it shows incoming offers and records results.
- `platform.rs` holds OS-specific code: the Defender check and exclusion, keeping the system awake, and revealing files in the file manager.
- `ui/` holds the Slint UI: `app.slint` (the window), `components/`, `views/` (sections of the main list), and `sheets/` (dialogs). Colors, spacing, type and timing come from the [Albedo](https://github.com/neerajsunil/albedo-theme) design system, vendored unchanged as `ui/albedo.slint`.

Run `YEET_SCREENSHOT=<dir> yeet` to render every screen to PNGs without opening a window. This is useful for reviewing UI changes.

## Choices worth knowing

- **TCP, one connection.** On LAN links with a 4 MiB socket buffer, a single TCP stream fills 1 Gbps. Multiple streams add ordering problems without adding speed.
- **zstd level 3, adaptive.** At 1 Gbps a single core runs zstd-3 far faster than the wire. Higher levels cost CPU for little extra saving.
- **No hashing during transfer.** TCP already checksums every packet, and hashing would cost CPU on slow machines. `yeet-cli verify` compares two trees when you want proof.
- **Software renderer.** The app uses Slint's software renderer. It keeps the binary small and has no GPU driver issues, and the UI is simple enough not to need a GPU.
