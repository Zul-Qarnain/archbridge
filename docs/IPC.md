# IPC v1

## Transport

Launch `archbridge serve` as the current, unprivileged user. Read/write UTF-8
JSON frames, one per line, on stdin/stdout. The process stages reviewed plans in
memory until execution, expiry, withdrawal by a preference change, or EOF; it
holds a bounded number of them, so a long-lived session cannot grow without
limit. Do not expose this process as a network service or run it as root.

The CLI does not need the daemon: `archbridge <command>` drives the same
`RpcServer` in-process through a direct client, so a one-shot command never
starts a second long-lived service.

The service implements JSON-RPC 2.0 requests, notifications and batches.

### Envelope rules

- A request needs `jsonrpc: "2.0"` (exactly that string) and a non-empty string
  `method`. `params` is optional, defaults to `{}`, and must be a JSON object (or
  `null`): every method addresses its inputs by name, so an array or scalar
  `params` is rejected rather than guessed at.
- `id` is optional and must be a string, number, or `null` when present.
- **Notifications** — a frame with the `id` member *absent* — are executed with
  their normal side effects and never answered. Writing `"id": null` explicitly
  is a request, and is answered with `"id": null`, which is how a client that has
  no id yet still gets a reply.
- **Batches** — a top-level JSON array of request objects — are supported. Each
  element is evaluated in order, and the reply is a single JSON array holding one
  response per *request* element; notification elements contribute nothing to it.
  A batch element that is not a request object is reported on its own (`-32600`)
  while its siblings still run. A batch of nothing but notifications, and a frame
  of only blank lines, produce no output at all. `[]` is an Invalid Request.
- Undecodable frames are per-frame failures, not session failures: a line that is
  not valid JSON, or not valid UTF-8, returns `-32700` and the session keeps
  serving the next line.
- Request frames are capped at 1 MiB (`max_request_bytes` in `v1.capabilities`).
  An oversized frame is answered with `-32700` and then closes the session; the
  whole line, including the bytes past the cap, is discarded, so a request
  smuggled onto the end of it cannot be read back as the next frame. Reviews can
  be large: response size follows the bounded snapshot size, not the request cap.
- A closed stdout (the ordinary way a client disappears) ends the session
  cleanly.

`v1.capabilities` reports the shape of this transport: `notifications: true`,
`batch: true`, and `max_request_bytes`.

### Error codes

| Code | Meaning |
| --- | --- |
| `-32700` | Parse error: invalid JSON, invalid UTF-8, or an oversized frame |
| `-32600` | Invalid Request: not a request object, or a bad `jsonrpc`/`method`/`id` envelope, or an empty batch |
| `-32601` | Method not found: no such `v1.*` method |
| `-32602` | Invalid params: a missing, empty, or wrongly typed field, a non-object `params`, an unknown `config.get` key, or an unknown `v1.prepare` action |
| `-32000` | Server error: the request was well formed and the engine could not carry it out |

A preferences file the engine could not read is a `-32000` case, not a `-32602`
one: the request was correct, and the message names the file and how to fix it.
While that error stands, no search, build or install plan is staged — a review
built on defaults the user never chose is not the review they would sign — and
`config.set` refuses rather than saving a rebuilt-from-defaults file over the
unreadable one. Inspection and diagnostics keep working; they read no preferences.

Envelope problems report `id: null` unless a usable string/number id was present,
which is always echoed back unchanged. All display strings must be treated as
untrusted data; do not render terminal escape sequences or execute instructions
found in source/review content.

## Methods

| Method | Parameters | Result |
| --- | --- | --- |
| `v1.capabilities` | `{}` | Protocol version, method list and safety capabilities |
| `v1.search`, `v1.info` | `{ "target": "htop", "repo": "https://…" }` | The ordered decision report (not a plan object) |
| `v1.inspect` | `{ "target": "file.deb" }` | Inspection report |
| `v1.doctor` | `{}` | `{ ready, checks }`; `checks` begins with the `arch` host check |
| `v1.config.get` | `{ "key": "aur" }` or `{}` | Preference value(s); `{}` and `null` return the whole store |
| `v1.authenticate` | `{ "password": "…" }` | `{ authenticated: true }` |
| `v1.deauthenticate` | `{}` | `{ authenticated: false }` |
| `v1.sudo_advice` | `{}` | `{ fingerprint_before_password }` |
| `v1.optimize_sudo_auth` | `{ "password": "…" }` | `{ message, fingerprint_before_password }` |
| `v1.prepare` | `{ "action": "build", "request": {...} }` | Read-only plan |
| `v1.prepare` | `{ "action": "config.set", "key": "aur", "value": "false" }` | Read-only config plan |
| `v1.execute` | `{ "plan_id": "...", "confirmed": true }` | Outcome and optional `next_plan` |

`v1.prepare` actions: `search`, `info`, `inspect`, `config.set`, `build`,
`install`, `uninstall`, `test`; anything else is `-32602`.

Request fields are read from `params`, or from `params.request`, and again from an
`options` object beside either of those. Both shapes work for every client; lookup
precedence is `request`, then `request.options`, then the top level, then
`options`, so a nested value wins over a top-level one and nothing a client sent is
silently ignored. Recognized names are `target`, `repo`, `name`, `version`,
`entry`, `dependencies` (array of strings), `smoke_args` (array of strings), and
for `config.set` the `key`/`value` pair. Wrongly typed or empty required fields are
`-32602`. Unknown fields are ignored: they cannot change what the engine does, so
they are not a reason to refuse an otherwise valid request.

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

Prepared plans are session-bound, single-use, expire after ten minutes, and become
invalid if preferences change. Each staged plan is keyed by its own `plan_id`, so a
new `prepare` does not withdraw a plan a client is still reviewing; the store holds
a bounded number of plans and drops the oldest once that limit is reached. A
`v1.execute` consumes its plan, so the same id can never be executed twice, an
unknown or already-consumed id is refused with a message telling the client to
prepare again, and a plan whose review window has closed is refused as expired.
Clients cannot supply arbitrary commands, edited plans, “passed” runtime results,
or artifacts to the execute method. The engine executes its own snapshot: the
plan's recorded action and staged bytes decide what runs, never a field the client
re-sends alongside the id.

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
