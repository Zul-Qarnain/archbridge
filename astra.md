# ArchBridge: Complete Architecture, System Specification & Project Guide (A to Z)

---

## 1. Executive Summary & Mission Objective

### 1.1 The Core Mission
**ArchBridge** is a modern, user-friendly, and secure package builder, translator, and installer for **Arch Linux**.

On Arch Linux, users frequently need software that is not in the official repositories (`core`, `extra`, `multilib`) or well-maintained in the Arch User Repository (AUR). Traditionally, users resorted to dangerous workarounds:
- Running `sudo make install` or `python setup.py install` directly on the host system, scattering untracked files across `/usr/bin` and `/usr/lib`, causing unresolvable file conflicts and impossible uninstallation.
- Running `debtap` to convert `.deb` files, which often fails due to mismatched shared library (`.so`) dependencies between Debian's freeze versions and Arch's rolling-release `glibc`.
- Running heavyweight container tools like **Docker** or **Podman** (or `distrobox`), downloading gigabytes of foreign OS layers (Ubuntu/Fedora images) just to run a single utility, completely bypassed by `pacman` and bloating disk space.

### 1.2 The ArchBridge Solution
ArchBridge bridges the gap by providing a **lightweight, native, secure pipeline** that takes:
- Upstream GitHub releases (binaries or source tarballs)
- Debian (`.deb`) packages
- Red Hat / Fedora (`.rpm`) packages
- Raw source trees (CMake, Meson, Rust/Cargo, Go, Python, Autotools)

...and transforms them into **pristine, 100% native Arch Linux packages (`.pkg.tar.zst`)** that are tracked, verified, and cleanly uninstallable via `pacman -U`.

### 1.3 Key Invariant: Zero Heavy Container Images
**ArchBridge strictly avoids Docker, Podman, and OCI container images:**
1. **No multi-gigabyte container base images:** ArchBridge never downloads or stores Docker Hub / Quay image layers.
2. **No background daemons:** No `dockerd` or `podman.service` required.
3. **Official Arch Maintainer Workflow:** Packages are built using Arch's standard `devtools` (`mkarchroot` and `makechrootpkg`), building inside a clean, ephemeral directory chroot on the local disk using the host's `/etc/pacman.conf`.
4. **Ephemeral Sandboxed Smoke Testing:** Post-build testing is done using standard `systemd-nspawn` with private network namespaces and unprivileged user namespace isolation.

---

## 2. System Architecture & Design Principles

```
+---------------------------------------------------------------------------------------+
|                                  User Interfaces                                      |
|   +---------------------------------------+   +-----------------------------------+   |
|   |  CLI Client (archbridge <command>)    |   |  PyQt6 GUI (archbridge-gui.py)    |   |
|   |  - search, inspect, recipe, build     |   |  - Browse & Import .deb/.rpm      |   |
|   |  - doctor, daemon, prepare, execute   |   |  - Visual Progress & 2-Stage Exec |   |
|   +---------------------------------------+   +-----------------------------------+   |
+-------------------------------------------|-------------------------------------------+
                                            | JSON-RPC 2.0 (v1.prepare, v1.execute)
+-------------------------------------------v-------------------------------------------+
|                              ArchBridge Core Engine                                   |
|  +----------------------+  +-------------------------+  +--------------------------+  |
|  |   Discovery          |  |   Safe Inspection       |  |   Recipe Generator       |  |
|  |   (discovery.rs)     |  |   (inspect.rs)          |  |   (recipe.rs)            |  |
|  +----------------------+  +-------------------------+  |   - CMake, Meson, Cargo  |  |
|  +----------------------+  +-------------------------+  |   - Go, Python, Make     |  |
|  |   Foreign Importer   |  |   Doctor Diagnoser      |  |   - Imported (.deb/.rpm) |  |
|  |   (build.rs)         |  |   (doctor.rs)           |  +--------------------------+  |
|  |   - deb ar extraction|  |   - Host capability     |  +--------------------------+  |
|  |   - rpm bsdtar stage |  |   - Safe HTTPS probe    |  |   Config System          |  |
|  +----------------------+  +-------------------------+  |   (config.rs)            |  |
|  +---------------------------------------------------+  +--------------------------+  |
|  |   Plan & Staging Coordinator (engine.rs)                                        |  |
|  +---------------------------------------------------------------------------------+  |
+-------------------------------------------|-------------------------------------------+
                                            | Isolated Execution
+-------------------------------------------v-------------------------------------------+
|                              Isolation & Build Primitives                             |
|  +------------------------------------------+  +------------------------------------+ |
|  | Clean Chroot Build (mkarchroot /         |  | Runtime Smoke Sandbox              | |
|  | makechrootpkg from devtools)             |  | (systemd-nspawn --private-network  | |
|  | - Pristine base + base-devel chroot      |  |  --private-users=pick --user 65534)| |
|  +------------------------------------------+  +------------------------------------+ |
+-------------------------------------------|-------------------------------------------+
                                            | Output
                                            v
                          Native Arch Package: package.pkg.tar.zst
                                            |
                                            v
                          Native Installation: sudo pacman -U package
```

### 2.1 Two-Phase Plan-Then-Execute Safety Protocol
ArchBridge enforces a strict safety invariant: **no destructive action or build process runs without an approved Plan.**
1. **Phase 1: Prepare (`v1.prepare`)**:
   - Queries repositories or inspects inputs (source tree, `.deb`, or `.rpm`).
   - Extracts metadata safely into memory.
   - Detects build system or generates an `Imported` package recipe.
   - Produces a structured `Plan` containing proposed filesystem changes, safety warnings, reviews, and step previews.
2. **Phase 2: Execution (`v1.execute`)**:
   - Takes the approved `plan_id`.
   - Executes the clean chroot build.
   - Runs runtime smoke tests (when an entry binary is defined).
   - Produces the output `.pkg.tar.zst` artifact.
   - Returns a follow-up `next_plan` ready to install via `pacman -U`.
   - If `--dry-run` is requested, execution halts immediately after Phase 1 without touching the filesystem.

### 2.2 Foreign Package Translation Architecture (`.deb` & `.rpm`)
Unlike legacy tools that run shell scripts or unpack directly onto the host, ArchBridge uses an isolated **three-step payload translation pipeline**:
1. **Zero-Script Payload Isolation**:
   - For `.deb` packages: ArchBridge traverses the `ar` archive header in memory (`src/build.rs`), locates the `data.tar.*` member, extracts it, and extracts the payload with `bsdtar`. Maintainer scripts (`preinst`, `postinst`, triggers) in `control.tar.gz` are **strictly ignored and never executed**.
   - For `.rpm` packages: ArchBridge unpacks the cpio payload with `bsdtar` without running RPM scriptlets.
   - Repository databases (`.db` files) are explicitly checked and rejected with clear user guidance.
2. **Automated `Imported` PKGBUILD Generation**:
   - `src/recipe.rs` synthesizes an official Arch `PKGBUILD` marked with `BuildSystem::Imported`.
   - Configures `options=('!strip')`, sets package metadata, and specifies a clean `package()` function that places files into `$pkgdir`.
3. **Pristine Chroot Repackaging**:
   - The payload is fed into `makechrootpkg` inside a clean chroot to produce a valid, signed, standard Arch Linux `.pkg.tar.zst` package.

### 2.3 Strict Security & Isolation Invariants
Enforced by automated source contract tests (`tests/source_contract.py`):
- **No Foreign Script Execution**: Maintainer scripts (`preinst`, `postinst`) in `.deb` and `.rpm` files are inspected and reviewed, but **never executed** on the host.
- **No `ldd` on Untrusted Binaries**: Dynamic library dependencies are inspected via raw ELF header parsing, never by running `ldd` (which can execute code on malicious binaries).
- **Pinned Bytes**: Package review structures work on cryptographically pinned SHA-256 byte buffers in memory to prevent Time-Of-Check to Time-Of-Use (TOCTOU) exploits.
- **No Integrity Bypass**: ArchBridge code never uses `--skipinteg`, `--skippgpcheck`, `--nodeps`, `--overwrite`, or `SigLevel = Never`.
- **Hard Runtime Sandbox**: Post-build testing via `systemd-nspawn` strictly enforces:
  - `--private-network`: Zero internet access.
  - `--private-users=pick`: Maps container execution to an unprivileged namespace (UID `65534`).
  - No host filesystem mounts (`--bind=`).
  - Strict 30-second timeout.

---

## 3. Technology Stack & Dependencies

### 3.1 Programming Languages
- **Rust (2021 Edition)**:
  - Core engine, JSON-RPC daemon, package extraction, recipe generation, chroot build orchestrator, and CLI binary.
  - Chosen for memory safety, performance, concurrency, and zero-runtime dependency distribution.
- **Python 3 (3.10+)**:
  - Desktop GUI frontend (`archbridge-gui.py`) using `PyQt6`.
  - Architectural source contract tests (`tests/source_contract.py`).
- **POSIX Shell / Bash**:
  - End-to-end integration acceptance scripts (`tests/acceptance.sh`).
- **YAML**:
  - Continuous Integration workflow configuration (`.github/workflows/ci.yml`).

### 3.2 Rust Crates & Libraries (`Cargo.toml`)
| Crate | Version | Purpose |
| :--- | :--- | :--- |
| **`serde`** | `1.0` (with `derive`) | Structured serialization/deserialization for plans, configuration, RPC messages, and reports. |
| **`serde_json`** | `1.0` | JSON-RPC 2.0 communication format between backend and frontend. |
| **`sha2`** | `0.10` | Cryptographic SHA-256 checksum generation for source archives and package file verification. |
| **`tar`** | `0.4` | In-memory archive extraction and inspection of `.tar`, `.tar.gz`, and source tarballs. |
| **`flate2`** | `1.0` | Gzip stream decompression for `.tar.gz` and debian `data.tar.gz`. |
| **`toml`** | `0.8` | Parsing and writing ArchBridge configuration files (`~/.config/archbridge/config.toml`). |
| **`tempfile`** | `3.0` | Secure creation of ephemeral job directories, staging roots, and test chroots. |

### 3.3 Host Tools Utilized
- **`bsdtar` / `tar`**: Multi-format archive and foreign payload handling.
- **`devtools` (`mkarchroot`, `makechrootpkg`)**: Official Arch Linux clean-chroot builders.
- **`systemd-nspawn`**: Unprivileged runtime smoke-test sandboxing.
- **`pacman` & `makepkg`**: Arch Linux package database and creation engine.

---

## 4. Complete Codebase Architecture: File-by-File Breakdown

```
archbridge/
├── .github/
│   ├── ISSUE_TEMPLATE/
│   │   ├── bug_report.md           # Issue template for reporting bugs
│   │   └── feature_request.md      # Issue template for feature proposals
│   ├── workflows/
│   │   ├── ci.yml                  # GitHub Actions CI matrix (fmt, tests, python checks)
│   │   └── release.yml             # Automated release pipeline
│   └── pull_request_template.md    # PR submission checklist and requirements
├── src/
│   ├── archive.rs                  # Archive scanning, tar traversal, SHA-256 reviews
│   ├── build.rs                    # Foreign package payload extraction, chroot builds, smoke testing
│   ├── config.rs                   # System configuration, repo priorities, defaults
│   ├── discovery.rs                # Multi-source discovery across AUR, repos, GitHub
│   ├── doctor.rs                   # System readiness diagnostics & HTTPS network probe
│   ├── engine.rs                   # Central state machine, Plan preparation & execution
│   ├── inspect.rs                  # Safe foreign format inspection (.deb, .rpm, ELF)
│   ├── lib.rs                      # Root crate library exposing modules
│   ├── main.rs                     # CLI binary entry point & JSON-RPC client
│   ├── process.rs                  # Subprocess runner with timeout & output capture
│   ├── recipe.rs                   # Build system detector & PKGBUILD generator (inc. Imported)
│   └── rpc.rs                      # JSON-RPC 2.0 server protocol dispatcher
├── tests/
│   ├── acceptance.sh               # Bash integration acceptance script
│   ├── cli.rs                      # Rust integration tests for CLI / RPC interface
│   ├── core.rs                     # Engine plans, DB rejection, Imported recipe tests
│   ├── doctor.rs                   # Doctor checks, network probe pass/fail/unavailable
│   ├── foreign_formats.rs          # Safe .deb/.rpm analysis without script execution
│   └── source_contract.py          # Python AST/source assertions verifying security rules
├── archbridge-gui.py               # Modern PyQt6 desktop application with package browser
├── Cargo.toml                      # Rust package manifest & release profiles
├── Cargo.lock                      # Locked dependency tree
├── .gitignore                      # Git exclusion rules
├── LICENSE                         # MIT License
├── README.md                       # Public documentation & getting started guide
├── CONTRIBUTING.md                 # Contribution guidelines
├── astra.md                        # Complete A-to-Z architecture documentation (this file)
├── EXPLANATION.md                  # Project overview (git-ignored)
└── .md                             # Local project summary (git-ignored)
```

### Detailed Breakdown of Every Module:

#### 1. `src/lib.rs`
Exposes all internal modules: `archive`, `build`, `config`, `discovery`, `doctor`, `engine`, `inspect`, `process`, `recipe`, and `rpc`. Allows both the CLI (`src/main.rs`) and external integration tests in `tests/` to consume the engine.

#### 2. `src/main.rs`
The command-line interface entry point. Serves as a **thin RPC client**:
- Parses CLI commands (`search`, `inspect`, `recipe`, `build`, `doctor`, `daemon`).
- Formats requests into JSON-RPC 2.0 and sends them to `RpcServer`.
- Enforces dry-run safety gates (exiting before execution if dry-run requested).

#### 3. `src/rpc.rs`
Implements the JSON-RPC 2.0 protocol server:
- `v1.capabilities`: Returns supported features and build systems.
- `v1.prepare`: Accepts an action and produces an inspectable `Plan`.
- `v1.execute`: Executes an approved `Plan` by ID.
- `v1.doctor`: Runs system diagnostics.
- `v1.config.get` / `v1.config.set`: Manages persistent user preferences.

#### 4. `src/engine.rs`
The central orchestration engine:
- Manages plan lifecycle and staged build states (`StagedBuild`).
- Handles `prepare_build`: Detects whether target is a source tree or foreign package (`.deb`/`.rpm`). For foreign packages, triggers `snapshot_foreign_package` and generates an `Imported` PKGBUILD.
- Handles `prepare_install`: Automatically converts foreign package installation requests into a clean repackaging build plan.
- Handles `execute_plan`: Executes native Arch packaging (or chroot builds), runs optional runtime smoke tests, and prepares and registers the subsequent `pacman -U` install plan (`next_plan`).

#### 5. `src/discovery.rs`
Multi-source intelligence engine:
- Queries official Arch repositories, the AUR, and GitHub releases.
- Ranks candidate sources by safety, provenance, license, and freshness.
- Produces a `DecisionReport` recommending whether to use native packages, build from source, or translate foreign packages.

#### 6. `src/inspect.rs`
Safe, non-destructive inspection:
- Inspects Debian `.deb` packages using Arch Linux's native `bsdtar` to parse control metadata (`Package`, `Version`, `Depends`, `Description`), systemd services, and desktop shortcuts, flagging maintainer scripts without executing them.
- Inspects Red Hat `.rpm` packages (verifies headers and cpio payloads via `bsdtar` without running scriptlets).
- Inspects ELF binaries for shared library (`.so`) dependencies via direct header parsing (never invokes `ldd` or runs code).

#### 7. `src/recipe.rs`
Build system detection and `PKGBUILD` generation:
- Supports: `BuildSystem::Imported`, `Cmake`, `Meson`, `Cargo`, `Make`, `Python`, `Go`.
- `BuildSystem::Imported` creates a pristine Arch `PKGBUILD` that unpacks the isolated payload (`src.tar.xz`, `src.tar.zst`, or `src.tar.gz`) into `$pkgdir` with `options=('!strip')`.

#### 8. `src/build.rs`
Foreign payload extraction, fast packaging, clean chroot building, and smoke testing:
- **`snapshot_foreign_package()`**: Streams foreign `.deb` payloads (native `data.tar.xz`/`data.tar.gz`) and `.rpm` payloads in fractions of a second, computes SHA-256 hashes, and records a security review. Explicitly guards against `.db` repo index files.
- **`execute_chroot_build()`**:
  1. Writes `PKGBUILD` and payload into an ephemeral job directory.
  2. For pre-compiled foreign packages (`BuildSystem::Imported`), invokes native `makepkg --force --clean` via `fakeroot` directly in the job directory, producing `.pkg.tar.zst` in seconds with zero downloads and no sudo requirement.
  3. When compiling untrusted C/Rust source trees or when root isolation is required, uses `mkarchroot` and `makechrootpkg` for clean chroot isolation.
  4. Collects and validates the resulting `.pkg.tar.zst` artifact.
- **`run_runtime_smoke_test()`**:
  1. Spawns `systemd-nspawn` with `--private-users=pick`, `--private-network`, and `--user 65534`.
  2. Executes the binary under a 30-second timeout to ensure no crashes or missing library errors.

#### 9. `src/doctor.rs`
System health diagnostics (`archbridge doctor`):
- Checks: `pacman`, `base-devel`, `devtools` (`mkarchroot`/`makechrootpkg`), GCC compiler toolchain, kernel user namespaces, pacman keyring, and disk space.
- Performs an HTTPS network probe to `https://archlinux.org` with strict TLS verification.
- Distinguishes between `pass`, `unavailable` (offline/DNS blocked), and `fail` (invalid response/TLS failure). Offline packaging remains ready even without network access.

#### 10. `src/config.rs`, `src/process.rs`, `src/archive.rs`
- `config.rs`: Manages `~/.config/archbridge/config.toml`.
- `process.rs`: Subprocess abstraction with execution timeouts and stdout/stderr capture.
- `archive.rs`: In-memory archive traversal and SHA-256 cryptographic verification.

#### 11. `archbridge-gui.py`
PyQt6 desktop GUI:
- **Package Browser & File Picker**: Dedicated "Browse Package..." button to choose local `.deb` or `.rpm` files directly.
- **Inspect & Import Action**: Inspect a `.deb`/`.rpm` and click "Import & Build" to immediately transition to build staging.
- **Real-Time Progress & Stage Feedback**: Animated progress bar (`QProgressBar`) and dynamic stage labels ("Preparing a verified build plan…", "Working… clean chroot setup…", "Build complete — ready to install").
- **Two-Stage Execution**: Seamlessly transitions from "Confirm & Execute Plan" (build) to "Confirm & Install Built Package" (`pacman -U`).
- **Interactive Doctor Dashboard**: Real-time host readiness checks with visual status indicators.

---

## 5. How It Was Pushed to GitHub & Git Configuration

### 5.1 GitHub Repository Details
- **Repository URL**: `https://github.com/Zul-Qarnain/archbridge`
- **Remote Configuration**: `git@github.com:Zul-Qarnain/archbridge.git` (SSH Authentication)
- **Default Branch**: `main`

### 5.2 Git History & Commit Flow
1. **Initial Release (`49896ab`)**: Initialized repository on `main`, configured open-source licensing, documentation, and GitHub Actions CI.
2. **CI Hardening Fix (`7ebcd9e`)**: Decoupled unit test assertions in `src/doctor.rs` and `tests/doctor.rs` from host package manager assumptions so that GitHub Actions CI passes on Ubuntu runners.
3. **Local Artifact Exclusion (`7330c9e`, `10ec7af`)**: Configured `.gitignore` to keep local scratch and private documentation files untracked.

### 5.3 `.gitignore` Rules
```gitignore
# Local source/reference artifacts
main.txt
main.tex
ui/
stitch_ui_clone_generator/
context.md
/.md
/EXPLANATION.md

# Build and Python-generated files
target/
__pycache__/
*.py[cod]

# Desktop and editor-local metadata
.directory
.codex/
.agents/
.archbridge/
```

---

## 6. How It Is Tested & Continuous Integration (CI)

```
                       Verification Pipeline
                       
  +-------------------------------------------------------------+
  | 1. Code Formatting: cargo fmt --check                       |
  +------------------------------|------------------------------+
                                 v
  +-------------------------------------------------------------+
  | 2. Rust Unit & Integration Tests: cargo test                |
  |    • 12 Unit Tests in src/doctor.rs                         |
  |    • 6 Core Integration Tests (tests/core.rs)               |
  |      - Repo DB rejection check                              |
  |      - Imported PKGBUILD script-free check                  |
  |      - CMake & recipe detection                             |
  |      - Engine plan preparation                              |
  |    • 5 Doctor Integration Tests (tests/doctor.rs)           |
  |    • 2 CLI Integration Tests (tests/cli.rs)                 |
  |    • 2 Foreign Format Tests (tests/foreign_formats.rs)      |
  +------------------------------|------------------------------+
                                 v
  +-------------------------------------------------------------+
  | 3. Source Contract Enforcement:                             |
  |    python3 -m unittest tests/source_contract.py             |
  +------------------------------|------------------------------+
                                 v
  +-------------------------------------------------------------+
  | 4. GUI Frontend Syntax Check:                               |
  |    python3 -m py_compile archbridge-gui.py                  |
  +-------------------------------------------------------------+
```

### 6.1 Rust Test Suites
1. **Unit Tests (`src/doctor.rs`)**:
   - Tests probe result evaluations for HTTPS pass, DNS offline, TLS failure, and timeouts.
   - Tests readiness calculation logic under both online and offline conditions.
2. **Integration Tests (`tests/` directory)**:
   - `tests/core.rs`: Validates repository database rejection, imported package PKGBUILD generation without script execution, build system detection, and plan staging.
   - `tests/doctor.rs`: Full doctor execution with mocked network probes.
   - `tests/foreign_formats.rs`: Safe `.deb` and `.rpm` inspection without code execution.
   - `tests/cli.rs`: JSON-RPC 2.0 capabilities and configuration get/set commands.

### 6.2 Structural Security Contract Tests (`tests/source_contract.py`)
Enforces architectural rules via Python AST inspection:
- Verifies CLI is purely an RPC client, not a direct build runner.
- Verifies `inspect.rs` never invokes `sh`, `bash`, `ldd`, or `Command::new`.
- Verifies smoke tests have mandatory isolation flags (`--private-users=pick`, `--private-network`, `--user 65534`).
- Verifies no integrity bypass flags (`--skipinteg`, `--skippgpcheck`, etc.) exist anywhere in the code.

### 6.3 GitHub Actions CI (`.github/workflows/ci.yml`)
- Triggers on every `push` and `pull_request` to `main`.
- Runs on `ubuntu-latest`.
- Steps executed:
  1. Installs stable Rust toolchain with `rustfmt`.
  2. Sets up Python 3.x.
  3. `cargo fmt --check`
  4. `cargo test`
  5. `python3 -m py_compile archbridge-gui.py`
  6. `python3 -m unittest tests/source_contract.py`
- **Current Status**: **Passing 100% Green**.

---

## 7. What Has Been Achieved So Far

1. **Full Core Engine & Architecture**:
   - Multi-source discovery, archive inspection, recipe generation, foreign payload translation (`.deb`/`.rpm`), chroot build orchestration, and runtime smoke testing are fully functional.
2. **Foreign Payload Repackaging with Zero Script Execution**:
   - Added `snapshot_foreign_package` and `BuildSystem::Imported` to safely translate `.deb` and `.rpm` packages into native `.pkg.tar.zst` packages without executing untrusted maintainer scripts.
3. **Production-Grade Doctor Diagnostics**:
   - `archbridge doctor` provides real-time verification of all host dependencies (pacman, devtools, compilers, user namespaces, keyring, storage, network probe).
4. **Zero Heavy Container Overhead**:
   - Fully replaced Docker/Podman requirements with native Arch Linux `devtools` (`mkarchroot` / `makechrootpkg`) and `systemd-nspawn`.
5. **Modern Desktop GUI & High-Performance CLI**:
   - Desktop GUI in Python/PyQt6 with file browser for `.deb`/`.rpm`, "Import & Build" workflow, animated progress indicators, multi-stage status tracking, and direct `pacman -U` installation.
6. **Open Source & CI Certified**:
   - Published on GitHub with an automated CI pipeline passing all tests on every commit.
