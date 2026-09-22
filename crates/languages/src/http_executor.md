# Built-in HTTP execution sessions

HTTP gutter actions and built-in tasks use `http_executor.cjs`, embedded by
`http.rs` into Zed and the remote server. Building either binary needs no npm
step. The context provider materializes the bridge in a process-owned temporary
directory; it does not load httpyac or run project code. For remote projects,
context construction, the bridge, Node, httpyac, and requests all run remotely.
The Rust HTTP language server remains independent and static-only.

## Requirements and use

Install Node and httpyac **on the machine executing the tasks**. Both must be
available in the project's terminal environment. The bridge supports httpyac
6.x and is tested against 6.16.7. It finds the package behind `httpyac` on PATH
(including npm global Windows launchers and Unix symlinks), then project and
standard global Node module locations. Nonstandard launchers/installations can
set `ZED_HTTPYAC_MODULE` to the absolute directory containing httpyac's
`package.json`. It does not download or install dependencies automatically.

Use the existing HTTP gutter action or Send tasks. A small client connects to a
long-lived Node worker, which owns `HttpFileStore` and calls `httpyac.send()`.
Repeated tasks reuse that store; sharing a terminal alone would not suffice.

### Tab-local response view

Built-in Send tasks open a response column inside the invoking HTTP file's tab,
without creating another pane or tab. The eye button in the editor toolbar and
`http: toggle response` show/hide it without sending a request. Drag the divider
to resize; double-click restores equal widths. Switching tabs or hiding the
column preserves its results and layout. A cloned editor starts without a response
column. Closing the source editor releases its results and cancels its active run.

The response column provides Raw, Pretty (selected by default), Headers, Request,
Tests and Log. The status line combines execution status with the selected response's
HTTP status, body size and duration. Headers retain duplicates; Request displays the
resolved request, including credentials. Tests and Log apply to the whole run.
Flat request tabs select the responses produced by Send All or dependencies.
Explicit WS / SSE requests default to Log when execution starts; Send All does too
if it contains a WS / SSE request. This uses httpyac's parsed protocol, including
its WebSocket / EventSource aliases, not the HTTP response's content type.
Explicit Headers / Request tasks and manual tab choices are preserved; later
messages never switch tabs automatically. Ordinary HTTP still defaults to Pretty.
Viewing, searching or exporting
results never resends requests. JSON Pretty preserves number literals and duplicate
keys. HTML is shown as source; binary bodies can be exported, not executed.

`Ctrl+F` in the response opens its own read-only search bar. The right-hand toolbar's
Copy copies the entire current section; Save explicitly writes the retained body bytes to a user-selected
local path, even for remote requests. Nothing is automatically persisted. The
source file remains the workspace item, so normal file saving still targets it.
The response text's context menu preserves the selection and provides Copy (selection
only), Copy All, Select All and Find Selection. Copy and Find Selection are disabled
without a selection. Input/password, confirmation and choice prompts appear inside
the response view.

The first version retains the latest 16 responses of a run, a 128-Ki-character
text preview and up to 512 KiB of body bytes per response; logs and test output
retain their latest 256 KiB each. Truncation is indicated, and Save Body is disabled
if the retained bytes are incomplete. These bounds apply to presentation, not to
httpyac's own execution cache or network downloads. A new send replaces the tab's
previous run. Concurrent sends in different tabs share the existing worker queue;
a second send in a busy tab is rejected until cancelled or completed.

`HTTP: Send request in terminal at line …` preserves the previous terminal workflow.
Native execution uses the resolved project/task environment plus terminal settings;
it does not start an interactive shell or evaluate its prompt/profile on every send.
Configure execution variables through project/task/terminal environment settings
rather than relying on per-command interactive-shell side effects. Remote execution
uses the existing transport's non-TTY command support; Node and httpyac still run
on the remote host. Collaboration guest execution is not supported.

```http
# @name ReqA
GET https://example.com/login

###
# @ref ReqA
GET https://example.com/data
Authorization: Bearer {{ReqA.Data.Token}}
```

Sending the second request repeatedly executes `ReqA` only when its result is
missing. `# @forceRef ReqA` refreshes it every time. Explicitly sending `ReqA`
also refreshes it. A bare `{{ReqA.Data.Token}}` can consume a previous result but
does not itself execute `ReqA`; use `@ref` for that dependency.

`HTTP: Reset execution sessions for this project` discards all environment
variants, including cookies and user sessions. It also interrupts active
requests; an interrupted request may already have reached the server.

Set `ZED_HTTPYAC_ENV=dev` (or `dev,local`, or a JSON string array) in the project
terminal environment/task env to select httpyac environments. Environment
ordering is retained. Projects, environment selections, different installed
engine versions, and changed process environment values have separate workers.
Per-shell/terminal metadata (including `STARSHIP_SESSION_KEY`, terminal IDs and
sizes, and `SSH_CLIENT`/`SSH_CONNECTION`/`SSH_TTY`) is excluded from the session
key, so a fresh task shell does not lose cached responses. This is an explicit
list, not a blanket exclusion of `SSH_*` or `STARSHIP_*`: credentials and
configuration such as `SSH_AUTH_SOCK` and `STARSHIP_CONFIG` still partition
workers. Requests in a worker are serialized, including simultaneous first clicks.

## Invalidation and safety

- State is in memory, not an on-disk response/token cache. The temporary IPC
  descriptor contains a loopback port, random authentication token, and owner
  information, not request results. Project scripts/configuration still have
  their usual httpyac permissions and may explicitly persist their own data.
- At the next send, changed HTTP documents (including previously imported files)
  are reparsed through httpyac's `HttpFileStore`. Like the VS Code integration,
  request blocks with identical parsed source keep their variables. Editing B's
  body/headers does not discard an unchanged A's result. Editing a block's own
  source, including comments, can invalidate its cached result.
- Editing/removing/renaming a named request also invalidates copied `@ref`
  variables and cached results referencing that name, transitively. Imported
  files are refreshed before assembling execution variables, so old values in
  the importing file cannot hide a changed dependency.
- Changes to global regions or blocks containing scripts/variable declarations
  conservatively clear all cached region variables and user sessions. Such code
  can affect other requests without an explicit named dependency. Arbitrary
  computed script/plugin dependencies cannot all be inferred; reset manually
  when a dependency is not expressed through named references.
- Changes to external body files, environment/config files, consulted non-HTTP
  files, and loaded project JS modules still restart the worker before sending.
  A file used both as an HTTP document and an external body is treated as an
  external input. Arbitrary filesystem access inside third-party plugins/scripts
  cannot all be tracked; reset manually after such changes.
- A new/modified input does not itself execute anything. Only an explicit Send
  task starts httpyac, project configuration, plugins, scripts, or requests.
- Workers stop when their Zed/remote-server owner exits, when the temporary
  directory disappears, or after 30 minutes idle. A failed or canceled execution
  also discards its worker to avoid reusing partially modified state.
- There is no automatic retry after a connection failure, failed assertion,
  HTTP 401, or interrupted request. The only internal reconnect is an explicit
  preflight restart response, before any script/request has begun. Token expiry
  is not inferred: send the login request, use `@forceRef`, or reset explicitly.
- Native tasks carry structured response, log, test and prompt events over piped
  stdio; the existing worker IPC remains authenticated. Terminal fallback still
  formats results and prompts in the terminal. There is no daemon response log.
  Client disconnect cancels the active worker. Native clients also require GUI
  heartbeats, so a broken remote pipe cannot leave a hidden request waiting forever.
  Long-lived streams occupy the worker queue until canceled or reset. Cancellation
  can discard shared worker state, not just the contents of one response column.
- IPC listens only on loopback and requires a per-worker random secret stored
  inside a private session subdirectory. Requests/paths are passed as JSON or
  environment values, never interpolated into shell command text.

## Tests

Install test dependencies only into a disposable directory, then run:

```sh
npm install --prefix target/httpyac-executor-test --ignore-scripts --no-audit --no-fund httpyac@6.16.7
python crates/languages/tests/test_http_executor.py target/httpyac-executor-test/node_modules/httpyac
cargo check -p languages -p zed -p remote_server
cargo test -p languages http::tests::request_tasks_use_the_persistent_executor
cargo test -p http_ui
ITERATIONS=20 cargo test -p http_ui
```

Python tests use a loopback-only HTTP fixture and temporary projects. They cover
cached references, body/header edits preserving dependencies, transitive
invalidation, global variable/script edits, request deletion/rename, imported
file edits, explicit/forced refresh, direct variable reuse, imports,
input/config/environment invalidation, concurrent starts, project isolation,
reset, cancellation, owner exit, authentication, output modes, scripts,
assertion failures, and actual shell commands with Unicode/spaces/quotes in
paths. On Windows they exercise cmd and PowerShell (plus pwsh if installed);
on Linux, sh and bash. Regression tests generate a new Starship session key in
each fresh shell and vary terminal/SSH metadata while asserting a single worker
and a single dependency request. Structured-mode tests also cover duplicate headers,
raw bytes, large/binary response bounds, script/response separation, assertions,
password prompts over stdin, Send All, reset, and disconnect cancellation. GPUI
tests cover editor-local ownership, source focus, local response search, hiding and
reopening, bounded results, prompt cleanup, and native-task source routing and
preflight errors. No external request endpoint is used.
