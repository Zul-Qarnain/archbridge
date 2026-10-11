# ArchBridge — full project documentation

## 1. Purpose and status

**Finds and builds the safest available installation path for software on Arch.**

ArchBridge is not a universal DEB/RPM converter. It discovers safer alternatives
first and treats foreign packages as inspection inputs, not installation recipes.

This repository contains an **initial Phase 1 implementation**. Rust compilation,
unit/integration tests, and local Doctor checks run in the current
development environment. Real clean-chroot package acceptance remains an explicit
Arch VM gate. See `docs/VALIDATION.md` for exactly what was and was not tested.

## 2. Architecture

```text
CLI presentation and consent
          |
          | JSON-RPC 2.0, newline-delimited, methods versioned v1.*
          v
Long-lived unprivileged `archbridge serve` process
          |
          v
Rust library: discovery / config / inspection / recipe / planner / execution
          |
          +-- read-only pacman queries and public HTTPS metadata
          +-- data-only archive inspection (no foreign script execution)
          +-- unprivileged makepkg, clean-chroot fallback (mkarchroot + makechrootpkg)
          +-- isolated runtime test (systemd-nspawn)
          +-- separately approved live installation
```

The CLI starts the same executable in `serve` mode and keeps it alive for the
session. The native GPUI desktop application (`archbridge gui`) interacts directly with
the engine, eliminating Python/PyQt6 runtime requirements.
The library is available as the `archbridge` crate.

### Stack choices

- Rust 2021 with a small synchronous core; minimum supported toolchain remains unverified.
- GPUI (Zed Editor's framework) provides GPU-accelerated, native rendering for the desktop GUI.
- Serde/serde_json for configuration, reports and IPC.
- SHA-256 hashes bind source/review/artifact bytes to prepared plans.
- `tar`, `flate2`, `toml`, and `tempfile` for bounded parsing/staging.
- Explicit argument vectors for child processes; no user-built host shell command.
- System `curl` handles HTTPS, timeouts, redirects and certificate verification.
- The Rust engine and GPUI frontend form a self-contained native binary.
  The Rust source-contract checks and the Bash acceptance harness are development
  tooling, not alternate packaging engines.

JSON-RPC was chosen over D-Bus because it has no session/system bus deployment
requirement and naturally gives each frontend a private, unprivileged session.
It is a long-lived service process, not a new process per operation. There is no
network listener, elevated service, or dependency on a GUI toolkit.

`mkarchroot` with `makechrootpkg` was chosen over wrapping `pkgctl build` for the
isolated path: PKGBUILD evaluation happens visibly inside a clean root and the
external Arch tools supply the bootstrap and packaging conventions. That path is
the **fallback**, not the default — see §6. This is not a claim that every
possible devtools/host configuration has been validated.

## 3. Implemented surface

| Command | Behavior |
| --- | --- |
| `search <name>` | Ordered evidence table as structured JSON; exact/fuzzy matches distinguished |
| `info <name>` | Same complete structured source/availability/recommendation report |
| `inspect <file.deb\|file.rpm>` | Metadata, dependencies, provided capabilities, scripts, desktop files, systemd units, and the ELF `DT_NEEDED` names the payload declares |
| `build <url\|directory\|PKGBUILD>` | Snapshot/review, conservative recipe if needed, then an unprivileged `makepkg` with a clean-chroot fallback, then staged runtime testing |
| `install <name\|file>` | Preference-aware discovery or native-artifact testing, then approved installation |
| `test <package.pkg.tar.zst>` | Unpack into a throwaway root, exec the entry binary in `systemd-nspawn`, and report the real exit code, stdout and stderr |
| `uninstall <installed-package-name>` | Planned `pacman -Rns`; takes a name, never a path |
| `config get [key]` | Read preferences without creating a config file |
| `config set <key> <value>` | Planned, confirmed, atomic preference update |
| `doctor` | Host, tooling, package-prerequisite, namespace, disk, compiler and HTTPS checks |
| `serve` | Versioned JSON-RPC service for any frontend |
| `gui` (aliases `ui`, `--gui`, `--ui`) | Native GPUI desktop app over the same in-process engine |

All mutating actions, including config changes and runtime tests, support
`--dry-run` and `--yes`. Plans, reviews and outcomes are JSON documents on stdout;
prompts and the `--dry-run` note go to stderr, so a pipe carries JSON only. Output
is JSON: there is no `--json` flag, and an unrecognized flag is a usage error
(exit 2) rather than something silently ignored. `archbridge <command> --help`
prints that one command's flags.

Exit codes: `0` successful query/action/dry-run, `1` operational/test failure,
`2` invalid CLI usage, `3` blocked plan or declined/missing consent.

### Examples

```sh
archbridge search bash
archbridge info yay
archbridge search my-tool --repo https://github.com/owner/project
archbridge config set aur false --yes
archbridge config set repo.my-tool https://github.com/owner/project --yes
archbridge config get all
archbridge inspect ./vendor-package.deb
archbridge inspect ./vendor-package.rpm
archbridge build ./source --name my-tool --entry my-tool --dry-run
archbridge build ./packaging/PKGBUILD --entry my-tool --dry-run
archbridge build https://github.com/owner/project --entry my-tool --dry-run
archbridge build https://gitlab.com/group/project --entry my-tool --dry-run
archbridge install my-tool --dry-run
archbridge test ./my-tool-1.0-1-x86_64.pkg.tar.zst --entry my-tool --smoke-arg --help --dry-run
```

Build options: `--name`, `--version`, `--entry`, repeated `--dependency <arch-name>`
and repeated `--smoke-arg <arg>`. The latter replaces the default `--version`
runtime invocation. `--entry` accepts a simple binary name or `/usr/bin/<name>`;
it never accepts a shell command. A local source version defaults to a source
snapshot digest; release tags that cannot be valid Arch versions need `--version`.

## 4. Discovery and preferences

The fixed order is:

1. Official Arch sync repositories, queried with `pacman -Ss`; custom repository
   names are not mislabeled official. No web scraping or implicit database refresh.
2. AUR RPC v5 name search. The selected package-base snapshot must be reviewed.
3. Flatpak reference/bundle assets in known upstream release metadata.
4. AppImage assets in that metadata.
5. GitHub/GitLab published release source.
6. DEB assets, inspection-only.
7. RPM assets, inspection-only.

Every row contains `source`, `availability`, `recommended`, `reason`, and
`candidates`. Availability is `available`, `unavailable`, `unknown`, or `disabled`.
No network failure is fabricated into a negative result. A failed higher-priority
lookup prevents automatic lower-priority selection until resolved or explicitly
disabled. Only exact identities can become recommendations.

Repository identity comes from an explicit canonical URL, a saved `repo.<name>`
association, or a matching official/AUR package's upstream URL. GitHub name-search
results are suggestions only: popularity is not identity verification. Confirm a
suggestion with `--repo`. GitLab.com repositories, including nested groups, work
when explicitly known; self-hosted GitLab and arbitrary source hosts do not.

Flatpak/AppImage detection is deliberately bounded to assets in the published
release metadata. It is not an exhaustive Flathub or web search. “Unavailable” in
these rows means no such asset was found in that release, not that none exists
anywhere. Repositories with no published release do not get guessed branch builds.

Preferences live in `$XDG_CONFIG_HOME/archbridge/config.json`, falling back to
`$HOME/.config/archbridge/config.json`. Supported boolean keys are `official`,
`aur`, `flatpak`, `appimage`, `upstream`, `deb`, and `rpm`; default is enabled.
`output_dir` is a string: an absolute directory that a finished package artifact
is copied into after a build. Empty (the default) leaves the artifact in the
per-job build directory, and the Build page says so until a folder is chosen.
The desktop app writes the value through the engine the moment it is picked, on
either the Build or Settings page, so no build can run against a folder that only
exists in unsaved UI state. A relative `output_dir` is rejected, and the copy is
re-hashed against the bytes the build produced before it is offered for install.
`keep_jobs` is a boolean (default `false`). After every build, successful or not,
the engine removes the job's sources, build trees and chroot from
`.archbridge/jobs/<job>`, keeping the `PKGBUILD`, `build.log` and — when no
`output_dir` published the package elsewhere — the built package the install plan
needs. Set `keep_jobs` to `true` to keep the whole tree.
Unknown keys/values and invalid config schemas fail rather than silently resetting
preferences. Read-only operations and dry-runs do not initialize directories.

## 5. Build-system support

| System | Detection / conservative contract |
| --- | --- |
| CMake | Root `CMakeLists.txt`; CMake/Ninja build, `/usr` prefix and staged install |
| Plain make | Root Makefile with explicit `install` target; standard `DESTDIR` and `PREFIX`/`prefix` |
| Cargo | Non-workspace package, `Cargo.lock`, explicit bin(s) or `src/main.rs`; `--locked` |
| Go | Root `go.mod` and root `package main`; `-mod=readonly -trimpath` |
| npm | `scripts.build`, `package-lock.json`, explicit CLI `bin` entries; ignore install-time lifecycle scripts |
| Yarn | Classic v1 lockfile, build script and CLI bin entries; ignore install scripts |
| Meson | Root `meson.build`; Ninja backend, no automatic wrap downloads, staged install |

Unsupported or ambiguous layouts report **“unsupported build system, manual
PKGBUILD required”**. Autotools is not misidentified as plain make. Library-only
Cargo crates, Cargo workspaces, nested Go commands, web-only npm applications,
Yarn Berry, multiple primary build systems, and symlink-containing source
snapshots require manual handling. Declaring a manual PKGBUILD does not waive
snapshot path-safety checks.

Detection is not dependency inference. Supply `--dependency` for known Arch
requirements or maintain a manual PKGBUILD. Foreign dependency-name mapping is
Phase 2 and absent. Generated `license=()` intentionally makes no unverified
licensing claim; audit and add correct license metadata/files before distribution.
The initial npm/Yarn template is deliberately simple and can include development
files/dependencies; it is not a polished distro packaging policy engine.

Source snapshots are read into bounded memory. Remote tar.gz sources require one
root directory. Links, devices, duplicate member names, traversal, and unsafe
paths are rejected. Text files are included in JSON-escaped review output and
binary files have hashes and sizes. The generated PKGBUILD is always displayed.
Snapshots receive real SHA-256 source checksums; no `SKIP` or integrity-bypass
flags are generated. Automatically generated debug split packages are disabled.

## 6. Build → test → install

1. `prepare` downloads/reads a bounded source snapshot, reviews files, generates
   a recipe if supported, and returns an immutable in-memory plan.
2. The frontend displays the plan before consent. No disk staging happens yet.
3. Execution creates a private `.archbridge/jobs/<id>` under the current working
   directory and writes the approved PKGBUILD and source bytes there.
4. **First attempt — unprivileged host build**: `makepkg --force --clean` runs in
   the job directory as the current user. No sudo, no chroot. If it produces a
   `.pkg.tar.*` artifact, that artifact is the result and the build is over.
5. **Fallback — clean chroot**, reached only when the native attempt produced no
   package: `sudo -n mkarchroot -C /etc/pacman.conf <job>/chroot/root base
   base-devel sudo`, then `sudo -n makechrootpkg -r <job>/chroot -- makepkg
   --syncdeps --noconfirm --cleanbuild --clean --force`. It can obtain official
   dependencies; AUR dependency recursion is not automated. Both phases are
   appended to `<job>/build.log`, so the native failure is still readable after
   the fallback runs.
6. If neither path leaves a `.pkg.tar.*` file, the plan fails and says which
   stages were attempted. A root that needs a password reports that it could not
   be authorized rather than hanging.
8. A test plan (`archbridge test <artifact>`) reveals the artifact's own file
   list with per-file SHA-256 digests, and names the exact `/usr/...` path the
   container will exec, resolved from that list. Review/confirm that stage too.
9. Executing it unpacks the artifact into a fresh temporary root and launches the
   entry point under `systemd-nspawn --private-users=pick --private-network
   --user 65534` with a 30-second timeout. Failures report the actual exit code,
   stdout and stderr. A root containing only the artifact has no dynamic loader,
   so a dynamically linked binary is reported as an inconclusive test instead of
   pretending the package is broken.
10. `build` stops after testing. `install` offers a separate live installation
    plan only when **every** test passes, using exact tested bytes.
11. `archbridge test` on its own stages the same container run: with `--dry-run`
    it prints the plan and says nothing was executed; confirmed, it runs the
    container and its exit code is the container's verdict.

The frontend loops over returned `next_plan` values. `--yes` skips prompts, never
plan display or mandatory tests. Any failure blocks the live-install transition.
Missing/ambiguous entry points are blocked, never counted as passing. An explicit
entry must belong to the package, including links that resolve to its own
executable content; a pre-existing base-system binary cannot stand in for it. A successful
`--version` invocation is only a smoke check, not proof of full GUI/daemon behavior.

Dry-run reports precise commands for the current stage, filesystem effects, full
available reviews and conditional downstream behavior. Future artifact names,
digests and package-manager dependency transactions cannot truthfully be known
before building/resolution; they are explicitly unresolved, not fabricated. The
artifact-copy step is a documented template populated only with validated names
from chroot enumeration. Later stages are displayed exactly before they execute.

### Other installation routes

- Official packages use a planned `sudo pacman -S --needed` transaction, with
  signature checks intact. No build or generated-package test is fabricated for
  a prebuilt official package. Keep the host fully upgraded separately.
- Foreign files contribute only their metadata package name to discovery. Their
  payload and scripts are never installed. Review identity mismatches explicitly.
- Flatpak automation supports reviewed `.flatpakref` release assets with HTTPS
  repositories and an explicit signing key. Binary `.flatpak` bundles are reported
  but require manual installation. Flatpak manages its own runtime/permissions.
- A single Type 2 AppImage release asset can be downloaded into memory, tested
  unprivileged/offline in a disposable Arch root with extract-and-run, then
  separately copied to `~/.local/bin/<name>.AppImage`. It is not repackaged.
  Multiple architecture/variant assets are blocked rather than guessed. Type 1,
  FUSE fallback and desktop integration are unsupported. Existing files are never
  overwritten. The smoke test does not execute the image on the host.

## 7. Foreign-package inspection

Neither `dpkg-deb` nor `rpm` is required or invoked. `bsdtar` (libarchive) unpacks
the DEB container and lists its `control.tar.*` / `data.tar.*` members; RPM
metadata is decoded from the file's own lead and header regions by `src/rpm.rs`.
Members are extracted only into a private temporary directory owned by the
current user, and temporary files are removed automatically.

The report includes metadata, dependency version expressions, provided
capabilities, maintainer-script and scriptlet text (with the interpreter rpm
would use), desktop files, systemd units, and the ELF `DT_NEEDED` names of the
payload's own objects. `src/elf.rs` reads those out of the binary's program and
section tables in memory — ELF32/64, either byte order — with every offset and
count validated against the file's length. No `ldd`, `readelf`, interpreter or
package entry point is executed, and nothing from the package is loaded.
Candidates are capped per package; a truncated or hostile object is refused by
the parser and simply contributes no library names, while a package whose payload
cannot be read is reported as such rather than as “no dependencies.”

Script statuses distinguish readable shell text from unsupported/unparseable
content; this is not a shell semantic analyzer. RPM query output preserves
scriptlet/trigger headers and content. No script is marked reviewed simply
because it parses. **There is no foreign-script executor.**
`--allow-unreviewed-scripts` is deliberately rejected on build/install; in Phase 1
it cannot turn inspection into execution. This is stricter than Phase 3's future
per-script override policy and avoids implementing that out-of-scope behavior.

## 8. Doctor and privacy

Doctor reports whether this is an Arch Linux host at all (os-release plus the
presence of `pacman` and `makepkg`), then each required executable, installed
`base-devel`, `devtools`, `gcc`, the namespace sysctls, free workspace disk
(2 GiB minimum), and HTTPS connectivity. `ready: false` exits 1 and names the
blocking checks; an unreachable or unrunnable probe reads `unavailable` and does
not by itself make the system unready. It does not create a chroot or mutate
keyrings. Kernel namespace
interfaces are a prerequisite check, not proof that the current VM permits all
privileged nspawn operations. Full validation requires the acceptance builds.

There is no telemetry endpoint, analytics identifier, local outcome history or
calibration collection. Search necessarily contacts requested public metadata
services and exposes normal HTTPS request metadata to those services. Build
artifacts and staging files remain for user diagnosis, not as a history database.
Interrupted jobs may need manual cleanup; see the security/validation documents.

## 9. Repository inventory

| Path | Responsibility |
| --- | --- |
| `Cargo.toml` | Rust library/binary and dependencies |
| `src/main.rs` | Thin CLI, JSON output, consent loop and RPC client |
| `src/rpc.rs` | Bounded line-framed versioned service |
| `src/engine.rs` | Plans, immutable snapshots, confirmation and install state machine |
| `src/config.rs` | Validated, persistent source/repository preferences |
| `src/discovery.rs` | Official/AUR/release metadata and recommendation policy |
| `src/archive.rs` | Bounded archive/local-source snapshotting and review manifests |
| `src/recipe.rs` | Conservative build detection and PKGBUILD generation |
| `src/inspect.rs` | Data-only DEB/RPM/ELF/native package inspection |
| `src/elf.rs` | Bounded, panic-free ELF header reader for `DT_NEEDED` |
| `src/rpm.rs` | Bounded, panic-free RPM lead/header decoder for metadata, deps and scriptlets |
| `src/build.rs` | Unprivileged makepkg, clean-chroot fallback, runtime smoke tests, staged artifacts |
| `src/doctor.rs` | Read-only prerequisite diagnostics |
| `src/process.rs` | Argument-vector process execution, timeouts and output limits |
| `tests/core.rs` | Pure Rust policy/parser/recipe/planning regressions |
| `tests/cli.rs` | CLI/RPC dry-run, persistence and consent regressions |
| `tests/foreign_formats.rs` | Explicit opt-in real-format integration tests |
| `tests/source_contract.rs` | Supplementary static checks over the crate's own source |
| `tests/acceptance.sh` | Explicitly gated disposable-Arch real-build matrix |

## 10. Remaining gates and excluded work

Before declaring Phase 1 complete: compile and format with Rust; resolve compiler
or integration findings; test real official/AUR/upstream discovery; inspect real
DEB and RPM inputs; successfully build/test three projects with different build
systems; verify missing-base-devel diagnosis in a disposable Arch machine; and
audit all privileged paths against current Arch tooling.

No Phase 2/3/4 work was added. Local history, foreign dependency mappings,
`explain`, experimental conversion, script translation/execution, signing,
repository hosting, GUI and Dolphin integration remain explicitly excluded.
Hardening and tests in this change are Phase 1 safety work, not new product scope.
