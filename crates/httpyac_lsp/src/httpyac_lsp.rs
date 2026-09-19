mod features;
mod index;
mod workspace;

#[cfg(test)]
mod tests;

pub const ARGUMENT: &str = "--httpyac-language-server";

/// Dispatch before GUI, single-instance, console attachment, or remote proxy setup.
pub fn run_if_requested() -> Option<anyhow::Result<()>> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(ARGUMENT)) {
        return None;
    }
    Some((|| {
        anyhow::ensure!(
            arguments.all(|argument| argument == "--stdio"),
            "Unexpected HTTP language server argument"
        );
        run_stdio()
    })())
}

pub fn run_stdio() -> anyhow::Result<()> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(64);
    std::thread::Builder::new()
        .name("httpyac-lsp-input".into())
        .spawn(move || {
            let mut input = std::io::stdin().lock();
            loop {
                let message = read_message(&mut input);
                let done = !matches!(&message, Ok(Some(_)));
                if sender.send(message).is_err() || done {
                    break;
                }
            }
        })?;
    let mut server = Server::new()?;
    let mut output = std::io::stdout().lock();
    loop {
        match receiver.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(Ok(Some(message))) => {
                if server.handle(message, &mut output)? {
                    return Ok(());
                }
            }
            Ok(Ok(None)) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Ok(Err(error)) => return Err(error),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if server.initialized && !server.shutdown {
                    server.refresh(&mut output)?;
                }
            }
        }
    }
}

fn read_message(input: &mut impl std::io::BufRead) -> anyhow::Result<Option<serde_json::Value>> {
    let mut length = None;
    let mut header_bytes = 0;
    loop {
        let mut line = String::new();
        // Bound malformed headers as well as bodies before allocating their contents.
        let read =
            std::io::BufRead::read_line(&mut std::io::Read::take(&mut *input, 8193), &mut line)?;
        if read == 0 {
            anyhow::ensure!(header_bytes == 0, "Truncated LSP header");
            return Ok(None);
        }
        header_bytes += read;
        anyhow::ensure!(header_bytes <= 8192, "LSP header too large");
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("Content-Length")
        {
            anyhow::ensure!(length.is_none(), "Duplicate Content-Length");
            length = Some(value.trim().parse::<usize>()?);
        }
    }
    let length = length.ok_or_else(|| anyhow::anyhow!("Missing Content-Length"))?;
    anyhow::ensure!(length <= 32 * 1024 * 1024, "LSP message too large");
    let mut body = vec![0; length];
    std::io::Read::read_exact(input, &mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

fn write_message(
    output: &mut impl std::io::Write,
    message: serde_json::Value,
) -> anyhow::Result<()> {
    let body = serde_json::to_vec(&message)?;
    write!(output, "Content-Length: {}\r\n\r\n", body.len())?;
    output.write_all(&body)?;
    output.flush()?;
    Ok(())
}

struct Server {
    workspace: workspace::Workspace,
    catalog: features::Catalog,
    initialized: bool,
    shutdown: bool,
    published: std::collections::BTreeMap<String, serde_json::Value>,
    dynamic_watchers: bool,
    watchers: serde_json::Value,
    watcher_generation: u64,
}

impl Server {
    fn new() -> anyhow::Result<Self> {
        Ok(Self {
            workspace: Default::default(),
            catalog: features::Catalog::load()?,
            initialized: false,
            shutdown: false,
            published: Default::default(),
            dynamic_watchers: false,
            watchers: serde_json::Value::Null,
            watcher_generation: 0,
        })
    }

    fn handle(
        &mut self,
        message: serde_json::Value,
        output: &mut impl std::io::Write,
    ) -> anyhow::Result<bool> {
        let Some(method) = message["method"].as_str() else {
            if !message["error"].is_null() {
                eprintln!("HTTP LSP client response: {}", message["error"]);
            }
            return Ok(false);
        };
        let id = message.get("id");
        let params = &message["params"];
        if method == "exit" {
            anyhow::ensure!(self.shutdown, "Client exited without shutdown");
            return Ok(true);
        }
        if let Some(id) = id {
            let result = if method == "initialize" && !self.initialized {
                self.dynamic_watchers = params
                    .pointer("/capabilities/workspace/didChangeWatchedFiles/dynamicRegistration")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                if let Some(folders) = params["workspaceFolders"].as_array() {
                    for folder in folders {
                        if let Some(uri) = folder["uri"].as_str() {
                            self.workspace.roots.insert(workspace::normalize_uri(uri));
                        }
                    }
                } else if let Some(uri) = params["rootUri"].as_str() {
                    self.workspace.roots.insert(workspace::normalize_uri(uri));
                }
                self.initialized = true;
                Ok(
                    serde_json::json!({"serverInfo": {"name": "httpyac-language-server", "version": env!("CARGO_PKG_VERSION")}, "capabilities": {
                        "positionEncoding": "utf-16",
                        "textDocumentSync": {"openClose": true, "change": 2, "save": {"includeText": false}},
                        "completionProvider": {"triggerCharacters": ["#", "@", "{", "<", "/", "\\", ":"]},
                        "definitionProvider": true, "referencesProvider": true, "renameProvider": {"prepareProvider": true},
                        "documentLinkProvider": {"resolveProvider": false}, "hoverProvider": true, "codeActionProvider": true,
                        "documentFormattingProvider": true, "workspaceSymbolProvider": true,
                        "workspace": {"workspaceFolders": {"supported": true, "changeNotifications": true}}
                    }}),
                )
            } else if !self.initialized {
                Err((-32002, "Server not initialized".to_owned()))
            } else if self.shutdown {
                Err((-32600, "Server has shut down".to_owned()))
            } else if method == "shutdown" {
                self.shutdown = true;
                Ok(serde_json::Value::Null)
            } else {
                if matches!(
                    method,
                    "textDocument/rename"
                        | "textDocument/prepareRename"
                        | "textDocument/references"
                        | "workspace/symbol"
                ) {
                    self.refresh(output)?;
                }
                self.request(method, params)
            };
            write_message(
                output,
                match result {
                    Ok(result) => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err((code, message)) => {
                        serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
                    }
                },
            )?;
        } else if self.initialized && !self.shutdown {
            if let Err(error) = self.notification(method, params, output) {
                write_message(
                    output,
                    serde_json::json!({"jsonrpc": "2.0", "method": "window/logMessage", "params": {"type": 1, "message": format!("HTTP LSP: {error:#}")}}),
                )?;
            }
        }
        Ok(false)
    }

    fn notification(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        output: &mut impl std::io::Write,
    ) -> anyhow::Result<()> {
        match method {
            "initialized" => self.refresh(output)?,
            "textDocument/didOpen" => {
                let document = &params["textDocument"];
                self.workspace.update(
                    string(document, "uri")?,
                    string(document, "text")?.to_owned(),
                    document["version"]
                        .as_i64()
                        .ok_or_else(|| anyhow::anyhow!("Missing document version"))?,
                );
                self.refresh(output)?;
            }
            "textDocument/didChange" => {
                let uri = workspace::normalize_uri(string(&params["textDocument"], "uri")?);
                let version = params["textDocument"]["version"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("Missing document version"))?;
                if self
                    .workspace
                    .open
                    .get(&uri)
                    .is_some_and(|previous| version <= *previous)
                {
                    return Ok(());
                }
                anyhow::ensure!(
                    self.workspace.open.contains_key(&uri),
                    "Document is not open"
                );
                let mut text = self
                    .workspace
                    .documents
                    .get(&uri)
                    .ok_or_else(|| anyhow::anyhow!("Unknown document"))?
                    .text
                    .clone();
                for change in params["contentChanges"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("Missing contentChanges"))?
                {
                    let replacement = string(change, "text")?;
                    if let Some(range) = change.get("range") {
                        let range: index::Range = serde_json::from_value(range.clone())?;
                        let start = index::offset(&text, range.start)
                            .ok_or_else(|| anyhow::anyhow!("Invalid UTF-16 range start"))?;
                        let end = index::offset(&text, range.end)
                            .ok_or_else(|| anyhow::anyhow!("Invalid UTF-16 range end"))?;
                        anyhow::ensure!(start <= end, "Reversed change range");
                        text.replace_range(start..end, replacement);
                    } else {
                        text = replacement.to_owned();
                    }
                }
                self.workspace.update(&uri, text, version);
                self.refresh_mode(output, false)?;
            }
            "textDocument/didClose" => {
                let uri = workspace::normalize_uri(string(&params["textDocument"], "uri")?);
                self.workspace.close(&uri);
                self.published.remove(&uri);
                write_message(
                    output,
                    serde_json::json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": uri, "diagnostics": []}}),
                )?;
                self.refresh(output)?;
            }
            "workspace/didChangeWorkspaceFolders" => {
                if let Some(removed) = params["event"]["removed"].as_array() {
                    for folder in removed {
                        self.workspace
                            .roots
                            .remove(&workspace::normalize_uri(string(folder, "uri")?));
                    }
                }
                if let Some(added) = params["event"]["added"].as_array() {
                    for folder in added {
                        self.workspace
                            .roots
                            .insert(workspace::normalize_uri(string(folder, "uri")?));
                    }
                }
                self.refresh(output)?;
            }
            "workspace/didChangeWatchedFiles" => {
                for change in params["changes"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("Missing file changes"))?
                {
                    self.workspace.invalidate(string(change, "uri")?);
                }
                self.refresh(output)?;
            }
            "textDocument/didSave" => {
                self.workspace
                    .invalidate(string(&params["textDocument"], "uri")?);
                self.refresh(output)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn refresh(&mut self, output: &mut impl std::io::Write) -> anyhow::Result<()> {
        self.refresh_mode(output, true)
    }

    fn refresh_mode(
        &mut self,
        output: &mut impl std::io::Write,
        scan_roots: bool,
    ) -> anyhow::Result<()> {
        // One owner applies updates and publishes diagnostics in order; background disk reads
        // can never overwrite an open buffer or publish a result from an older snapshot.
        self.workspace.synchronize(scan_roots);
        for (uri, version) in &self.workspace.open {
            let Some(document) = self.workspace.documents.get(uri) else {
                continue;
            };
            let params = serde_json::json!({"uri": uri, "version": version, "diagnostics": features::diagnostics(&self.workspace, document, &self.catalog)});
            if self.published.get(uri) != Some(&params) {
                write_message(
                    output,
                    serde_json::json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": params}),
                )?;
                self.published.insert(uri.clone(), params);
            }
        }
        if self.dynamic_watchers {
            let mut watchers =
                vec![serde_json::json!({"globPattern": "**/*.{http,rest}", "kind": 7})];
            let mut directories = std::collections::BTreeSet::new();
            for document in self.workspace.documents.values() {
                for reference in &document.paths {
                    if let Some(path) =
                        workspace::resolve_path(&document.uri, &reference.occurrence.name)
                        && let Some(parent) = path.parent().and_then(workspace::file_uri)
                    {
                        directories.insert(parent);
                    }
                }
            }
            watchers.extend(directories.into_iter().map(|uri| serde_json::json!({"globPattern": {"baseUri": uri, "pattern": "*"}, "kind": 7})));
            let watchers = serde_json::Value::Array(watchers);
            if watchers != self.watchers {
                if self.watcher_generation > 0 {
                    write_message(
                        output,
                        serde_json::json!({"jsonrpc": "2.0", "id": format!("httpyac-unwatch-{}", self.watcher_generation), "method": "client/unregisterCapability", "params": {"unregisterations": [{"id": format!("httpyac-watch-{}", self.watcher_generation), "method": "workspace/didChangeWatchedFiles"}]}}),
                    )?;
                }
                self.watcher_generation += 1;
                write_message(
                    output,
                    serde_json::json!({"jsonrpc": "2.0", "id": format!("httpyac-watch-{}", self.watcher_generation), "method": "client/registerCapability", "params": {"registrations": [{"id": format!("httpyac-watch-{}", self.watcher_generation), "method": "workspace/didChangeWatchedFiles", "registerOptions": {"watchers": watchers}}]}}),
                )?;
                self.watchers = watchers;
            }
        }
        Ok(())
    }

    fn request(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, (i32, String)> {
        if ![
            "workspace/symbol",
            "textDocument/completion",
            "textDocument/definition",
            "textDocument/references",
            "textDocument/prepareRename",
            "textDocument/rename",
            "textDocument/documentLink",
            "textDocument/hover",
            "textDocument/formatting",
            "textDocument/codeAction",
        ]
        .contains(&method)
        {
            return Err((-32601, format!("Unsupported method: {method}")));
        }
        let result = (|| -> anyhow::Result<serde_json::Value> {
            if method == "workspace/symbol" {
                return Ok(serde_json::json!(features::symbols(
                    &self.workspace,
                    string(params, "query")?
                )));
            }
            let uri = workspace::normalize_uri(string(&params["textDocument"], "uri")?);
            let document = self
                .workspace
                .documents
                .get(&uri)
                .ok_or_else(|| anyhow::anyhow!("Unknown document"))?;
            let position = || -> anyhow::Result<index::Position> {
                Ok(serde_json::from_value(params["position"].clone())?)
            };
            Ok(match method {
                "textDocument/completion" => serde_json::json!(features::completions(
                    &self.workspace,
                    document,
                    position()?,
                    &self.catalog
                )),
                "textDocument/definition" => {
                    serde_json::json!(features::definition(&self.workspace, document, position()?))
                }
                "textDocument/references" => serde_json::json!(features::references(
                    &self.workspace,
                    &uri,
                    position()?,
                    params["context"]["includeDeclaration"]
                        .as_bool()
                        .unwrap_or(false)
                )),
                "textDocument/prepareRename" => self
                    .workspace
                    .at(&uri, position()?)
                    .filter(|binding| binding.candidates.len() == 1)
                    .map(|binding| serde_json::json!(binding.range))
                    .unwrap_or_default(),
                "textDocument/rename" => {
                    self.workspace
                        .rename(&uri, position()?, string(params, "newName")?)?
                }
                "textDocument/documentLink" => serde_json::json!(features::links(document)),
                "textDocument/hover" => {
                    features::hover(&self.workspace, document, position()?, &self.catalog)
                }
                "textDocument/formatting" => serde_json::json!(features::formatting(document)),
                "textDocument/codeAction" => serde_json::json!(features::actions(
                    &self.workspace,
                    document,
                    params["context"]["diagnostics"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    serde_json::from_value(params["range"].clone())?
                )),
                _ => return Err(anyhow::anyhow!("Unsupported method: {method}")),
            })
        })();
        result.map_err(|error| {
            (
                if error.to_string().starts_with("Unsupported method:") {
                    -32601
                } else {
                    -32602
                },
                error.to_string(),
            )
        })
    }
}

fn string<'a>(value: &'a serde_json::Value, key: &str) -> anyhow::Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing string field: {key}"))
}
