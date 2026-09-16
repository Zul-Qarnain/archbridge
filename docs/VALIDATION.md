# Validation and Phase 1 acceptance

## Environment evidence — September 16, 2026

The current development host provides Rust/Cargo, `pacman`, `base-devel`,
`devtools`, `mkarchroot`, `makechrootpkg`, compiler tooling, Python 3, and PyQt6.
Local Rust compilation/tests and Python UI checks pass. HTTPS DNS access is
environment-dependent and real package builds still require a disposable Arch VM.

Current local gates:

- `cargo fmt --check`: passed.
- `cargo build --release`: passed.
- `cargo test`: passed; Cargo currently reports 12 Rust tests.
- Python compile and source-contract tests: passed.
- `archbridge doctor`: `ready: true`; offline network is reported as unavailable.
- Real clean-chroot package acceptance: not claimed until run in a disposable Arch VM.

## 1. Compile and run local regressions

On a machine with the Rust toolchain and allowed dependency access:

```sh
cargo fmt --check
cargo check --all-targets
cargo test
python3 -m unittest discover -s tests -p source_contract.py -v
bash -n tests/acceptance.sh
```

The Rust tests cover archive traversal/duplicates/links, snapshot hashes,
preference ordering, repository identity validation, official-repo filtering,
unknown-source fallback blocking, recipe constraints, script non-execution,
runtime isolation flags, RPC errors/limits, dry-run non-mutation, persistent
config and noninteractive consent. CLI tests spawn the same JSON-RPC server as
real CLI operations. They do not require privileged Arch builds.

Because no Rust formatter ran here, apply `cargo fmt` if required, then rerun the
checks. Resolve compiler/test findings before continuing. The supplementary Python
test suite only checks source contracts; do not report it as passing Rust tests.

## 2. Real foreign formats

```sh
cargo test --test foreign_formats deb_reports_scripts_dependencies_units_and_elf_without_execution -- --ignored
ARCHBRIDGE_TEST_RPM=./fixtures/vendor.rpm cargo test --test foreign_formats real_rpm_inspection_never_claims_installability -- --ignored
target/debug/archbridge inspect ./fixtures/vendor.deb
target/debug/archbridge inspect ./fixtures/vendor.rpm
```

The DEB integration test constructs an actual DEB containing a real ELF, dependency
alternatives, a desktop file, a systemd unit and a harmless sentinel-writing
postinst. It asserts that the sentinel is never created. The RPM test intentionally
requires a supplied real fixture with dependencies, scriptlets and desktop/unit
data. It never installs that RPM. Compare reports with trusted format-tool queries.
Add malicious/corrupt archive corpus cases before claiming parser hardening.

## 3. Disposable Arch acceptance

Provision a disposable Arch VM with the prerequisites listed in README. A
restricted container that cannot run nspawn/user namespaces is not equivalent to
a successful sandbox test. Fully update the VM independently first. Do not execute
this acceptance procedure on an important host.

```sh
cargo build
target/debug/archbridge doctor
ARCHBRIDGE_DISPOSABLE_ARCH_VM=yes bash tests/acceptance.sh
```

The harness first prints discovery results and dry-run plans for:

- an official package (`bash`);
- an AUR candidate (`yay`; verify it is still AUR-only when testing);
- a deliberately distinct query with explicitly associated GitHub source;
- Ninja (CMake), hexyl (Cargo), and nnn (plain make).

These are **candidate acceptance projects, not recorded successful builds**.
Release contents/build layouts may change and must be reviewed at test time.
The engine prints exact fetched source hashes; retain output when recording a
real validation result. If a project is legitimately outside conservative support,
use a reviewed supported project or manual recipe, not a detection bypass.

After reviewing those plans, explicitly opt into actual builds and smoke tests:

```sh
ARCHBRIDGE_DISPOSABLE_ARCH_VM=yes ARCHBRIDGE_RUN_BUILDS=yes bash tests/acceptance.sh
```

The harness requests `build`, not live `install`. All three must produce native
packages and real passing smoke reports. Inspect generated PKGBUILDs, dependency
metadata, package contents, license handling and installed paths. Check that
build inputs and artifacts exist only in the planned job directories and that
disposable roots are removed. Retain any failing command's actual output.

Test missing prerequisites **only inside the disposable VM**:

```sh
sudo pacman -R --noconfirm base-devel
target/debug/archbridge doctor
sudo pacman -S --needed base-devel
```

The middle command must exit nonzero and show the `base-devel` check failing with
a concrete package-query error. A merely present compiler is not a substitute
for checking the meta-package.

## 4. Release-blocking safety scenarios

1. `--dry-run --yes` on config/build/install/test must never create a job/root,
   write preferences, execute package code or invoke a live install command.
2. AUR/supplied PKGBUILDs with hostile top-level statements must appear in review
   output without those statements executing on the host.
3. Tampering with the original source/native package after prepare must not
   change the bytes executed/tested. Changing preferences must invalidate a plan.
4. Reusing a plan ID, changing an ID, or executing without confirmation must fail.
5. A runtime nonzero exit, missing loader/library, signal/crash or timeout must
   block live install and preserve real diagnostics.
6. Native hooks must be displayed before test/live execution. No DEB/RPM script
   can execute with default flags, `--yes`, or `--allow-unreviewed-scripts`.
7. Source preference changes must apply across new CLI sessions. API failure must
   stay `unknown` and not silently promote an untrusted fallback.
8. A failed setup/build/test/cleanup must not claim success or produce a host
   install continuation. No system root may be used as a build target.
9. Test Flatpak references and Type 2 AppImages independently with real upstream
   assets; headless GUI failures are failures, not synthetic passes.

Only after this checklist and the real-package criteria pass should the project
be called a completed Phase 1 MVP or proceed to Phase 2.
