#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Symbol {
    pub uri: String,
    pub request: bool,
    pub index: usize,
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub candidates: Vec<Symbol>,
    pub range: crate::index::Range,
    pub response: bool,
    pub declaration: bool,
}

#[derive(Clone, Default)]
pub struct Workspace {
    pub documents: std::collections::BTreeMap<String, crate::index::Document>,
    pub open: std::collections::BTreeMap<String, i64>,
    pub roots: std::collections::BTreeSet<String>,
    pub imports: std::collections::BTreeMap<String, Vec<String>>,
    pub dependents: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    root_files: std::collections::BTreeSet<String>,
    disk_stamps: std::collections::BTreeMap<String, (std::time::SystemTime, u64)>,
    complete: bool,
}

pub fn file_uri(path: &std::path::Path) -> Option<String> {
    url::Url::from_file_path(normalize_path(path))
        .ok()
        .map(|uri| uri.to_string())
}

pub fn normalize_uri(uri: &str) -> String {
    url::Url::parse(uri)
        .ok()
        .and_then(|uri| uri.to_file_path().ok())
        .and_then(|path| file_uri(&path))
        .unwrap_or_else(|| uri.to_owned())
}

fn normalize_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut result = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !result.pop() && !path.is_absolute() {
                    result.push("..");
                }
            }
            _ => result.push(component.as_os_str()),
        }
    }
    result
}

pub fn resolve_path(uri: &str, reference: &str) -> Option<std::path::PathBuf> {
    if reference.contains("{{") || crate::index::matches(r"(?i)^[a-z][a-z0-9+.-]*://", reference) {
        return None;
    }
    let path = url::Url::parse(uri).ok()?.to_file_path().ok()?;
    let reference = if cfg!(windows) {
        reference.replace('/', "\\")
    } else {
        reference.to_owned()
    };
    let target = if reference == "~"
        || reference.starts_with("~/")
        || (cfg!(windows) && reference.starts_with("~\\"))
    {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
        std::path::PathBuf::from(home).join(reference.get(2..).unwrap_or_default())
    } else {
        path.parent()?.join(reference)
    };
    Some(normalize_path(&target))
}

impl Workspace {
    pub fn update(&mut self, uri: &str, text: String, version: i64) {
        let uri = normalize_uri(uri);
        if self
            .open
            .get(&uri)
            .is_some_and(|previous| *previous >= version)
        {
            return;
        }
        self.open.insert(uri.clone(), version);
        self.documents
            .insert(uri.clone(), crate::index::Document::parse(uri, text));
    }

    pub fn close(&mut self, uri: &str) {
        let uri = normalize_uri(uri);
        self.open.remove(&uri);
        self.documents.remove(&uri);
        self.disk_stamps.remove(&uri);
    }

    pub fn refresh(&mut self) {
        self.synchronize(true);
    }

    pub fn invalidate(&mut self, uri: &str) {
        self.disk_stamps.remove(&normalize_uri(uri));
    }

    pub fn synchronize(&mut self, scan_roots: bool) {
        self.complete = true;
        if scan_roots {
            self.root_files.clear();
            for root in &self.roots {
                if let Ok(uri) = url::Url::parse(root)
                    && let Ok(path) = uri.to_file_path()
                {
                    scan(&path, &mut self.root_files);
                }
            }
        }
        let mut keep = self.root_files.clone();
        keep.extend(self.open.keys().cloned());
        self.imports.clear();
        self.dependents.clear();
        let mut pending: Vec<_> = keep.iter().cloned().collect();
        let mut visited = std::collections::BTreeSet::new();
        while let Some(uri) = pending.pop() {
            if !visited.insert(uri.clone()) {
                continue;
            }
            if visited.len() > 10_000 {
                self.complete = false;
                eprintln!("HTTP index reached its 10000-document limit; rename is disabled");
                break;
            }
            if !self.open.contains_key(&uri) {
                let path = url::Url::parse(&uri)
                    .ok()
                    .and_then(|uri| uri.to_file_path().ok());
                let stamp = path
                    .as_ref()
                    .and_then(|path| std::fs::metadata(path).ok())
                    .and_then(|metadata| {
                        metadata
                            .modified()
                            .ok()
                            .map(|modified| (modified, metadata.len()))
                    });
                let unchanged = stamp.is_some()
                    && self.disk_stamps.get(&uri) == stamp.as_ref()
                    && self.documents.contains_key(&uri);
                if !unchanged {
                    self.disk_stamps.remove(&uri);
                    if let Some(stamp) = stamp {
                        self.disk_stamps.insert(uri.clone(), stamp);
                    }
                    match path.and_then(|path| read_file(&path)) {
                        Some(text) => {
                            if !self
                                .documents
                                .get(&uri)
                                .is_some_and(|document| document.text == text)
                            {
                                self.documents.insert(
                                    uri.clone(),
                                    crate::index::Document::parse(uri.clone(), text),
                                );
                            }
                        }
                        None => {
                            self.documents.remove(&uri);
                            continue;
                        }
                    }
                }
            }
            let Some(document) = self.documents.get(&uri) else {
                continue;
            };
            let imports: Vec<_> = document
                .paths
                .iter()
                .filter(|reference| reference.kind == "import")
                .filter_map(|reference| resolve_path(&uri, &reference.occurrence.name))
                .filter_map(|path| file_uri(&path))
                .collect();
            for imported in &imports {
                keep.insert(imported.clone());
                self.dependents
                    .entry(imported.clone())
                    .or_default()
                    .insert(uri.clone());
                pending.push(imported.clone());
            }
            self.imports.insert(uri, imports);
        }
        self.documents.retain(|uri, _| keep.contains(uri));
        self.disk_stamps.retain(|uri, _| keep.contains(uri));
    }

    pub fn reachable(&self, uri: &str) -> std::collections::BTreeSet<String> {
        let mut result = std::collections::BTreeSet::new();
        let mut pending = vec![uri.to_owned()];
        while let Some(uri) = pending.pop() {
            if !result.insert(uri.clone()) {
                continue;
            }
            if let Some(imports) = self.imports.get(&uri) {
                pending.extend(imports.iter().cloned());
            }
        }
        result
    }

    pub fn affected(&self, uri: &str) -> std::collections::BTreeSet<String> {
        let mut result = std::collections::BTreeSet::new();
        let mut pending = vec![uri.to_owned()];
        while let Some(uri) = pending.pop() {
            if !result.insert(uri.clone()) {
                continue;
            }
            if let Some(dependents) = self.dependents.get(&uri) {
                pending.extend(dependents.iter().cloned());
            }
        }
        result
    }

    pub fn definitions(&self, uri: &str, name: &str, request: bool) -> Vec<Symbol> {
        self.reachable(uri)
            .iter()
            .filter_map(|uri| self.documents.get(uri))
            .flat_map(|document| {
                if request {
                    document
                        .requests
                        .iter()
                        .enumerate()
                        .filter(|(_, definition)| {
                            definition.explicit && definition.symbol.name == name
                        })
                        .map(|(index, _)| Symbol {
                            uri: document.uri.clone(),
                            request: true,
                            index,
                        })
                        .collect::<Vec<_>>()
                } else {
                    document
                        .variables
                        .iter()
                        .enumerate()
                        .filter(|(_, definition)| {
                            definition.name == name
                                || name
                                    .strip_prefix(&definition.name)
                                    .is_some_and(|suffix| suffix.starts_with('.'))
                        })
                        .map(|(index, _)| Symbol {
                            uri: document.uri.clone(),
                            request: false,
                            index,
                        })
                        .chain(
                            document
                                .requests
                                .iter()
                                .enumerate()
                                .filter(|(_, definition)| {
                                    definition.explicit
                                        && (definition.response_name == name
                                            || name
                                                .strip_prefix(&definition.response_name)
                                                .is_some_and(|suffix| suffix.starts_with('.')))
                                })
                                .map(|(index, _)| Symbol {
                                    uri: document.uri.clone(),
                                    request: true,
                                    index,
                                }),
                        )
                        .collect()
                }
            })
            .collect()
    }

    pub fn occurrence(&self, symbol: &Symbol) -> &crate::index::Occurrence {
        let document = &self.documents[&symbol.uri];
        if symbol.request {
            &document.requests[symbol.index].symbol
        } else {
            &document.variables[symbol.index]
        }
    }

    pub fn binding(
        &self,
        uri: &str,
        occurrence: &crate::index::Occurrence,
        request: bool,
        declaration: bool,
    ) -> Binding {
        let mut candidates = self.definitions(uri, &occurrence.name, request);
        let mut range = occurrence.range;
        if !request && !declaration {
            let length = candidates
                .iter()
                .map(|symbol| self.reference_name(symbol).len())
                .max();
            if let Some(length) = length {
                candidates.retain(|symbol| self.reference_name(symbol).len() == length);
                if let Some(symbol) = candidates.first() {
                    range.end.character = range.start.character
                        + self.reference_name(symbol).encode_utf16().count() as u32;
                }
            } else {
                range.end.character = range.start.character
                    + occurrence
                        .name
                        .split('.')
                        .next()
                        .unwrap_or_default()
                        .encode_utf16()
                        .count() as u32;
            }
        }
        Binding {
            candidates,
            range,
            response: !request,
            declaration,
        }
    }

    pub fn reference_name(&self, symbol: &Symbol) -> &str {
        if symbol.request {
            &self.documents[&symbol.uri].requests[symbol.index].response_name
        } else {
            &self.occurrence(symbol).name
        }
    }

    pub fn bindings(&self, uri: &str) -> Vec<Binding> {
        let Some(document) = self.documents.get(uri) else {
            return Vec::new();
        };
        let mut bindings = Vec::new();
        for request in document.requests.iter().filter(|request| request.explicit) {
            bindings.push(self.binding(uri, &request.symbol, true, true));
        }
        for variable in &document.variables {
            bindings.push(self.binding(uri, variable, false, true));
        }
        for reference in &document.request_references {
            if !reference.name.contains("{{") {
                bindings.push(self.binding(uri, reference, true, false));
            }
        }
        for reference in &document.variable_references {
            bindings.push(self.binding(uri, reference, false, false));
        }
        bindings
    }

    pub fn at(&self, uri: &str, position: crate::index::Position) -> Option<Binding> {
        let document = self.documents.get(uri)?;
        if let Some(reference) = document
            .variable_references
            .iter()
            .find(|reference| reference.range.contains(position))
        {
            return Some(self.binding(uri, reference, false, false));
        }
        self.bindings(uri)
            .into_iter()
            .find(|binding| binding.range.contains(position))
    }

    pub fn rename(
        &self,
        uri: &str,
        position: crate::index::Position,
        new_name: &str,
    ) -> anyhow::Result<serde_json::Value> {
        anyhow::ensure!(
            self.complete,
            "Workspace indexing is incomplete; cannot safely rename"
        );
        let binding = self
            .at(uri, position)
            .ok_or_else(|| anyhow::anyhow!("No symbol at this position"))?;
        anyhow::ensure!(
            binding.candidates.len() == 1,
            "Cannot rename an unresolved or ambiguous symbol"
        );
        let target = &binding.candidates[0];
        anyhow::ensure!(
            if target.request {
                crate::index::valid_name(new_name) && new_name == new_name.trim()
            } else {
                crate::index::matches(r"^[A-Za-z_][A-Za-z0-9_.-]*$", new_name)
            },
            "Invalid symbol name"
        );
        let new_reference = if target.request {
            crate::index::response_name(new_name)
        } else {
            new_name.to_owned()
        };
        anyhow::ensure!(
            !target.request || crate::index::matches(r"^[A-Za-z_][A-Za-z0-9_-]*$", &new_reference),
            "Request name cannot be represented safely as a response variable"
        );
        let mut changes: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
            Default::default();
        for document in self.documents.values() {
            if !self.reachable(&document.uri).contains(&target.uri) {
                continue;
            }
            for reachable in self.reachable(&document.uri) {
                let Some(other) = self.documents.get(&reachable) else {
                    continue;
                };
                for (index, definition) in other
                    .requests
                    .iter()
                    .enumerate()
                    .filter(|(_, request)| request.explicit)
                {
                    let symbol = Symbol {
                        uri: reachable.clone(),
                        request: true,
                        index,
                    };
                    if &symbol == target {
                        continue;
                    }
                    anyhow::ensure!(
                        !(target.request && definition.symbol.name == new_name)
                            && !names_overlap(&definition.response_name, &new_reference),
                        "Rename would collide with another request or response variable"
                    );
                }
                for (index, definition) in other.variables.iter().enumerate() {
                    let symbol = Symbol {
                        uri: reachable.clone(),
                        request: false,
                        index,
                    };
                    if &symbol != target {
                        anyhow::ensure!(
                            !names_overlap(&definition.name, &new_reference),
                            "Rename would collide with another variable"
                        );
                    }
                }
            }
            for binding in self.bindings(&document.uri) {
                if !binding.candidates.contains(target) {
                    continue;
                }
                anyhow::ensure!(
                    binding.candidates.len() == 1,
                    "A reference is ambiguous; no files were changed"
                );
                let replacement = if binding.response && target.request {
                    &new_reference
                } else {
                    new_name
                };
                changes
                    .entry(document.uri.clone())
                    .or_default()
                    .push(serde_json::json!({"range": binding.range, "newText": replacement}));
            }
        }
        let edits: Vec<_> = changes.into_iter().map(|(uri, edits)| serde_json::json!({"textDocument": {"uri": uri, "version": self.open.get(&uri)}, "edits": edits})).collect();
        Ok(serde_json::json!({"documentChanges": edits}))
    }
}

fn names_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('.'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

fn read_file(path: &std::path::Path) -> Option<String> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() <= 16 * 1024 * 1024 => {}
        Ok(_) => {
            eprintln!(
                "HTTP index skipped non-file or oversized document: {}",
                path.display()
            );
            return None;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!("HTTP index: {}: {error}", path.display());
            return None;
        }
    }
    match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(error) => {
            eprintln!("HTTP index: {}: {error}", path.display());
            None
        }
    }
}

fn scan(root: &std::path::Path, result: &mut std::collections::BTreeSet<String>) {
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                eprintln!("HTTP index: {}: {error}", directory.display());
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    eprintln!("HTTP index: {error}");
                    continue;
                }
            };
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(error) => {
                    eprintln!("HTTP index: {error}");
                    continue;
                }
            };
            if kind.is_dir() {
                if ![".git", "node_modules", "target"]
                    .contains(&entry.file_name().to_string_lossy().as_ref())
                {
                    pending.push(entry.path());
                }
            } else if kind.is_file()
                && entry.path().extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("http") || extension.eq_ignore_ascii_case("rest")
                })
            {
                if let Some(uri) = file_uri(&entry.path()) {
                    result.insert(uri);
                }
            }
        }
    }
}
