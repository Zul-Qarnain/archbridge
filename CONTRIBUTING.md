# Contributing to ArchBridge

Thanks for helping improve ArchBridge. Contributions should preserve the safety
boundary: untrusted package content is inspected as data, plans are shown before
mutations, and foreign maintainer scripts are never executed.

## Before changing code

1. Read [PROJECT.md](PROJECT.md) for architecture and ownership.
2. Read [docs/SECURITY.md](docs/SECURITY.md) for trust boundaries.
3. Keep changes in the layer that owns the behavior.
4. Add a focused regression test for behavior changes.

## Local checks

```sh
cargo fmt --check
cargo build
XDG_CONFIG_HOME=$(mktemp -d) cargo test
python3 -m py_compile archbridge-gui.py
python3 -m unittest tests/source_contract.py
```

Run `target/debug/archbridge doctor` before Arch-specific work. Use a disposable
Arch VM for clean-chroot builds and runtime acceptance tests.

## Pull requests

- Explain the problem, responsible layer, and user-visible result.
- Keep commits focused and avoid unrelated formatting or dependency changes.
- Include test commands and results.
- Call out environment limitations or unresolved security risks.
- Do not include secrets, local configuration, build output, or generated assets.

## Commit style

Use concise imperative messages, for example:

```text
fix: wait for GUI workers during shutdown
```

## Code of conduct

Be respectful, specific, and collaborative. Security reports should not be filed
publicly; use the private process described in [SECURITY.md](docs/SECURITY.md).
