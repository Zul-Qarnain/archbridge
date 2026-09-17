# ArchBridge Baseline

Prepared: 2026-09-17

This is a local, commit-specific snapshot. It is not a release certification.

## Repository identity

- Working directory: `/home/zulqarnain/archbridge`
- Branch: `main`
- Baseline commit: `10ec7afae0c8ee07255485ae0b543240345ecf05`
- Remote: `git@github.com:Zul-Qarnain/archbridge.git` (credentials not embedded)
- Working tree: dirty
- User changes present: `.gitignore`, `archbridge-gui.py`, `src/build.rs`, `src/engine.rs`, `src/recipe.rs`, `tests/core.rs`
- Untracked files: `agent.md`, `astra.md`
- Generated/untracked workspace material observed: `.archbridge/` and `astra.md` in the working session; these were not removed.
- `.git/info/exclude`: default template only; no broad local exclusion was found.
- `.gitignore`: excludes build output, Python caches, `.archbridge/`, and the user-requested document/UI reference paths. It must be reviewed before publication.

## Source inventory

Present Rust core modules: `archive`, `build`, `config`, `discovery`, `doctor`, `engine`, `inspect`, `process`, `recipe`, and `rpc`, with `main.rs`/`lib.rs` entry points.

Present frontend: `archbridge-gui.py` using PyQt6 and the Rust JSON-RPC server.

Present tests: Rust unit/integration tests in `tests/`, `tests/source_contract.py`, and `tests/acceptance.sh`.

Present project documents: `README.md`, `PROJECT.md`, `CONTRIBUTING.md`, `UI_DESIGN.md`, and the existing `docs/` files. The supplied `agent.md` and `astra.md` are working documents, not verified implementation evidence.

## Environment

| Item | Observed value |
|---|---|
| OS/kernel | Arch Linux, x86_64, kernel `7.2.2-1-cachyos` |
| Rust | `rustc 1.98.1` |
| Cargo | `cargo 1.98.1` |
| Python | `3.14.7` |
| PyQt6 | importable from `/usr/lib/python3.14/site-packages/PyQt6` |
| Arch tools | `pacman`, `makepkg`, `mkarchroot`, `makechrootpkg`, `systemd-nspawn` available |
| Foreign tools | `dpkg-deb` and `rpm` unavailable; `bsdtar` available |
| Compression | `zstd` available |
| Disk | approximately 46 GiB free on the project filesystem |

No prerequisites were installed and no host mutation was performed for this report.

## Baseline commands

| Command | Result |
|---|---|
| `cargo fmt --check` | passed |
| `cargo check --locked --all-targets` | passed |
| `XDG_CONFIG_HOME=$(mktemp -d) cargo test --offline` | passed; current Rust suite passed |
| `cargo clippy --locked --all-targets -- -D warnings` | failed; 8 existing lint findings in `src/build.rs`, `src/doctor.rs`, and `src/recipe.rs` |
| `python3 -m py_compile archbridge-gui.py` | passed |
| `python3 -m unittest tests/source_contract.py` | passed; 8 tests |
| `bash -n tests/acceptance.sh` | passed |
| Arch clean-chroot build acceptance | not run; requires real package input, network/cache state, and privileged disposable acceptance environment |
| Real DEB/RPM parser comparison | blocked/not run; `dpkg-deb` and `rpm` are absent |
| GitHub/remote publication | not performed |

After the baseline snapshot, the existing lint findings were repaired without
changing the product surface. The current portable gate is now green:
`cargo fmt --check`, `cargo check --locked --all-targets`,
`cargo clippy --locked --all-targets -- -D warnings`, offline Rust tests,
Python syntax, source-contract tests, `bash -n tests/acceptance.sh`, and
`git diff --check` all pass.

## Requirement status

| Requirement | Status | Evidence/limitation |
|---|---|---|
| Rust core and CLI compile | implemented-and-tested | locked check and tests pass |
| PyQt6 frontend uses the Rust RPC core | implemented-and-tested | Python syntax and source contract pass; GUI interaction not automated here |
| `.deb`/`.rpm` import/repackaging | implemented-unverified | local code exists; real foreign fixtures and clean-root build not run |
| Foreign maintainer scripts never execute | implemented-and-tested in current policy tests | real vendor fixture comparison not run |
| `.db` rejected as non-payload | implemented-and-tested | regression test covers repository-index message |
| Clean chroot build | implemented-unverified | tool availability observed; real build not run |
| Runtime smoke test | implemented-unverified | no disposable package test root acceptance run |
| Clippy warning-free build | failed | 8 warnings promoted to errors |
| Three real supported build systems | not run | requires provisioned acceptance inputs |
| GitHub release publication | out-of-scope for this task | no publish authorization was given |

## Phase-0 conclusion

The checkout is usable and reproducible for portable tests, but it is not yet a
release candidate. The next milestone is execution/build integrity: fix the
runner and clean-build failure paths, then add the required regression matrix.
