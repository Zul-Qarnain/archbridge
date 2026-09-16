# IPC v1

## Transport

Launch `archbridge serve` as the current, unprivileged user. Read/write one UTF-8
JSON object per line on stdin/stdout. The process keeps one prepared plan in memory
until execution, expiry, replacement by a new plan, or EOF. Do not expose this
process as a network service or run it as root.

The service implements the request/response subset of JSON-RPC 2.0. Requests need
`jsonrpc: "2.0"`, an `id` (string/number/null), and `method`; `params` defaults to
`{}`. Batches and notifications are explicitly rejected. A malformed JSON line
returns `-32700`; invalid envelopes return `-32600`; operation errors currently
use `-32000` with a concrete message. Oversized requests close the session.

Request frames are capped at 1 MiB. Reviews can be large: response size follows
the bounded snapshot size, not the 1 MiB request cap. All display strings must be
treated as untrusted data; do not render terminal escape sequences or execute
instructions found in source/review content.

## Methods

| Method | Parameters | Result |
| --- | --- | --- |
| `v1.capabilities` | `{}` | Protocol/build version, methods and safety capabilities |
| `v1.search`, `v1.info` | Request object | Ordered decision report |
| `v1.inspect` | `{ "target": "file.deb" }` | Inspection report |
| `v1.doctor` | `{}` | `{ ready, checks }` |
| `v1.config.get` | `{ "key": "aur" }` or `{}` | Preference value(s) |
| `v1.prepare` | `{ "action": "build", "request": {...} }` | Read-only plan |
| `v1.prepare` | `{ "action": "config.set", "key": "aur", "value": "false" }` | Read-only config plan |
| `v1.execute` | `{ "plan_id": "...", "confirmed": true }` | Outcome and optional `next_plan` |

Request objects accept `target`, optional `repo`, `options`, and
`allow_unreviewed_scripts` (false by default; true cannot enable foreign execution).
Options accept `name`, `version`, `entry`, `smoke_args` and `dependencies`.
Unknown request/option fields are rejected.

## Example session

```json
{"jsonrpc":"2.0","id":1,"method":"v1.prepare","params":{"action":"build","request":{"target":"./source","options":{"entry":"my-tool"}}}}
```

The response's `result` is a plan containing:

- `protocol_version`, `plan_id`, `action`, `summary`;
- `steps`: program, argument vector, purpose, timeout for each child process;
- `filesystem_changes`: writes that happen only upon execution;
- `reviews`: path, SHA-256, byte count, executable flag, UTF-8 text if available;
- `warnings`, optional discovery `decision`, `downstream`, and optional `blocked`.

Display the entire plan/reviews. A dry-run stops here. The PyQt6 GUI and CLI both
use this same contract. After explicit approval:

```json
{"jsonrpc":"2.0","id":2,"method":"v1.execute","params":{"plan_id":"ID_FROM_PREPARE","confirmed":true}}
```

Prepared plans are session-bound, single-use, expire after ten minutes, and become
invalid if preferences change. Clients cannot supply arbitrary commands, edited
plans, “passed” runtime results, or artifacts to the execute method. The engine
executes its own snapshot. A new `prepare` replaces any previous pending plan.

If `next_plan` is non-null, display it and obtain new approval before executing
that ID. Build success is not runtime success. Runtime failure produces `ok:false`
and no host-install continuation. Review/consent is a frontend responsibility;
the RPC capability grants the same authority as the local user, not an untrusted
remote caller. Never make a frontend auto-confirm arbitrary incoming requests.

## Dry-run semantics and dynamic stages

`v1.prepare` has no filesystem writes, config initialization, package installs,
chroot creation, or script execution. It may perform HTTPS GETs and read-only
metadata queries and download bounded bytes into memory. A direct `inspect` may
use temporary files for `readelf`; it is not part of the dry-run preparation path.

The exact current-stage command plan is returned together with explicit
conditional downstream descriptions. Artifact filenames/hashes and runtime
entry-point inference happen only after artifacts exist; they are not invented
in the initial plan. `sudo`, `curl`, package managers and nspawn never inherit the
RPC stdin. They cannot consume the next request as an interactive answer.

The CLI always prints plans, including with `--yes`. An integrating GUI must do
the equivalent. There is no D-Bus interface. The graphical frontend is PyQt6 and
remains a client of this JSON-RPC boundary.
