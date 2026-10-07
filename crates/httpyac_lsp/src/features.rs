#[derive(serde::Deserialize)]
pub struct Catalog {
    pub metadata: Vec<Entry>,
    #[serde(rename = "emptyLine")]
    pub empty_line: Vec<Entry>,
    pub variables: Vec<Entry>,
    pub headers: Vec<Entry>,
}
#[derive(serde::Deserialize)]
pub struct Entry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub text: Option<String>,
    #[serde(default)]
    pub completions: Vec<String>,
}
impl Catalog {
    pub fn load() -> anyhow::Result<Self> {
        Ok(serde_json::from_str(include_str!("catalog.json"))?)
    }
    fn builtin(&self, name: &str) -> bool {
        let root = name.split('.').next().unwrap_or(name);
        [
            "$shared",
            "$processEnv",
            "$dotenv",
            "$random",
            "$timestamp",
            "$isoTimestamp",
            "request",
            "response",
            "client",
            "exports",
            "module",
        ]
        .contains(&root)
            || self.variables.iter().any(|entry| {
                let normalized = entry
                    .name
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches("()");
                normalized == name || normalized == root
            })
    }
}

fn items(entries: &[Entry], kind: u32) -> Vec<serde_json::Value> {
    entries.iter().map(|entry| serde_json::json!({"label": entry.name, "kind": kind, "detail": entry.description, "documentation": entry.description, "insertText": entry.text.as_ref().unwrap_or(&entry.name)})).collect()
}

pub fn completions(
    workspace: &crate::workspace::Workspace,
    document: &crate::index::Document,
    position: crate::index::Position,
    catalog: &Catalog,
) -> Vec<serde_json::Value> {
    let Some(prefix) = document.prefix(position) else {
        return Vec::new();
    };
    let reachable: Vec<_> = workspace
        .reachable(&document.uri)
        .into_iter()
        .filter_map(|uri| workspace.documents.get(&uri))
        .collect();
    let request = document.requests.iter().find(|request| {
        request.method.range.start.line < position.line
            && request.end_line >= position.line as usize
    });
    let mut result = Vec::new();
    if crate::index::matches(
        r"(?i)^\s*(?:#|//)\s*@(?:forceRef|ref)(?:\s+|=)\s*.*$",
        prefix,
    ) {
        result.extend(reachable.iter().flat_map(|document| &document.requests).filter(|request| request.explicit).map(|request| serde_json::json!({"label": request.symbol.name, "kind": 18, "detail": format!("{} {}", request.method.name, request.url.name), "documentation": request.description})));
    } else if crate::index::matches(r"\{\{\s*[A-Za-z0-9_.$-]*$", prefix) {
        for document in &reachable {
            result.extend(document.variables.iter().map(|variable| serde_json::json!({"label": variable.name, "kind": 6, "detail": "httpYac file variable"})));
            result.extend(document.requests.iter().filter(|request| request.explicit).map(|request| serde_json::json!({"label": request.response_name, "kind": 6, "detail": "Named response"})));
        }
        result.extend(items(&catalog.variables, 6));
    } else if crate::index::matches(r"^\s*(?:#|//)\s*@[A-Za-z0-9_-]*$", prefix) {
        result.extend(catalog.metadata.iter().map(|entry| serde_json::json!({"label": format!("@{}", entry.name), "kind": 10, "detail": entry.description, "documentation": entry.description, "insertText": format!("@{}{}", entry.name, if entry.completions.is_empty() { "" } else { " " })})));
    } else if let Some(captures) = crate::index::expression(
        r"(?i)^\s*(?:(?:#|//)\s*@(?:import|proto)(?:\s+|=)\s*|proto\s*<\s*|[<>]\s+)(.*)$",
    )
    .captures(prefix)
    {
        let typed = captures
            .get(1)
            .map(|capture| capture.as_str())
            .unwrap_or_default()
            .trim_start_matches(['\'', '"']);
        let directory = typed
            .rfind(if cfg!(windows) {
                &['/', '\\'][..]
            } else {
                &['/'][..]
            })
            .map(|offset| &typed[..=offset])
            .unwrap_or(".");
        if let Some(path) = crate::workspace::resolve_path(&document.uri, directory) {
            match std::fs::read_dir(path) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) => {
                                let directory = entry.path().is_dir();
                                result.push(serde_json::json!({"label": format!("{}{}", entry.file_name().to_string_lossy(), if directory { std::path::MAIN_SEPARATOR_STR } else { "" }), "kind": if directory { 19 } else { 17 }}));
                            }
                            Err(error) => eprintln!("HTTP completion: {error}"),
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => eprintln!("HTTP completion: {error}"),
            }
        }
    } else if request.is_none() && crate::index::matches(r"^\s*[A-Za-z-]*$", prefix) {
        result.extend(crate::index::METHODS.iter().map(|method| serde_json::json!({"label": method, "kind": 14, "detail": "HTTP/httpYac request method", "insertText": format!("{method} ")})));
        result.extend(items(&catalog.empty_line, 15));
    } else if request.is_some()
        && !document.protected.contains(&(position.line as usize))
        && crate::index::matches(r"^\s*(?:[A-Za-z0-9_-]*|[A-Za-z0-9_-]+\s*:\s*.*)$", prefix)
    {
        result.extend(["Accept", "Accept-Encoding", "Authorization", "Cache-Control", "Content-Type", "Cookie", "If-Match", "Origin", "Referer", "User-Agent", "X-API-Key", "X-Auth-Token", "X-Request-ID"].iter().map(|header| serde_json::json!({"label": header, "kind": 5, "insertText": format!("{header}: ")})));
        result.extend(items(&catalog.headers, 5));
    }
    let mut seen = std::collections::BTreeSet::new();
    result.retain(|item| {
        seen.insert(
            item["label"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase(),
        )
    });
    result
}

fn diagnostic(
    range: crate::index::Range,
    code: &str,
    message: String,
    severity: u32,
) -> serde_json::Value {
    serde_json::json!({"range": range, "code": code, "message": message, "source": "httpyac", "severity": severity})
}

pub fn diagnostics(
    workspace: &crate::workspace::Workspace,
    document: &crate::index::Document,
    catalog: &Catalog,
) -> Vec<serde_json::Value> {
    let mut result = Vec::new();
    for request in &document.requests {
        if request.explicit {
            if !crate::index::valid_name(&request.symbol.name) {
                result.push(diagnostic(
                    request.symbol.range,
                    "invalid-name",
                    format!("非法的 request name: {}", request.symbol.name),
                    1,
                ));
            }
            if workspace
                .definitions(&document.uri, &request.symbol.name, true)
                .len()
                > 1
            {
                result.push(diagnostic(
                    request.symbol.range,
                    "duplicate-name",
                    format!(
                        "当前文件或导入图中存在重复的 request name: {}",
                        request.symbol.name
                    ),
                    1,
                ));
            }
        }
        if !crate::index::METHODS.contains(&request.method.name.to_ascii_uppercase().as_str()) {
            result.push(diagnostic(
                request.method.range,
                "invalid-method",
                format!("不支持的方法: {}", request.method.name),
                1,
            ));
        }
        let url = &request.url.name;
        if !url.contains("{{")
            && !url.starts_with(['/', ':'])
            && !crate::index::matches(r"(?i)^(?:https?|wss?|grpc|mqtts?|amqp)://\S+$", url)
        {
            result.push(diagnostic(
                request.url.range,
                "invalid-url",
                format!("URL 看起来不完整: {url}"),
                2,
            ));
        }
    }
    for reference in &document.request_references {
        if reference.name.contains("{{") {
            continue;
        }
        let definitions = workspace.definitions(&document.uri, &reference.name, true);
        if definitions.is_empty() {
            result.push(diagnostic(
                reference.range,
                "undefined-ref",
                format!("未定义的 request reference: {}", reference.name),
                1,
            ));
        } else if definitions.len() > 1 {
            result.push(diagnostic(
                reference.range,
                "ambiguous-reference",
                format!("请求引用存在歧义: {}", reference.name),
                1,
            ));
        }
    }
    let reachable: Vec<_> = workspace
        .reachable(&document.uri)
        .into_iter()
        .filter_map(|uri| workspace.documents.get(&uri))
        .collect();
    for reference in &document.variable_references {
        if catalog.builtin(&reference.name) {
            continue;
        }
        let binding = workspace.binding(&document.uri, reference, false, false);
        if binding.candidates.len() > 1 {
            result.push(diagnostic(
                binding.range,
                "ambiguous-reference",
                format!("变量引用存在歧义: {}", reference.name),
                1,
            ));
        } else if binding.candidates.is_empty() {
            let root = reference.name.split('.').next().unwrap_or_default();
            if reachable
                .iter()
                .any(|document| document.runtime_names.contains(root))
            {
                continue;
            }
            let similar = nearest(
                root,
                reachable.iter().flat_map(|document| {
                    document
                        .variables
                        .iter()
                        .map(|variable| variable.name.as_str())
                }),
            );
            // Absence from a static index cannot prove absence from a CLI environment or plugin.
            result.push(if let Some(similar) = similar {
                diagnostic(
                    binding.range,
                    "undefined-variable",
                    format!("静态索引未找到变量 {root}，是否为 {similar}？也可能由运行时提供。"),
                    2,
                )
            } else {
                diagnostic(
                    binding.range,
                    "unresolved-variable",
                    format!("无法静态确定变量 {root}；它可能由环境、脚本或插件提供。"),
                    4,
                )
            });
        }
    }
    for metadata in &document.metadata {
        let name = metadata.name.name.to_ascii_lowercase();
        if !catalog
            .metadata
            .iter()
            .any(|entry| entry.name.eq_ignore_ascii_case(&name))
        {
            result.push(diagnostic(
                metadata.name.range,
                "unknown-metadata",
                format!("未知的 httpYac metadata: @{}", metadata.name.name),
                2,
            ));
        }
        let Some(value) = &metadata.value else {
            if ["name", "ref", "forceref", "import"].contains(&name.as_str()) {
                result.push(diagnostic(
                    metadata.name.range,
                    "missing-metadata-value",
                    format!("@{name} 需要参数"),
                    1,
                ));
            }
            continue;
        };
        let valid = match name.as_str() {
            "name" => crate::index::valid_name(&value.name),
            "ref" | "forceref" => {
                value.name.contains("{{") || crate::index::valid_name(&value.name)
            }
            "timeout" => {
                value.name.contains("{{")
                    || crate::index::matches(r"(?i)^\s*\d+(?:\.\d+)?(?:\s*ms)?\s*$", &value.name)
            }
            "loop" => crate::index::matches(
                r"(?i)^\s*(?:for\s+(?:\d+|.+\s+of\s+.+)|while\s+.+)\s*$",
                &value.name,
            ),
            "ratelimit" => crate::index::matches(
                r"(?i)^\s*(?:(?:slot\s*:?\s*\S+)\s*)?(?:(?:minIdleTime\s*:?\s*\d+)\s*)?(?:max\s*:?\s*\d+)(?:\s+expire\s*:?\s*\d+)?\s*$",
                &value.name,
            ),
            _ => true,
        };
        if !valid {
            result.push(diagnostic(
                value.range,
                "invalid-metadata-value",
                format!("@{name} 参数格式无效"),
                1,
            ));
        }
    }
    for reference in &document.paths {
        let Some(path) = crate::workspace::resolve_path(&document.uri, &reference.occurrence.name)
        else {
            continue;
        };
        let uri = crate::workspace::file_uri(&path);
        if !path.exists()
            && !uri
                .as_ref()
                .is_some_and(|uri| workspace.open.contains_key(uri))
        {
            result.push(diagnostic(
                reference.occurrence.range,
                "missing-file",
                format!("文件不存在: {}", reference.occurrence.name),
                1,
            ));
        }
        if reference.kind == "import"
            && uri.is_some_and(|uri| workspace.reachable(&uri).contains(&document.uri))
        {
            result.push(diagnostic(
                reference.occurrence.range,
                "import-cycle",
                format!("检测到 import 循环: {}", reference.occurrence.name),
                1,
            ));
        }
    }
    for (index, request) in document
        .requests
        .iter()
        .enumerate()
        .filter(|(_, request)| request.explicit)
    {
        let target = crate::workspace::Symbol {
            uri: document.uri.clone(),
            request: true,
            index,
        };
        for reference in &request.references {
            let mut pending = workspace.definitions(&document.uri, &reference.name, true);
            if pending.len() != 1 {
                continue;
            }
            let mut visited = std::collections::BTreeSet::new();
            while let Some(symbol) = pending.pop() {
                if symbol == target {
                    result.push(diagnostic(
                        reference.range,
                        "reference-cycle",
                        format!("检测到 request reference 循环: {}", reference.name),
                        1,
                    ));
                    break;
                }
                if !visited.insert(symbol.clone()) {
                    continue;
                }
                for reference in &workspace.documents[&symbol.uri].requests[symbol.index].references
                {
                    let definitions = workspace.definitions(&symbol.uri, &reference.name, true);
                    if definitions.len() == 1 {
                        pending.extend(definitions);
                    }
                }
            }
        }
    }
    for (range, text) in &document.json_bodies {
        if !text.contains("{{")
            && let Err(error) = serde_json::from_str::<serde_json::Value>(text)
        {
            result.push(diagnostic(
                *range,
                "invalid-json",
                format!("JSON body 无效: {error}"),
                1,
            ));
        }
    }
    result
}

pub fn definition(
    workspace: &crate::workspace::Workspace,
    document: &crate::index::Document,
    position: crate::index::Position,
) -> Vec<serde_json::Value> {
    if let Some(reference) = document
        .paths
        .iter()
        .find(|reference| reference.occurrence.range.contains(position))
    {
        return crate::workspace::resolve_path(&document.uri, &reference.occurrence.name)
            .and_then(|path| crate::workspace::file_uri(&path))
            .map(|uri| {
                vec![serde_json::json!({"uri": uri, "range": crate::index::Range::default()})]
            })
            .unwrap_or_default();
    }
    workspace.at(&document.uri, position).map(|binding| binding.candidates.iter().map(|symbol| serde_json::json!({"uri": symbol.uri, "range": workspace.occurrence(symbol).range})).collect()).unwrap_or_default()
}

pub fn references(
    workspace: &crate::workspace::Workspace,
    uri: &str,
    position: crate::index::Position,
    include_declaration: bool,
) -> Vec<serde_json::Value> {
    let Some(binding) = workspace.at(uri, position) else {
        return Vec::new();
    };
    if binding.candidates.len() != 1 {
        return Vec::new();
    }
    workspace
        .documents
        .keys()
        .flat_map(|uri| {
            workspace
                .bindings(uri)
                .into_iter()
                .filter(|candidate| {
                    candidate.candidates == binding.candidates
                        && (include_declaration || !candidate.declaration)
                })
                .map(move |candidate| serde_json::json!({"uri": uri, "range": candidate.range}))
        })
        .collect()
}

pub fn links(document: &crate::index::Document) -> Vec<serde_json::Value> {
    document.paths.iter().filter_map(|reference| {
        let target = crate::workspace::resolve_path(&document.uri, &reference.occurrence.name).and_then(|path| crate::workspace::file_uri(&path))?;
        Some(serde_json::json!({"range": reference.occurrence.range, "target": target, "tooltip": format!("{} file", reference.kind)}))
    }).collect()
}

pub fn formatting(document: &crate::index::Document) -> Vec<serde_json::Value> {
    document.lines.iter().enumerate().filter(|(line, _)| !document.protected.contains(line)).filter_map(|(line_number, line)| {
        let trimmed = line.trim_end();
        (trimmed != line).then(|| serde_json::json!({"range": crate::index::range(line_number, line, trimmed.len(), line.len()), "newText": ""}))
    }).collect()
}

pub fn symbols(workspace: &crate::workspace::Workspace, query: &str) -> Vec<serde_json::Value> {
    workspace.documents.values().flat_map(|document| document.requests.iter().filter(|request| request.explicit && request.symbol.name.to_lowercase().contains(&query.to_lowercase())).map(|request| serde_json::json!({"name": request.symbol.name, "kind": 6, "location": {"uri": document.uri, "range": request.symbol.range}, "containerName": format!("{} {}", request.method.name, request.url.name)}))).collect()
}

pub fn hover(
    workspace: &crate::workspace::Workspace,
    document: &crate::index::Document,
    position: crate::index::Position,
    catalog: &Catalog,
) -> serde_json::Value {
    let content = if let Some(metadata) = document
        .metadata
        .iter()
        .find(|metadata| metadata.name.range.contains(position))
    {
        catalog
            .metadata
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(&metadata.name.name))
            .map(|entry| format!("**@{}**\n\n{}", entry.name, entry.description))
    } else if let Some(request) = document
        .requests
        .iter()
        .find(|request| request.method.range.contains(position))
    {
        Some(format!(
            "**{}** request\n\n{}",
            request.method.name,
            if request.description.is_empty() {
                &request.url.name
            } else {
                &request.description
            }
        ))
    } else if let Some(reference) = document
        .variable_references
        .iter()
        .find(|reference| reference.range.contains(position))
    {
        let root = reference.name.split('.').next().unwrap_or_default();
        let builtin = catalog.variables.iter().find(|entry| {
            let normalized = entry
                .name
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_end_matches("()");
            normalized == root || normalized == reference.name
        });
        let binding = workspace.binding(&document.uri, reference, false, false);
        let description = if let Some(entry) = builtin {
            entry.description.clone()
        } else if binding.candidates.len() == 1 {
            let symbol = &binding.candidates[0];
            if symbol.request {
                let request = &workspace.documents[&symbol.uri].requests[symbol.index];
                format!(
                    "Named response from {} {}",
                    request.method.name, request.url.name
                )
            } else {
                "httpYac file variable".to_owned()
            }
        } else {
            "Value supplied at runtime, or unresolved/ambiguous in the static index.".to_owned()
        };
        Some(format!("**{}**\n\n{description}", reference.name))
    } else {
        document.lines.get(position.line as usize).and_then(|line| {
            let captures = crate::index::expression(r"^\s*([A-Za-z0-9_-]+)\s*:").captures(line)?;
            let header = captures.get(1)?.as_str();
            let description = catalog
                .headers
                .iter()
                .find(|entry| entry.name.eq_ignore_ascii_case(header))
                .map(|entry| entry.description.as_str())
                .unwrap_or("Header names are case-insensitive.");
            Some(format!("**{header}** HTTP header\n\n{description}"))
        })
    };
    content
        .map(|content| serde_json::json!({"contents": {"kind": "markdown", "value": content}}))
        .unwrap_or(serde_json::Value::Null)
}

pub fn actions(
    workspace: &crate::workspace::Workspace,
    document: &crate::index::Document,
    diagnostics: &[serde_json::Value],
    range: crate::index::Range,
) -> Vec<serde_json::Value> {
    let mut result = Vec::new();
    let reachable: Vec<_> = workspace
        .reachable(&document.uri)
        .into_iter()
        .filter_map(|uri| workspace.documents.get(&uri))
        .collect();
    for diagnostic in diagnostics {
        let Some(code) = diagnostic["code"].as_str() else {
            continue;
        };
        if code != "undefined-ref" && code != "undefined-variable" {
            continue;
        }
        let Ok(range) = serde_json::from_value::<crate::index::Range>(diagnostic["range"].clone())
        else {
            continue;
        };
        let name = crate::index::offset(&document.text, range.start)
            .zip(crate::index::offset(&document.text, range.end))
            .and_then(|(start, end)| document.text.get(start..end));
        let Some(name) = name else {
            continue;
        };
        let names: Vec<_> = if code == "undefined-ref" {
            reachable
                .iter()
                .flat_map(|document| {
                    document
                        .requests
                        .iter()
                        .filter(|request| request.explicit)
                        .map(|request| request.symbol.name.as_str())
                })
                .collect()
        } else {
            reachable
                .iter()
                .flat_map(|document| {
                    document
                        .variables
                        .iter()
                        .map(|variable| variable.name.as_str())
                })
                .collect()
        };
        if let Some(similar) = nearest(name, names.into_iter()) {
            result.push(serde_json::json!({"title": format!("改为 {} {similar}", if code == "undefined-ref" { "@ref" } else { "变量" }), "kind": "quickfix", "diagnostics": [diagnostic], "edit": {"changes": {&document.uri: [{"range": range, "newText": similar}]}}}));
        }
    }
    let request = document
        .requests
        .iter()
        .find(|request| {
            request.method.range.start.line <= range.start.line
                && request.end_line >= range.end.line as usize
        })
        .or_else(|| document.requests.first());
    let newline = if document.text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    if let Some(request) = request {
        if !request.explicit {
            let mut name = "requestName".to_owned();
            let mut suffix = 1;
            while !workspace.definitions(&document.uri, &name, true).is_empty()
                || !workspace
                    .definitions(&document.uri, &name, false)
                    .is_empty()
            {
                name = format!("requestName{suffix}");
                suffix += 1;
            }
            result.push(insert_action(
                document,
                "为请求创建 @name",
                request.method.range.start.line,
                format!("# @name {name}{newline}"),
            ));
        }
        let start = request.method.range.start.line as usize + 1;
        let headers: Vec<_> = document.lines[start..=request.end_line.max(start.saturating_sub(1))]
            .iter()
            .take_while(|line| !line.trim().is_empty())
            .collect();
        if !headers
            .iter()
            .any(|line| crate::index::matches(r"(?i)^\s*Content-Type\s*:", line))
        {
            result.push(insert_action(
                document,
                "添加 JSON Content-Type/Accept headers",
                start as u32,
                format!("Content-Type: application/json{newline}Accept: application/json{newline}"),
            ));
        }
    }
    result
}

fn insert_action(
    document: &crate::index::Document,
    title: &str,
    line: u32,
    mut text: String,
) -> serde_json::Value {
    let position = if line as usize >= document.lines.len() {
        text.insert_str(
            0,
            if document.text.contains("\r\n") {
                "\r\n"
            } else {
                "\n"
            },
        );
        crate::index::Position {
            line: document.lines.len().saturating_sub(1) as u32,
            character: document
                .lines
                .last()
                .map(|line| line.encode_utf16().count() as u32)
                .unwrap_or_default(),
        }
    } else {
        crate::index::Position { line, character: 0 }
    };
    serde_json::json!({"title": title, "kind": "quickfix", "edit": {"changes": {&document.uri: [{"range": crate::index::Range { start: position, end: position }, "newText": text}]}}})
}

pub fn nearest<'a>(source: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    candidates
        .filter(|candidate| *candidate != source)
        .map(|candidate| (candidate, distance(source, candidate)))
        .min_by_key(|(_, distance)| *distance)
        .filter(|(_, distance)| *distance <= 2.max(source.chars().count() / 3))
        .map(|(candidate, _)| candidate)
}

fn distance(left: &str, right: &str) -> usize {
    let left: Vec<_> = left.chars().collect();
    let mut rows: Vec<_> = (0..=left.len()).collect();
    for (column, right) in right.chars().enumerate() {
        let mut previous = rows[0];
        rows[0] = column + 1;
        for (row, left) in left.iter().enumerate() {
            let old = rows[row + 1];
            rows[row + 1] = (old + 1)
                .min(rows[row] + 1)
                .min(previous + usize::from(*left != right));
            previous = old;
        }
    }
    rows[left.len()]
}
