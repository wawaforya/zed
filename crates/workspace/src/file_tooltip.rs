use gpui::{IntoElement, ParentElement, Styled};
use ui::{ActiveTheme, LabelCommon};

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct FileTooltipDetails {
    file_type: gpui::SharedString,
    size: gpui::SharedString,
    path: gpui::SharedString,
    update: gpui::SharedString,
}

impl FileTooltipDetails {
    pub fn new(
        entry: Option<&project::Entry>,
        path: &util::rel_path::RelPath,
        path_style: util::paths::PathStyle,
        languages: &std::sync::Arc<language::LanguageRegistry>,
    ) -> Self {
        let extension = path
            .as_std_path()
            .extension()
            .and_then(|extension| extension.to_str());
        let file_type = if entry.is_some_and(|entry| entry.is_fifo) {
            gpui::SharedString::from("Named Pipe")
        } else if let Some(language_name) =
            languages.language_name_for_file_path(path.as_std_path())
        {
            match extension {
                Some(extension) => format!("{language_name} (.{extension})").into(),
                None => language_name.into(),
            }
        } else {
            match extension {
                Some(extension) => format!("{} File", extension.to_uppercase()).into(),
                None => gpui::SharedString::from("File"),
            }
        };

        Self {
            file_type,
            size: entry
                .filter(|entry| !entry.is_fifo)
                .map(|entry| util::size::format_file_size(entry.size, false).into())
                .unwrap_or_else(|| "Unavailable".into()),
            path: path.display(path_style).to_string().into(),
            update: entry
                .and_then(|entry| entry.mtime)
                .map(|mtime| {
                    chrono::DateTime::<chrono::Local>::from(mtime.timestamp_for_user())
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                        .into()
                })
                .unwrap_or_else(|| "Unavailable".into()),
        }
    }
}

pub fn render_file_tooltip(details: &FileTooltipDetails, cx: &mut gpui::App) -> gpui::AnyElement {
    ui::v_flex()
        .w_72()
        .min_w_0()
        .gap_0p5()
        .children(
            [
                ("Type", details.file_type.clone()),
                ("Size", details.size.clone()),
                ("Path", details.path.clone()),
                ("Update", details.update.clone()),
            ]
            .into_iter()
            .map(|(label, value)| {
                ui::h_flex()
                    .w_full()
                    .min_w_0()
                    .items_start()
                    .gap_2()
                    .child(
                        gpui::div().w(gpui::rems(3.5)).flex_none().child(
                            ui::Label::new(label)
                                .size(ui::LabelSize::Small)
                                .color(ui::Color::Muted),
                        ),
                    )
                    .child(
                        gpui::div()
                            .min_w_0()
                            .flex_1()
                            .whitespace_normal()
                            .child(ui::Label::new(value).size(ui::LabelSize::Small)),
                    )
            }),
        )
        .text_color(cx.theme().colors().text)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    #[gpui::test]
    fn missing_file_metadata(cx: &gpui::TestAppContext) {
        let languages = std::sync::Arc::new(language::LanguageRegistry::new(
            cx.background_executor.clone(),
        ));

        for (path, file_type) in [
            ("src/notes.txt", "Plain Text (.txt)"),
            ("src/file.unknown", "UNKNOWN File"),
            ("LICENSE", "File"),
        ] {
            let path = util::rel_path::RelPath::new_test(path);
            let details = super::FileTooltipDetails::new(
                None,
                &path,
                util::paths::PathStyle::Unix,
                &languages,
            );
            assert_eq!(details.file_type.as_ref(), file_type);
            assert_eq!(details.size.as_ref(), "Unavailable");
            assert_eq!(details.path.as_ref(), path.as_unix_str());
            assert_eq!(details.update.as_ref(), "Unavailable");
        }
    }

    #[gpui::test]
    fn file_metadata_and_named_pipe(cx: &gpui::TestAppContext) {
        let languages = std::sync::Arc::new(language::LanguageRegistry::new(
            cx.background_executor.clone(),
        ));
        let mut entry = project::Entry {
            id: project::ProjectEntryId::from_usize(1),
            kind: project::EntryKind::File,
            path: util::rel_path::RelPath::new_test("src/notes.txt").into(),
            inode: 0,
            mtime: Some(fs::MTime::from_seconds_and_nanos(1_700_000_000, 0)),
            canonical_path: None,
            is_ignored: false,
            is_hidden: false,
            is_always_included: false,
            is_external: false,
            is_private: false,
            size: 1024,
            char_bag: Default::default(),
            is_fifo: false,
        };
        let details = super::FileTooltipDetails::new(
            Some(&entry),
            &entry.path,
            util::paths::PathStyle::Windows,
            &languages,
        );
        assert_eq!(details.file_type.as_ref(), "Plain Text (.txt)");
        assert_eq!(
            details.size.as_ref(),
            util::size::format_file_size(1024, false)
        );
        assert_eq!(details.path.as_ref(), "src\\notes.txt");
        assert_eq!(
            details.update.as_ref(),
            chrono::DateTime::<chrono::Local>::from(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)
            )
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
        );

        entry.is_fifo = true;
        entry.mtime = None;
        let details = super::FileTooltipDetails::new(
            Some(&entry),
            &entry.path,
            util::paths::PathStyle::Unix,
            &languages,
        );
        assert_eq!(details.file_type.as_ref(), "Named Pipe");
        assert_eq!(details.size.as_ref(), "Unavailable");
        assert_eq!(details.update.as_ref(), "Unavailable");
    }
}
