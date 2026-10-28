# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- First version of the desktop app, the `yeet-cli` command-line tool, and the `yeet-core` engine.
- Automatic device discovery (mDNS), plus connecting by IP address.
- Adaptive per-file-type zstd compression, and bundling of small files.
- Reconnecting and resuming after dropped connections, including address changes.
- Pacing for the drives on both ends: spinning disks get fewer, in-order reads and writes; FAT drives skip space reservation and refuse 4 GB+ files up front. The receiver reports its drive in ACCEPT (protocol `YET1`).
- Head start while choosing a device: picked items are scanned and partly read ahead, and the save folder's drive is probed at startup.
- Automatic compression turns itself off when sending to the same computer.
- A Windows Security banner that offers to exclude the save folder from scanning.
- The Albedo design system for the interface: flat surfaces and a star accent, following the system light or dark mode.
