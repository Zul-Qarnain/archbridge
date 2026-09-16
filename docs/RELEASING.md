# Release process

Releases are GitHub tags created from a passing main branch. Do not publish a
release until the validation gates below pass on a supported Arch environment.

## Maintainer checklist

1. Review the change log and security-impacting changes.
2. Run `cargo fmt --check`, `cargo build --release`, and `cargo test`.
3. Run Python syntax/source-contract checks.
4. Run `archbridge doctor`.
5. Run the opt-in disposable Arch acceptance harness.
6. Inspect the final diff and confirm no secrets, local docs, or build outputs are included.
7. Create an annotated tag such as `v0.1.0`.
8. Create GitHub release notes with supported environment, known limitations, and test evidence.

## Versioning

Use Semantic Versioning for CLI/RPC compatibility:

- MAJOR: incompatible RPC or CLI behavior.
- MINOR: backward-compatible commands or capabilities.
- PATCH: backward-compatible fixes.

Keep `v1.*` RPC methods compatible within the major protocol version.

## Release artifacts

Build artifacts must identify the commit, target architecture, and verification
commands. Never claim clean-chroot or runtime validation unless it actually ran.
