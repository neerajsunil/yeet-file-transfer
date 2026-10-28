# yeet

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust 1.92+](https://img.shields.io/badge/rust-1.92%2B-orange.svg)](https://rustup.rs)
![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey.svg)
[![Release](https://img.shields.io/github/v/release/neerajsunil/yeet-file-transfer)](https://github.com/neerajsunil/yeet-file-transfer/releases/latest)
[![CI](https://github.com/neerajsunil/yeet-file-transfer/actions/workflows/ci.yml/badge.svg)](https://github.com/neerajsunil/yeet-file-transfer/actions/workflows/ci.yml)

yeet copies files and folders between computers on the same local network. It is designed for large transfers, such as game installs, source trees and photo libraries, over 1 Gbps Ethernet and Wi-Fi, where the network is usually the bottleneck.

It ships as a desktop app (`yeet`) and a command-line tool (`yeet-cli`), both built on the same engine (`yeet-core`).

## Contents

- [Features](#features)
- [Benchmarks](#benchmarks)
- [Installation](#installation)
- [Usage](#usage)
- [Command line](#command-line)
- [Windows Security](#windows-security)
- [How it works](#how-it-works)
- [Project status](#project-status)
- [Contributing](#contributing)
- [License](#license)

## Features

| | |
|---|---|
| **Adaptive compression** | Each file type is tested as it is sent. Types that compress are sent with zstd; types that don't (video, most game archives) are sent as-is, so there is no CPU cost for data that won't shrink. |
| **Small-file bundling** | Many small files are packed into shared 4 MiB chunks, removing the per-file overhead that makes folders like `node_modules` or a Cargo registry slow to copy. |
| **Resume after drops** | If Wi-Fi drops or a cable is pulled, the transfer reconnects and continues from where it stopped, including when the other computer's IP address has changed. The receiver keeps partial progress for 10 minutes. |
| **Discovery** | Computers on the network find each other over mDNS. If discovery is blocked, connect by IP address instead. |
| **Safe writes** | Existing files are never overwritten (`Game` arrives as `Game (2)`), and no path can be written outside the destination folder. |
| **Drive-aware** | Spinning disks get fewer, sequential reads and writes. FAT drives skip space reservation and reject files over 4 GB before the transfer starts. |
| **Cross-platform** | Windows, macOS and Linux. |

## Benchmarks

### Real-world transfers

Two Windows PCs connected over 1 Gbps Ethernet. Each folder was copied from one PC to the other with the standard Windows tools over SMB, then with yeet. Sources were read beforehand, so every method started with a warm cache on the sending side.

| Game | Size | Files | Copy-Item (Explorer-style) | robocopy | robocopy /MT:16 | yeet | vs. fastest SMB |
|---|---:|---:|---:|---:|---:|---:|---:|
| Kerbal Space Program | 5.0 GB | 3,371 | 64.9 s | 76.8 s | 48.7 s | **21.1 s** | 2.3× faster |
| Rust toolchain (1.99.1) | 1.4 GB | 66,077 | 374.4 s | 507.0 s | – | **26.4 s** | 14× faster |

SMB copies every byte and pays a round trip for each file. yeet sent 44% of the bytes for Kerbal Space Program (2.20 of 4.99 GB) and 18% for the Rust toolchain (0.25 of 1.39 GB). On the toolchain's 66,077 small files, the standard Explorer-style copy took over six minutes; yeet finished in under half a minute, 14× faster.

> **These numbers are the best case for SMB.** Wired gigabit Ethernet has high bandwidth and sub-millisecond latency, which is where SMB performs best. On Wi-Fi and slower links, yeet's lead grows: every byte that compression removes saves more time when bandwidth is scarce, and SMB's per-file round trips cost more as latency rises. In the loopback tests below, the same small-file folder goes from 1.5× faster at 1 Gbps to 5.2× faster at 100 Mbps.

<details>
<summary>Test setup</summary>

| | Sender | Receiver |
|---|---|---|
| CPU | AMD Ryzen 7 7700 | Intel Core 5 210H |
| Memory | 32 GB | 16 GB |
| Storage | NVMe SSD | NVMe SSD |
| Network | Realtek PCIe GbE, 1 Gbps | Realtek PCIe GbE, 1 Gbps |
| OS | Windows 11 | Windows 11 |

Real-time Defender scanning was off on the receiver, so neither SMB nor yeet paid a scanning cost. Reproduce with [`scripts/compare.ps1`](scripts/compare.ps1). Raw results: [`tools/bench/results/compare.csv`](tools/bench/results/compare.csv).

</details>

### Loopback transfers

Loopback transfers on NVMe storage, with `yeet-cli --limit` capping the link speed. "Compression off" is the same build with `--compress none`, which equals the time a plain copy needs at that link speed.

| Dataset | Link | Compression off | yeet (auto) | Bytes on the wire | Speedup |
|---|---|---:|---:|---:|---:|
| Many small files (Rust toolchains) | 100 Mbps | 119.5 s | **23.0 s** | 18% | 5.2× |
| | 500 Mbps | 23.9 s | **10.1 s** | 18% | 2.4× |
| | 1 Gbps | 12.9 s | **8.4 s** | 18% | 1.5× |
| Compressible game data | 100 Mbps | 245.0 s | **121.3 s** | 50% | 2.0× |
| | 500 Mbps | 49.0 s | **24.3 s** | 50% | 2.0× |
| | 1 Gbps | 24.5 s | **12.1 s** | 50% | 2.0× |
| Already-compressed game data | 100 Mbps | 277.1 s | **276.1 s** | 100% | 1.0× |
| | 1 Gbps | 27.7 s | **27.6 s** | 100% | 1.0× |

On already-compressed data, yeet detects that compression doesn't help and sends the data unchanged, matching the uncompressed time. Raw results: [`tools/bench/results/transfer-nvme.txt`](tools/bench/results/transfer-nvme.txt).

### Running the benchmarks

To compare yeet with SMB between two Windows PCs (the receiver needs an SMB share, SSH access, and `yeet-cli receive` running):

```powershell
scripts\compare.ps1 -Share \\pc\bench -RemoteDir C:\bench -Ssh pc -To 192.168.1.20:7878 -Sources 'D:\Games\SomeGame'
```

To estimate savings for a folder without a second PC:

```bash
cargo run --release -p yeet-bench -- scan <folder>
```

To measure real transfers over loopback at a given link speed:

```bash
cargo build --release -p yeet-cli
scripts/run.sh /tmp/dest 1g --verify <folder>
```

`scripts/matrix.sh` runs the full dataset × link × compression matrix. Results from real networks are welcome; see [Contributing](#contributing).

## Installation

Download the archive for your platform from the [latest release](https://github.com/neerajsunil/yeet-file-transfer/releases/latest):

| Platform | Download |
|---|---|
| Windows 10/11 (x64) | `yeet-<version>-windows-x64.zip` |
| Linux (x64, glibc 2.35+) | `yeet-<version>-linux-x64.tar.gz` |
| macOS (Apple silicon) | `yeet-<version>-macos-arm64.tar.gz` |

Each archive contains the desktop app (`yeet`) and the command-line tool (`yeet-cli`). There is no installer: extract the archive and run `yeet`. Checksums are in `SHA256SUMS.txt` on the release page.

The binaries are not code-signed yet:

- **Windows:** SmartScreen may show "Windows protected your PC". Choose **More info**, then **Run anyway**.
- **macOS:** remove the download quarantine before the first run:

  ```bash
  xattr -d com.apple.quarantine yeet yeet-cli
  ```

### Building from source

Install [Rust](https://rustup.rs) 1.92 or newer, then run:

```bash
cargo build --release
```

This produces `target/release/yeet` (desktop app) and `target/release/yeet-cli` (command line).

On Linux, building needs the fontconfig and xkbcommon development headers. On Debian or Ubuntu:

```bash
sudo apt install libfontconfig1-dev libxkbcommon-dev
```

File dialogs use `xdg-desktop-portal`, which most desktop environments already include.

## Usage

1. Open yeet on both computers.
2. On the sending computer, add the files or folders to send.
3. Pick the receiving computer from the list and press **Send**.
4. Accept the transfer on the receiving computer.

Received files are saved to `Downloads/yeet`. You can change this in Settings.

If the other computer doesn't appear (some networks block discovery), choose **Connect by IP address** and enter the address shown in the other computer's Settings.

## Command line

Receive into a folder:

```bash
yeet-cli receive ~/Downloads/incoming
```

Send a folder to another computer:

```bash
yeet-cli send ./MyGame --to 192.168.1.20:7878
```

| Option | Applies to | Description |
|---|---|---|
| `--port <n>` | `receive` | Port to listen on. Default `7878`. |
| `--once` | `receive` | Exit after one transfer. |
| `--to <host:port>` | `send` | Address of the receiving computer. |
| `--limit <rate>` | `send` | Cap the send rate, for example `100m`, `500m` or `1g`. |
| `--compress <mode>` | `send` | `auto` (default), `none`, or a fixed level such as `zstd3`. |
| `--threads <n>` | both | Number of worker threads. |
| `--no-local` | `send` | Keep compression on when sending to the same computer. |

`yeet-cli verify <a> <b>` compares two folders by size and hash. Run `yeet-cli` with no arguments for the full list.

## Windows Security

Microsoft Defender scans each file as it is written. For folders with many small files, scanning can take longer than the transfer itself. When Defender is on, yeet shows a banner with an option to exclude only the save folder from scanning. Adding the exclusion requires administrator approval.

## How it works

```
sender:    scan ──▶ plan ──▶ packers (N threads) ──▶ writer ──▶ TCP
receiver:  TCP ──▶ reader ──▶ writers (N threads) ──▶ disk
                                         └──▶ ACKs back to sender
```

- A single TCP connection with a 4 MiB socket buffer, which fills a 1 Gbps link without the ordering problems of multiple streams.
- Large files are split into 4 MiB blocks; small files are bundled into shared blocks.
- zstd level 3 per block, switched off per file type when it saves less than 5%.
- On reconnect, the receiver sends a list of what it already has, and the sender skips it.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design and [docs/PROTOCOL.md](docs/PROTOCOL.md) for the wire format.

### Repository layout

| Path | Contents |
|---|---|
| `crates/yeet-core` | Transfer engine: protocol, send, receive, resume, discovery |
| `crates/yeet-app` | Desktop app (Slint) |
| `crates/yeet-cli` | Command-line tool and developer utilities |
| `tools/bench` | Compression and link-speed benchmark |
| `scripts/` | Loopback transfer, drop-recovery and matrix test scripts |

## Project status

yeet is in early development (0.x). The wire protocol (`YET1`) may change between versions, so both computers should run the same build. See [CHANGELOG.md](CHANGELOG.md) for what has changed.

yeet has no encryption or authentication yet and is intended for trusted local networks. Every incoming transfer must be accepted on the receiving computer.

## Contributing

Bug reports, benchmark results from real networks and pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for build, test and style guidelines.

## Credits

The interface uses the [Albedo](https://github.com/neerajsunil/albedo-theme) design system (MIT, see `crates/yeet-app/ui/albedo.LICENSE`).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
