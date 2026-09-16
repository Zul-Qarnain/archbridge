# Development workflow

## Repository layout

```text
src/                 Rust library, CLI, RPC, discovery, planning, and safety code
archbridge-gui.py    PyQt6 desktop frontend
tests/               Rust integration tests, Python contracts, acceptance harness
docs/                Public architecture, security, IPC, validation, and release docs
.github/             CI and contribution templates
```

## Ownership

- `src/discovery.rs`: source identity, availability, and recommendations.
- `src/engine.rs`: plans, staged actions, confirmation, and execution invariants.
- `src/inspect.rs`: data-only DEB/RPM inspection.
- `src/build.rs`: clean-chroot build and isolated runtime testing.
- `src/rpc.rs`: versioned frontend boundary.
- `archbridge-gui.py`: presentation and user interaction only.

Do not duplicate packaging logic in the UI.

## Test layers

Use fast unit/integration tests for Rust behavior, source-contract tests for
cross-language invariants, and disposable Arch acceptance tests for privileged
tooling. A passing local test suite is not a substitute for real clean-chroot
validation.
