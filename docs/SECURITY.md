# Security model

## Trust boundaries

The host kernel, installed Arch tooling, normal user account, package-manager
configuration, workspace and sudo policy are trusted. Source archives, AUR
recipes, foreign package payloads, metadata and generated native artifacts are
untrusted. Code review by the person granting consent is still required.

This is an unverified implementation, not a security certification. Do not use
it with hostile inputs on an important host until it has passed an independent
review and real Arch acceptance tests. Use a disposable VM, not a chroot alone,
when you need a stronger isolation boundary against malicious code.

## Enforced design rules

- No foreign-script executor exists. DEB scripts/RPM scriptlets are data only.
  The unreviewed-script flag cannot override that Phase 1 rule.
- No host `source PKGBUILD`, `makepkg`, `ldd`, shell interpolation of input names,
  or execution of inspected ELF content.
- Source tar extraction is replaced with bounded in-memory regular-file parsing.
  Traversal, links, duplicate paths, devices, malformed UTF-8 paths and ambiguous
  source roots fail closed. Native control-file links and non-text install hooks
  are rejected.
- Source files are snapshotted before review. Native package review and runtime
  installation use the same pinned bytes, not a second read of the original file.
- Plans have unguessable IDs, are held in one private RPC session, expire in ten
  minutes, are single-use and are invalidated by preference changes.
- `sudo -n` cannot silently consume a password prompt from RPC stdin. Obtain sudo
  credentials separately; the service itself is not elevated.
- `mkarchroot` creates a fresh root. `systemd-nspawn` explicitly uses private user
  namespaces, ignores external `.nspawn` settings and supplies no writable host
  bind mounts. No insecure namespace fallback is implemented.
- Build commands run as an unprivileged chroot builder. Dependency resolution
  can elevate inside the container, not onto the host. Review supplied PKGBUILDs
  and all bundled text before permitting execution.
- Runtime launch uses UID 65534, a private network, a short timeout and a distinct
  root. Native install hooks run only after they have been included in a review
  plan; live hooks require a separately displayed installation stage.
  Entry points must resolve to executable content shipped by that artifact, not
  an unrelated executable already present in the base root.
- HTTPS is mandatory for metadata downloads, and curl redirects cannot downgrade
  the protocol. Certificate checking and package integrity are not disabled.
- Generated native packages require passing runtime checks before live install.
  Passing an exit-0 smoke command does not establish that code is safe.
- User-local AppImage installation copies exact tested bytes without executing
  them on the host and refuses to overwrite an existing file.

## Limits and residual risks

The initial resource caps are 256 MiB for source/package/process archive data,
64 MiB per regular archive member, 20,000 archive entries, 32 native artifacts,
8 MiB normal command output and 1 MiB RPC requests. There are process timeouts
and bounded output retention. These are not complete cgroup CPU/memory/disk quotas;
a malicious build can exhaust disk or consume resources before timeout. Local
directory recursion is also not hardened against an adversarial same-user writer.

Build/dependency setup has network access and can reach host/LAN services.
Runtime smoke checks do not. Network namespace isolation alone is not a complete
firewall for build-time fetches. No claim of reproducible offline builds is made.

The current user's account can edit its own job directories. Protect the workspace
from other writers. The implementation rejects symlinked job ancestors and uses
private job directories and exclusive file creation, but does not defend against
an attacker already controlling that same user account, system binaries, sudo
policy or package-manager configuration.

`readelf`, `dpkg-deb`, `rpm`, `bsdtar` and tar decompression still parse untrusted
data and can themselves have vulnerabilities. Keep tools current and inspect
hostile files in a VM. `readelf` uses non-executable temporary files; inspection
is not an implementation of a full ELF loader or cross-distro dependency solver.

Shell text classification is intentionally not a proof that a script is safe.
RPM trigger output can remain a group of unreviewed scriptlets. No group or script
can execute through Phase 1. Repository-search suggestions are not verified
publisher identities. SHA-256 pins fetched bytes but does not authenticate the
publisher of a source release or AppImage.

Flatpak remains a distinct trust system. A displayed signing key inside an
upstream-provided reference is not independent publisher authentication. Review
remote URLs/keys and Flatpak permissions; the headless Arch smoke test is not
substituted for Flatpak's own application sandbox.

## Cleanup

Normal build/test completion and handled failures remove the disposable root.
Cleanup failure is reported, never hidden; a failed test cleanup blocks live
installation. Inputs and packages remain in the displayed `.archbridge/jobs/<id>`
directory for diagnosis. These are explicit build outputs, not outcome telemetry.

SIGKILL, power loss, lost sudo credentials, kernel/container failures or frontend
termination can leave a root. Verify no nspawn process/mount uses that particular
job, then manually remove only the displayed job directory. Do not copy a wildcard
`sudo rm` command from untrusted package output. Cleanup uses `--one-file-system`
to avoid traversing other mounted filesystems. No root directory is reused for a
later build or test.

## Reference conventions

- Arch clean chroots: https://wiki.archlinux.org/title/DeveloperWiki:Building_in_a_clean_chroot
- mkarchroot: https://man.archlinux.org/man/mkarchroot.1
- systemd-nspawn: https://man.archlinux.org/man/systemd-nspawn.1
- AUR RPC: https://wiki.archlinux.org/title/Aurweb_RPC_interface
- GitHub releases: https://docs.github.com/en/rest/releases/releases
- GitLab releases: https://docs.gitlab.com/api/releases/
- bsdtar: https://man.archlinux.org/man/bsdtar.1
- AppImage extract-and-run: https://docs.appimage.org/user-guide/troubleshooting/fuse.html