# ArchBridge

ArchBridge finds and builds the safest available installation path for software on
Arch Linux. It prefers official repositories and AUR, supports verified upstream
source discovery, and inspects foreign packages without executing their scripts.

Status: early open-source development. Review the security model and run it in a
disposable Arch VM before using it with untrusted sources.

## Quick start

```sh
cargo build
./target/debug/archbridge doctor
./target/debug/archbridge gui
```

The desktop UI is Python/PyQt6. The packaging, discovery, planning, and safety
engine is Rust. The CLI and UI use the same JSON-RPC core.

CLI examples:

```sh
./target/debug/archbridge search bash
./target/debug/archbridge inspect ./package.deb
./target/debug/archbridge build ./source --entry my-tool --dry-run
./target/debug/archbridge install my-tool --dry-run
```

## Prerequisites

Build and runtime workflows need Rust/Cargo, Python 3 with PyQt6, `pacman`,
`base-devel`, `devtools`, `systemd-nspawn`, `sudo`, `curl`, `bsdtar`, and
`readelf`. Optional foreign-format inspection needs `dpkg-deb` or `rpm`.

ArchBridge does not install prerequisites automatically. Run it as a normal user;
review every plan before granting consent. `--dry-run` always wins over `--yes`.

## Project guide

- [Architecture and implementation](PROJECT.md)
- [Development workflow](docs/DEVELOPMENT.md)
- [IPC contract](docs/IPC.md)
- [Security model](docs/SECURITY.md)
- [Validation and acceptance](docs/VALIDATION.md)
- [Release process](docs/RELEASING.md)
- [Contributing](CONTRIBUTING.md)

## License

ArchBridge is released under the [MIT License](LICENSE). Contributions are
welcome under the same license.
