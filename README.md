# ArchBridge

ArchBridge finds and builds the safest available installation path for software on Arch Linux. It prefers official repositories and AUR, supports verified upstream source discovery, and inspects foreign packages (.deb / .rpm) without executing untrusted scripts.

ArchBridge is written entirely in **100% native Rust**, featuring a high-performance GPU-accelerated desktop user interface built on **GPUI** (the UI framework that powers Zed Editor), unified with a fast, deterministic CLI and JSON-RPC core engine.

---

## Key Highlights

- **100% Native Rust & GPUI:** Zero Python runtime or PyQt6 dependencies. Pure GPU-rendered desktop interface with sub-millisecond tab switching and minimal memory footprint.
- **Source Priority Engine:** Transparently checks Official Arch Linux repositories, AUR, Flathub Flatpaks, AppImages, and verified upstream Git releases.
- **Static Inspection of Foreign Packages:** Safely inspects Debian (`.deb`) and RedHat (`.rpm`) packages, extracting metadata, payload trees, control fields, and dependencies without executing untrusted maintainer scripts (`preinst`, `postinst`, `%pre`, `%post`).
- **Clean Chroot & Isolated Building:** Builds packages using clean Arch chroot tooling (`devtools` / `mkarchroot` / `systemd-nspawn`) with network isolation and dropped privileges.
- **Comprehensive Doctor Diagnostics:** Offline-resilient health probes check system prerequisites, pacman keys, chroot environments, and repository accessibility.
- **Adaptive Multi-Theme Engine:** Includes Dark (Arch Navy), Midnight OLED (true-black contrast), and Modern Light visual themes.

---

## Quick Start

### 1. Build from Source

```sh
# Clone the repository
git clone https://github.com/Zul-Qarnain/archbridge.git
cd archbridge

# Build release binary
cargo build --release
```

### 2. Launch Native GPU Desktop Interface

```sh
# Run the GPUI desktop interface directly
./target/release/archbridge gui

# Or via cargo in development
cargo run -- gui
```

### 3. Run Doctor Diagnostics

```sh
./target/release/archbridge doctor
```

### 4. CLI Usage

```sh
# Search for packages across official and configured sources
./target/release/archbridge search vlc

# Inspect a foreign .deb package safely
./target/release/archbridge inspect ./package.deb

# Prepare build plan for an upstream source (dry run)
./target/release/archbridge build ./source --entry my-tool --dry-run

# Plan installation
./target/release/archbridge install my-tool --dry-run
```

---

## Prerequisites

ArchBridge requires standard Arch Linux development and system administration tools:

- **Rust & Cargo:** 1.80+ (for building)
- **Arch Packaging Tools:** `pacman`, `base-devel`, `devtools`
- **System Isolation:** `systemd-nspawn`
- **Privilege Management:** `sudo` (for authorized pacman operations)
- **Utilities:** `curl`, `bsdtar`, `readelf`
- **Optional Foreign Tools:** `dpkg-deb` (for advanced deb extraction) or `rpm` (for rpm metadata extraction)

*Note: ArchBridge does not install prerequisites automatically. Run as a normal user. `--dry-run` always wins over `--yes`.*

---

## Project Guide

- [Architecture & Implementation](PROJECT.md)
- [Development Workflow](docs/DEVELOPMENT.md)
- [IPC & JSON-RPC Contract](docs/IPC.md)
- [Security Model](docs/SECURITY.md)
- [Validation & Acceptance](docs/VALIDATION.md)
- [Architecture Decisions (ADR)](docs/DECISIONS.md)
- [Release Process](docs/RELEASING.md)
- [Contributing](CONTRIBUTING.md)

---

## License

ArchBridge is released under the [MIT License](LICENSE).
