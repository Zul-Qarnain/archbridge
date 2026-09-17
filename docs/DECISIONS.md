# Architecture Decisions

## ADR-001: One Rust core with a thin PyQt6 frontend

Status: accepted for the current implementation.

The Rust crate owns discovery, inspection, plans, recipes, process execution,
builds, and installation decisions. `archbridge-gui.py` communicates through the
versioned JSON-RPC service and must not call `pacman`, execute recipes, or import
foreign payloads itself. A future QML/Kirigami migration requires a separate
owner-approved decision and must reuse this core.

## ADR-002: Safety-first foreign package policy

Status: requires reconciliation before release.

DEB/RPM control scripts and triggers are inspection evidence only and are never
executed. A copied payload is not proof of ABI compatibility, dependency mapping,
service setup, or runtime correctness. Any foreign repackaging route must remain
clearly experimental, display omitted-script consequences, and require separate
test/install evidence before live installation. `.db`/`.files` repository indexes
are not package payloads.

## ADR-003: Plan is the execution authority

Status: accepted; execution lifecycle remains an active repair milestone.

The approved typed plan, exact arguments, reviewed inputs, and current artifact
must be the sole authority for execution. Display strings must not be reparsed to
reconstruct mutations. Continuations must be registered, single-use, expiry-aware,
and invalidated when relevant input or configuration changes.

## ADR-004: No publication or host prerequisite installation by default

Status: accepted.

Local code and documentation may be prepared, but GitHub pushes, releases,
repository settings, package publication, and host prerequisite installation
require explicit owner authorization and a separate verified step.
