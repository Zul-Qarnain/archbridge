# ArchBridge — Developer & Agent Guide

This document provides architectural context, commands, structural invariants, and development guidelines for AI agents and human contributors working in the ArchBridge codebase.

---

## 1. Project Overview & Core Philosophy

**ArchBridge** finds and builds the safest available installation path for software on Arch Linux.

### Key Tenets
1. **Safer Alternatives First**: Discovery prioritizes native sources before building from source or inspecting foreign packages.
   - Priority order: `Official Repositories` > `AUR` > `Flatpak` > `AppImage` > `Upstream Git/Source` > `DEB (Inspect only)` > `RPM (Inspect only)`.
2. **Foreign Packages are Data, Never Run**: `.deb` and `.rpm` packages are parsed strictly for metadata, file lists, dependencies, and systemd units. Maintainer scripts (`preinst`, `postinst`, triggers, scriptlets) are displayed for review but **never executed**.
3. **The Plan is the Sole Execution Authority**: Mutating actions must go through a staged, immutable, hashed `Plan` that is reviewed and confirmed before execution.
4. **Isolated Sandboxing**: Package building uses clean chroots (`mkarchroot` / `makechrootpkg`). Runtime smoke tests run inside unprivileged `systemd-nspawn` containers (`--private-users=pick`, `--private-network`, UID 65534).
5. **Single Native Binary**: Built in Rust 2021, providing the CLI, the RPC daemon (`archbridge serve`), and the native GPUI desktop application (`archbridge gui`) in a single executable.

---

## 2. Essential Commands

### Build & Check
```sh
# Build debug binary
cargo build

# Build optimized release binary
cargo build --release

# Format check (must pass in CI)
cargo fmt --check

# Format codebase
cargo fmt

# Type-check all targets (lib, bins, tests, benches)
cargo check --all-targets
```

### Testing
```sh
# Run all standard Rust unit & integration tests
cargo test

# Run supplementary source contract checks (enforces architectural invariants)
python3 -m unittest tests/source_contract.py

# Run foreign format inspection integration tests (includes ignored fixtures)
cargo test --test foreign_formats -- --nocapture

# Syntax check the acceptance test script
bash -n tests/acceptance.sh
```

### Running ArchBridge
```sh
# Launch native GPUI desktop application
cargo run -- gui
# or: target/debug/archbridge gui

# Search for package availability across all sources
cargo run -- search <package-name>
cargo run -- search <name> --repo <git-url>

# Inspect a foreign package (.deb or .rpm) in data-only mode
cargo run -- inspect <path/to/file.deb|file.rpm>

# Prepare a build plan with dry-run
cargo run -- build <url|directory|PKGBUILD> --name <name> --entry <bin> --dry-run

# Run system diagnostic health checks
cargo run -- doctor

# Start long-lived JSON-RPC 2.0 daemon
cargo run -- serve
```

### Full Acceptance Testing (Disposable Arch VM Only)
*Never run privileged build/install acceptance on development hosts; use a disposable VM.*
```sh
# Dry-run plans across candidate projects
ARCHBRIDGE_DISPOSABLE_ARCH_VM=yes bash tests/acceptance.sh

# Full build and smoke-test execution
ARCHBRIDGE_DISPOSABLE_ARCH_VM=yes ARCHBRIDGE_RUN_BUILDS=yes bash tests/acceptance.sh
```

---

## 3. Architecture & Code Organization

### Directory Layout
```text
archbridge/
├── src/
│   ├── lib.rs            # Library root exposing all core modules
│   ├── main.rs           # CLI entry point, arg parsing, DirectRpcClient
│   ├── engine.rs         # Execution engine, Plan lifecycle, pending plans store
│   ├── rpc.rs            # JSON-RPC 2.0 protocol layer (stdin/stdout newline-delimited)
│   ├── discovery.rs      # Multi-source candidate search & recommendation ranking
│   ├── inspect.rs        # Data-only inspection of .deb and .rpm archives
│   ├── archive.rs        # In-memory tarball snapshotting, SHA-256 hashing, path validation
│   ├── recipe.rs         # Build system auto-detection & PKGBUILD template generator
│   ├── build.rs          # Clean chroot builds (mkarchroot) & isolated runtime smoke tests
│   ├── doctor.rs         # System packaging prerequisite diagnostics & network probe
│   ├── process.rs        # Safe process execution wrapper (Step / ProcessOutput with timeouts)
│   ├── config.rs         # Persistent user preferences (~/.config/archbridge/config.json)
│   └── gui/              # Native GPUI desktop application
│       ├── mod.rs        # GUI runner (Application::new().run(...))
│       ├── app.rs        # Main GPUI application views and UI event handlers
│       ├── state.rs      # Reactive AppState, SharedEngine, async rpc_call
│       └── theme.rs      # Theme palettes (Dark, Light, Cyber, Nord, Synthwave)
├── tests/
│   ├── core.rs           # Core engine, config, and recipe unit tests
│   ├── cli.rs            # CLI argument parsing and DirectRpcClient tests
│   ├── doctor.rs         # Doctor probe evaluation & readiness tests
│   ├── foreign_formats.rs# Real DEB/RPM inspection & script non-execution tests
│   ├── source_contract.py# Python contract asserting structural security invariants
│   └── acceptance.sh     # Disposable Arch VM end-to-end acceptance harness
├── docs/                 # Architectural Decision Records, Security Model, IPC specs
└── Cargo.toml            # Package manifest & release profiles
```

### Control & Data Flow

```text
[CLI / Terminal]         [Native GPUI Desktop]
       │                          │
       │ (DirectRpcClient)        │ (SharedEngine / in-process RpcServer)
       ▼                          ▼
 ┌────────────────────────────────────────────────────────┐
 │                    RPC Protocol Layer                  │
 │                      (src/rpc.rs)                      │
 └───────────────────────────┬────────────────────────────┘
                             │
                             ▼
 ┌────────────────────────────────────────────────────────┐
 │                 ArchBridge Engine Core                 │
 │                     (src/engine.rs)                    │
 ├───────────────────────────┬────────────────────────────┤
 │ 1. Prepare Action         │ Generates immutable Plan   │
 │ 2. Review Plan            │ Files, hashes, steps, diff │
 │ 3. User Consent           │ Explicit user approval     │
 │ 4. Execute Plan           │ Single-use plan execution  │
 └───────┬───────────────────┴─────────────┬──────────────┘
         │                                 │
         ▼                                 ▼
┌──────────────────┐             ┌────────────────────────┐
│ Discovery / Repo │             │ Build & Test Sandbox   │
│ (discovery.rs)   │             │ (build.rs, process.rs) │
│ - Official pacman│             │ - makepkg / mkarchroot │
│ - AUR query      │             │ - systemd-nspawn smoke │
│ - Git / Upstream │             │ - UID 65534 isolation  │
└──────────────────┘             └────────────────────────┘
```

---

## 4. Key Invariants & Security Rules

Enforced continuously by unit tests and `tests/source_contract.py`:

1. **CLI is an RPC Client, Not a Packaging Engine**: `src/main.rs` communicates via `DirectRpcClient` calling methods on `RpcServer` (`v1.prepare`, `v1.execute`). Direct packaging tools (`makepkg`, `mkarchroot`) must never be called directly in `main.rs`.
2. **No Foreign Script Execution**: `src/inspect.rs` extracts metadata only. `maintainer_scripts` entries must have `executed: false`. Never invoke `sh`, `bash`, `ldd`, or `Command::new` to evaluate foreign packages or inspected ELFs.
3. **No Unsigned / Insecure Package Manager Bypasses**: Never use `--skipinteg`, `--skippgpcheck`, `--nodeps`, `--overwrite`, or `SigLevel = Never` anywhere in the codebase.
4. **Hard Sandbox Isolation for Smoke Tests**: `run_runtime_smoke_test` must use:
   - `--private-users=pick`
   - `--private-network`
   - `--user 65534` (nobody)
   - Max 30-second timeout
   - No writable host bind mounts (`--bind=`)
5. **Pinned Byte Verification**: Source snapshots and package files are hashed (SHA-256) at review time. Execution and inspection must verify and use the exact pinned bytes.
6. **Plan Authority & Lifecycle**:
   - Unguessable `plan_id` generated via SHA-256 digest of action + target + timestamp.
   - Plans expire after 10 minutes.
   - Plans are single-use; executing consumes the plan.
   - Modifying configuration invalidates pending plans.
   - Dry-runs stop before calling `v1.execute`.

---

## 5. Coding Patterns & Conventions

### Rust Patterns
- **Error Handling**: Library functions return `Result<T, String>`. Do not panic or `unwrap()` on untrusted input.
- **Process Execution**: Wrap all external command execution in `Step` (`src/process.rs`) with explicit argument vectors and mandatory timeouts. Never execute raw shell strings (`sh -c`).
- **Archive Handling**: Archive parsing is bounded in-memory (`src/archive.rs`). Paths with `..`, leading slashes, symlinks pointing outside the tree, or non-regular files are rejected.
- **Serialization**: Use `serde` derive macros (`Serialize`, `Deserialize`). RPC payloads use standard `serde_json::Value`.

### Doctor Readiness Semantics (`src/doctor.rs`)
- `ready: true` requires all essential local build tools: `pacman`, `base-devel`, `devtools` (`mkarchroot`, `makechrootpkg`), `compiler` (gcc), and `keyring`.
- `network` check statuses:
  - `pass`: HTTPS endpoint reachable with valid TLS certificate.
  - `unavailable`: Network offline or DNS resolution failed. **Does NOT mark system unready** (`ready: true`), because local and offline packaging/inspection can proceed.
  - `fail`: Endpoint responded with HTTP error or TLS certificate verification/handshake failed. Indicates a potential MITM or security misconfiguration and **marks system unready** (`ready: false`).

### Native GPUI Frontend (`src/gui/`)
- Uses GPUI (Zed's GPU-accelerated framework).
- **State Management**: `AppState` in `src/gui/state.rs` stores UI state. Asynchronous operations use `rpc_call(engine, method, params, callback)` to call the core engine without blocking UI threads.
- **Theming**: Palettes are defined in `src/gui/theme.rs`. Use `get_palette(app.state.theme)` to obtain theme-adaptive colors (`pal.bg_darkest`, `pal.text_primary`, etc.) for dark, light, cyber, nord, and synthwave themes.
- **Safety**: The GUI must only display real system data (e.g. packages confirmed by `pacman -Qe`). Never display hardcoded mock packages as installed software.

---

## 6. Exit Codes

CLI commands follow standard exit code conventions:
- `0`: Successful query, action, or dry-run.
- `1`: Operational error, build failure, or smoke test failure.
- `2`: Invalid CLI arguments or usage syntax.
- `3`: Blocked plan or user declined confirmation.
