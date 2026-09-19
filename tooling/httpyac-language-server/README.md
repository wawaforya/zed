# Built-in httpyac language server

Vendored from the private `httpyac-language-server` repository at
`11a85e004f629dbfb19209e5ed658210d88c6750` (MIT). The TypeScript implementation
is retained; only its packaging and static catalog loading are changed.

Zed and the personal remote server embed `bundle/server.cjs`. The Rust adapter
atomically materializes it under Zed's language-server directory, named by its
SHA-256 digest, and launches it through `NodeRuntime`. Node can be downloaded
by Zed; npm, npm link, the extension, and a separately installed LSP are not
needed on the user's machine. An explicit `lsp.httpyac-language-server.binary`
configuration still overrides the bundled server. Legacy PATH installations
are not automatically selected.

The server never imports httpyac. `src/catalog.json` contains static catalogs
extracted from pinned httpyac 6.16.7; the execution engine is only a development
dependency. No request, plugin, script, project configuration or hook is
executed while analyzing documents. User-triggered tasks separately invoke
`httpyac` from the project environment (remote for remote projects).

To update the server:

```sh
cd tooling/httpyac-language-server
npm ci
npm test
node scripts/catalog.cjs --check
```

To update catalogs intentionally, run `npm run catalog` before `npm test`.
Commit the source, lockfile, catalog, bundle, digest and license files together.
Cargo builds use these checked-in artifacts and do not run npm. `npm test`
checks completion/navigation/formatting and starts a copy of the bundled
server in a temporary directory without node_modules, verifying that malicious
scripts/plugins and request URLs are not executed.

The catalog version is independent of the user's execution CLI version. XML
and GraphQL injection support remains optional, provided by their extensions.
Disable the old HTTP development extension after switching to the built-in
language to avoid duplicate snippets from the old extension.
