# IPC v1

## Transport

Launch `archbridge serve` as the current, unprivileged user. Read/write one UTF-8
JSON object per line on stdin/stdout. The process holds every plan it has staged
in memory until that plan is executed, expires, or is withdrawn; nothing is
written to disk at prepare time. Do not expose this process as a network service
or run it as root.

The CLI does not need the daemon: `archbridge <command>` drives the same
`RpcServer` in-process through a direct client, so a one-shot command never
starts a second long-lived service.

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
| `v1.capabilities` | `{}` | Protocol version, method list and safety capabilities |
| `v1.search`, `v1.info` | Request object | Ordered decision report |
| `v1.inspect` | `{ "target": "file.deb" }` | Inspection report (also staged as an `inspect` plan) |
| `v1.doctor` | `{}` | `{ ready, checks }`; `checks` begins with the `arch` host check |
| `v1.config.get` | `{ "key": "aur" }` or `{}` | Preference value(s); `all` returns the whole object |
| `v1.authenticate` | `{ "password": "..." }` | Caches a sudo timestamp for non-interactive steps |
| `v1.deauthenticate` | `{}` | Invalidates the cached sudo credential |
| `v1.sudo_advice` | `{}` | Whether this host waits for a fingerprint before prompting |
| `v1.optimize_sudo_auth` | `{ "password": "..." }` | Drops the fingerprint wait from `/etc/pam.d/sudo` |
| `v1.prepare` | `{ "action": "build", "request": {...} }` | Staged plan; actions are `search`, `info`, `inspect`, `build`, `install`, `uninstall`, `test` |
| `v1.prepare` | `{ "action": "config.set", "key": "aur", "value": "false" }` | Config plan, passed at the top level rather than inside `request` |
| `v1.execute` | `{ "plan_id": "...", "confirmed": true }` | Outcome and optional `next_plan` |

Request objects accept `target` and an optional `repo`. Options accept `name`,
`version`, `entry`, `smoke_args` and `dependencies`. Fields the method does not
use are ignored rather than rejected — flag validation is the CLI's job, and the
CLI exits 2 on an unknown flag.

There is no `allow_unreviewed_scripts` option. Foreign script execution is not a
capability the protocol can be asked to enable.

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

Display the entire plan/reviews. A dry-run stops here. The GPUI desktop GUI and CLI both
use this same contract. After explicit approval:

```json
{"jsonrpc":"2.0","id":2,"method":"v1.execute","params":{"plan_id":"ID_FROM_PREPARE","confirmed":true}}
```

Prepared plans are session-bound, single-use, expire ten minutes after they were
staged, and are all withdrawn when a preference changes — a plan reviewed under
the old source settings is no longer the plan the user agreed to. Clients cannot
supply arbitrary commands, edited plans, “passed” runtime results, or artifacts
to the execute method: the engine executes its own snapshot. Staging a new plan
does not withdraw the ones already under review.

If `next_plan` is non-null, display it and obtain new approval before executing
that ID. Build success is not runtime success. Runtime failure produces `ok:false`
and no host-install continuation. Review/consent is a frontend responsibility;
the RPC capability grants the same authority as the local user, not an untrusted
remote caller. Never make a frontend auto-confirm arbitrary incoming requests.

## Dry-run semantics and dynamic stages

`v1.prepare` creates no build directory, config file, chroot or live change, and
executes no package script. It may perform HTTPS GETs and read-only metadata
queries, download bounded bytes into memory, and — for `inspect` and for a foreign
`build` — unpack payload members into a private temporary directory that is
removed again. Library needs come from reading the ELF headers of those extracted
files in memory; no `readelf`, `ldd`, interpreter or package script is ever run.

The exact current-stage command plan is returned together with explicit
conditional downstream descriptions. Artifact filenames/hashes and runtime
entry-point inference happen only after artifacts exist; they are not invented
in the initial plan. `sudo`, `curl`, package managers and nspawn never inherit the
RPC stdin. They cannot consume the next request as an interactive answer.

The CLI always prints plans, including with `--yes`. An integrating GUI must do
the equivalent. There is no D-Bus interface. The graphical frontend is native GPUI and
interacts with this verified core.
