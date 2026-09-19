#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub fn contains(self, position: Position) -> bool {
        self.start <= position && position <= self.end
    }
}

pub fn byte_offset(line: &str, character: u32) -> Option<usize> {
    let mut units = 0;
    for (offset, character_value) in line.char_indices() {
        if units == character {
            return Some(offset);
        }
        units += character_value.len_utf16() as u32;
        if units > character {
            return None;
        }
    }
    (units == character).then_some(line.len())
}

pub fn offset(text: &str, position: Position) -> Option<usize> {
    let mut start = 0;
    for (line_number, line) in text.split('\n').enumerate() {
        if line_number == position.line as usize {
            return byte_offset(line.strip_suffix('\r').unwrap_or(line), position.character)
                .map(|column| start + column);
        }
        start += line.len() + 1;
    }
    None
}

pub fn range(line: usize, source: &str, start: usize, end: usize) -> Range {
    Range {
        start: Position {
            line: line as u32,
            character: source[..start].encode_utf16().count() as u32,
        },
        end: Position {
            line: line as u32,
            character: source[..end].encode_utf16().count() as u32,
        },
    }
}

pub fn matches(pattern: &str, text: &str) -> bool {
    expression(pattern).is_match(text)
}

pub fn expression(pattern: &str) -> std::sync::Arc<regex::Regex> {
    static EXPRESSIONS: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<regex::Regex>>>,
    > = std::sync::LazyLock::new(Default::default);
    let mut expressions = EXPRESSIONS.lock().expect("regex cache lock");
    expressions
        .entry(pattern.to_owned())
        .or_insert_with(|| {
            std::sync::Arc::new(regex::Regex::new(pattern).expect("static HTTP regex"))
        })
        .clone()
}

pub const METHODS: &[&str] = &[
    "GET",
    "POST",
    "PUT",
    "DELETE",
    "PATCH",
    "HEAD",
    "OPTIONS",
    "CONNECT",
    "TRACE",
    "PROPFIND",
    "PROPPATCH",
    "MKCOL",
    "COPY",
    "MOVE",
    "LOCK",
    "UNLOCK",
    "CHECKOUT",
    "CHECKIN",
    "REPORT",
    "MERGE",
    "MKACTIVITY",
    "MKWORKSPACE",
    "VERSION-CONTROL",
    "BASELINE-CONTROL",
    "MKCALENDAR",
    "ACL",
    "SEARCH",
    "GRAPHQL",
    "GRPC",
    "WS",
    "WSS",
    "WEBSOCKET",
    "SSE",
    "EVENTSOURCE",
    "MQTT",
    "MQTTS",
    "AMQP",
];

#[derive(Clone, Debug)]
pub struct Occurrence {
    pub name: String,
    pub range: Range,
}
#[derive(Clone, Debug)]
pub struct Request {
    pub symbol: Occurrence,
    pub explicit: bool,
    pub response_name: String,
    pub method: Occurrence,
    pub url: Occurrence,
    pub end_line: usize,
    pub description: String,
    pub references: Vec<Occurrence>,
}
#[derive(Clone, Debug)]
pub struct PathReference {
    pub kind: &'static str,
    pub occurrence: Occurrence,
}
#[derive(Clone, Debug)]
pub struct Metadata {
    pub name: Occurrence,
    pub value: Option<Occurrence>,
}
#[derive(Clone, Debug)]
pub struct Document {
    pub uri: String,
    pub text: String,
    pub lines: Vec<String>,
    pub requests: Vec<Request>,
    pub variables: Vec<Occurrence>,
    pub variable_references: Vec<Occurrence>,
    pub request_references: Vec<Occurrence>,
    pub metadata: Vec<Metadata>,
    pub paths: Vec<PathReference>,
    pub protected: std::collections::BTreeSet<usize>,
    pub json_bodies: Vec<(Range, String)>,
    pub runtime_names: std::collections::BTreeSet<String>,
}

pub fn response_name(name: &str) -> String {
    let replaced: String = name
        .trim()
        .chars()
        .map(|character| {
            if character.is_whitespace() {
                '-'
            } else {
                character
            }
        })
        .collect();
    let mut result = String::new();
    let mut characters = replaced.chars();
    while let Some(character) = characters.next() {
        if character == '-' {
            if let Some(next) = characters.next() {
                result.extend(next.to_uppercase());
            } else {
                result.push(character);
            }
        } else {
            result.push(character);
        }
    }
    result
}

pub fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && !name.contains(['#', '\r', '\n'])
}

fn occurrence(line: usize, source: &str, capture: regex::Match<'_>) -> Occurrence {
    Occurrence {
        name: capture.as_str().to_owned(),
        range: range(line, source, capture.start(), capture.end()),
    }
}

fn path_reference(
    kind: &'static str,
    line: usize,
    source: &str,
    value: &str,
) -> Option<PathReference> {
    let value = value.trim().trim_start_matches('@').trim();
    let value = if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len().saturating_sub(1)]
    } else {
        value
    };
    if value.is_empty() || value.contains("{{") {
        return None;
    }
    let start = source.find(value)?;
    Some(PathReference {
        kind,
        occurrence: Occurrence {
            name: value.to_owned(),
            range: range(line, source, start, start + value.len()),
        },
    })
}

impl Document {
    pub fn parse(uri: String, text: String) -> Self {
        let lines: Vec<String> = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
            .collect();
        let mut document = Self {
            uri,
            text,
            lines,
            requests: Vec::new(),
            variables: Vec::new(),
            variable_references: Vec::new(),
            request_references: Vec::new(),
            metadata: Vec::new(),
            paths: Vec::new(),
            protected: Default::default(),
            json_bodies: Vec::new(),
            runtime_names: Default::default(),
        };
        let mut pending_name = None;
        let mut pending_description = String::new();
        let mut pending_references = Vec::new();
        let mut script_end: Option<&str> = None;
        let mut current_request: Option<usize> = None;
        let metadata_pattern =
            expression(r"^\s*(?:#|//)\s*@([A-Za-z][A-Za-z0-9_-]*)(?:(?:\s+|=\s*)(.+?))?\s*$");
        let request_pattern =
            expression(r"^\s*([A-Za-z][A-Za-z-]*)\s+(\S+)(?:\s+HTTP/\d(?:\.\d)?)?\s*$");
        let variable_pattern =
            expression(r"^\s*@?([A-Za-z_][A-Za-z0-9_.-]*)\s*(?:=|:=)\s*(.*?)\s*$");
        let reference_pattern = expression(r"\{\{\s*(\$?[A-Za-z_][A-Za-z0-9_.-]*)");
        for (line_number, source) in document.lines.iter().enumerate() {
            let trimmed = source.trim();
            if let Some(ending) = script_end {
                document.protected.insert(line_number);
                collect_runtime_names(source, &mut document.runtime_names);
                if trimmed.ends_with(ending) {
                    script_end = None;
                }
                continue;
            }
            if matches(r"^\s*[<>]\s*\{%", source)
                || (trimmed.starts_with("{{") && !matches(r"^\{\{[^}]+}}$", trimmed))
                || matches(r"^\{\{\s*[@+](?:request|response|streaming)\b", trimmed)
            {
                let ending = if trimmed.starts_with(['<', '>']) {
                    "%}"
                } else {
                    "}}"
                };
                if !trimmed.ends_with(ending) {
                    script_end = Some(ending);
                }
                document.protected.insert(line_number);
                collect_runtime_names(source, &mut document.runtime_names);
                continue;
            }
            if matches(r"^#{3,}(?:\s+.*)?$", trimmed) {
                if let Some(index) = current_request.take() {
                    document.requests[index].end_line = line_number.saturating_sub(1);
                }
                pending_name = None;
                pending_description.clear();
                pending_references.clear();
                continue;
            }
            if let Some(captures) = metadata_pattern.captures(source) {
                let name = occurrence(line_number, source, captures.get(1).expect("metadata name"));
                let value = captures
                    .get(2)
                    .map(|capture| occurrence(line_number, source, capture));
                match name.name.to_ascii_lowercase().as_str() {
                    "name" => pending_name = value.clone(),
                    "description" => {
                        pending_description = value
                            .as_ref()
                            .map(|value| value.name.clone())
                            .unwrap_or_default()
                    }
                    "ref" | "forceref" => {
                        if let Some(value) = &value {
                            document.request_references.push(value.clone());
                            pending_references.push(value.clone());
                        }
                    }
                    "import" | "proto" => {
                        if let Some(value) = &value {
                            let kind = if name.name.eq_ignore_ascii_case("import") {
                                "import"
                            } else {
                                "proto"
                            };
                            document.paths.extend(path_reference(
                                kind,
                                line_number,
                                source,
                                &value.name,
                            ));
                        }
                    }
                    _ => {}
                }
                document.metadata.push(Metadata { name, value });
                continue;
            }
            if let Some(captures) = request_pattern.captures(source) {
                let method = occurrence(
                    line_number,
                    source,
                    captures.get(1).expect("request method"),
                );
                if METHODS.contains(&method.name.to_ascii_uppercase().as_str())
                    || matches(r"^[A-Z-]+$", &method.name)
                {
                    if let Some(index) = current_request {
                        document.requests[index].end_line = line_number.saturating_sub(1);
                    }
                    let url =
                        occurrence(line_number, source, captures.get(2).expect("request URL"));
                    let explicit = pending_name.is_some();
                    let symbol = pending_name.take().unwrap_or_else(|| Occurrence {
                        name: format!("{} {}", method.name, url.name),
                        range: method.range,
                    });
                    current_request = Some(document.requests.len());
                    document.requests.push(Request {
                        response_name: if explicit {
                            response_name(&symbol.name)
                        } else {
                            String::new()
                        },
                        symbol,
                        explicit,
                        method,
                        url,
                        end_line: document.lines.len().saturating_sub(1),
                        description: std::mem::take(&mut pending_description),
                        references: std::mem::take(&mut pending_references),
                    });
                }
            }
            if let Some(captures) = variable_pattern.captures(source) {
                document.variables.push(occurrence(
                    line_number,
                    source,
                    captures.get(1).expect("variable name"),
                ));
            }
            for captures in reference_pattern.captures_iter(source) {
                document.variable_references.push(occurrence(
                    line_number,
                    source,
                    captures.get(1).expect("variable reference"),
                ));
            }
            if let Some(captures) = expression(r"(?i)^\s*([<>])\s+(.+?)\s*$").captures(source) {
                let value = captures.get(2).expect("external path").as_str();
                let script = matches(r#"(?i)\.(?:[cm]?js|ts)["']?$"#, value);
                if script
                    || captures
                        .get(1)
                        .is_some_and(|capture| capture.as_str() == "<")
                {
                    document.paths.extend(path_reference(
                        if script { "script" } else { "body" },
                        line_number,
                        source,
                        value,
                    ));
                }
            }
            if let Some(captures) = expression(r"(?i)^\s*proto\s*<\s*(.+?)\s*$").captures(source) {
                document.paths.extend(path_reference(
                    "proto",
                    line_number,
                    source,
                    captures.get(1).expect("proto path").as_str(),
                ));
            }
        }
        document.index_bodies();
        document
    }

    fn index_bodies(&mut self) {
        for request in &self.requests {
            let header_start = request.method.range.start.line as usize + 1;
            let end = request.end_line;
            let mut start = header_start;
            while start <= end && !self.lines[start].trim().is_empty() {
                start += 1;
            }
            while start <= end && self.lines[start].trim().is_empty() {
                start += 1;
            }
            if start > end {
                continue;
            }
            let json = self.lines[header_start..start].iter().any(|line| {
                matches(
                    r"(?i)^\s*Content-Type\s*:\s*application/(?:[A-Za-z0-9_.+-]*\+)?json\b",
                    line,
                )
            });
            let mut body_end = end;
            let mut stack = Vec::new();
            let mut in_string = false;
            let mut escaped = false;
            let mut handlebars = false;
            let mut block = false;
            let mut started = false;
            for line_number in start..=end {
                let source = &self.lines[line_number];
                if post_body(source) {
                    body_end = line_number.saturating_sub(1);
                    break;
                }
                if !json {
                    continue;
                }
                let mut characters = source.chars().peekable();
                while let Some(character) = characters.next() {
                    let next = characters.peek().copied();
                    if handlebars {
                        if character == '}' && next == Some('}') {
                            handlebars = false;
                            characters.next();
                        }
                        continue;
                    }
                    if block {
                        if character == '*' && next == Some('/') {
                            block = false;
                            characters.next();
                        }
                        continue;
                    }
                    if in_string {
                        if escaped {
                            escaped = false;
                        } else if character == '\\' {
                            escaped = true;
                        } else if character == '"' {
                            in_string = false;
                        }
                        continue;
                    }
                    if character == '"' {
                        in_string = true;
                        continue;
                    }
                    if character == '/' && next == Some('/') {
                        break;
                    }
                    if character == '/' && next == Some('*') {
                        block = true;
                        characters.next();
                        continue;
                    }
                    if character == '{' && next == Some('{') {
                        handlebars = true;
                        characters.next();
                        continue;
                    }
                    if character == '{' || character == '[' {
                        stack.push(character);
                        started = true;
                    }
                    if character == '}' || character == ']' {
                        if stack.last() == Some(&if character == '}' { '{' } else { '[' }) {
                            stack.pop();
                        }
                        if started && stack.is_empty() {
                            body_end = line_number;
                            break;
                        }
                    }
                }
                if started && stack.is_empty() {
                    break;
                }
            }
            if body_end < start {
                continue;
            }
            while body_end > start && self.lines[body_end].trim().is_empty() {
                body_end -= 1;
            }
            self.protected.extend(start..=body_end);
            if json && started {
                let mut normalized = self.lines[start..=body_end].to_vec();
                let mut line_number = 0;
                while line_number < normalized.len() {
                    if normalized[line_number].trim_start().starts_with("//") {
                        normalized[line_number].clear();
                    } else if normalized[line_number].trim() == "/*" {
                        if let Some(relative) = normalized[line_number + 1..]
                            .iter()
                            .position(|line| line.trim() == "*/")
                        {
                            let closing = line_number + 1 + relative;
                            for line in &mut normalized[line_number..=closing] {
                                line.clear();
                            }
                            line_number = closing;
                        }
                    }
                    line_number += 1;
                }
                self.json_bodies.push((
                    Range {
                        start: Position {
                            line: start as u32,
                            character: 0,
                        },
                        end: Position {
                            line: body_end as u32,
                            character: self.lines[body_end].encode_utf16().count() as u32,
                        },
                    },
                    normalized.join("\n"),
                ));
            }
        }
    }

    pub fn prefix(&self, position: Position) -> Option<&str> {
        let line = self.lines.get(position.line as usize)?;
        line.get(..byte_offset(line, position.character)?)
    }
}

fn post_body(source: &str) -> bool {
    matches(
        r"^\s*(?:#{3,}|(?:#|//)\s*@|\?\?|>>!?|>\s*\{%|HTTP/\d|\{\{\s*[@+](?:request|response|streaming)\b)",
        source,
    ) || matches(r"(?i)^\s*>\s+.+\.(?:[cm]?js|ts)\s*$", source)
}

fn collect_runtime_names(source: &str, names: &mut std::collections::BTreeSet<String>) {
    for capture in expression(r#"(?:client\.(?:global|variables)\.set|request\.variables\.set)\s*\(\s*["']([A-Za-z_][A-Za-z0-9_]*)["']"#).captures_iter(source) {
        if let Some(name) = capture.get(1) { names.insert(name.as_str().to_owned()); }
    }
    for capture in expression(r"\b(?:exports|module\.exports)\.([A-Za-z_][A-Za-z0-9_]*)\s*=")
        .captures_iter(source)
    {
        if let Some(name) = capture.get(1) {
            names.insert(name.as_str().to_owned());
        }
    }
}
