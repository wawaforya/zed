use language::ToOffset as _;

#[derive(Clone, Debug)]
pub(super) struct Request {
    pub id: u64,
    pub anchor: language::Anchor,
    pub range: std::ops::Range<language::Anchor>,
    pub name: String,
    pub title: String,
    pub group: Vec<String>,
    pub method: String,
    pub url: String,
    pub source: String,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Grouping {
    #[default]
    DoubleUnderscore,
    FirstUnderscore,
    ExplicitOnly,
}

pub(super) fn presentation(
    name: &str,
    title: &str,
    explicit_group: Option<&str>,
    grouping: Grouping,
) -> (Vec<String>, String) {
    let (mut group, leaf) = match grouping {
        Grouping::DoubleUnderscore => {
            let mut parts = name.split("__").collect::<Vec<_>>();
            let leaf = parts.pop().unwrap_or(name);
            (
                parts.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                leaf,
            )
        }
        Grouping::FirstUnderscore => name
            .split_once('_')
            .map(|(group, leaf)| (vec![group.to_owned()], leaf))
            .unwrap_or_else(|| (Vec::new(), name)),
        Grouping::ExplicitOnly => (Vec::new(), name),
    };
    if let Some(explicit) = explicit_group {
        group = explicit.split('/').map(str::to_owned).collect();
    }
    group = group
        .into_iter()
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect();
    (
        group,
        if title.is_empty() { leaf } else { title }.to_owned(),
    )
}

pub(super) fn index(
    snapshot: &language::BufferSnapshot,
    previous: &[Request],
    next_id: &mut u64,
    grouping: Grouping,
) -> Option<(Vec<Request>, Vec<std::ops::Range<language::Anchor>>)> {
    let layer = snapshot
        .syntax_layers()
        .find(|layer| layer.language.name().as_ref() == "HTTP")?;
    let root = layer.node();
    let text = snapshot.text();
    let mut requests = Vec::new();
    let mut context = Vec::new();
    let mut cursor = root.walk();
    for section in root.named_children(&mut cursor) {
        if section.kind() != "section" {
            continue;
        }
        let mut start = section.start_byte();
        let mut children = section.walk();
        if let Some(separator) = section
            .named_children(&mut children)
            .find(|child| child.kind() == "request_separator")
        {
            start = separator.end_byte();
        }
        let end = section.end_byte();
        let range = snapshot.anchor_before(start)..snapshot.anchor_after(end);
        let Some(request) = section.child_by_field_name("request") else {
            if start < end {
                context.push(range);
            }
            continue;
        };
        let Some(url) = request.child_by_field_name("url") else {
            continue;
        };
        let source = text.get(start..end).unwrap_or_default().to_owned();
        let mut name = String::new();
        let mut title = String::new();
        let mut group = None;
        // Inspect syntax nodes, never comment-like text inside bodies or scripts.
        let mut children = section.walk();
        let mut request_children = request.walk();
        for child in section
            .named_children(&mut children)
            .chain(request.named_children(&mut request_children))
        {
            if !matches!(child.kind(), "metadata" | "comment") {
                continue;
            }
            let line = text.get(child.byte_range()).unwrap_or_default().trim();
            let line = line
                .strip_prefix('#')
                .or_else(|| line.strip_prefix("//"))
                .unwrap_or(line)
                .trim();
            if let Some(value) = metadata(line, "name") {
                name = value.to_owned();
            }
            if let Some(value) = metadata(line, "title") {
                title = value.to_owned();
            }
            if let Some(value) = line.strip_prefix("zed-group:") {
                group = Some(value.trim().to_owned());
            }
        }
        if title.is_empty() && name.is_empty() {
            let mut children = section.walk();
            if let Some(separator) = section
                .named_children(&mut children)
                .find(|child| child.kind() == "request_separator")
            {
                title = separator
                    .child_by_field_name("value")
                    .and_then(|value| text.get(value.byte_range()))
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
            }
        }
        let url = text.get(url.byte_range()).unwrap_or_default().to_owned();
        let method = request
            .child_by_field_name("method")
            .and_then(|node| text.get(node.byte_range()))
            .unwrap_or_else(|| match url.split_once("://").map(|(scheme, _)| scheme) {
                Some("http" | "https") => "GET",
                Some("ws" | "wss") => "WS",
                Some("grpc") => "GRPC",
                Some("mqtt" | "mqtts") => "MQTT",
                Some("amqp" | "amqps") => "AMQP",
                _ => "AUTO",
            })
            .to_owned();
        if name.is_empty() && title.is_empty() {
            title = format!("{method} {url}");
        }
        let (group, title) = presentation(&name, &title, group.as_deref(), grouping);
        let existing = previous
            .iter()
            .find(|previous| {
                previous.anchor.is_valid(snapshot) && {
                    let offset = previous.anchor.to_offset(snapshot);
                    request.start_byte() <= offset && offset < request.end_byte()
                }
            })
            .or_else(|| {
                previous.iter().find(|previous| {
                    // Replacing the method can delete the execution anchor without deleting the region.
                    !requests
                        .iter()
                        .any(|request: &Request| request.id == previous.id)
                        && previous.range.start.to_offset(snapshot) <= request.start_byte()
                        && request.start_byte() < previous.range.end.to_offset(snapshot)
                })
            });
        let id = existing.map(|request| request.id).unwrap_or_else(|| {
            *next_id += 1;
            *next_id
        });
        requests.push(Request {
            id,
            anchor: snapshot.anchor_after(request.start_byte()),
            range,
            name,
            title,
            group,
            method,
            url,
            source,
        });
    }
    // Keep the last usable index while an incomplete edit temporarily breaks a request.
    if root.has_error() {
        for old in previous {
            if old.anchor.is_valid(snapshot) && !requests.iter().any(|request| request.id == old.id)
            {
                let offset = old.anchor.to_offset(snapshot);
                if !requests.iter().any(|request| {
                    request.range.start.to_offset(snapshot) <= offset
                        && offset < request.range.end.to_offset(snapshot)
                }) {
                    requests.push(old.clone());
                }
            }
        }
        requests.sort_by_key(|request| request.anchor.to_offset(snapshot));
    }
    Some((requests, context))
}

fn metadata<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let tail = line.strip_prefix('@')?.strip_prefix(name)?;
    if !tail.is_empty() && !tail.starts_with([' ', '\t', '=']) {
        return None;
    }
    Some(tail.trim_start_matches([' ', '\t', '=']).trim())
}

pub(super) enum ListRow {
    Group {
        path: Vec<String>,
        label: String,
        depth: usize,
    },
    Request {
        index: usize,
        depth: usize,
    },
}

#[derive(Default)]
struct Group {
    children: Vec<(String, Group)>,
    requests: Vec<usize>,
}

pub(super) fn list_rows(
    requests: &[Request],
    filter: &str,
    file_order: bool,
    collapsed: &std::collections::BTreeSet<Vec<String>>,
) -> Vec<ListRow> {
    let filter = filter.to_lowercase();
    let mut root = Group::default();
    for (index, request) in requests.iter().enumerate() {
        if !format!(
            "{} {} {} {} {}",
            request.name,
            request.title,
            request.method,
            request.url,
            request.group.join("/")
        )
        .to_lowercase()
        .contains(&filter)
        {
            continue;
        }
        let mut group = &mut root;
        for name in &request.group {
            let position = match group.children.iter().position(|(other, _)| other == name) {
                Some(position) => position,
                None => {
                    group.children.push((name.clone(), Group::default()));
                    group.children.len() - 1
                }
            };
            group = &mut group.children[position].1;
        }
        group.requests.push(index);
    }
    fn visit(
        group: &mut Group,
        path: &mut Vec<String>,
        requests: &[Request],
        file_order: bool,
        collapsed: &std::collections::BTreeSet<Vec<String>>,
        filtering: bool,
        rows: &mut Vec<ListRow>,
    ) {
        if !file_order {
            group
                .children
                .sort_by(|(left, _), (right, _)| natural_cmp(left, right));
            group.requests.sort_by(|left, right| {
                natural_cmp(&requests[*left].title, &requests[*right].title).then(left.cmp(right))
            });
        }
        for (name, child) in &mut group.children {
            path.push(name.clone());
            rows.push(ListRow::Group {
                path: path.clone(),
                label: name.clone(),
                depth: path.len() - 1,
            });
            if filtering || !collapsed.contains(path) {
                visit(
                    child, path, requests, file_order, collapsed, filtering, rows,
                );
            }
            path.pop();
        }
        if path.is_empty() && !group.requests.is_empty() {
            // An empty path identifies the synthetic group, distinct from a real "Ungrouped" name.
            rows.push(ListRow::Group {
                path: Vec::new(),
                label: "Ungrouped".into(),
                depth: 0,
            });
            if !filtering && collapsed.contains(path) {
                return;
            }
        }
        rows.extend(group.requests.iter().map(|index| ListRow::Request {
            index: *index,
            depth: path.len().max(1),
        }));
    }
    let mut rows = Vec::new();
    visit(
        &mut root,
        &mut Vec::new(),
        requests,
        file_order,
        collapsed,
        !filter.is_empty(),
        &mut rows,
    );
    rows
}

pub(super) fn natural_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    let left = left.to_lowercase();
    let right = right.to_lowercase();
    let mut left = left.chars().peekable();
    let mut right = right.chars().peekable();
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (Some(left_character), Some(right_character))
                if left_character.is_ascii_digit() && right_character.is_ascii_digit() =>
            {
                let mut left_digits = String::new();
                let mut right_digits = String::new();
                while left.peek().is_some_and(char::is_ascii_digit) {
                    left_digits.extend(left.next());
                }
                while right.peek().is_some_and(char::is_ascii_digit) {
                    right_digits.extend(right.next());
                }
                let left_digits = left_digits.trim_start_matches('0');
                let right_digits = right_digits.trim_start_matches('0');
                let order = left_digits
                    .len()
                    .cmp(&right_digits.len())
                    .then_with(|| left_digits.cmp(right_digits));
                if !order.is_eq() {
                    return order;
                }
            }
            (Some(left_character), Some(right_character)) => {
                let order = left_character.cmp(&right_character);
                if !order.is_eq() {
                    return order;
                }
                left.next();
                right.next();
            }
            (left, right) => return left.cmp(&right),
        }
    }
}

pub(super) fn environments(text: &str) -> anyhow::Result<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Environment configuration must be a JSON object"))?;
    Ok(object
        .iter()
        .filter(|(name, value)| !name.starts_with('$') && value.is_object())
        .map(|(name, _)| name.clone())
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn names_and_explicit_groups_do_not_change_request_identity() {
        assert_eq!(
            super::presentation(
                "admin__user__01_get_profile",
                "",
                None,
                super::Grouping::DoubleUnderscore
            ),
            (vec!["admin".into(), "user".into()], "01_get_profile".into())
        );
        assert_eq!(
            super::presentation("user_login", "", None, super::Grouping::DoubleUnderscore),
            (vec![], "user_login".into())
        );
        assert_eq!(
            super::presentation("user_login", "", None, super::Grouping::FirstUnderscore),
            (vec!["user".into()], "login".into())
        );
        assert_eq!(
            super::presentation(
                "stableName",
                "02_列表",
                Some("01_用户/管理"),
                super::Grouping::DoubleUnderscore
            ),
            (vec!["01_用户".into(), "管理".into()], "02_列表".into())
        );
    }

    #[test]
    fn natural_order_is_numeric_stable_and_does_not_overflow() {
        let mut names = vec![
            "10_logout",
            "2_refresh",
            "1_login",
            "999999999999999999999999999999",
        ];
        names.sort_by(|a, b| super::natural_cmp(a, b));
        assert_eq!(
            names,
            [
                "1_login",
                "2_refresh",
                "10_logout",
                "999999999999999999999999999999"
            ]
        );
        assert!(super::natural_cmp("01_用户", "1_用户").is_eq());
        assert!(super::natural_cmp("a2", "A10").is_lt());
    }

    #[gpui::test]
    fn nested_groups_are_unique_filterable_and_naturally_sorted(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        let buffer = cx.new(|cx| language::Buffer::local("", cx));
        let anchor = buffer.read_with(cx, |buffer, _| buffer.anchor_before(0));
        let requests = [
            "user__10_last",
            "admin__user__2_edit",
            "user__2_first",
            "admin__1_settings",
            "ungrouped",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            let (group, title) = super::presentation(name, "", None, Default::default());
            super::Request {
                id: index as u64,
                anchor,
                range: anchor..anchor,
                name: name.into(),
                title,
                group,
                method: "GET".into(),
                url: "/users".into(),
                source: String::new(),
            }
        })
        .collect::<Vec<_>>();
        let rows = super::list_rows(&requests, "", false, &Default::default());
        let indices = rows
            .iter()
            .filter_map(|row| match row {
                super::ListRow::Request { index, .. } => Some(*index),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(indices, [1, 3, 2, 0, 4]);
        let groups = rows
            .iter()
            .filter_map(|row| match row {
                super::ListRow::Group { path, .. } => Some(path),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            groups
                .iter()
                .filter(|path| path.as_slice() == ["admin"])
                .count(),
            1
        );
        let collapsed = std::collections::BTreeSet::from([vec!["admin".into()]]);
        let filtered = super::list_rows(&requests, "edit", false, &collapsed);
        assert_eq!(filtered.len(), 3, "filtering expands matching ancestors");
        let ordered = super::list_rows(&requests, "", true, &Default::default());
        assert!(
            matches!(ordered.first(), Some(super::ListRow::Group { label, .. }) if label == "user")
        );
    }

    #[test]
    fn environments_are_static_and_exclude_shared_values() {
        assert_eq!(
            super::environments(
                r#"{"$shared":{"token":"secret"},"dev":{"host":"localhost"},"prod":{}}"#
            )
            .unwrap(),
            ["dev", "prod"]
        );
        assert!(super::environments("module.exports = {}").is_err());
    }
}
