use super::*;

#[derive(Clone, Copy)]
enum JsonDocumentKind {
    Json,
    Jsonc,
}

impl Editor {
    pub(crate) fn can_transform_json_document(&self, cx: &App) -> bool {
        self.mode.is_full() && !self.read_only(cx) && self.json_document_kind(cx).is_some()
    }

    fn json_document_kind(&self, cx: &App) -> Option<JsonDocumentKind> {
        let buffer = self.buffer.read(cx).as_singleton()?;
        let buffer = buffer.read(cx);
        let language = buffer.language()?;
        if language.name() == "JSON" {
            Some(JsonDocumentKind::Json)
        } else if language.name() == "JSONC" {
            Some(JsonDocumentKind::Jsonc)
        } else {
            None
        }
    }

    pub(crate) fn can_transform_json_selection(&self, cx: &App) -> bool {
        if self.read_only(cx) {
            return false;
        }

        let selections = self.selections.disjoint_anchors();
        let snapshot = self.buffer.read(cx).snapshot(cx);
        !selections.is_empty()
            && selections.iter().all(|selection| {
                selection.start.to_offset(&snapshot) != selection.end.to_offset(&snapshot)
            })
    }

    pub fn minify_json(
        &mut self,
        _: &Minify,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if !self.can_transform_json_document(cx) {
            return Ok(());
        }

        let Some(document_kind) = self.json_document_kind(cx) else {
            return Ok(());
        };
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let whitespace_ranges = json_whitespace_ranges(&snapshot.text(), document_kind)?;
        if whitespace_ranges.is_empty() {
            return Ok(());
        }

        let edits = whitespace_ranges.into_iter().map(|range| {
            (
                MultiBufferOffset(range.start)..MultiBufferOffset(range.end),
                String::new(),
            )
        });
        self.transact(window, cx, |this, _, cx| {
            this.buffer
                .update(cx, |buffer, cx| buffer.edit(edits, None, cx));
        });

        Ok(())
    }

    pub fn stringify_json_selections(
        &mut self,
        _: &StringifySelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.transform_json_selections(window, cx, |text| {
            serde_json::to_string(text).context("failed to encode selection as a JSON string")
        })
    }

    pub fn parse_json_selections(
        &mut self,
        _: &ParseSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.transform_json_selections(window, cx, |text| {
            serde_json::from_str::<String>(text)
                .context("selection is not a valid JSON string literal")
        })
    }

    fn transform_json_selections(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        mut transform: impl FnMut(&str) -> Result<String>,
    ) -> Result<()> {
        if !self.can_transform_json_selection(cx) {
            return Ok(());
        }

        let buffer = self.buffer.read(cx).snapshot(cx);
        let selections = self.selections.all_adjusted(&self.display_snapshot(cx));
        let mut new_selections = Vec::with_capacity(selections.len());
        let mut edits = Vec::with_capacity(selections.len());

        for (index, selection) in selections.into_iter().enumerate() {
            let start = buffer.point_to_offset(selection.start);
            let end = buffer.point_to_offset(selection.end);
            let old_text = buffer.text_for_range(start..end).collect::<String>();
            let new_text = transform(&old_text)
                .with_context(|| format!("failed to transform JSON selection {}", index + 1))?;

            new_selections.push(Selection {
                start: buffer.anchor_before(start),
                end: buffer.anchor_after(end),
                goal: SelectionGoal::None,
                id: selection.id,
                reversed: selection.reversed,
            });
            edits.push((start..end, new_text));
        }

        self.transact(window, cx, |this, window, cx| {
            this.buffer
                .update(cx, |buffer, cx| buffer.edit(edits, None, cx));
            this.change_selections(Default::default(), window, cx, |selections| {
                selections.select(new_selections);
            });
            this.request_autoscroll(Autoscroll::fit(), cx);
        });

        Ok(())
    }
}

fn json_whitespace_ranges(
    text: &str,
    document_kind: JsonDocumentKind,
) -> Result<Vec<Range<usize>>> {
    // Validation is separate from the edits, so minification preserves duplicate keys, comments,
    // number spellings, and string escapes.
    match document_kind {
        JsonDocumentKind::Json => {
            serde_json::from_str::<&serde_json::value::RawValue>(text)
                .context("document is not valid JSON")?;
        }
        JsonDocumentKind::Jsonc => {
            serde_json_lenient::from_str::<serde_json_lenient::Value>(text)
                .context("document is not valid JSONC")?;
        }
    }

    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
        } else if byte == b'"' {
            in_string = true;
            index += 1;
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && !matches!(bytes[index], b'\r' | b'\n') {
                index += 1;
            }
            if bytes.get(index) == Some(&b'\r') {
                index += 1;
            }
            if bytes.get(index) == Some(&b'\n') {
                index += 1;
            }
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                index += 1;
            }
            index = (index + 2).min(bytes.len());
        } else if matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
            let start = index;
            index += 1;
            while index < bytes.len() && matches!(bytes[index], b' ' | b'\t' | b'\r' | b'\n') {
                index += 1;
            }
            ranges.push(start..index);
        } else {
            index += 1;
        }
    }

    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minification_preserves_json_lexemes() {
        let input = " { \"duplicate\": 1, \"duplicate\": 1e+10, \"text\": \"a b\\\\c\" } \n";
        let ranges =
            json_whitespace_ranges(input, JsonDocumentKind::Json).expect("JSON should be valid");
        let mut output = input.to_owned();
        for range in ranges.into_iter().rev() {
            output.replace_range(range, "");
        }

        assert_eq!(
            output,
            "{\"duplicate\":1,\"duplicate\":1e+10,\"text\":\"a b\\\\c\"}"
        );
    }

    #[test]
    fn minification_preserves_jsonc_comments_and_trailing_commas() {
        let input = concat!(
            "{\n",
            "  // Keep this comment\n",
            "  \"url\": \"https://zed.dev/a/*b*/\",\n",
            "  \"value\": 1 /* unit */,\n",
            "}\n",
        );
        let ranges =
            json_whitespace_ranges(input, JsonDocumentKind::Jsonc).expect("JSONC should be valid");
        let mut output = input.to_owned();
        for range in ranges.into_iter().rev() {
            output.replace_range(range, "");
        }

        assert_eq!(
            output,
            concat!(
                "{// Keep this comment\n",
                "\"url\":\"https://zed.dev/a/*b*/\",\"value\":1/* unit */,}"
            )
        );
    }

    #[test]
    fn minification_rejects_invalid_json() {
        assert!(json_whitespace_ranges("{ \"key\": }", JsonDocumentKind::Json).is_err());
        assert!(json_whitespace_ranges("{ \"key\": }", JsonDocumentKind::Jsonc).is_err());
    }
}
