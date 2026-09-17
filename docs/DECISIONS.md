# Architecture Decisions

## ADR-001: 100% Native Rust Core and GPUI Frontend

Status: accepted (migrated from initial PyQt6 prototype).

The Rust crate owns discovery, inspection, plans, recipes, process execution,
builds, installation decisions, and native desktop user interface rendering using
GPUI (Zed Editor's framework). This eliminates all external Python/PyQt6 dependencies,
providing GPU-accelerated rendering, sub-millisecond tab switching, low memory
overhead, and a unified single binary distribution (`archbridge gui`).

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
