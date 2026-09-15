use gpui::AppContext as _;

pub struct BlameDiff {
    pub state: BlameDiffState,
    pub scroll_handle: gpui::ScrollHandle,
    _task: gpui::Task<()>,
}

pub enum BlameDiffState {
    Loading,
    Unavailable(gpui::SharedString),
    Ready(BlameDiffHunk),
}

pub struct BlameDiffHunk {
    pub header: gpui::SharedString,
    pub lines: Vec<BlameDiffLine>,
    pub truncated: bool,
}

pub struct BlameDiffLine {
    pub old_row: Option<u32>,
    pub new_row: Option<u32>,
    pub prefix: char,
    pub text: gpui::SharedString,
    pub is_current: bool,
}

impl BlameDiff {
    pub(super) fn new(
        repository: Option<gpui::Entity<project::git_store::Repository>>,
        entry: &::git::blame::BlameEntry,
        row: Option<u32>,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let request = repository.zip(row).filter(|_| !entry.sha.is_zero());
        let (state, task) = if let Some((repository, row)) = request {
            let path = ::git::repository::RepoPath::new(&entry.filename);
            let receiver = repository.update(cx, |repository, _| {
                repository.load_commit_diff(entry.sha.to_string(), false)
            });
            let task = cx.spawn(async move |this, cx| {
                let result = async {
                    let path = path?;
                    let commit = receiver.await??;
                    if commit.is_shallow_boundary {
                        return Ok(BlameDiffState::Unavailable(
                            "Diff unavailable at shallow history boundary".into(),
                        ));
                    }
                    let Some(file) = commit.files.into_iter().find(|file| file.path == path) else {
                        return Ok(BlameDiffState::Unavailable(
                            "No file diff against the first parent".into(),
                        ));
                    };
                    load_file_diff(file, row, cx).await
                }
                .await;
                let state = match result {
                    Ok(state) => state,
                    Err(error) => {
                        log::warn!("Failed to load inline blame diff: {error:#}");
                        BlameDiffState::Unavailable("Failed to load commit diff".into())
                    }
                };
                if let Err(error) = this.update(cx, |this, cx| {
                    if let BlameDiffState::Ready(hunk) = &state
                        && let Some(index) = hunk.lines.iter().position(|line| line.is_current)
                        && index >= 8
                    {
                        this.scroll_handle.scroll_to_item(index.saturating_sub(4));
                    }
                    this.state = state;
                    cx.notify();
                }) {
                    log::debug!("Inline blame diff dismissed: {error:#}");
                }
            });
            (BlameDiffState::Loading, task)
        } else {
            (
                BlameDiffState::Unavailable("Historical line unavailable".into()),
                gpui::Task::ready(()),
            )
        };
        Self {
            state,
            scroll_handle: gpui::ScrollHandle::new(),
            _task: task,
        }
    }
}

async fn load_file_diff(
    file: project::git_store::CommitFile,
    row: u32,
    cx: &mut gpui::AsyncApp,
) -> anyhow::Result<BlameDiffState> {
    if file.is_binary {
        return Ok(BlameDiffState::Unavailable("Binary diff not shown".into()));
    }
    // The existing commit API loads all files; bound the additional diff computation here.
    const MAX_FILE_BYTES: usize = 1024 * 1024;
    if file
        .old_text
        .as_ref()
        .is_some_and(|text| text.len() > MAX_FILE_BYTES)
        || file
            .new_text
            .as_ref()
            .is_some_and(|text| text.len() > MAX_FILE_BYTES)
    {
        return Ok(BlameDiffState::Unavailable(
            "File too large for an inline preview".into(),
        ));
    }
    let mut old_text = file.old_text;
    let mut new_text = file.new_text.unwrap_or_default();
    if let Some(text) = &mut old_text {
        language::LineEnding::normalize(text);
    }
    language::LineEnding::normalize(&mut new_text);
    let buffer = cx.new(|cx| language::Buffer::local(new_text.clone(), cx));
    let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());
    let diff = cx.new(|cx| buffer_diff::BufferDiff::new(&snapshot.text, None, None, cx));
    diff.update(cx, |diff, cx| {
        diff.set_base_text(
            old_text.as_deref().map(std::sync::Arc::from),
            snapshot.text.clone(),
            cx,
        )
    })
    .await;
    let hunks = diff.read_with(cx, |diff, cx| {
        let diff = diff.snapshot(cx);
        diff.hunks(&snapshot.text)
            .map(|hunk| {
                let old_start = diff
                    .base_text()
                    .offset_to_point(hunk.diff_base_byte_range.start);
                let old_end = diff
                    .base_text()
                    .offset_to_point(hunk.diff_base_byte_range.end);
                (
                    old_start.row..old_end.row + u32::from(old_end.column > 0),
                    hunk.range.start.row..hunk.range.end.row + u32::from(hunk.range.end.column > 0),
                )
            })
            .collect::<Vec<_>>()
    });
    Ok(cx
        .background_spawn(async move {
            preview_hunk(
                old_text.as_deref().unwrap_or_default(),
                &new_text,
                &hunks,
                row,
            )
            .map(BlameDiffState::Ready)
            .unwrap_or_else(|| {
                BlameDiffState::Unavailable("No matching hunk against the first parent".into())
            })
        })
        .await)
}

fn preview_hunk(
    old_text: &str,
    new_text: &str,
    hunks: &[(std::ops::Range<u32>, std::ops::Range<u32>)],
    row: u32,
) -> Option<BlameDiffHunk> {
    let index = hunks.iter().position(|(_, new)| new.contains(&row))?;
    let (old, new) = hunks.get(index)?;
    let old_lines = old_text.lines().collect::<Vec<_>>();
    let new_lines = new_text.lines().collect::<Vec<_>>();
    if row as usize >= new_lines.len() {
        return None;
    }
    let previous = index.checked_sub(1).and_then(|index| hunks.get(index));
    let next = hunks.get(index + 1);
    let before = 3
        .min(
            old.start
                .saturating_sub(previous.map_or(0, |(old, _)| old.end)),
        )
        .min(
            new.start
                .saturating_sub(previous.map_or(0, |(_, new)| new.end)),
        );
    let after = 3
        .min(
            next.map_or(old_lines.len() as u32, |(old, _)| old.start)
                .saturating_sub(old.end),
        )
        .min(
            next.map_or(new_lines.len() as u32, |(_, new)| new.start)
                .saturating_sub(new.end),
        );
    let mut preview = BlameDiffHunk {
        header: format!(
            "@@ -{},{} +{},{} @@",
            if old.is_empty() && before == 0 && after == 0 {
                old.start
            } else {
                old.start - before + 1
            },
            old.len() as u32 + before + after,
            new.start - before + 1,
            new.len() as u32 + before + after,
        )
        .into(),
        lines: Vec::new(),
        truncated: false,
    };
    for offset in 0..before {
        let old_row = old.start - before + offset;
        let new_row = new.start - before + offset;
        preview.push_line(
            ' ',
            Some(old_row),
            Some(new_row),
            new_lines.get(new_row as usize)?,
            row,
        );
    }
    const MAX_SIDE_LINES: u32 = 60;
    for old_row in old.start..old.end.min(old.start + MAX_SIDE_LINES) {
        preview.push_line(
            '-',
            Some(old_row),
            None,
            old_lines.get(old_row as usize)?,
            row,
        );
    }
    if old.len() > MAX_SIDE_LINES as usize {
        preview.omitted();
    }
    let new_start = row
        .saturating_sub(MAX_SIDE_LINES / 2)
        .max(new.start)
        .min(new.end.saturating_sub(MAX_SIDE_LINES).max(new.start));
    let new_end = new.end.min(new_start + MAX_SIDE_LINES);
    if new_start > new.start {
        preview.omitted();
    }
    for new_row in new_start..new_end {
        preview.push_line(
            '+',
            None,
            Some(new_row),
            new_lines.get(new_row as usize)?,
            row,
        );
    }
    if new_end < new.end {
        preview.omitted();
    }
    for offset in 0..after {
        let old_row = old.end + offset;
        let new_row = new.end + offset;
        preview.push_line(
            ' ',
            Some(old_row),
            Some(new_row),
            new_lines.get(new_row as usize)?,
            row,
        );
    }
    Some(preview)
}

impl BlameDiffHunk {
    fn push_line(
        &mut self,
        prefix: char,
        old_row: Option<u32>,
        new_row: Option<u32>,
        text: &str,
        row: u32,
    ) {
        if text.chars().count() > 2000 {
            self.truncated = true;
        }
        let text = util::truncate_and_trailoff(text, 2000);
        self.lines.push(BlameDiffLine {
            old_row,
            new_row,
            prefix,
            text: text.into(),
            is_current: new_row == Some(row),
        });
    }

    fn omitted(&mut self) {
        self.truncated = true;
        self.lines.push(BlameDiffLine {
            old_row: None,
            new_row: None,
            prefix: '…',
            text: "Lines omitted".into(),
            is_current: false,
        });
    }
}

#[cfg(test)]
mod tests {
    #[gpui::test]
    async fn test_blame_diff_hunks(cx: &mut gpui::TestAppContext) {
        let file = project::git_store::CommitFile {
            path: ::git::repository::repo_path("历史 file.txt"),
            old_text: Some(
                "old first\r\none\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nold last".into(),
            ),
            new_text: Some(
                "new first\r\none\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nnew last".into(),
            ),
            is_binary: false,
        };
        let state = super::load_file_diff(file, 7, &mut cx.to_async())
            .await
            .unwrap();
        let super::BlameDiffState::Ready(hunk) = state else {
            panic!("expected the second hunk");
        };
        assert_eq!(hunk.header.as_ref(), "@@ -5,4 +5,4 @@");
        let changed = hunk
            .lines
            .iter()
            .filter(|line| line.prefix != ' ')
            .map(|line| (line.prefix, line.text.as_ref(), line.is_current))
            .collect::<Vec<_>>();
        assert_eq!(
            changed,
            vec![('-', "old last", false), ('+', "new last", true)]
        );
    }

    #[gpui::test]
    async fn test_blame_diff_root_and_unavailable(cx: &mut gpui::TestAppContext) {
        let file = || project::git_store::CommitFile {
            path: ::git::repository::repo_path("new.txt"),
            old_text: None,
            new_text: Some("one\ntwo\n".into()),
            is_binary: false,
        };
        let state = super::load_file_diff(file(), 1, &mut cx.to_async())
            .await
            .unwrap();
        let super::BlameDiffState::Ready(hunk) = state else {
            panic!("expected root diff")
        };
        assert_eq!(hunk.header.as_ref(), "@@ -0,0 +1,2 @@");
        assert!(hunk.lines.iter().all(|line| line.prefix == '+'));
        assert!(
            hunk.lines
                .iter()
                .any(|line| line.is_current && line.text.as_ref() == "two")
        );

        let mut binary = file();
        binary.is_binary = true;
        assert!(matches!(
            super::load_file_diff(binary, 1, &mut cx.to_async())
                .await
                .unwrap(),
            super::BlameDiffState::Unavailable(_)
        ));
        let mut large = file();
        large.new_text = Some("x".repeat(1024 * 1024 + 1));
        assert!(matches!(
            super::load_file_diff(large, 0, &mut cx.to_async())
                .await
                .unwrap(),
            super::BlameDiffState::Unavailable(_)
        ));
        let mut unchanged = file();
        unchanged.old_text = unchanged.new_text.clone();
        assert!(matches!(
            super::load_file_diff(unchanged, 1, &mut cx.to_async())
                .await
                .unwrap(),
            super::BlameDiffState::Unavailable(_)
        ));
    }

    #[test]
    fn test_blame_diff_truncation_keeps_current_line() {
        let old = (0..200)
            .map(|row| format!("old {row}\n"))
            .collect::<String>();
        let new = (0..200)
            .map(|row| format!("new {row}\n"))
            .collect::<String>();
        let hunk = super::preview_hunk(&old, &new, &[(0..200, 0..200)], 170).unwrap();
        assert!(hunk.truncated);
        assert!(hunk.lines.len() <= 123);
        assert!(hunk.lines.iter().any(|line| line.prefix == '-'));
        assert!(
            hunk.lines
                .iter()
                .any(|line| line.is_current && line.text.as_ref() == "new 170")
        );
        let long = "文".repeat(3000);
        let hunk = super::preview_hunk("", &long, &[(0..0, 0..1)], 0).unwrap();
        assert!(hunk.truncated);
        assert!(hunk.lines[0].text.chars().count() <= 2001);
    }

    #[test]
    fn test_blame_diff_does_not_include_neighboring_changes() {
        let hunk = super::preview_hunk(
            "a\nb\nc\nd\ne\n",
            "A\nb\nC\nd\nE\n",
            &[(0..1, 0..1), (2..3, 2..3), (4..5, 4..5)],
            2,
        )
        .unwrap();
        assert_eq!(hunk.header.as_ref(), "@@ -2,3 +2,3 @@");
        assert_eq!(
            hunk.lines
                .iter()
                .map(|line| line.text.as_ref())
                .collect::<Vec<_>>(),
            vec!["b", "c", "C", "d"]
        );
        assert!(super::preview_hunk("a\nb\n", "A\nb\n", &[(0..1, 0..1)], 1).is_none());
    }
}
