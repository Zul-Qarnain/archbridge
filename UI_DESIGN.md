# ArchBridge — Modern UI/UX Design Specification & Architecture

## 1. Vision & Design Philosophy

**ArchBridge** bridges the gap between software discovery and Arch Linux packaging. The GUI must look and feel like a modern, hyper-polished next-generation developer application (akin to Raycast, Linear, modern KDE Kirigami, and Zed Editor), emphasizing:

1. **Clarity & Safety First**: Every risky action (building, installing, script review) is visibly previewed in a dry-run state with explicit consent gates.
2. **Modern Aesthetics**: Sleek dark mode, subtle glassmorphic surfaces, refined micro-borders (`#30363d`), distinct source badges, crisp typography, and fluid status indicators.
3. **Information Density with Zero Clutter**: Clean collapsible cards, structured decision matrices, and tabbed inspection views rather than raw JSON dumps.
4. **Zero-Logic Decoupling**: All UI interactions trigger JSON-RPC 2.0 calls to `archbridge serve` over standard IPC.

---

## 2. Visual Design System

### 2.1 Color Palette

| Token | Hex Value | Semantic Purpose |
|---|---|---|
| **Background (App)** | `#090d13` | Deep space canvas background |
| **Background (Sidebar / Cards)** | `#0f141c` | Surface card & sidebar background |
| **Background (Elevated / Modals)** | `#161f2e` | Popovers, active dropdowns, dialogs |
| **Surface Hover** | `#1c2638` | Interactive row & button hover state |
| **Border (Subtle)** | `#212c3d` | Dividers, card boundaries, tabs |
| **Border (Active / Focus)** | `#388bfd` | Focused inputs, selected items |
| **Accent Primary (Arch Cyan)** | `#1793d1` | Brand identity, primary buttons, links |
| **Accent Secondary (Blue Glow)**| `#58a6ff` | Highlights, badges, active tabs |
| **Success (Green)** | `#2ea043` | Tests passed, verified packages, pass badges |
| **Warning (Amber)** | `#d29922` | Review required, unreviewed script notice |
| **Danger / Blocked (Red)** | `#f85149` | Build error, blocked plan, missing dependency |
| **Text (Primary / Headers)** | `#f0f6fc` | High-contrast readable headings & titles |
| **Text (Body / Secondary)** | `#c9d1d9` | Standard UI labels and readable body text |
| **Text (Muted / Subtitles)** | `#8b949e` | Timestamps, file paths, helper descriptions |

### 2.2 Typography
- **Primary Interface Font**: `Inter`, `SF Pro Display`, `Segoe UI`, or system `Ubuntu / Cantarell` (13px body, 15px card titles, 20px view headers).
- **Code & Logs Font**: `JetBrains Mono`, `Fira Code`, or `Consolas` (12px, 1.4 line height).

### 2.3 Badges & Source Tags
- **Official Repo**: Blue pill (`#1f6feb` background, `#f0f6fc` text) `[OFFICIAL]`
- **AUR**: Purple pill (`#8957e5` background, `#f0f6fc` text) `[AUR]`
- **Flatpak**: Teal pill (`#008080` background, `#ffffff` text) `[FLATPAK]`
- **AppImage**: Orange pill (`#bd5614` background, `#ffffff` text) `[APPIMAGE]`
- **Upstream (GitHub/GitLab)**: Cyan pill (`#1793d1` background, `#ffffff` text) `[UPSTREAM]`
- **DEB Inspection**: Yellow-amber outline `[DEB FILE]`
- **RPM Inspection**: Red-amber outline `[RPM FILE]`

---

## 3. Application Layout Architecture

```text
+---------------------------------------------------------------------------------------+
|  ARCHBRIDGE   [● IPC Connected (v1.0)]                                [ Doctor Status ]|
+---------------------+-----------------------------------------------------------------+
|  SIDEBAR            |  MAIN CONTENT VIEWPORT                                          |
|                     |                                                                 |
|  🔍 Discovery       |  [ Search bar / Target input ] [ --repo URL ] [ Search Action ] |
|  📦 Inspect (.deb)  |  -------------------------------------------------------------  |
|  🔨 Build & Install |  💡 RECOMMENDED PATH: AUR (yay v13.0.1)                         |
|  🩺 Doctor Health   |  -------------------------------------------------------------  |
|  ⚙️ Preferences     |  DECISION MATRIX CARDS:                                         |
|                     |  ┌───────────────────────────────────────────────────────────┐  |
|                     |  │ [OFFICIAL] Core / bash (v5.3.15)            [Install Plan]│  |
|                     |  │ [AUR]      yay (v13.0.1)      ⭐ RECOMMENDED [Build Plan]  │  |
|                     |  │ [UPSTREAM] github.com/owner/repo            [Build Source]│  |
|                     |  └───────────────────────────────────────────────────────────┘  |
+---------------------+-----------------------------------------------------------------+
|  STATUS BAR: Ready | Active Chroot: None | Disk: 142 GiB Free | Keyring: Verified     |
+---------------------------------------------------------------------------------------+
```

---

## 4. Screen-by-Screen Wireframes & Interactions

### Screen 1: 🔍 Discovery & Search Hub (`/discover`)
- **Top Bar**: Search bar with real-time query button, search history dropdown, and optional `--repo` input.
- **Recommendation Banner**: Highlights the safest path found based on ArchBridge hierarchy (`Official -> AUR -> Flatpak -> AppImage -> Upstream`).
- **Structured Decision Cards**:
  - Each source card displays Source Badge, Availability (`AVAILABLE`, `UNAVAILABLE`, `DISABLED`), Candidate list with descriptions and versions.
  - Direct action buttons: `"Prepare Install Plan"`, `"Review Source & Build"`.

---

### Screen 2: 📦 Foreign Package Inspector (`/inspect`)
- **Dropzone Header**: Drag-and-drop or browse button for `.deb` and `.rpm` files.
- **Summary Header Grid**:
  - Package Name, Version, Architecture, Formats, and License.
  - Safety Guarantee Badge: `🛡️ Maintainer scripts analyzed in data-only mode (EXECUTED: FALSE)`.
- **4 Tabbed Detail Sections**:
  1. **Maintainer Scripts**: Syntax-highlighted code editor for `postinst`, `preinst`, `prerm`, `postrm`, and RPM scriptlets.
  2. **Dependencies**: Required package names with Arch mapping hints.
  3. **Desktop & Systemd**: List of `.desktop` launcher entries and `.service` units.
  4. **Dynamic Libraries**: Extracted ELF `DT_NEEDED` libraries from `readelf -d`.

---

### Screen 3: 🔨 Build & Clean Chroot Studio (`/build`)
- **Workflow Stepper (Visual 4-Stage Tracker)**:
  `1. Prepare Plan` $\rightarrow$ `2. Snapshot & PKGBUILD Review` $\rightarrow$ `3. Clean Chroot Build` $\rightarrow$ `4. Smoke Test & Install`
- **Interactive Dry-Run Plan Review**:
  - Displays generated `PKGBUILD` with syntax highlighting.
  - Filesystem change previews (`.archbridge/jobs/job-<id>`).
  - Input file hashes (`SHA-256`) and safety review manifest.
- **Execution Terminal Console**:
  - Real-time stream of `mkarchroot`, `makechrootpkg`, and container smoke tests.
  - Post-build smoke test status: Exit code, stdout, stderr, entry-point verification.

---

### Screen 4: 🩺 Doctor & System Diagnostics (`/doctor`)
- **System Readiness Score Meter**: Shows overall system capability (Chroot ready, Pacman ready, Toolchain ready).
- **8 Grid Status Cards**:
  1. `pacman` (Installed & version)
  2. `base-devel` (Meta-package verification)
  3. `devtools` (`mkarchroot` / `makechrootpkg` availability)
  4. `namespaces` (Kernel user namespace clone status)
  5. `disk_space` (Workspace free storage >= 8 GiB)
  6. `compiler` (`gcc`, `cargo`, `go` toolchain status)
  7. `network` (HTTPS connectivity to Arch/AUR)
  8. `keyring` (Pacman keyring validation)
- **One-Click Fix Button**: Suggestions for installing missing prerequisites (e.g. `sudo pacman -S --needed devtools`).

---

### Screen 5: ⚙️ Source Preferences & Configuration (`/settings`)
- **Source Toggles**: Beautiful switch toggles for `official`, `aur`, `flatpak`, `appimage`, `upstream`, `deb`, and `rpm`.
- **Repository Aliases**: Table of mapped package-to-repo URLs (`repo.mytool -> https://github.com/owner/mytool`).
- **Atomic Save**: Persists changes to `$XDG_CONFIG_HOME/archbridge/config.json`.

---

## 5. IPC JSON-RPC 2.0 Mapping Reference

All frontend actions communicate with `archbridge serve` over standard JSON-RPC:

| Action | RPC Method | Parameters | Expected Result |
|---|---|---|---|
| Search software | `v1.search` | `{"target": "name", "repo": "optional_url"}` | `DecisionReport` table with recommendations |
| Inspect file | `v1.inspect` | `{"target": "/path/to/pkg.deb"}` | `InspectionReport` with scripts & dependencies |
| Prepare build | `v1.prepare` | `{"action": "build", "request": {"target": "..."}}` | `Plan` object with steps, reviews, & plan_id |
| Execute build | `v1.execute` | `{"plan_id": "...", "confirmed": true}` | `ExecutionResult` with test results & next_plan |
| Run Doctor | `v1.doctor` | `{}` | `DoctorReport` with 8 diagnostic check results |
| Get / Set Config | `v1.config.get` / `v1.prepare` | `{"key": "aur", "value": "true"}` | Preference values and configuration plan |

---

## 6. Next Steps for Implementation

1. **User Implementation**: Build out the frontend layout (QML/Kirigami, PyQt6, or modern web frontend) using this design specification.
2. **Backend Engine**: ArchBridge's Rust core engine (`target/release/archbridge serve`) is fully built, tested, and ready to power all capabilities.
