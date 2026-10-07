# Built-in httpyac language server

The production server is Rust: `crates/httpyac_lsp`. Zed and the personal
remote server link the same library and launch their own executable with
`--httpyac-language-server`. This mode runs before GUI, single-instance,
console attachment, or remote proxy initialization. Only LSP messages are
written to stdout; errors are written to stderr.

`script/bundle-windows-custom.ps1` includes the server in Zed.exe automatically.
`script/package-install-remote-server-linux` includes it in remote_server.
No extra LSP executable, Node, npm, download, or installation is required for
HTTP editing. Remote HTTP files are analyzed on the remote machine. Build
both ends from the same revision and release channel, then reconnect after
installing an updated remote server. An explicit
`lsp.httpyac-language-server.binary` configuration still overrides the built-in
server. Legacy PATH installations are not automatically selected.

Request execution is separate: user-triggered tasks connect to a persistent
Node worker using the installed httpyac library in the project's environment,
on the remote host for remote projects. Named responses can be reused across
tasks; a Reset execution sessions task explicitly discards them. Node and
httpyac are required only for execution. See
[HTTP execution sessions](../languages/src/http_executor.md) for installation,
environment selection, invalidation, and tests. Analysis never runs HTTP
requests, JavaScript, project configuration, hooks, or plugins.

The index uses UTF-16 protocol positions and preserves request bodies and
scripts during formatting. Open buffers override disk contents. Watched-file
notifications and a two-second idle polling fallback refresh imports and
workspace symbols; typing does not rescan workspace directories. Unchanged
disk files reuse their indexes. References and rename resolve definitions
within each document's import graph, including reverse importers; ambiguous
or colliding renames are rejected. The index skips `.git`, `node_modules`,
`target`, symlink directories during discovery, and disk documents larger
than 16 MiB. A 10,000-document import traversal limit disables rename if hit.

Unknown variables are not proof of an error: environments and plugins can
supply them at execution time. Unresolved names produce hints, likely typos
produce qualified warnings, and recognizable literal script exports suppress
those hints without executing scripts. Saved `http-client.env.json` and
`http-client.private.env.json` files are indexed from the document's ancestors up to
its workspace root, including both files under the root's `.zed` directory. The index
retains only variable names. `$shared` and the document's selected environments supply
completion and suppress unresolved-variable hints; unselected environments do not.
Watched-file notifications and idle polling refresh saved configuration changes.
Invalid configuration is reported without exposing variable values. Dynamic JavaScript
configuration and plugin environments remain runtime-only.

Zed synchronizes each file's selection through the advertised `httpyac.setEnvironment`
`workspace/executeCommand` command. Its single argument contains `path` (an absolute
path on the project host), `environments` (an ordered string array), and `revision`
(a monotonically increasing integer per document). Stale revisions are ignored. Path
to URI conversion occurs on the server, including when a Windows client edits a Unix
remote project. The command only updates static analysis; it never executes code or
requests. File selections are remembered in the project session and replayed after
language server restarts; no environment selection is written to disk.

## Validation

Run from the repository root:

```sh
cargo test -p httpyac_lsp
cargo build -p httpyac_lsp --example stdio --release
python crates/httpyac_lsp/tests/test_stdio.py target/release/examples/stdio
```

On Windows the example is `target/release/examples/stdio.exe`. Its release
build uses the Windows GUI subsystem, like Zed. The same protocol test can
be run against the actual packaged `Zed.exe` or Linux `remote_server`; it tests
stdio framing, Unicode, incremental changes, imported completion/navigation,
isolated rename, file-change diagnostics, polling, shutdown, and absence of
script/plugin/network execution. Tests use temporary files, not real requests.

## Catalog and attribution

The Rust implementation was migrated from the private
`httpyac-language-server` repository at
`11a85e004f629dbfb19209e5ed658210d88c6750` (MIT); its attribution is retained
in `LICENSE`. The old TypeScript implementation, tests, JavaScript bundle,
and npm build tooling have been removed.

The Rust server embeds `src/catalog.json`, extracted from httpyac 6.16.7.
Its license is retained alongside it in `src/LICENSE-httpyac-catalog`.
The catalog version is independent of the user's execution CLI version.
Catalog updates must preserve this attribution and be validated with the
Rust tests and stdio test above. Cargo builds never invoke npm.

XML and GraphQL injection support remains optional, provided by their
extensions. The LSP's template completions remain enabled; the separate
built-in HTTP snippet-provider source has been removed.
