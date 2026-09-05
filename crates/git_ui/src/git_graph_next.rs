pub use crate::commit_context_menu::{CopyCommitSha, CopyCommitTag, OpenCommitView};
use crate::{
    commit_context_menu::{CommitContextMenuData, CommitContextMenuSource, commit_context_menu},
    commit_tooltip::CommitAvatar,
    commit_view::CommitView,
    git_status_icon,
};
use collections::{BTreeMap, HashMap, IndexSet};
use editor::Editor;
use file_icons::FileIcons;
use git::{
    BuildCommitPermalinkParams, GitHostingProviderRegistry, GitRemote, Oid, ParsedGitRemote,
    parse_git_remote_url,
    repository::{InitialGraphCommitData, LogOrder, LogSource, RepoPath, SearchCommitArgs},
    status::{FileStatus, StatusCode, TrackedStatus},
};
use gpui::{
    Action, Anchor, AnyElement, App, Bounds, ClickEvent, ClipboardItem, DefiniteLength,
    DismissEvent, DragMoveEvent, ElementId, Empty, Entity, EventEmitter, FocusHandle, Focusable,
    Hsla, MouseButton, MouseDownEvent, PathBuilder, Pixels, Point, ScrollHandle, ScrollStrategy,
    ScrollWheelEvent, SharedString, Subscription, Task, TextStyleRefinement,
    UniformListScrollHandle, WeakEntity, Window, actions, anchored, deferred, point, prelude::*,
    px, uniform_list,
};
use language::line_diff;
use markdown::{Markdown, MarkdownElement};
use menu::{Cancel, SelectFirst, SelectLast, SelectNext, SelectPrevious};
use picker::{Picker, PickerDelegate};
use project::{
    ProjectPath,
    git_store::{
        CommitDataState, CommitDiff, CommitFile, GitGraphEvent, GitStore, GitStoreEvent,
        GraphDataResponse, Repository, RepositoryEvent, RepositoryId,
    },
};
use smallvec::{SmallVec, smallvec};
use std::{
    cell::Cell,
    ops::Range,
    rc::Rc,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use zed_actions::{
    buffer_search,
    search::{SelectNextMatch, SelectPreviousMatch, ToggleCaseSensitive},
};

use theme::AccentColors;
use time::{OffsetDateTime, UtcOffset, format_description::BorrowedFormatItem};
use ui::{
    Chip, ColumnWidthConfig, CommonAnimationExt as _, ContextMenu, DiffStat, Divider,
    HeaderResizeInfo, HighlightedLabel, IndentGuideColors, ListItem, ListItemSpacing,
    RedistributableColumnsState, ScrollableHandle, Table, TableInteractionState,
    TableRenderContext, TableResizeBehavior, Tooltip, WithScrollbar, bind_redistributable_columns,
    prelude::*, redistribute_hidden_fractions, redistribute_hidden_widths,
    render_redistributable_columns_resize_handles, render_table_header, table_row::TableRow,
};
use util::{ResultExt, debug_panic};
use workspace::{
    ModalView, Workspace,
    item::{Item, ItemEvent, TabTooltipContent},
};

const COMMIT_CIRCLE_RADIUS: Pixels = px(3.5);
const COMMIT_CIRCLE_STROKE_WIDTH: Pixels = px(1.5);
const LANE_WIDTH: Pixels = px(16.0);
const LEFT_PADDING: Pixels = px(12.0);
const LINE_WIDTH: Pixels = px(1.5);
const RESIZE_HANDLE_WIDTH: f32 = 8.0;
const COPIED_STATE_DURATION: Duration = Duration::from_secs(2);
const COMMIT_TAG_LIST_WIDTH_IN_REMS: Rems = rems(10.);
const TREE_INDENT: f32 = 20.0;
const TABLE_COLUMN_COUNT: usize = 4;
const ROW_VERTICAL_PADDING: Pixels = px(4.0);

struct CopiedState {
    copied_at: Option<Instant>,
}

impl CopiedState {
    fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { copied_at: None }
    }

    fn is_copied(&self) -> bool {
        self.copied_at
            .map(|t| t.elapsed() < COPIED_STATE_DURATION)
            .unwrap_or(false)
    }

    fn mark_copied(&mut self) {
        self.copied_at = Some(Instant::now());
    }
}

struct DraggedGraphDetailSplitHandle;
struct DraggedDetailContentSplitHandle;

struct CommitTagPicker {
    picker: Entity<Picker<CommitTagPickerDelegate>>,
}

impl CommitTagPicker {
    fn new(tag_names: Vec<SharedString>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let delegate = CommitTagPickerDelegate {
            picker: cx.entity().downgrade(),
            tag_names,
            selected_index: 0,
        };
        let picker = cx.new(|cx| {
            Picker::nonsearchable_uniform_list(delegate, window, cx)
                .initial_width(COMMIT_TAG_LIST_WIDTH_IN_REMS)
        });
        Self { picker }
    }
}

impl EventEmitter<DismissEvent> for CommitTagPicker {}
impl ModalView for CommitTagPicker {}

impl Focusable for CommitTagPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for CommitTagPicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().child(self.picker.clone())
    }
}

struct CommitTagPickerDelegate {
    picker: WeakEntity<CommitTagPicker>,
    tag_names: Vec<SharedString>,
    selected_index: usize,
}

impl PickerDelegate for CommitTagPickerDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "commit-tag"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Copy Tag".into()
    }

    fn match_count(&self) -> usize {
        self.tag_names.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn update_matches(
        &mut self,
        _query: String,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        Task::ready(())
    }

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        if let Some(tag_name) = self.tag_names.get(self.selected_index) {
            cx.write_to_clipboard(ClipboardItem::new_string(tag_name.to_string()));
        }
        self.dismissed(window, cx);
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.picker
            .update(cx, |_this, cx| cx.emit(DismissEvent))
            .ok();
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        Some(
            ListItem::new(ix)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(Label::new(self.tag_names.get(ix)?.clone())),
        )
    }
}

#[derive(Clone)]
struct ChangedFileEntry {
    status: FileStatus,
    file_name: SharedString,
    dir_path: SharedString,
    repo_path: RepoPath,
    diff_stats: Option<(usize, usize)>,
}

impl ChangedFileEntry {
    fn from_commit_file(file: &CommitFile, _cx: &App) -> Self {
        let file_name: SharedString = file
            .path
            .file_name()
            .map(|n| n.to_string())
            .unwrap_or_default()
            .into();
        let dir_path: SharedString = file
            .path
            .parent()
            .map(|p| p.as_unix_str().to_string())
            .unwrap_or_default()
            .into();

        let status_code = match (&file.old_text, &file.new_text) {
            (None, Some(_)) => StatusCode::Added,
            (Some(_), None) => StatusCode::Deleted,
            _ => StatusCode::Modified,
        };

        let status = FileStatus::Tracked(TrackedStatus {
            index_status: status_code,
            worktree_status: StatusCode::Unmodified,
        });

        Self {
            status,
            file_name,
            dir_path,
            repo_path: file.path.clone(),
            diff_stats: (!file.is_binary).then(|| compute_file_diff_stats(file)),
        }
    }

    fn open_in_commit_view(
        &self,
        commit_sha: &SharedString,
        repository: &WeakEntity<Repository>,
        workspace: &WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) {
        CommitView::open(
            commit_sha.to_string(),
            repository.clone(),
            workspace.clone(),
            None,
            Some(self.repo_path.clone()),
            window,
            cx,
        );
    }

    fn render(
        &self,
        ix: usize,
        depth: usize,
        directory_label: Option<SharedString>,
        commit_sha: SharedString,
        repository: WeakEntity<Repository>,
        workspace: WeakEntity<Workspace>,
        git_graph: WeakEntity<GitGraphNext>,
        selected: bool,
        _cx: &App,
    ) -> AnyElement {
        let file_name = self.file_name.clone();
        let dir_path = self.dir_path.clone();

        ListItem::new(("changed-file", ix))
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected)
            .indent_level(depth)
            .indent_step_size(px(TREE_INDENT))
            .start_slot(git_status_icon(self.status))
            .child(
                Label::new(file_name.clone())
                    .size(LabelSize::Small)
                    .truncate(),
            )
            .when_some(directory_label, |this, directory_label| {
                this.child(
                    Label::new(directory_label)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate_start(),
                )
            })
            .tooltip({
                let meta = if dir_path.is_empty() {
                    file_name
                } else {
                    format!("{}/{}", dir_path, file_name).into()
                };
                move |_, cx| Tooltip::with_meta("View Changes", None, meta.clone(), cx)
            })
            .on_click({
                let entry = self.clone();
                move |event: &ClickEvent, window, cx| {
                    if event.click_count() >= 2 {
                        entry.open_in_commit_view(&commit_sha, &repository, &workspace, window, cx);
                    } else {
                        git_graph
                            .update(cx, |git_graph, cx| {
                                git_graph.select_changed_file(entry.repo_path.clone(), window, cx);
                            })
                            .ok();
                    }
                }
            })
            .into_any_element()
    }
}

enum ChangedFileTreeEntry {
    Directory(ChangedFileDirectoryEntry),
    File(ChangedFileTreeStatusEntry),
}

struct ChangedFileTreeStatusEntry {
    entry: ChangedFileEntry,
    depth: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ChangedFilesViewMode {
    Flat,
    #[default]
    Tree,
}

impl ChangedFilesViewMode {
    fn toggled(self) -> Self {
        match self {
            Self::Flat => Self::Tree,
            Self::Tree => Self::Flat,
        }
    }

    fn is_tree(self) -> bool {
        matches!(self, Self::Tree)
    }
}

struct ChangedFileDirectoryEntry {
    path: RepoPath,
    name: SharedString,
    depth: usize,
    expanded: bool,
}

impl ChangedFileDirectoryEntry {
    fn render(&self, ix: usize, git_graph: WeakEntity<GitGraphNext>, cx: &App) -> AnyElement {
        let path = self.path.clone();
        let expanded = self.expanded;
        let folder_icon = FileIcons::get_folder_icon(expanded, path.as_std_path(), cx)
            .map(|icon| {
                Icon::from_path(icon)
                    .size(IconSize::Small)
                    .color(Color::Muted)
            })
            .unwrap_or_else(|| {
                let icon = if expanded {
                    IconName::FolderOpen
                } else {
                    IconName::Folder
                };
                Icon::new(icon).size(IconSize::Small).color(Color::Muted)
            });

        ListItem::new(("changed-file-dir", ix))
            .spacing(ListItemSpacing::Sparse)
            .indent_level(self.depth)
            .indent_step_size(px(TREE_INDENT))
            .start_slot(folder_icon)
            .child(
                Label::new(self.name.clone())
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .truncate(),
            )
            .tooltip({
                let name = self.name.clone();
                move |_, cx| Tooltip::with_meta("Toggle Folder", None, name.clone(), cx)
            })
            .on_click(move |_, _, cx| {
                git_graph
                    .update(cx, |git_graph, cx| {
                        git_graph
                            .changed_files_expanded_dirs
                            .insert(path.clone(), !expanded);
                        cx.notify();
                    })
                    .ok();
            })
            .into_any_element()
    }
}

#[derive(Default)]
struct ChangedFileTreeNode {
    name: SharedString,
    path: Option<RepoPath>,
    children: BTreeMap<SharedString, ChangedFileTreeNode>,
    files: Vec<ChangedFileEntry>,
}

fn build_changed_file_tree_entries(
    mut files: Vec<ChangedFileEntry>,
    expanded_dirs: &HashMap<RepoPath, bool>,
) -> Vec<ChangedFileTreeEntry> {
    files.sort_by(|a, b| a.repo_path.cmp(&b.repo_path));

    let mut root = ChangedFileTreeNode::default();
    for file in files {
        let components: Vec<&str> = file.repo_path.components().collect();
        if components.is_empty() {
            root.files.push(file);
            continue;
        }

        let mut current = &mut root;
        let mut current_path = String::new();

        for (ix, component) in components.iter().enumerate() {
            if ix == components.len() - 1 {
                current.files.push(file.clone());
            } else {
                if !current_path.is_empty() {
                    current_path.push('/');
                }
                current_path.push_str(component);

                let Ok(dir_path) = RepoPath::new(&current_path) else {
                    continue;
                };
                let component = SharedString::from(component.to_string());

                current = current
                    .children
                    .entry(component.clone())
                    .or_insert_with(|| ChangedFileTreeNode {
                        name: component,
                        path: Some(dir_path),
                        ..Default::default()
                    });
            }
        }
    }

    flatten_changed_file_tree(&root, 0, expanded_dirs)
}

fn flatten_changed_file_tree(
    node: &ChangedFileTreeNode,
    depth: usize,
    expanded_dirs: &HashMap<RepoPath, bool>,
) -> Vec<ChangedFileTreeEntry> {
    let mut entries = Vec::new();

    for child in node.children.values() {
        let (terminal, name) = compact_changed_file_directory_chain(child);
        let Some(path) = terminal.path.clone().or_else(|| child.path.clone()) else {
            continue;
        };
        let expanded = *expanded_dirs.get(&path).unwrap_or(&true);
        let child_entries = flatten_changed_file_tree(terminal, depth + 1, expanded_dirs);

        entries.push(ChangedFileTreeEntry::Directory(ChangedFileDirectoryEntry {
            path,
            name,
            depth,
            expanded,
        }));

        if expanded {
            entries.extend(child_entries);
        }
    }

    entries.extend(
        node.files
            .iter()
            .cloned()
            .map(|entry| ChangedFileTreeEntry::File(ChangedFileTreeStatusEntry { entry, depth })),
    );
    entries
}

fn compact_changed_file_directory_chain(
    mut node: &ChangedFileTreeNode,
) -> (&ChangedFileTreeNode, SharedString) {
    let mut parts = vec![node.name.clone()];
    while node.files.is_empty() && node.children.len() == 1 {
        let Some(child) = node.children.values().next() else {
            continue;
        };
        if child.path.is_none() {
            break;
        }
        parts.push(child.name.clone());
        node = child;
    }
    (node, SharedString::from(parts.join("/")))
}

enum QueryState {
    Pending(SharedString),
    Confirmed((SharedString, Task<()>)),
    Empty,
}

impl QueryState {
    fn next_state(&mut self) {
        match self {
            Self::Confirmed((query, _)) => *self = Self::Pending(std::mem::take(query)),
            _ => {}
        };
    }
}

struct SearchState {
    case_sensitive: bool,
    editor: Entity<Editor>,
    state: QueryState,
    matches: IndexSet<Oid>,
    selected_index: Option<usize>,
}

struct GraphDetailSplitState {
    top_ratio: f32,
    visible_top_ratio: f32,
}

impl GraphDetailSplitState {
    fn new() -> Self {
        Self {
            top_ratio: 0.25,
            visible_top_ratio: 0.25,
        }
    }

    fn restore_ratio(&mut self, ratio: f64) {
        let ratio = ratio as f32;
        if ratio.is_finite() {
            self.top_ratio = ratio.clamp(0.2, 0.8);
            self.visible_top_ratio = self.top_ratio;
        }
    }

    fn visible_top_ratio(&self) -> f32 {
        self.visible_top_ratio
    }

    fn bottom_ratio(&self) -> f32 {
        1.0 - self.visible_top_ratio
    }

    fn on_drag_move(
        &mut self,
        drag_event: &DragMoveEvent<DraggedGraphDetailSplitHandle>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let bounds = drag_event.bounds;
        let bounds_height = bounds.bottom() - bounds.top();
        if bounds_height <= px(0.) {
            return;
        }

        let new_ratio = (drag_event.event.position.y - bounds.top()) / bounds_height;
        self.visible_top_ratio = new_ratio.clamp(0.2, 0.8);
    }

    fn commit_ratio(&mut self) {
        self.top_ratio = self.visible_top_ratio;
    }

    fn on_double_click(&mut self) {
        self.top_ratio = 0.25;
        self.visible_top_ratio = 0.25;
    }
}

struct DetailContentSplitState {
    left_ratio: f32,
    visible_left_ratio: f32,
}

impl DetailContentSplitState {
    fn new() -> Self {
        Self {
            left_ratio: 0.2,
            visible_left_ratio: 0.2,
        }
    }

    fn restore_ratio(&mut self, ratio: f64) {
        let ratio = ratio as f32;
        if ratio.is_finite() {
            self.left_ratio = ratio.clamp(0.15, 0.6);
            self.visible_left_ratio = self.left_ratio;
        }
    }

    fn visible_left_ratio(&self) -> f32 {
        self.visible_left_ratio
    }

    fn right_ratio(&self) -> f32 {
        1.0 - self.visible_left_ratio
    }

    fn on_drag_move(
        &mut self,
        drag_event: &DragMoveEvent<DraggedDetailContentSplitHandle>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let bounds = drag_event.bounds;
        let bounds_width = bounds.right() - bounds.left();
        if bounds_width <= px(0.) {
            return;
        }

        let new_ratio = (drag_event.event.position.x - bounds.left()) / bounds_width;
        self.visible_left_ratio = new_ratio.clamp(0.15, 0.6);
    }

    fn commit_ratio(&mut self) {
        self.left_ratio = self.visible_left_ratio;
    }

    fn on_double_click(&mut self) {
        self.left_ratio = 0.2;
        self.visible_left_ratio = 0.2;
    }
}

actions!(
    git_graph_next,
    [
        /// Focuses the search field.
        FocusSearch,
        /// Focuses the next git graph tab stop.
        FocusNextTabStop,
        /// Focuses the previous git graph tab stop.
        FocusPreviousTabStop,
        /// Selects a commit half a page above the current selection.
        ScrollUp,
        /// Selects a commit half a page below the current selection.
        ScrollDown,
        /// Toggles the selected commit's changed files between flat and tree views.
        ToggleChangedFilesView,
    ]
);

fn timestamp_format() -> &'static [BorrowedFormatItem<'static>] {
    static FORMAT: OnceLock<Vec<BorrowedFormatItem<'static>>> = OnceLock::new();
    FORMAT.get_or_init(|| {
        time::format_description::parse("[day] [month repr:short] [year] [hour]:[minute]")
            .unwrap_or_default()
    })
}

fn format_timestamp(timestamp: i64) -> String {
    let Ok(datetime) = OffsetDateTime::from_unix_timestamp(timestamp) else {
        return "Unknown".to_string();
    };

    let local_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let local_datetime = datetime.to_offset(local_offset);

    local_datetime
        .format(timestamp_format())
        .unwrap_or_default()
}

fn accent_colors_count(accents: &AccentColors) -> usize {
    accents.0.len()
}

#[derive(Copy, Clone, Debug)]
struct BranchColor(u8);

#[derive(Debug)]
enum LaneState {
    Empty,
    Active {
        child: Oid,
        parent: Oid,
        color: Option<BranchColor>,
        starting_row: usize,
        starting_col: usize,
        destination_column: Option<usize>,
        segments: SmallVec<[CommitLineSegment; 1]>,
    },
}

impl LaneState {
    fn to_commit_lines(
        &mut self,
        ending_row: usize,
        lane_column: usize,
        parent_column: usize,
        parent_color: BranchColor,
    ) -> Option<CommitLine> {
        let state = std::mem::replace(self, LaneState::Empty);

        match state {
            LaneState::Active {
                #[cfg_attr(not(test), allow(unused_variables))]
                parent,
                #[cfg_attr(not(test), allow(unused_variables))]
                child,
                color,
                starting_row,
                starting_col,
                destination_column,
                mut segments,
            } => {
                let final_destination = destination_column.unwrap_or(parent_column);
                let final_color = color.unwrap_or(parent_color);

                Some(CommitLine {
                    #[cfg(test)]
                    child,
                    #[cfg(test)]
                    parent,
                    child_column: starting_col,
                    full_interval: starting_row..ending_row,
                    color_idx: final_color.0 as usize,
                    segments: {
                        match segments.last_mut() {
                            Some(CommitLineSegment::Straight { to_row })
                                if *to_row == usize::MAX =>
                            {
                                if final_destination != lane_column {
                                    *to_row = ending_row - 1;

                                    let curved_line = CommitLineSegment::Curve {
                                        to_column: final_destination,
                                        on_row: ending_row,
                                        curve_kind: CurveKind::Checkout,
                                    };

                                    if *to_row == starting_row {
                                        let last_index = segments.len() - 1;
                                        segments[last_index] = curved_line;
                                    } else {
                                        segments.push(curved_line);
                                    }
                                } else {
                                    *to_row = ending_row;
                                }
                            }
                            Some(CommitLineSegment::Curve {
                                on_row,
                                to_column,
                                curve_kind,
                            }) if *on_row == usize::MAX => {
                                if *to_column == usize::MAX {
                                    *to_column = final_destination;
                                }
                                if matches!(curve_kind, CurveKind::Merge) {
                                    *on_row = starting_row + 1;
                                    if *on_row < ending_row {
                                        if *to_column != final_destination {
                                            segments.push(CommitLineSegment::Straight {
                                                to_row: ending_row - 1,
                                            });
                                            segments.push(CommitLineSegment::Curve {
                                                to_column: final_destination,
                                                on_row: ending_row,
                                                curve_kind: CurveKind::Checkout,
                                            });
                                        } else {
                                            segments.push(CommitLineSegment::Straight {
                                                to_row: ending_row,
                                            });
                                        }
                                    } else if *to_column != final_destination {
                                        segments.push(CommitLineSegment::Curve {
                                            to_column: final_destination,
                                            on_row: ending_row,
                                            curve_kind: CurveKind::Checkout,
                                        });
                                    }
                                } else {
                                    *on_row = ending_row;
                                    if *to_column != final_destination {
                                        segments.push(CommitLineSegment::Straight {
                                            to_row: ending_row,
                                        });
                                        segments.push(CommitLineSegment::Curve {
                                            to_column: final_destination,
                                            on_row: ending_row,
                                            curve_kind: CurveKind::Checkout,
                                        });
                                    }
                                }
                            }
                            Some(CommitLineSegment::Curve {
                                on_row, to_column, ..
                            }) => {
                                if *on_row < ending_row {
                                    if *to_column != final_destination {
                                        segments.push(CommitLineSegment::Straight {
                                            to_row: ending_row - 1,
                                        });
                                        segments.push(CommitLineSegment::Curve {
                                            to_column: final_destination,
                                            on_row: ending_row,
                                            curve_kind: CurveKind::Checkout,
                                        });
                                    } else {
                                        segments.push(CommitLineSegment::Straight {
                                            to_row: ending_row,
                                        });
                                    }
                                } else if *to_column != final_destination {
                                    segments.push(CommitLineSegment::Curve {
                                        to_column: final_destination,
                                        on_row: ending_row,
                                        curve_kind: CurveKind::Checkout,
                                    });
                                }
                            }
                            _ => {}
                        }

                        segments
                    },
                })
            }
            LaneState::Empty => None,
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            LaneState::Empty => true,
            LaneState::Active { .. } => false,
        }
    }
}

struct CommitEntry {
    data: Arc<InitialGraphCommitData>,
    lane: usize,
    color_idx: usize,
}

type ActiveLaneIdx = usize;

enum AllCommitCount {
    NotLoaded,
    Loading(usize),
    FullyLoaded(usize),
}

#[derive(Debug)]
enum CurveKind {
    Merge,
    Checkout,
}

#[derive(Debug)]
enum CommitLineSegment {
    Straight {
        to_row: usize,
    },
    Curve {
        to_column: usize,
        on_row: usize,
        curve_kind: CurveKind,
    },
}

#[derive(Debug)]
struct CommitLine {
    #[cfg(test)]
    child: Oid,
    #[cfg(test)]
    parent: Oid,
    child_column: usize,
    full_interval: Range<usize>,
    color_idx: usize,
    segments: SmallVec<[CommitLineSegment; 1]>,
}

impl CommitLine {
    fn get_first_visible_segment_idx(&self, first_visible_row: usize) -> Option<(usize, usize)> {
        if first_visible_row > self.full_interval.end {
            return None;
        } else if first_visible_row <= self.full_interval.start {
            return Some((0, self.child_column));
        }

        let mut current_column = self.child_column;

        for (idx, segment) in self.segments.iter().enumerate() {
            match segment {
                CommitLineSegment::Straight { to_row } => {
                    if *to_row >= first_visible_row {
                        return Some((idx, current_column));
                    }
                }
                CommitLineSegment::Curve {
                    to_column, on_row, ..
                } => {
                    if *on_row >= first_visible_row {
                        return Some((idx, current_column));
                    }
                    current_column = *to_column;
                }
            }
        }

        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CommitLineKey {
    child: Oid,
    parent: Oid,
}

struct GraphData {
    lane_states: SmallVec<[LaneState; 8]>,
    lane_colors: HashMap<ActiveLaneIdx, BranchColor>,
    parent_to_lanes: HashMap<Oid, SmallVec<[usize; 1]>>,
    next_color: BranchColor,
    accent_colors_count: usize,
    commits: Vec<Rc<CommitEntry>>,
    max_commit_count: AllCommitCount,
    max_lanes: usize,
    lines: Vec<Rc<CommitLine>>,
    active_commit_lines: HashMap<CommitLineKey, usize>,
    active_commit_lines_by_parent: HashMap<Oid, SmallVec<[usize; 1]>>,
}

impl GraphData {
    fn new(accent_colors_count: usize) -> Self {
        GraphData {
            lane_states: SmallVec::default(),
            lane_colors: HashMap::default(),
            parent_to_lanes: HashMap::default(),
            next_color: BranchColor(0),
            accent_colors_count,
            commits: Vec::default(),
            max_commit_count: AllCommitCount::NotLoaded,
            max_lanes: 0,
            lines: Vec::default(),
            active_commit_lines: HashMap::default(),
            active_commit_lines_by_parent: HashMap::default(),
        }
    }

    fn clear(&mut self) {
        self.lane_states.clear();
        self.lane_colors.clear();
        self.parent_to_lanes.clear();
        self.commits.clear();
        self.lines.clear();
        self.active_commit_lines.clear();
        self.active_commit_lines_by_parent.clear();
        self.next_color = BranchColor(0);
        self.max_commit_count = AllCommitCount::NotLoaded;
        self.max_lanes = 0;
    }

    fn first_empty_lane_idx(&mut self) -> ActiveLaneIdx {
        self.lane_states
            .iter()
            .position(LaneState::is_empty)
            .unwrap_or_else(|| {
                self.lane_states.push(LaneState::Empty);
                self.lane_states.len() - 1
            })
    }

    fn get_lane_color(&mut self, lane_idx: ActiveLaneIdx) -> BranchColor {
        let accent_colors_count = self.accent_colors_count;
        *self.lane_colors.entry(lane_idx).or_insert_with(|| {
            let color_idx = self.next_color;
            self.next_color = BranchColor((self.next_color.0 + 1) % accent_colors_count as u8);
            color_idx
        })
    }

    fn add_commits(&mut self, commits: &[Arc<InitialGraphCommitData>]) {
        self.commits.reserve(commits.len());
        self.lines.reserve(commits.len() / 2);

        for commit in commits.iter() {
            let commit_row = self.commits.len();

            let commit_lane = self
                .parent_to_lanes
                .get(&commit.sha)
                .and_then(|lanes| lanes.iter().min().copied());

            let commit_lane = commit_lane.unwrap_or_else(|| self.first_empty_lane_idx());

            let commit_color = self.get_lane_color(commit_lane);

            if let Some(lanes) = self.parent_to_lanes.remove(&commit.sha) {
                for lane_column in lanes {
                    let state = &mut self.lane_states[lane_column];

                    if let LaneState::Active {
                        starting_row,
                        segments,
                        ..
                    } = state
                    {
                        if let Some(CommitLineSegment::Curve {
                            to_column,
                            curve_kind: CurveKind::Merge,
                            ..
                        }) = segments.first_mut()
                        {
                            let curve_row = *starting_row + 1;
                            let would_overlap =
                                if lane_column != commit_lane && curve_row < commit_row {
                                    self.commits[curve_row..commit_row]
                                        .iter()
                                        .any(|c| c.lane == commit_lane)
                                } else {
                                    false
                                };

                            if would_overlap {
                                *to_column = lane_column;
                            }
                        }
                    }

                    if let Some(commit_line) =
                        state.to_commit_lines(commit_row, lane_column, commit_lane, commit_color)
                    {
                        self.lines.push(Rc::new(commit_line));
                    }
                }
            }

            commit
                .parents
                .iter()
                .enumerate()
                .for_each(|(parent_idx, parent)| {
                    if parent_idx == 0 {
                        self.lane_states[commit_lane] = LaneState::Active {
                            parent: *parent,
                            child: commit.sha,
                            color: Some(commit_color),
                            starting_col: commit_lane,
                            starting_row: commit_row,
                            destination_column: None,
                            segments: smallvec![CommitLineSegment::Straight { to_row: usize::MAX }],
                        };

                        self.parent_to_lanes
                            .entry(*parent)
                            .or_default()
                            .push(commit_lane);
                    } else {
                        let new_lane = self.first_empty_lane_idx();

                        self.lane_states[new_lane] = LaneState::Active {
                            parent: *parent,
                            child: commit.sha,
                            color: None,
                            starting_col: commit_lane,
                            starting_row: commit_row,
                            destination_column: None,
                            segments: smallvec![CommitLineSegment::Curve {
                                to_column: usize::MAX,
                                on_row: usize::MAX,
                                curve_kind: CurveKind::Merge,
                            },],
                        };

                        self.parent_to_lanes
                            .entry(*parent)
                            .or_default()
                            .push(new_lane);
                    }
                });

            self.max_lanes = self.max_lanes.max(self.lane_states.len());

            self.commits.push(Rc::new(CommitEntry {
                data: commit.clone(),
                lane: commit_lane,
                color_idx: commit_color.0 as usize,
            }));
        }

        self.max_commit_count = AllCommitCount::Loading(self.commits.len());
    }
}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut workspace::Workspace, _, _| {
        workspace.register_action_renderer(|div, workspace, _window, cx| {
            div.when(
                workspace.project().read(cx).active_repository(cx).is_some(),
                |div| {
                    let workspace = workspace.weak_handle();
                    div.on_action(move |_: &zed_actions::git_graph_next::Open, window, cx| {
                        workspace
                            .update(cx, |workspace, cx| {
                                let Some(repository) =
                                    workspace.project().read(cx).active_repository(cx)
                                else {
                                    return;
                                };
                                let repository_id = repository.read(cx).id;
                                let git_store = workspace.project().read(cx).git_store().clone();
                                open_or_reuse_graph_next(
                                    workspace,
                                    repository_id,
                                    git_store,
                                    LogSource::All,
                                    None,
                                    window,
                                    cx,
                                );
                            })
                            .ok();
                    })
                },
            )
        });
    })
    .detach();
}

pub fn open_or_reuse_graph_next(
    workspace: &mut Workspace,
    repo_id: RepositoryId,
    git_store: Entity<GitStore>,
    log_source: LogSource,
    sha: Option<String>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let existing = workspace.items_of_type::<GitGraphNext>(cx).find(|graph| {
        let graph = graph.read(cx);
        graph.repo_id == repo_id && graph.log_source == log_source
    });

    let git_graph = if let Some(existing) = existing {
        workspace.activate_item(&existing, true, true, window, cx);
        existing
    } else {
        let workspace_handle = workspace.weak_handle();
        let git_graph = cx.new(|cx| {
            GitGraphNext::new(
                repo_id,
                git_store,
                workspace_handle,
                Some(log_source),
                window,
                cx,
            )
        });
        workspace.add_item_to_active_pane(Box::new(git_graph.clone()), None, true, window, cx);
        git_graph
    };

    if let Some(sha) = sha {
        cx.defer(move |cx| {
            git_graph.update(cx, |graph, cx| {
                graph.select_commit_by_sha(sha.as_str(), cx);
            });
        });
    }
}

fn lane_center_x(bounds: Bounds<Pixels>, lane: f32) -> Pixels {
    bounds.origin.x + LEFT_PADDING + lane * LANE_WIDTH + LANE_WIDTH / 2.0
}

fn to_row_center(
    to_row: usize,
    row_height: Pixels,
    scroll_offset: Pixels,
    bounds: Bounds<Pixels>,
) -> Pixels {
    bounds.origin.y + to_row as f32 * row_height + row_height / 2.0 - scroll_offset
}

fn draw_commit_circle(center_x: Pixels, center_y: Pixels, color: Hsla, window: &mut Window) {
    let radius = COMMIT_CIRCLE_RADIUS;

    let mut builder = PathBuilder::fill();

    // Start at the rightmost point of the circle
    builder.move_to(point(center_x + radius, center_y));

    // Draw the circle using two arc_to calls (top half, then bottom half)
    builder.arc_to(
        point(radius, radius),
        px(0.),
        false,
        true,
        point(center_x - radius, center_y),
    );
    builder.arc_to(
        point(radius, radius),
        px(0.),
        false,
        true,
        point(center_x + radius, center_y),
    );
    builder.close();

    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

fn compute_file_diff_stats(file: &CommitFile) -> (usize, usize) {
    let old_text = file.old_text.as_deref().unwrap_or("");
    let new_text = file.new_text.as_deref().unwrap_or("");
    line_diff(old_text, new_text)
        .iter()
        .fold((0, 0), |(added, removed), (old_range, new_range)| {
            (
                added + (new_range.end - new_range.start) as usize,
                removed + (old_range.end - old_range.start) as usize,
            )
        })
}

struct GitGraphContextMenu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    target_entry_index: Option<usize>,
    _subscription: Subscription,
}

struct DetailPanelCommitMessage {
    sha: Oid,
    message: Entity<Markdown>,
    scroll_handle: ScrollHandle,
}

pub struct GitGraphNext {
    focus_handle: FocusHandle,
    search_state: SearchState,
    graph_data: GraphData,
    git_store: Entity<GitStore>,
    workspace: WeakEntity<Workspace>,
    context_menu: Option<GitGraphContextMenu>,
    table_interaction_state: Entity<TableInteractionState>,
    column_widths: Entity<RedistributableColumnsState>,
    /// Per-column visibility mask owned by the view (not the resize state) so columns can be
    /// hidden regardless of whether the table is resizable. `true` means the column is hidden.
    column_visibility: TableRow<bool>,
    selected_entry_idx: Option<usize>,
    hovered_entry_idx: Option<usize>,
    graph_canvas_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    log_source: LogSource,
    log_order: LogOrder,
    selected_commit_files: Vec<ChangedFileEntry>,
    selected_commit_diff_stats: Option<(usize, usize)>,
    selected_commit_diff_error: Option<SharedString>,
    selected_commit_diff: Option<CommitDiff>,
    selected_commit_view: Option<Entity<crate::git_graph_next_diff::GitGraphNextDiff>>,
    pending_commit_view: Option<(Oid, CommitDiff)>,
    selected_changed_file: Option<RepoPath>,
    showing_all_diff_lines: bool,
    _commit_diff_task: Option<Task<()>>,
    selected_commit_message: Option<DetailPanelCommitMessage>,
    _selected_commit_message_task: Option<Task<()>>,
    graph_detail_split_state: Entity<GraphDetailSplitState>,
    detail_content_split_state: Entity<DetailContentSplitState>,
    repo_id: RepositoryId,
    changed_files_scroll_handle: UniformListScrollHandle,
    changed_files_view_mode: ChangedFilesViewMode,
    changed_files_expanded_dirs: HashMap<RepoPath, bool>,
    pending_select_sha: Option<Oid>,
}

impl GitGraphNext {
    fn invalidate_state(&mut self, cx: &mut Context<Self>) {
        self.graph_data.clear();
        self.search_state.matches.clear();
        self.search_state.selected_index = None;
        self.search_state.state.next_state();
        self.context_menu = None;
        cx.emit(ItemEvent::Edit);
        cx.notify();
    }

    /// Computes the height of a single commit row in the git graph.
    ///
    /// The returned value is snapped to the nearest physical pixel. This is
    /// required so that the canvas's float math and the `uniform_list` layout
    /// (which snaps to device pixels) agree on row positions; otherwise rows
    /// drift apart as the user scrolls when `ui_font_size` is fractional.
    fn row_height(window: &Window, _cx: &App) -> Pixels {
        let rem_size = window.rem_size();
        let line_height = window.text_style().line_height_in_pixels(rem_size);
        let raw = line_height + ROW_VERTICAL_PADDING;
        let scale = window.scale_factor();

        (raw * scale).round() / scale
    }

    fn visible_row_count(&self, window: &Window, cx: &App) -> usize {
        let row_height = Self::row_height(window, cx);
        let viewport_height = self
            .table_interaction_state
            .read(cx)
            .scroll_handle
            .0
            .borrow()
            .last_item_size
            .map_or(window.viewport_size().height, |size| size.item.height);

        ((viewport_height / row_height).ceil() as usize).min(self.graph_data.commits.len())
    }

    fn graph_canvas_content_width(&self) -> Pixels {
        (LANE_WIDTH * self.graph_data.max_lanes.max(6) as f32) + LEFT_PADDING * 2.0
    }

    fn preview_column_fractions(&self, window: &Window, cx: &App) -> [f32; 5] {
        let raw = self
            .column_widths
            .read(cx)
            .preview_fractions(window.rem_size());
        let fractions = redistribute_hidden_fractions(&raw, Some(&self.column_visibility));

        // Hidden columns occupy no space in the layout, so report them as zero here even though
        // the shared redistribution helper preserves their stored width for when they return.
        let value = |idx: usize| {
            if self.column_visibility.get(idx).copied().unwrap_or(false) {
                0.0
            } else {
                fractions[idx]
            }
        };

        let is_path_history = matches!(self.log_source, LogSource::Path(_));
        let graph_fraction = if is_path_history { 0.0 } else { value(0) };
        let offset = if is_path_history { 0 } else { 1 };

        [
            graph_fraction,
            value(offset),
            value(offset + 1),
            value(offset + 2),
            value(offset + 3),
        ]
    }

    fn table_column_width_config(&self, window: &Window, cx: &App) -> ColumnWidthConfig {
        let [_, description, date, author, commit] = self.preview_column_fractions(window, cx);
        let table_total = description + date + author + commit;

        let widths = if table_total > 0.0 {
            vec![
                DefiniteLength::Fraction(description / table_total),
                DefiniteLength::Fraction(date / table_total),
                DefiniteLength::Fraction(author / table_total),
                DefiniteLength::Fraction(commit / table_total),
            ]
        } else {
            vec![
                DefiniteLength::Fraction(0.25),
                DefiniteLength::Fraction(0.25),
                DefiniteLength::Fraction(0.25),
                DefiniteLength::Fraction(0.25),
            ]
        };

        ColumnWidthConfig::explicit(widths)
    }

    fn graph_viewport_width(&self, window: &Window, cx: &App) -> Pixels {
        let container = self.column_widths.read(cx).cached_container_width();
        let graph_fraction = self.preview_column_fractions(window, cx)[0];
        if container > px(0.) && graph_fraction > 0.0 {
            container * graph_fraction
        } else {
            self.graph_canvas_content_width()
        }
    }

    pub fn new(
        repo_id: RepositoryId,
        git_store: Entity<GitStore>,
        workspace: WeakEntity<Workspace>,
        log_source: Option<LogSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        cx.on_focus(&focus_handle, window, |_, _, cx| cx.notify())
            .detach();

        let accent_colors = cx.theme().accents();
        let graph = GraphData::new(accent_colors_count(accent_colors));
        let log_source = log_source.unwrap_or_default();
        let log_order = LogOrder::default();

        cx.subscribe(&git_store, |this, _, event, cx| match event {
            GitStoreEvent::RepositoryUpdated(updated_repo_id, repo_event, _) => {
                if this.repo_id == *updated_repo_id {
                    if let Some(repository) = this.get_repository(cx) {
                        this.on_repository_event(repository, repo_event, cx);
                    }
                }
            }
            _ => {}
        })
        .detach();

        let search_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Search commits…", window, cx);
            editor
        });

        let table_interaction_state = cx.new(|cx| {
            let mut state = TableInteractionState::new(cx);
            state.focus_handle = state.focus_handle.tab_index(1).tab_stop(true);
            state
        });

        let column_widths = if matches!(log_source, LogSource::Path(_)) {
            cx.new(|_cx| {
                RedistributableColumnsState::new(
                    4,
                    vec![
                        DefiniteLength::Fraction(0.72),
                        DefiniteLength::Fraction(0.12),
                        DefiniteLength::Fraction(0.1),
                        DefiniteLength::Fraction(0.06),
                    ],
                    vec![
                        TableResizeBehavior::Resizable,
                        TableResizeBehavior::Resizable,
                        TableResizeBehavior::Resizable,
                        TableResizeBehavior::Resizable,
                    ],
                )
            })
        } else {
            cx.new(|_cx| {
                RedistributableColumnsState::new(
                    5,
                    vec![
                        DefiniteLength::Fraction(0.14),
                        DefiniteLength::Fraction(0.6192),
                        DefiniteLength::Fraction(0.1032),
                        DefiniteLength::Fraction(0.086),
                        DefiniteLength::Fraction(0.0516),
                    ],
                    vec![
                        TableResizeBehavior::Resizable,
                        TableResizeBehavior::Resizable,
                        TableResizeBehavior::Resizable,
                        TableResizeBehavior::Resizable,
                        TableResizeBehavior::Resizable,
                    ],
                )
            })
        };
        let mut column_visibility = TableRow::from_element(
            false,
            if matches!(log_source, LogSource::Path(_)) {
                TABLE_COLUMN_COUNT
            } else {
                TABLE_COLUMN_COUNT + 1
            },
        );
        if matches!(log_source, LogSource::Branch(_)) {
            column_visibility.as_mut_slice()[0] = true;
        }
        let mut row_height = Self::row_height(window, cx);

        cx.observe_global_in::<settings::SettingsStore>(window, move |this, window, cx| {
            let new_row_height = Self::row_height(window, cx);
            if new_row_height != row_height {
                // The `uniform_list` powering the table caches the item size
                // from its last layout; invalidate it so it re-measures with
                // the new row height on the next frame.
                this.table_interaction_state.update(cx, |state, _cx| {
                    state.scroll_handle.0.borrow_mut().last_item_size = None;
                });
                row_height = new_row_height;
                cx.notify();
            }
        })
        .detach();

        let mut this = GitGraphNext {
            focus_handle,
            git_store,
            search_state: SearchState {
                case_sensitive: false,
                editor: search_editor,
                matches: IndexSet::default(),
                selected_index: None,
                state: QueryState::Empty,
            },
            workspace,
            graph_data: graph,
            _commit_diff_task: None,
            context_menu: None,
            table_interaction_state,
            column_widths,
            column_visibility,
            selected_entry_idx: None,
            hovered_entry_idx: None,
            graph_canvas_bounds: Rc::new(Cell::new(None)),
            selected_commit_files: Vec::new(),
            selected_commit_diff_stats: None,
            selected_commit_diff_error: None,
            selected_commit_diff: None,
            selected_commit_view: None,
            pending_commit_view: None,
            selected_changed_file: None,
            showing_all_diff_lines: false,
            selected_commit_message: None,
            _selected_commit_message_task: None,
            log_source,
            log_order,
            graph_detail_split_state: cx.new(|_cx| GraphDetailSplitState::new()),
            detail_content_split_state: cx.new(|_cx| DetailContentSplitState::new()),
            repo_id,
            changed_files_scroll_handle: UniformListScrollHandle::new(),
            changed_files_view_mode: ChangedFilesViewMode::default(),
            changed_files_expanded_dirs: HashMap::default(),
            pending_select_sha: None,
        };

        this.fetch_initial_graph_data(cx);
        this
    }

    fn on_repository_event(
        &mut self,
        repository: Entity<Repository>,
        event: &RepositoryEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            RepositoryEvent::GraphEvent((source, order), event)
                if source == &self.log_source && order == &self.log_order =>
            {
                match event {
                    GitGraphEvent::FullyLoaded => {
                        if let Some(pending_sha_index) =
                            self.pending_select_sha.take().and_then(|oid| {
                                repository
                                    .read(cx)
                                    .get_graph_data(source.clone(), *order)
                                    .and_then(|data| data.commit_oid_to_index.get(&oid).copied())
                            })
                        {
                            self.select_entry(pending_sha_index, ScrollStrategy::Nearest, cx);
                        }
                        let count = match self.graph_data.max_commit_count {
                            AllCommitCount::FullyLoaded(count) | AllCommitCount::Loading(count) => {
                                count
                            }
                            AllCommitCount::NotLoaded => 0,
                        };
                        self.graph_data.max_commit_count = AllCommitCount::FullyLoaded(count);
                        cx.notify();
                    }
                    GitGraphEvent::LoadingError => {
                        cx.notify();
                    }
                    GitGraphEvent::CountUpdated(commit_count) => {
                        let old_count = self.graph_data.commits.len();

                        if let Some(pending_selection_index) =
                            repository.update(cx, |repository, cx| {
                                let GraphDataResponse {
                                    commits,
                                    is_loading,
                                    error: _,
                                } = repository.graph_data(
                                    source.clone(),
                                    *order,
                                    old_count..*commit_count,
                                    cx,
                                );
                                self.graph_data.add_commits(commits);

                                let pending_sha_index = self.pending_select_sha.and_then(|oid| {
                                    repository.get_graph_data(source.clone(), *order).and_then(
                                        |data| data.commit_oid_to_index.get(&oid).copied(),
                                    )
                                });

                                if !is_loading && pending_sha_index.is_none() {
                                    self.pending_select_sha.take();
                                }

                                pending_sha_index
                            })
                        {
                            self.select_entry(pending_selection_index, ScrollStrategy::Nearest, cx);
                            self.pending_select_sha.take();
                        }

                        cx.notify();
                    }
                }
            }
            RepositoryEvent::HeadChanged | RepositoryEvent::BranchListChanged => {
                // Only invalidate if we scanned atleast once,
                // meaning we are not inside the initial repo loading state
                // NOTE: this fixes an loading performance regression
                if repository.read(cx).scan_id > 1 {
                    self.pending_select_sha = None;
                    self.invalidate_state(cx);
                }
            }
            RepositoryEvent::StashEntriesChanged if self.log_source == LogSource::All => {
                // Stash entries initial's scan id is 2, so we don't want to invalidate the graph before that
                if repository.read(cx).scan_id > 2 {
                    self.pending_select_sha = None;
                    self.invalidate_state(cx);
                }
            }
            RepositoryEvent::GraphEvent(_, _) => {}
            _ => {}
        }
    }

    fn fetch_initial_graph_data(&mut self, cx: &mut App) {
        if let Some(repository) = self.get_repository(cx) {
            repository.update(cx, |repository, cx| {
                let commits = repository
                    .graph_data(self.log_source.clone(), self.log_order, 0..usize::MAX, cx)
                    .commits;
                self.graph_data.add_commits(commits);
            });
        }
    }

    fn get_repository(&self, cx: &App) -> Option<Entity<Repository>> {
        let git_store = self.git_store.read(cx);
        git_store.repositories().get(&self.repo_id).cloned()
    }

    /// Checks whether a ref name from git's `%D` decoration
    ///  format refers to the currently checked-out branch.
    fn is_head_ref(ref_name: &str, head_branch_name: &Option<SharedString>) -> bool {
        head_branch_name.as_ref().is_some_and(|head| {
            ref_name == head.as_ref() || ref_name.strip_prefix("HEAD -> ") == Some(head.as_ref())
        })
    }

    /// Extracts a ref name (branch, remote ref, or tag) from a decoration in
    /// git's `%D` format, returning `None` for a detached `HEAD`.
    fn ref_name_from_decoration(decoration: &str) -> Option<SharedString> {
        let name = decoration
            .strip_prefix("tag: ")
            .or_else(|| decoration.strip_prefix("HEAD -> "))
            .unwrap_or(decoration);
        if name.is_empty() || name == "HEAD" {
            return None;
        }
        Some(SharedString::from(name.to_string()))
    }

    fn render_chip(
        &self,
        name: &SharedString,
        accent_color: gpui::Hsla,
        is_head: bool,
    ) -> impl IntoElement {
        Chip::new(name.clone())
            .label_size(LabelSize::Small)
            .truncate()
            .tooltip({
                let name = name.clone();
                move |_, cx| Tooltip::simple(name.clone(), cx)
            })
            .map(|chip| {
                if is_head {
                    chip.icon(IconName::Check)
                        .bg_color(accent_color.opacity(0.25))
                        .border_color(accent_color.opacity(0.5))
                } else {
                    chip.bg_color(accent_color.opacity(0.08))
                        .border_color(accent_color.opacity(0.25))
                }
            })
    }

    /// Renders a ref chip for the commit at `commit_idx`. Chips that name a ref
    /// (branch, remote ref, or tag) get a right-click handler that opens a
    /// ref-specific context menu, so that custom commands can be resolved
    /// against the clicked ref.
    fn render_ref_chip(
        &self,
        name: &SharedString,
        accent_color: gpui::Hsla,
        is_head: bool,
        commit_idx: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chip = self.render_chip(name, accent_color, is_head);
        let Some(ref_name) = Self::ref_name_from_decoration(name) else {
            return chip.into_any_element();
        };
        div()
            .min_w_0()
            .child(chip)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.deploy_entry_context_menu(
                        event.position,
                        commit_idx,
                        Some(ref_name.clone()),
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .into_any_element()
    }

    fn render_table_rows(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Vec<AnyElement>> {
        let repository = self.get_repository(cx);

        let head_branch_name: Option<SharedString> = repository.as_ref().and_then(|repo| {
            repo.read(cx)
                .snapshot()
                .branch
                .as_ref()
                .map(|branch| SharedString::from(branch.name().to_string()))
        });

        let row_height = Self::row_height(window, cx);

        // We fetch data outside the visible viewport to avoid loading entries when
        // users scroll through the git graph
        if let Some(repository) = repository.as_ref() {
            const FETCH_RANGE: usize = 100;
            repository.update(cx, |repository, cx| {
                self.graph_data.commits[range.start.saturating_sub(FETCH_RANGE)
                    ..(range.end + FETCH_RANGE)
                        .min(self.graph_data.commits.len().saturating_sub(1))]
                    .iter()
                    .for_each(|commit| {
                        repository.fetch_commit_data(commit.data.sha, false, cx);
                    });
            });
        }

        range
            .map(|idx| {
                let Some((commit, repository)) =
                    self.graph_data.commits.get(idx).zip(repository.as_ref())
                else {
                    return vec![
                        div().h(row_height).into_any_element(),
                        div().h(row_height).into_any_element(),
                        div().h(row_height).into_any_element(),
                        div().h(row_height).into_any_element(),
                    ];
                };

                let data = repository.update(cx, |repository, cx| {
                    repository
                        .fetch_commit_data(commit.data.sha, false, cx)
                        .clone()
                });

                let short_sha = commit.data.sha.display_short();
                let mut formatted_time = String::new();
                let subject: SharedString;
                let author_name: SharedString;

                if let CommitDataState::Loaded(ref data) = data {
                    subject = data.subject.clone();
                    author_name = data.author_name.clone();
                    formatted_time = format_timestamp(data.commit_timestamp);
                } else {
                    subject = "Loading…".into();
                    author_name = "".into();
                }

                let accent_colors = cx.theme().accents();
                let accent_color = accent_colors
                    .0
                    .get(commit.color_idx)
                    .copied()
                    .unwrap_or_else(|| accent_colors.0.first().copied().unwrap_or_default());

                let is_selected = self.selected_entry_idx == Some(idx);
                let is_matched = self.search_state.matches.contains(&commit.data.sha);
                let column_label = |label: SharedString| {
                    Label::new(label)
                        .when(!is_selected, |c| c.color(Color::Muted))
                        .truncate()
                        .into_any_element()
                };

                let subject_label = if is_matched {
                    let query = match &self.search_state.state {
                        QueryState::Confirmed((query, _)) => Some(query.clone()),
                        _ => None,
                    };
                    let highlight_ranges = query
                        .and_then(|q| {
                            let ranges = if self.search_state.case_sensitive {
                                subject
                                    .match_indices(q.as_str())
                                    .map(|(start, matched)| start..start + matched.len())
                                    .collect::<Vec<_>>()
                            } else {
                                let q = q.to_lowercase();
                                let subject_lower = subject.to_lowercase();

                                subject_lower
                                    .match_indices(&q)
                                    .filter_map(|(start, matched)| {
                                        let end = start + matched.len();
                                        subject.is_char_boundary(start).then_some(()).and_then(
                                            |_| subject.is_char_boundary(end).then_some(start..end),
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            };

                            (!ranges.is_empty()).then_some(ranges)
                        })
                        .unwrap_or_default();
                    HighlightedLabel::from_ranges(subject, highlight_ranges)
                        .when(!is_selected, |c| c.color(Color::Muted))
                        .truncate()
                        .into_any_element()
                } else {
                    column_label(subject)
                };

                vec![
                    div()
                        .id(ElementId::NamedInteger("commit-subject".into(), idx as u64))
                        .overflow_hidden()
                        .child(
                            h_flex()
                                .gap_2()
                                .overflow_hidden()
                                .children((!commit.data.ref_names.is_empty()).then(|| {
                                    h_flex().gap_1().children(commit.data.ref_names.iter().map(
                                        |name| {
                                            let is_head =
                                                Self::is_head_ref(name.as_ref(), &head_branch_name);
                                            self.render_ref_chip(
                                                name,
                                                accent_color,
                                                is_head,
                                                idx,
                                                cx,
                                            )
                                        },
                                    ))
                                }))
                                .child(subject_label),
                        )
                        .into_any_element(),
                    column_label(formatted_time.into()),
                    column_label(author_name),
                    column_label(short_sha.into()),
                ]
            })
            .collect()
    }

    fn cancel(&mut self, _: &Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        self.selected_entry_idx = None;
        self.selected_commit_files.clear();
        self.selected_commit_diff_stats = None;
        self.selected_commit_diff_error = None;
        self.selected_commit_diff = None;
        self.selected_commit_view = None;
        self.pending_commit_view = None;
        self.selected_changed_file = None;
        self.changed_files_expanded_dirs.clear();
        cx.emit(ItemEvent::Edit);
        cx.notify();
    }

    fn select_first(&mut self, _: &SelectFirst, _window: &mut Window, cx: &mut Context<Self>) {
        self.select_entry(0, ScrollStrategy::Nearest, cx);
    }

    fn select_prev(&mut self, _: &SelectPrevious, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(selected_entry_idx) = &self.selected_entry_idx {
            self.select_entry(
                selected_entry_idx.saturating_sub(1),
                ScrollStrategy::Nearest,
                cx,
            );
        } else {
            self.select_first(&SelectFirst, window, cx);
        }
    }

    fn select_next(&mut self, _: &SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(selected_entry_idx) = &self.selected_entry_idx {
            self.select_entry(
                selected_entry_idx
                    .saturating_add(1)
                    .min(self.graph_data.commits.len().saturating_sub(1)),
                ScrollStrategy::Nearest,
                cx,
            );
        } else {
            self.select_prev(&SelectPrevious, window, cx);
        }
    }

    fn select_last(&mut self, _: &SelectLast, _window: &mut Window, cx: &mut Context<Self>) {
        self.select_entry(
            self.graph_data.commits.len().saturating_sub(1),
            ScrollStrategy::Nearest,
            cx,
        );
    }

    fn scroll_up(&mut self, _: &ScrollUp, window: &mut Window, cx: &mut Context<Self>) {
        let step = (self.visible_row_count(window, cx) / 2).max(1);
        let target_idx = self.selected_entry_idx.unwrap_or(0).saturating_sub(step);

        self.select_entry(target_idx, ScrollStrategy::Nearest, cx);
    }

    fn scroll_down(&mut self, _: &ScrollDown, window: &mut Window, cx: &mut Context<Self>) {
        let Some(last_entry_idx) = self.graph_data.commits.len().checked_sub(1) else {
            return;
        };

        let step = (self.visible_row_count(window, cx) / 2).max(1);
        let target_idx = self
            .selected_entry_idx
            .unwrap_or(0)
            .saturating_add(step)
            .min(last_entry_idx);

        self.select_entry(target_idx, ScrollStrategy::Nearest, cx);
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        self.open_selected_commit_view(window, cx);
    }

    fn toggle_changed_files_view(
        &mut self,
        _: &ToggleChangedFilesView,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.changed_files_view_mode = self.changed_files_view_mode.toggled();
        self.changed_files_scroll_handle
            .scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn search(&mut self, query: SharedString, cx: &mut Context<Self>) {
        let Some(repo) = self.get_repository(cx) else {
            return;
        };

        self.search_state.matches.clear();
        self.search_state.selected_index = None;
        self.search_state.editor.update(cx, |editor, _cx| {
            editor.set_text_style_refinement(Default::default());
        });

        if query.as_str().is_empty() {
            self.search_state.state = QueryState::Empty;
            cx.notify();
            return;
        }

        let (request_tx, request_rx) = async_channel::unbounded::<Oid>();

        repo.update(cx, |repo, cx| {
            repo.search_commits(
                self.log_source.clone(),
                SearchCommitArgs {
                    query: query.clone(),
                    case_sensitive: self.search_state.case_sensitive,
                },
                request_tx,
                cx,
            );
        });

        let search_task = cx.spawn(async move |this, cx| {
            while let Ok(first_oid) = request_rx.recv().await {
                let mut pending_oids = vec![first_oid];
                while let Ok(oid) = request_rx.try_recv() {
                    pending_oids.push(oid);
                }

                this.update(cx, |this, cx| {
                    if this.search_state.selected_index.is_none() {
                        this.search_state.selected_index = Some(0);
                        this.select_commit_by_sha(first_oid, cx);
                    }

                    this.search_state.matches.extend(pending_oids);
                    cx.notify();
                })
                .ok();
            }

            this.update(cx, |this, cx| {
                if this.search_state.matches.is_empty() {
                    this.search_state.editor.update(cx, |editor, cx| {
                        editor.set_text_style_refinement(TextStyleRefinement {
                            color: Some(Color::Error.color(cx)),
                            ..Default::default()
                        });
                    });
                }
            })
            .ok();
        });

        self.search_state.state = QueryState::Confirmed((query, search_task));
        cx.emit(ItemEvent::Edit);
    }

    fn confirm_search(&mut self, _: &menu::Confirm, _window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_state.editor.read(cx).text(cx).into();
        self.search(query, cx);
    }

    fn activate_search_editor_if_focused(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_state.editor.update(cx, |editor, cx| {
            if editor.is_focused(window) {
                editor.select_all(&Default::default(), window, cx);
                editor.show_cursor(cx);
            }
        });
    }

    fn focus_next_tab_stop(
        &mut self,
        _: &FocusNextTabStop,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus_next(cx);
        self.activate_search_editor_if_focused(window, cx);
        cx.stop_propagation();
        cx.notify();
    }

    fn focus_previous_tab_stop(
        &mut self,
        _: &FocusPreviousTabStop,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus_prev(cx);
        self.activate_search_editor_if_focused(window, cx);
        cx.stop_propagation();
        cx.notify();
    }

    fn select_entry(
        &mut self,
        idx: usize,
        scroll_strategy: ScrollStrategy,
        cx: &mut Context<Self>,
    ) {
        if self.selected_entry_idx == Some(idx) || idx >= self.graph_data.commits.len() {
            debug_assert!(
                idx < self.graph_data.commits.len(),
                "attempted to select out of bounds index: {idx}, commits.len: {}",
                self.graph_data.commits.len()
            );
            return;
        }

        self.selected_entry_idx = Some(idx);
        self.selected_commit_files.clear();
        self.selected_commit_diff_stats = None;
        self.selected_commit_diff_error = None;
        self.selected_commit_diff = None;
        self.selected_commit_view = None;
        self.pending_commit_view = None;
        self.selected_changed_file = None;
        self.selected_commit_message = None;
        self._selected_commit_message_task = None;
        self.changed_files_expanded_dirs.clear();
        self.changed_files_scroll_handle
            .scroll_to_item(0, ScrollStrategy::Top);
        self.table_interaction_state.update(cx, |state, cx| {
            state.scroll_handle.scroll_to_item(idx, scroll_strategy);
            cx.notify();
        });

        let Some(commit) = self.graph_data.commits.get(idx) else {
            return;
        };

        let Some(repository) = self.get_repository(cx) else {
            return;
        };

        let commit_message_handle = commit.data.sha;
        let selected_sha = commit.data.sha;
        let diff_handle = selected_sha.to_string();

        self.load_selected_commit_message(cx, &commit_message_handle, &repository);

        let diff_receiver =
            repository.update(cx, |repo, _| repo.load_commit_diff(diff_handle, false));

        self._commit_diff_task = Some(cx.spawn(async move |this, cx| {
            let diff = match diff_receiver.await {
                Ok(Ok(diff)) => Ok(diff),
                Ok(Err(error)) => Err(error),
                Err(error) => Err(anyhow::anyhow!("failed to receive commit diff: {error}")),
            };
            this.update(cx, |this, cx| {
                let is_still_selected = this
                    .selected_entry_idx
                    .and_then(|idx| this.graph_data.commits.get(idx))
                    .is_some_and(|commit| commit.data.sha == selected_sha);
                if !is_still_selected {
                    return;
                }

                match diff {
                    Ok(diff) => {
                        this.selected_commit_files = diff
                            .files
                            .iter()
                            .map(|file| ChangedFileEntry::from_commit_file(file, cx))
                            .collect();
                        this.selected_commit_diff_stats = Some(
                            this.selected_commit_files
                                .iter()
                                .filter_map(|file| file.diff_stats)
                                .fold((0, 0), |(added, removed), (file_added, file_removed)| {
                                    (added + file_added, removed + file_removed)
                                }),
                        );
                        let selected_path = diff.files.first().map(|file| file.path.clone());
                        this.selected_changed_file = selected_path;
                        this.selected_commit_diff = Some(diff);
                        if this.selected_changed_file.is_some() {
                            this.queue_selected_commit_view(selected_sha);
                        }
                    }
                    Err(error) => {
                        this.selected_commit_diff_error = Some(error.to_string().into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));

        cx.emit(ItemEvent::Edit);
        cx.notify();
    }

    fn load_selected_commit_message(
        &mut self,
        cx: &mut Context<'_, Self>,
        sha: &Oid,
        repository: &Entity<Repository>,
    ) {
        if self
            .selected_commit_message
            .as_ref()
            .is_some_and(|old| old.sha == *sha)
        {
            return;
        }

        self._selected_commit_message_task = None;
        match repository.update(cx, |repo, cx| {
            repo.fetch_commit_data(*sha, true, cx).clone()
        }) {
            CommitDataState::Loaded(commit_data) => {
                self.set_selected_commit_message(cx, commit_data.sha, commit_data.message.clone());
            }
            CommitDataState::Loading(Some(receiver)) => {
                self._selected_commit_message_task = Some(cx.spawn(async move |this, cx| {
                    if let Ok(commit_data) = receiver.await {
                        this.update(cx, |this, cx| {
                            this.set_selected_commit_message(
                                cx,
                                commit_data.sha,
                                commit_data.message.clone(),
                            );
                        })
                        .log_err();
                    }
                }))
            }
            _ => {
                debug_panic!(
                    "Fetched commit data asynchronously, but was not given a listener or cached commit data."
                );
            }
        };
    }

    fn set_selected_commit_message(
        &mut self,
        cx: &mut Context<'_, GitGraphNext>,
        sha: Oid,
        message: SharedString,
    ) {
        let languages = self
            .workspace
            .read_with(cx, |workspace, cx| {
                workspace.project().read(cx).languages().clone()
            })
            .log_err();
        self.selected_commit_message = Some(DetailPanelCommitMessage {
            sha,
            message: cx.new(|cx| Markdown::new(message, languages, None, cx)),
            scroll_handle: ScrollHandle::new(),
        });
        self._selected_commit_message_task = None;
        cx.notify();
    }

    fn select_previous_match(&mut self, cx: &mut Context<Self>) {
        if self.search_state.matches.is_empty() {
            return;
        }

        let mut prev_selection = self.search_state.selected_index.unwrap_or_default();

        if prev_selection == 0 {
            prev_selection = self.search_state.matches.len() - 1;
        } else {
            prev_selection -= 1;
        }

        let Some(&oid) = self.search_state.matches.get_index(prev_selection) else {
            return;
        };

        self.search_state.selected_index = Some(prev_selection);
        self.select_commit_by_sha(oid, cx);
    }

    fn select_next_match(&mut self, cx: &mut Context<Self>) {
        if self.search_state.matches.is_empty() {
            return;
        }

        let mut next_selection = self
            .search_state
            .selected_index
            .map(|index| index + 1)
            .unwrap_or_default();

        if next_selection >= self.search_state.matches.len() {
            next_selection = 0;
        }

        let Some(&oid) = self.search_state.matches.get_index(next_selection) else {
            return;
        };

        self.search_state.selected_index = Some(next_selection);
        self.select_commit_by_sha(oid, cx);
    }

    fn set_log_source(&mut self, log_source: LogSource, cx: &mut Context<Self>) {
        if self.log_source == log_source {
            return;
        }

        self.selected_entry_idx = None;
        self.selected_commit_files.clear();
        self.selected_commit_diff_stats = None;
        self.selected_commit_diff_error = None;
        self.selected_commit_diff = None;
        self.selected_commit_view = None;
        self.pending_commit_view = None;
        self.selected_changed_file = None;
        self.selected_commit_message = None;
        self._selected_commit_message_task = None;
        self._commit_diff_task = None;
        self.changed_files_expanded_dirs.clear();
        self.pending_select_sha = None;
        self.log_source = log_source;

        if self.column_visibility.cols() == TABLE_COLUMN_COUNT + 1
            && let Some(graph_column) = self.column_visibility.as_mut_slice().first_mut()
        {
            *graph_column = matches!(self.log_source, LogSource::Branch(_));
        }

        self.invalidate_state(cx);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn log_source_for_test(&self) -> &LogSource {
        &self.log_source
    }

    pub fn set_repo_id(&mut self, repo_id: RepositoryId, cx: &mut Context<Self>) {
        if repo_id != self.repo_id
            && self
                .git_store
                .read(cx)
                .repositories()
                .contains_key(&repo_id)
        {
            self.repo_id = repo_id;
            self.invalidate_state(cx);
        }
    }

    pub fn select_commit_by_sha(&mut self, sha: impl TryInto<Oid>, cx: &mut Context<Self>) {
        fn inner(this: &mut GitGraphNext, oid: Oid, cx: &mut Context<GitGraphNext>) {
            let Some(selected_repository) = this.get_repository(cx) else {
                return;
            };

            let Some(index) = selected_repository
                .read(cx)
                .get_graph_data(this.log_source.clone(), this.log_order)
                .and_then(|data| data.commit_oid_to_index.get(&oid))
                .copied()
            else {
                this.pending_select_sha = Some(oid);
                return;
            };

            this.pending_select_sha = None;
            this.select_entry(index, ScrollStrategy::Center, cx);
        }

        if let Ok(oid) = sha.try_into() {
            inner(self, oid, cx);
        }
    }

    fn queue_selected_commit_view(&mut self, sha: Oid) {
        let Some(path) = self.selected_changed_file.as_ref() else {
            return;
        };
        let Some(file) = self
            .selected_commit_diff
            .as_ref()
            .and_then(|diff| diff.files.iter().find(|file| &file.path == path))
        else {
            return;
        };

        self.selected_commit_view = None;
        self.pending_commit_view = Some((
            sha,
            CommitDiff {
                files: vec![CommitFile {
                    path: file.path.clone(),
                    old_text: file.old_text.clone(),
                    new_text: file.new_text.clone(),
                    is_binary: file.is_binary,
                }],
                is_shallow_boundary: self
                    .selected_commit_diff
                    .as_ref()
                    .is_some_and(|diff| diff.is_shallow_boundary),
            },
        ));
    }

    fn select_changed_file(
        &mut self,
        repo_path: RepoPath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_changed_file = Some(repo_path);
        if let Some(sha) = self
            .selected_entry_idx
            .and_then(|idx| self.graph_data.commits.get(idx))
            .map(|commit| commit.data.sha)
        {
            self.queue_selected_commit_view(sha);
        }
        cx.notify();
    }

    fn toggle_showing_all_diff_lines(&mut self, cx: &mut Context<Self>) {
        self.showing_all_diff_lines = !self.showing_all_diff_lines;
        if let Some(sha) = self
            .selected_entry_idx
            .and_then(|idx| self.graph_data.commits.get(idx))
            .map(|commit| commit.data.sha)
        {
            self.queue_selected_commit_view(sha);
        }
        cx.notify();
    }

    fn open_selected_commit_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected_entry_index) = self.selected_entry_idx else {
            return;
        };

        self.open_commit_view(selected_entry_index, window, cx);
    }

    fn open_commit_view(
        &mut self,
        entry_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(commit_entry) = self.graph_data.commits.get(entry_index) else {
            return;
        };

        let Some(repository) = self.get_repository(cx) else {
            return;
        };

        CommitView::open(
            commit_entry.data.sha.to_string(),
            repository.downgrade(),
            self.workspace.clone(),
            None,
            None,
            window,
            cx,
        );
    }

    fn copy_commit_sha(&mut self, entry_index: usize, cx: &mut Context<Self>) {
        let Some(commit) = self.graph_data.commits.get(entry_index) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(commit.data.sha.to_string()));
    }

    fn copy_selected_commit_sha(
        &mut self,
        _: &CopyCommitSha,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(selected_entry_index) = self.selected_entry_idx else {
            return;
        };
        self.copy_commit_sha(selected_entry_index, cx);
    }

    fn copy_commit_tag(&mut self, entry_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(commit) = self.graph_data.commits.get(entry_index) else {
            return;
        };

        let tag_names = commit
            .data
            .tag_names()
            .into_iter()
            .map(|tag_name| SharedString::from(tag_name.to_string()))
            .collect::<Vec<_>>();

        match tag_names.as_slice() {
            [] => {}
            [tag_name] => cx.write_to_clipboard(ClipboardItem::new_string(tag_name.to_string())),
            _ => {
                self.workspace
                    .update(cx, |workspace, cx| {
                        workspace.toggle_modal(window, cx, |window, cx| {
                            CommitTagPicker::new(tag_names, window, cx)
                        });
                    })
                    .ok();
            }
        }
    }

    fn copy_selected_commit_tag(
        &mut self,
        _: &CopyCommitTag,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(selected_entry_index) = self.selected_entry_idx else {
            return;
        };
        self.copy_commit_tag(selected_entry_index, window, cx);
    }

    fn deploy_entry_context_menu(
        &mut self,
        position: Point<Pixels>,
        index: usize,
        ref_name: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(commit) = self.graph_data.commits.get(index) else {
            return;
        };
        let repository = self
            .get_repository(cx)
            .map(|repository| repository.downgrade());
        let context_menu = commit_context_menu(
            CommitContextMenuData {
                sha: commit.data.sha,
                tag_names: commit
                    .data
                    .tag_names()
                    .into_iter()
                    .map(|tag_name| SharedString::from(tag_name.to_string()))
                    .collect(),
            },
            CommitContextMenuSource::GitGraph,
            ref_name,
            self.focus_handle.clone(),
            repository,
            self.workspace.clone(),
            window,
            cx,
        );
        self.set_context_menu(context_menu, position, Some(index), window, cx);
    }

    fn set_context_menu(
        &mut self,
        context_menu: Entity<ContextMenu>,
        position: Point<Pixels>,
        target_entry_index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&context_menu.focus_handle(cx), cx);

        let subscription = cx.subscribe_in(
            &context_menu,
            window,
            |this, _, _: &DismissEvent, window, cx| {
                if this.context_menu.as_ref().is_some_and(|context_menu| {
                    context_menu
                        .menu
                        .focus_handle(cx)
                        .contains_focused(window, cx)
                }) {
                    cx.focus_self(window);
                }
                this.context_menu.take();
                cx.notify();
            },
        );
        self.context_menu = Some(GitGraphContextMenu {
            menu: context_menu,
            position,
            target_entry_index,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn toggle_column_visibility(&mut self, col_idx: usize, cx: &mut Context<Self>) {
        if col_idx == 0 && matches!(self.log_source, LogSource::Branch(_)) {
            return;
        }
        if let Some(slot) = self.column_visibility.as_mut_slice().get_mut(col_idx) {
            *slot = !*slot;
            // Column visibility is persisted per item, so schedule a workspace serialization.
            cx.emit(ItemEvent::Edit);
        }
    }

    fn deploy_header_context_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_path_history = matches!(self.log_source, LogSource::Path(_));
        let is_branch_filtered = matches!(self.log_source, LogSource::Branch(_));
        let columns: &[&str] = if is_path_history {
            &["Description", "Date", "Author", "Commit"]
        } else {
            &["Graph", "Description", "Date", "Author", "Commit"]
        };

        let filter = self.column_visibility.clone();
        let visible_count = filter
            .as_slice()
            .iter()
            .filter(|filtered| !**filtered)
            .count();

        let focus_handle = self.focus_handle.clone();
        let git_graph = cx.entity();
        let context_menu = ContextMenu::build(window, cx, |mut context_menu, _window, _cx| {
            context_menu = context_menu.context(focus_handle).header("Columns");
            for (col_idx, label) in columns.iter().enumerate() {
                let is_visible = !filter.get(col_idx).copied().unwrap_or(false);
                // Disable hiding the last remaining visible column and showing the graph while
                // a branch filter is active.
                let can_toggle =
                    (!is_visible || visible_count > 1) && !(is_branch_filtered && col_idx == 0);
                let git_graph = git_graph.clone();
                context_menu = context_menu.toggleable_entry_disabled_when(
                    label.to_string(),
                    is_visible,
                    !can_toggle,
                    IconPosition::End,
                    None,
                    move |_window, cx| {
                        git_graph.update(cx, |this, cx| {
                            this.toggle_column_visibility(col_idx, cx);
                            cx.notify();
                        });
                    },
                );
            }
            context_menu
        });

        self.set_context_menu(context_menu, position, None, window, cx);
    }

    fn render_branch_filter(&self, cx: &mut Context<Self>) -> AnyElement {
        if matches!(self.log_source, LogSource::Path(_)) {
            return Empty.into_any_element();
        }

        let repository = self.get_repository(cx);
        let selected_branch = match &self.log_source {
            LogSource::Branch(branch) => Some(branch.clone()),
            _ => None,
        };
        let label = selected_branch
            .as_ref()
            .and_then(|selected| {
                repository.as_ref().and_then(|repository| {
                    repository
                        .read(cx)
                        .branch_list
                        .iter()
                        .find(|branch| branch.ref_name == *selected)
                        .map(|branch| SharedString::from(branch.name().to_string()))
                })
            })
            .or_else(|| {
                selected_branch.as_ref().map(|branch| {
                    SharedString::from(
                        branch
                            .strip_prefix("refs/heads/")
                            .or_else(|| branch.strip_prefix("refs/remotes/"))
                            .unwrap_or(branch)
                            .to_string(),
                    )
                })
            })
            .unwrap_or_else(|| "All Branches".into());
        let workspace = self.workspace.clone();
        let graph = cx.weak_entity();
        let is_branch_selected = selected_branch.is_some();

        h_flex()
            .gap_0p5()
            .child(
                ui::PopoverMenu::new("git-graph-branch-filter")
                    .menu(move |window, cx| {
                        let graph = graph.clone();
                        let on_select = std::sync::Arc::new(
                            move |branch: git::repository::Branch,
                                  _window: &mut Window,
                                  cx: &mut App| {
                                graph
                                    .update(cx, |graph, cx| {
                                        graph.set_log_source(
                                            LogSource::Branch(branch.ref_name.clone()),
                                            cx,
                                        );
                                    })
                                    .ok();
                            },
                        );
                        Some(crate::branch_picker::select_popover(
                            workspace.clone(),
                            repository.clone(),
                            selected_branch.clone(),
                            on_select,
                            window,
                            cx,
                        ))
                    })
                    .trigger_with_tooltip(
                        Button::new("git-graph-branch-filter-trigger", label)
                            .start_icon(
                                Icon::new(IconName::GitBranch)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .end_icon(
                                Icon::new(IconName::ChevronDown)
                                    .size(IconSize::XSmall)
                                    .color(Color::Muted),
                            ),
                        Tooltip::text("Filter Commits by Branch"),
                    ),
            )
            .children(is_branch_selected.then(|| {
                IconButton::new("git-graph-clear-branch-filter", IconName::Close)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Show All Branches"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_log_source(LogSource::All, cx);
                    }))
            }))
            .into_any_element()
    }

    fn render_search_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let branch_filter = self.render_branch_filter(cx);
        let color = cx.theme().colors();
        let query_focus_handle = self
            .search_state
            .editor
            .focus_handle(cx)
            .tab_index(1)
            .tab_stop(true);

        h_flex()
            .key_context("GitGraphSearchBar")
            .tab_index(1)
            .tab_group()
            .tab_stop(false)
            .w_full()
            .p_1p5()
            .gap_1p5()
            .border_b_1()
            .border_color(color.border_variant)
            .child(branch_filter)
            .child(
                h_flex()
                    .h_8()
                    .flex_1()
                    .min_w_0()
                    .px_1p5()
                    .gap_1()
                    .track_focus(&query_focus_handle)
                    .border_1()
                    .border_color(color.border_variant)
                    .rounded_md()
                    .bg(color.toolbar_background)
                    .on_action(cx.listener(Self::confirm_search))
                    .child(self.search_state.editor.clone())
                    .child({
                        let focus_handle = query_focus_handle.clone();
                        IconButton::new("git-graph-search-case-sensitive", IconName::CaseSensitive)
                            .shape(ui::IconButtonShape::Square)
                            .toggle_state(self.search_state.case_sensitive)
                            .on_click({
                                let focus_handle = query_focus_handle.clone();
                                move |_, window, cx| {
                                    if !focus_handle.is_focused(window) {
                                        window.focus(&focus_handle, cx);
                                    }
                                    window.dispatch_action(ToggleCaseSensitive.boxed_clone(), cx);
                                }
                            })
                            .tooltip(move |_window, cx| {
                                Tooltip::for_action_in(
                                    "Match Case Sensitivity",
                                    &ToggleCaseSensitive,
                                    &focus_handle,
                                    cx,
                                )
                            })
                    }),
            )
            .child(
                h_flex()
                    .min_w_64()
                    .gap_1()
                    .child({
                        let focus_handle = self.focus_handle.clone();
                        IconButton::new("git-graph-search-prev", IconName::ChevronLeft)
                            .shape(ui::IconButtonShape::Square)
                            .icon_size(IconSize::Small)
                            .tooltip(move |_, cx| {
                                Tooltip::for_action_in(
                                    "Select Previous Match",
                                    &SelectPreviousMatch,
                                    &focus_handle,
                                    cx,
                                )
                            })
                            .map(|this| {
                                if self.search_state.matches.is_empty() {
                                    this.disabled(true)
                                } else {
                                    this.disabled(false).on_click(cx.listener(|this, _, _, cx| {
                                        this.select_previous_match(cx);
                                    }))
                                }
                            })
                    })
                    .child({
                        let focus_handle = self.focus_handle.clone();
                        IconButton::new("git-graph-search-next", IconName::ChevronRight)
                            .shape(ui::IconButtonShape::Square)
                            .icon_size(IconSize::Small)
                            .tooltip(move |_, cx| {
                                Tooltip::for_action_in(
                                    "Select Next Match",
                                    &SelectNextMatch,
                                    &focus_handle,
                                    cx,
                                )
                            })
                            .map(|this| {
                                if self.search_state.matches.is_empty() {
                                    this.disabled(true)
                                } else {
                                    this.disabled(false).on_click(cx.listener(|this, _, _, cx| {
                                        this.select_next_match(cx);
                                    }))
                                }
                            })
                    })
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(
                                Label::new(format!(
                                    "{}/{}",
                                    self.search_state
                                        .selected_index
                                        .map(|index| index + 1)
                                        .unwrap_or(0),
                                    self.search_state.matches.len()
                                ))
                                .size(LabelSize::Small)
                                .when(self.search_state.matches.is_empty(), |this| {
                                    this.color(Color::Disabled)
                                }),
                            )
                            .when(
                                matches!(
                                    &self.search_state.state,
                                    QueryState::Confirmed((_, task)) if !task.is_ready()
                                ),
                                |this| {
                                    this.child(
                                        Icon::new(IconName::ArrowCircle)
                                            .color(Color::Accent)
                                            .size(IconSize::Small)
                                            .with_rotate_animation(2)
                                            .into_any_element(),
                                    )
                                },
                            ),
                    ),
            )
    }

    fn render_loading_spinner(&self, cx: &App) -> AnyElement {
        let rems = TextSize::Large.rems(cx);
        Icon::new(IconName::LoadCircle)
            .size(IconSize::Custom(rems))
            .color(Color::Accent)
            .with_rotate_animation(3)
            .into_any_element()
    }

    fn initialize_pending_commit_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((sha, diff)) = self.pending_commit_view.take() else {
            return;
        };
        let is_still_selected = self
            .selected_entry_idx
            .and_then(|idx| self.graph_data.commits.get(idx))
            .is_some_and(|commit| commit.data.sha == sha);
        if !is_still_selected {
            return;
        }

        let Some(repository) = self.get_repository(cx) else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let project = workspace.read(cx).project().clone();
        let diff_view = cx.new(|cx| {
            crate::git_graph_next_diff::GitGraphNextDiff::new(
                sha,
                diff,
                self.showing_all_diff_lines,
                repository,
                project,
                workspace,
                window,
                cx,
            )
        });
        cx.observe(&diff_view, |_, _, cx| cx.notify()).detach();
        self.selected_commit_view = Some(diff_view);
    }

    fn render_commit_detail_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(selected_idx) = self.selected_entry_idx else {
            return Empty.into_any_element();
        };

        let Some(commit_entry) = self.graph_data.commits.get(selected_idx) else {
            return Empty.into_any_element();
        };

        let Some(repository) = self.get_repository(cx) else {
            return Empty.into_any_element();
        };

        let data = repository.update(cx, |repository, cx| {
            repository
                .fetch_commit_data(commit_entry.data.sha, false, cx)
                .clone()
        });

        let full_sha: SharedString = commit_entry.data.sha.to_string().into();
        let ref_names = commit_entry.data.ref_names.clone();

        let head_branch_name: Option<SharedString> = repository
            .read(cx)
            .snapshot()
            .branch
            .as_ref()
            .map(|branch| SharedString::from(branch.name().to_string()));

        let accent_colors = cx.theme().accents();
        let accent_color = accent_colors
            .0
            .get(commit_entry.color_idx)
            .copied()
            .unwrap_or_else(|| accent_colors.0.first().copied().unwrap_or_default());

        let (author_name, author_email, commit_timestamp) = match &data {
            CommitDataState::Loaded(data) => (
                data.author_name.clone(),
                data.author_email.clone(),
                Some(data.commit_timestamp),
            ),
            CommitDataState::Loading(_) => ("Loading…".into(), "".into(), None),
        };

        let date_string = commit_timestamp
            .and_then(|ts| OffsetDateTime::from_unix_timestamp(ts).ok())
            .map(|datetime| {
                let local_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
                let local_datetime = datetime.to_offset(local_offset);
                let format =
                    time::format_description::parse("[month repr:short] [day], [year]").ok();
                format
                    .and_then(|f| local_datetime.format(&f).ok())
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        let remote = repository.update(cx, |repo, cx| {
            let remote_url = repo.default_remote_url()?;
            let provider_registry = GitHostingProviderRegistry::default_global(cx);
            let (provider, parsed) = parse_git_remote_url(provider_registry, &remote_url)?;
            Some(GitRemote {
                host: provider,
                owner: parsed.owner.into(),
                repo: parsed.repo.into(),
            })
        });

        let avatar = {
            let author_email_for_avatar = if author_email.is_empty() {
                None
            } else {
                Some(author_email.clone())
            };

            CommitAvatar::new(&full_sha, author_email_for_avatar, remote.as_ref())
                .size(px(32.))
                .render(window, cx)
        };

        let changed_files_count = self.selected_commit_files.len();

        let (total_lines_added, total_lines_removed) =
            self.selected_commit_diff_stats.unwrap_or((0, 0));

        let mut changed_file_entries = self.selected_commit_files.clone();
        if !self.changed_files_view_mode.is_tree() {
            changed_file_entries.sort_by_key(|file| match file.status {
                FileStatus::Tracked(TrackedStatus {
                    index_status: StatusCode::Added,
                    ..
                }) => 0,
                FileStatus::Tracked(TrackedStatus {
                    index_status: StatusCode::Deleted,
                    ..
                }) => 2,
                _ => 1,
            });
        }
        let changed_file_entries = Rc::new(changed_file_entries);
        let tree_entries: Rc<Vec<ChangedFileTreeEntry>> = if self.changed_files_view_mode.is_tree()
        {
            Rc::new(build_changed_file_tree_entries(
                changed_file_entries.as_ref().clone(),
                &self.changed_files_expanded_dirs,
            ))
        } else {
            Rc::default()
        };

        let is_tree_view = self.changed_files_view_mode.is_tree();
        let view_toggle = IconButton::new("toggle-changed-files-view", IconName::ListTree)
            .icon_size(IconSize::Small)
            .toggle_state(self.changed_files_view_mode.is_tree())
            .tooltip({
                let tooltip = if is_tree_view {
                    "Show Flat View"
                } else {
                    "Show Tree View"
                };
                move |_, cx| Tooltip::for_action(tooltip, &ToggleChangedFilesView, cx)
            })
            .on_click(cx.listener(|this, _, _window, cx| {
                this.changed_files_view_mode = this.changed_files_view_mode.toggled();
                this.changed_files_scroll_handle
                    .scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }));

        v_flex()
            .min_w(px(240.))
            .min_h_0()
            .h_full()
            .bg(cx.theme().colors().editor_background)
            .flex_basis(DefiniteLength::Fraction(
                self.detail_content_split_state
                    .read(cx)
                    .visible_left_ratio(),
            ))
            .child(
                v_flex()
                    .relative()
                    .w_full()
                    .p_2()
                    .gap_2()
                    .child(
                        div().absolute().top_2().right_2().child(
                            IconButton::new("close-detail", IconName::Close)
                                .icon_size(IconSize::Small)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.selected_entry_idx = None;
                                    this.selected_commit_files.clear();
                                    this.selected_commit_diff_stats = None;
                                    this.selected_commit_diff_error = None;
                                    this.selected_commit_diff = None;
                                    this.selected_commit_view = None;
                                    this.pending_commit_view = None;
                                    this.selected_changed_file = None;
                                    this.selected_commit_message = None;
                                    this._selected_commit_message_task = None;
                                    this.changed_files_expanded_dirs.clear();
                                    this._commit_diff_task = None;
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        h_flex().py_1().pr_6().w_full().gap_2().child(avatar).child(
                            v_flex().min_w_0().child(Label::new(author_name)).child(
                                Label::new(date_string)
                                    .color(Color::Muted)
                                    .size(LabelSize::Small),
                            ),
                        ),
                    )
                    .children((!ref_names.is_empty()).then(|| {
                        h_flex()
                            .gap_1()
                            .flex_wrap()
                            .children(ref_names.iter().map(|name| {
                                let is_head = Self::is_head_ref(name.as_ref(), &head_branch_name);
                                self.render_ref_chip(name, accent_color, is_head, selected_idx, cx)
                            }))
                    }))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_1()
                            .when(!author_email.is_empty(), |this| {
                                let copied_state: Entity<CopiedState> = window.use_keyed_state(
                                    "author-email-copy",
                                    cx,
                                    CopiedState::new,
                                );
                                let is_copied = copied_state.read(cx).is_copied();

                                let (icon, icon_color, tooltip_label) = if is_copied {
                                    (IconName::Check, Color::Success, "Email Copied!")
                                } else {
                                    (IconName::Envelope, Color::Muted, "Copy Email")
                                };

                                let copy_email = author_email.clone();
                                let author_email_for_tooltip = author_email.clone();

                                this.child(
                                    Button::new("author-email-copy", author_email.clone())
                                        .start_icon(
                                            Icon::new(icon).size(IconSize::Small).color(icon_color),
                                        )
                                        .label_size(LabelSize::Small)
                                        .truncate(true)
                                        .color(Color::Muted)
                                        .tooltip(move |_, cx| {
                                            Tooltip::with_meta(
                                                tooltip_label,
                                                None,
                                                author_email_for_tooltip.clone(),
                                                cx,
                                            )
                                        })
                                        .on_click(move |_, _, cx| {
                                            copied_state.update(cx, |state, _cx| {
                                                state.mark_copied();
                                            });
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copy_email.to_string(),
                                            ));
                                            let state_id = copied_state.entity_id();
                                            cx.spawn(async move |cx| {
                                                cx.background_executor()
                                                    .timer(COPIED_STATE_DURATION)
                                                    .await;
                                                cx.update(|cx| {
                                                    cx.notify(state_id);
                                                })
                                            })
                                            .detach();
                                        }),
                                )
                            })
                            .child({
                                let copy_sha = full_sha.clone();
                                let copied_state: Entity<CopiedState> =
                                    window.use_keyed_state("sha-copy", cx, CopiedState::new);
                                let is_copied = copied_state.read(cx).is_copied();

                                let (icon, icon_color, tooltip_label) = if is_copied {
                                    (IconName::Check, Color::Success, "Commit SHA Copied!")
                                } else {
                                    (IconName::Hash, Color::Muted, "Copy Commit SHA")
                                };

                                Button::new("sha-button", &full_sha)
                                    .start_icon(
                                        Icon::new(icon).size(IconSize::Small).color(icon_color),
                                    )
                                    .label_size(LabelSize::Small)
                                    .truncate(true)
                                    .color(Color::Muted)
                                    .tooltip({
                                        let full_sha = full_sha.clone();
                                        move |_, cx| {
                                            Tooltip::with_meta(
                                                tooltip_label,
                                                None,
                                                full_sha.clone(),
                                                cx,
                                            )
                                        }
                                    })
                                    .on_click(move |_, _, cx| {
                                        copied_state.update(cx, |state, _cx| {
                                            state.mark_copied();
                                        });
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy_sha.to_string(),
                                        ));
                                        let state_id = copied_state.entity_id();
                                        cx.spawn(async move |cx| {
                                            cx.background_executor()
                                                .timer(COPIED_STATE_DURATION)
                                                .await;
                                            cx.update(|cx| {
                                                cx.notify(state_id);
                                            })
                                        })
                                        .detach();
                                    })
                            })
                            .when_some(remote.clone(), |this, remote| {
                                let provider_name = remote.host.name();
                                let icon = ui::git_hosting_provider_icon(provider_name.as_str());
                                let parsed_remote = ParsedGitRemote {
                                    owner: remote.owner.as_ref().into(),
                                    repo: remote.repo.as_ref().into(),
                                };
                                let params = BuildCommitPermalinkParams {
                                    sha: full_sha.as_ref(),
                                };
                                let url = remote
                                    .host
                                    .build_commit_permalink(&parsed_remote, params)
                                    .to_string();

                                this.child(
                                    Button::new(
                                        "view-on-provider",
                                        format!("View on {}", provider_name),
                                    )
                                    .start_icon(
                                        Icon::new(icon).size(IconSize::Small).color(Color::Muted),
                                    )
                                    .label_size(LabelSize::Small)
                                    .truncate(true)
                                    .color(Color::Muted)
                                    .on_click(
                                        move |_, _, cx| {
                                            cx.open_url(&url);
                                        },
                                    ),
                                )
                            }),
                    ),
            )
            .child(Divider::horizontal())
            .child(self.render_commit_message(window, cx))
            .child(Divider::horizontal())
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .p_2()
                            .pr_3()
                            .pb_1()
                            .gap_1()
                            .w_full()
                            .justify_between()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Label::new(format!(
                                            "{} Changed {}",
                                            changed_files_count,
                                            if changed_files_count == 1 {
                                                "File"
                                            } else {
                                                "Files"
                                            }
                                        ))
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                    )
                                    .child(Divider::vertical())
                                    .child(view_toggle),
                            )
                            .child(DiffStat::new(
                                "commit-diff-stat",
                                total_lines_added,
                                total_lines_removed,
                            )),
                    )
                    .child(
                        div()
                            .id("changed-files-container")
                            .flex_1()
                            .min_h_0()
                            .child({
                                let flat_entries = changed_file_entries;

                                let entry_count = if is_tree_view {
                                    tree_entries.len()
                                } else {
                                    flat_entries.len()
                                };
                                let commit_sha = full_sha.clone();
                                let repository = repository.downgrade();
                                let workspace = self.workspace.clone();
                                let git_graph = cx.weak_entity();
                                let selected_changed_file = self.selected_changed_file.clone();
                                let indent_tree_entries = tree_entries.clone();

                                uniform_list(
                                    "changed-files-list",
                                    entry_count,
                                    move |range, _window, cx| {
                                        range
                                            .map(|ix| {
                                                if is_tree_view {
                                                    match &tree_entries[ix] {
                                                        ChangedFileTreeEntry::Directory(entry) => {
                                                            entry.render(ix, git_graph.clone(), cx)
                                                        }
                                                        ChangedFileTreeEntry::File(entry) => {
                                                            entry.entry.render(
                                                                ix,
                                                                entry.depth,
                                                                None,
                                                                commit_sha.clone(),
                                                                repository.clone(),
                                                                workspace.clone(),
                                                                git_graph.clone(),
                                                                selected_changed_file.as_ref()
                                                                    == Some(&entry.entry.repo_path),
                                                                cx,
                                                            )
                                                        }
                                                    }
                                                } else {
                                                    let directory_label = (!flat_entries[ix]
                                                        .dir_path
                                                        .is_empty())
                                                    .then(|| flat_entries[ix].dir_path.clone());
                                                    flat_entries[ix].render(
                                                        ix,
                                                        0,
                                                        directory_label,
                                                        commit_sha.clone(),
                                                        repository.clone(),
                                                        workspace.clone(),
                                                        git_graph.clone(),
                                                        selected_changed_file.as_ref()
                                                            == Some(&flat_entries[ix].repo_path),
                                                        cx,
                                                    )
                                                }
                                            })
                                            .collect()
                                    },
                                )
                                .when(is_tree_view, |list| {
                                    list.with_decoration(
                                        ui::indent_guides(
                                            px(TREE_INDENT),
                                            IndentGuideColors::panel(cx),
                                        )
                                        .with_left_offset(
                                            ui::LIST_ITEM_INDENT_GUIDE_LEFT_OFFSET - px(2.),
                                        )
                                        .with_compute_indents_fn(
                                            cx.entity(),
                                            move |_, range, _window, _cx| {
                                                range
                                                    .map(|ix| match indent_tree_entries.get(ix) {
                                                        Some(ChangedFileTreeEntry::Directory(
                                                            entry,
                                                        )) => entry.depth,
                                                        Some(ChangedFileTreeEntry::File(entry)) => {
                                                            entry.depth
                                                        }
                                                        None => 0,
                                                    })
                                                    .collect()
                                            },
                                        ),
                                    )
                                })
                                .size_full()
                                .track_scroll(&self.changed_files_scroll_handle)
                            })
                            .vertical_scrollbar_for(&self.changed_files_scroll_handle, window, cx),
                    ),
            )
            .child(Divider::horizontal())
            .child(
                h_flex().p_1p5().w_full().child(
                    Button::new("view-commit", "View Commit")
                        .full_width()
                        .start_icon(
                            Icon::new(IconName::GitCommit)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                        .style(ButtonStyle::OutlinedGhost)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_selected_commit_view(window, cx);
                        })),
                ),
            )
            .into_any_element()
    }

    fn render_graph_canvas(
        &self,
        window: &Window,
        cx: &mut Context<GitGraphNext>,
    ) -> impl IntoElement {
        let row_height = Self::row_height(window, cx);
        let visible_row_count = self.visible_row_count(window, cx);
        let table_state = self.table_interaction_state.read(cx);
        let viewport_height = table_state
            .scroll_handle
            .0
            .borrow()
            .last_item_size
            .map(|size| size.item.height)
            .unwrap_or(window.viewport_size().height);
        let loaded_commit_count = self.graph_data.commits.len();

        let content_height = row_height * loaded_commit_count;
        let max_scroll = (content_height - viewport_height).max(px(0.));
        let scroll_offset_y = (-table_state.scroll_offset().y).clamp(px(0.), max_scroll);

        let first_visible_row = (scroll_offset_y / row_height).floor() as usize;
        let vertical_scroll_offset = scroll_offset_y - (first_visible_row as f32 * row_height);

        let graph_viewport_width = self.graph_viewport_width(window, cx);
        let graph_width = if self.graph_canvas_content_width() > graph_viewport_width {
            self.graph_canvas_content_width()
        } else {
            graph_viewport_width
        };
        let last_visible_row = first_visible_row + visible_row_count + 1;

        let viewport_range = first_visible_row.min(loaded_commit_count.saturating_sub(1))
            ..(last_visible_row).min(loaded_commit_count);
        let rows = self.graph_data.commits[viewport_range.clone()].to_vec();
        let commit_lines: Vec<_> = self
            .graph_data
            .lines
            .iter()
            .filter(|line| {
                line.full_interval.start <= viewport_range.end
                    && line.full_interval.end >= viewport_range.start
            })
            .cloned()
            .collect();

        let mut lines: BTreeMap<usize, Vec<_>> = BTreeMap::new();

        let hovered_entry_idx = self.hovered_entry_idx;
        let selected_entry_idx = self.selected_entry_idx;
        let context_menu_target_index = self
            .context_menu
            .as_ref()
            .and_then(|menu| menu.target_entry_index);
        let is_focused = self.focus_handle.is_focused(window);
        let graph_canvas_bounds = self.graph_canvas_bounds.clone();

        gpui::canvas(
            move |_bounds, _window, _cx| {},
            move |bounds: Bounds<Pixels>, _: (), window: &mut Window, cx: &mut App| {
                graph_canvas_bounds.set(Some(bounds));

                window.paint_layer(bounds, |window| {
                    let accent_colors = cx.theme().accents();

                    let hover_bg = cx.theme().colors().element_hover.opacity(0.6);
                    let selected_bg = if is_focused {
                        cx.theme().colors().element_selected
                    } else {
                        cx.theme().colors().element_hover
                    };

                    for visible_row_idx in 0..rows.len() {
                        let absolute_row_idx = first_visible_row + visible_row_idx;
                        let is_hovered = hovered_entry_idx == Some(absolute_row_idx);
                        let is_selected = selected_entry_idx == Some(absolute_row_idx);
                        let is_context_menu_target =
                            context_menu_target_index == Some(absolute_row_idx);

                        if is_hovered || is_selected || is_context_menu_target {
                            let row_y = bounds.origin.y + visible_row_idx as f32 * row_height
                                - vertical_scroll_offset;

                            let row_bounds = Bounds::new(
                                point(bounds.origin.x, row_y),
                                gpui::Size {
                                    width: bounds.size.width,
                                    height: row_height,
                                },
                            );

                            let bg_color = if is_selected || is_context_menu_target {
                                selected_bg
                            } else {
                                hover_bg
                            };
                            window.paint_quad(gpui::fill(row_bounds, bg_color));
                        }
                    }

                    for (row_idx, row) in rows.into_iter().enumerate() {
                        let row_color = accent_colors.color_for_index(row.color_idx as u32);
                        let row_y_center =
                            bounds.origin.y + row_idx as f32 * row_height + row_height / 2.0
                                - vertical_scroll_offset;

                        let commit_x = lane_center_x(bounds, row.lane as f32);

                        draw_commit_circle(commit_x, row_y_center, row_color, window);
                    }

                    for line in commit_lines {
                        let Some((start_segment_idx, start_column)) =
                            line.get_first_visible_segment_idx(first_visible_row)
                        else {
                            continue;
                        };

                        let line_x = lane_center_x(bounds, start_column as f32);

                        let start_row = line.full_interval.start as i32 - first_visible_row as i32;

                        let from_y =
                            bounds.origin.y + start_row as f32 * row_height + row_height / 2.0
                                - vertical_scroll_offset
                                + COMMIT_CIRCLE_RADIUS;

                        let mut current_row = from_y;
                        let mut current_column = line_x;

                        let mut builder = PathBuilder::stroke(LINE_WIDTH);
                        builder.move_to(point(line_x, from_y));

                        let segments = &line.segments[start_segment_idx..];
                        let desired_curve_height = row_height / 3.0;
                        let desired_curve_width = LANE_WIDTH / 3.0;

                        for (segment_idx, segment) in segments.iter().enumerate() {
                            let is_last = segment_idx + 1 == segments.len();

                            match segment {
                                CommitLineSegment::Straight { to_row } => {
                                    let mut dest_row = to_row_center(
                                        to_row - first_visible_row,
                                        row_height,
                                        vertical_scroll_offset,
                                        bounds,
                                    );
                                    if is_last {
                                        dest_row -= COMMIT_CIRCLE_RADIUS;
                                    }

                                    let dest_point = point(current_column, dest_row);

                                    current_row = dest_point.y;
                                    builder.line_to(dest_point);
                                    builder.move_to(dest_point);
                                }
                                CommitLineSegment::Curve {
                                    to_column,
                                    on_row,
                                    curve_kind,
                                } => {
                                    let mut to_column = lane_center_x(bounds, *to_column as f32);

                                    let mut to_row = to_row_center(
                                        *on_row - first_visible_row,
                                        row_height,
                                        vertical_scroll_offset,
                                        bounds,
                                    );

                                    // This means that this branch was a checkout
                                    let going_right = to_column > current_column;
                                    let column_shift = if going_right {
                                        COMMIT_CIRCLE_RADIUS + COMMIT_CIRCLE_STROKE_WIDTH
                                    } else {
                                        -COMMIT_CIRCLE_RADIUS - COMMIT_CIRCLE_STROKE_WIDTH
                                    };

                                    match curve_kind {
                                        CurveKind::Checkout => {
                                            if is_last {
                                                to_column -= column_shift;
                                            }

                                            let available_curve_width =
                                                (to_column - current_column).abs();
                                            let available_curve_height =
                                                (to_row - current_row).abs();
                                            let curve_width =
                                                desired_curve_width.min(available_curve_width);
                                            let curve_height =
                                                desired_curve_height.min(available_curve_height);
                                            let signed_curve_width = if going_right {
                                                curve_width
                                            } else {
                                                -curve_width
                                            };
                                            let curve_start =
                                                point(current_column, to_row - curve_height);
                                            let curve_end =
                                                point(current_column + signed_curve_width, to_row);
                                            let curve_control = point(current_column, to_row);

                                            builder.move_to(point(current_column, current_row));
                                            builder.line_to(curve_start);
                                            builder.move_to(curve_start);
                                            builder.curve_to(curve_end, curve_control);
                                            builder.move_to(curve_end);
                                            builder.line_to(point(to_column, to_row));
                                        }
                                        CurveKind::Merge => {
                                            if is_last {
                                                to_row -= COMMIT_CIRCLE_RADIUS;
                                            }

                                            let merge_start = point(
                                                current_column + column_shift,
                                                current_row - COMMIT_CIRCLE_RADIUS,
                                            );
                                            let available_curve_width =
                                                (to_column - merge_start.x).abs();
                                            let available_curve_height =
                                                (to_row - merge_start.y).abs();
                                            let curve_width =
                                                desired_curve_width.min(available_curve_width);
                                            let curve_height =
                                                desired_curve_height.min(available_curve_height);
                                            let signed_curve_width = if going_right {
                                                curve_width
                                            } else {
                                                -curve_width
                                            };
                                            let curve_start = point(
                                                to_column - signed_curve_width,
                                                merge_start.y,
                                            );
                                            let curve_end =
                                                point(to_column, merge_start.y + curve_height);
                                            let curve_control = point(to_column, merge_start.y);

                                            builder.move_to(merge_start);
                                            builder.line_to(curve_start);
                                            builder.move_to(curve_start);
                                            builder.curve_to(curve_end, curve_control);
                                            builder.move_to(curve_end);
                                            builder.line_to(point(to_column, to_row));
                                        }
                                    }
                                    current_row = to_row;
                                    current_column = to_column;
                                    builder.move_to(point(current_column, current_row));
                                }
                            }
                        }

                        builder.close();
                        lines.entry(line.color_idx).or_default().push(builder);
                    }

                    for (color_idx, builders) in lines {
                        let line_color = accent_colors.color_for_index(color_idx as u32);

                        for builder in builders {
                            if let Ok(path) = builder.build() {
                                // we paint each color on it's own layer to stop overlapping lines
                                // of different colors changing the color of a line
                                window.paint_layer(bounds, |window| {
                                    window.paint_path(path, line_color);
                                });
                            }
                        }
                    }
                })
            },
        )
        .w(graph_width)
        .h_full()
    }

    fn row_at_position(
        &self,
        position_y: Pixels,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<usize> {
        let canvas_bounds = self.graph_canvas_bounds.get()?;
        let table_state = self.table_interaction_state.read(cx);
        let scroll_offset_y = -table_state.scroll_offset().y;

        let local_y = position_y - canvas_bounds.origin.y;

        if local_y >= px(0.) && local_y < canvas_bounds.size.height {
            let absolute_y = local_y + scroll_offset_y;
            let row_height = Self::row_height(window, cx);
            let absolute_row = (absolute_y / row_height).floor() as usize;

            if absolute_row < self.graph_data.commits.len() {
                return Some(absolute_row);
            }
        }

        None
    }

    fn handle_graph_mouse_move(
        &mut self,
        event: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(row) = self.row_at_position(event.position.y, window, cx) {
            if self.hovered_entry_idx != Some(row) {
                self.hovered_entry_idx = Some(row);
                cx.notify();
            }
        } else if self.hovered_entry_idx.is_some() {
            self.hovered_entry_idx = None;
            cx.notify();
        }
    }

    fn handle_entry_click(
        &mut self,
        entry_idx: usize,
        event: &ClickEvent,
        scroll_strategy: ScrollStrategy,
        focus_handle: Option<&FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Right-clicks open the context menu, not the details panel.
        if event.is_right_click() {
            return;
        }

        if let Some(focus_handle) = focus_handle {
            focus_handle.focus(window, cx);
        }

        self.select_entry(entry_idx, scroll_strategy, cx);

        if event.click_count() >= 2 {
            self.open_commit_view(entry_idx, window, cx);
        }
    }

    fn handle_graph_click(
        &mut self,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(row) = self.row_at_position(event.position().y, window, cx) {
            self.handle_entry_click(row, event, ScrollStrategy::Nearest, None, window, cx);
        }
    }

    fn handle_entry_secondary_mouse_down(
        &mut self,
        entry_idx: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.deploy_entry_context_menu(event.position, entry_idx, None, window, cx);
        cx.stop_propagation();
    }

    fn handle_graph_secondary_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.row_at_position(event.position.y, window, cx) else {
            return;
        };

        self.handle_entry_secondary_mouse_down(row, event, window, cx);
    }

    fn handle_graph_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let line_height = window.line_height();
        let delta = event.delta.pixel_delta(line_height);

        let table_state = self.table_interaction_state.read(cx);
        let current_offset = table_state.scroll_offset();

        let viewport_height = table_state.scroll_handle.viewport().size.height;

        let commit_count = match self.graph_data.max_commit_count {
            AllCommitCount::Loading(count) => count,
            AllCommitCount::FullyLoaded(count) => count,
            AllCommitCount::NotLoaded => self.graph_data.commits.len(),
        };
        let content_height = Self::row_height(window, cx) * commit_count;
        let max_vertical_scroll = (viewport_height - content_height).min(px(0.));

        let new_y = (current_offset.y + delta.y).clamp(max_vertical_scroll, px(0.));
        let new_offset = Point::new(current_offset.x, new_y);

        if new_offset != current_offset {
            table_state.set_scroll_offset(new_offset);
            cx.notify();
        }
    }

    fn commit_count_and_loading_state(&mut self, cx: &mut Context<Self>) -> (usize, bool) {
        match self.graph_data.max_commit_count {
            AllCommitCount::FullyLoaded(count) => (count, false),
            AllCommitCount::Loading(count) => {
                let is_loading = self
                    .get_repository(cx)
                    .map(|repository| {
                        repository.update(cx, |repository, cx| {
                            repository
                                .graph_data(self.log_source.clone(), self.log_order, 0..0, cx)
                                .is_loading
                        })
                    })
                    .unwrap_or(false);

                (count, is_loading)
            }
            AllCommitCount::NotLoaded => {
                let (commit_count, is_loading) = if let Some(repository) = self.get_repository(cx) {
                    repository.update(cx, |repository, cx| {
                        // Start loading the graph data if we haven't started already
                        let GraphDataResponse {
                            commits,
                            is_loading,
                            error: _,
                        } = repository.graph_data(
                            self.log_source.clone(),
                            self.log_order,
                            0..usize::MAX,
                            cx,
                        );
                        self.graph_data.add_commits(commits);
                        (commits.len(), is_loading)
                    })
                } else {
                    (0, false)
                };

                (commit_count, is_loading)
            }
        }
    }

    fn render_graph_detail_resize_handle(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("git-graph-detail-resize-container")
            .relative()
            .w_full()
            .flex_shrink_0()
            .h(px(1.))
            .bg(cx.theme().colors().border_variant)
            .child(
                div()
                    .id("git-graph-detail-resize-handle")
                    .absolute()
                    .top(px(-RESIZE_HANDLE_WIDTH / 2.0))
                    .w_full()
                    .h(px(RESIZE_HANDLE_WIDTH))
                    .cursor_row_resize()
                    .block_mouse_except_scroll()
                    .on_click(cx.listener(|this, event: &ClickEvent, _window, cx| {
                        if event.click_count() >= 2 {
                            this.graph_detail_split_state.update(cx, |state, _| {
                                state.on_double_click();
                            });
                            cx.emit(ItemEvent::Edit);
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }))
                    .on_drag(DraggedGraphDetailSplitHandle, |_, _, _, cx| {
                        cx.new(|_| gpui::Empty)
                    }),
            )
            .into_any_element()
    }

    fn render_detail_content_resize_handle(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("git-graph-detail-content-resize-container")
            .relative()
            .h_full()
            .flex_shrink_0()
            .w(px(1.))
            .bg(cx.theme().colors().border_variant)
            .child(
                div()
                    .id("git-graph-detail-content-resize-handle")
                    .absolute()
                    .left(px(-RESIZE_HANDLE_WIDTH / 2.0))
                    .w(px(RESIZE_HANDLE_WIDTH))
                    .h_full()
                    .cursor_col_resize()
                    .block_mouse_except_scroll()
                    .on_click(cx.listener(|this, event: &ClickEvent, _window, cx| {
                        if event.click_count() >= 2 {
                            this.detail_content_split_state.update(cx, |state, _| {
                                state.on_double_click();
                            });
                            cx.emit(ItemEvent::Edit);
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }))
                    .on_drag(DraggedDetailContentSplitHandle, |_, _, _, cx| {
                        cx.new(|_| gpui::Empty)
                    }),
            )
            .into_any_element()
    }

    fn render_commit_diff_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let error = self.selected_commit_diff_error.clone().or_else(|| {
            self.selected_commit_view
                .as_ref()
                .and_then(|diff_view| diff_view.read(cx).load_error())
        });
        let editor = self
            .selected_commit_view
            .as_ref()
            .filter(|_| error.is_none())
            .map(|diff_view| diff_view.read(cx).editor());
        let has_no_changes = self.selected_commit_diff.is_some()
            && self.selected_commit_files.is_empty()
            && error.is_none();
        let selected_file = self.selected_changed_file.as_ref().and_then(|path| {
            self.selected_commit_files
                .iter()
                .find(|entry| &entry.repo_path == path)
                .cloned()
        });
        let selected_file_diff_stats = selected_file.as_ref().and_then(|file| file.diff_stats);
        let selected_file_heading = if let Some(file) = selected_file {
            let path: SharedString = file.repo_path.as_unix_str().to_string().into();
            let tooltip_path = path.clone();
            h_flex()
                .id("git-graph-selected-file-heading")
                .min_w_0()
                .flex_1()
                .gap_1()
                .child(git_status_icon(file.status))
                .child(Label::new(path).size(LabelSize::Small).truncate())
                .tooltip(move |_, cx| {
                    Tooltip::with_meta("Current File", None, tooltip_path.clone(), cx)
                })
                .into_any_element()
        } else {
            Label::new("Changes")
                .size(LabelSize::Small)
                .into_any_element()
        };
        let (excerpt_icon, excerpt_tooltip) = if self.showing_all_diff_lines {
            (IconName::ChevronDownUp, "Show Changes Only")
        } else {
            (IconName::ChevronUpDown, "Show All Lines")
        };

        v_flex()
            .min_w_0()
            .min_h_0()
            .h_full()
            .flex_basis(DefiniteLength::Fraction(
                self.detail_content_split_state.read(cx).right_ratio(),
            ))
            .child(
                h_flex()
                    .h(px(34.))
                    .px_2()
                    .flex_shrink_0()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(selected_file_heading)
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .children(selected_file_diff_stats.map(|(added, removed)| {
                                DiffStat::new("git-graph-selected-file-diff-stat", added, removed)
                            }))
                            .child(
                                IconButton::new("git-graph-toggle-diff-excerpts", excerpt_icon)
                                    .icon_size(IconSize::Small)
                                    .disabled(editor.is_none())
                                    .tooltip(Tooltip::text(excerpt_tooltip))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.toggle_showing_all_diff_lines(cx);
                                    })),
                            )
                            .child(
                                IconButton::new("git-graph-previous-diff-hunk", IconName::ArrowUp)
                                    .icon_size(IconSize::Small)
                                    .disabled(editor.is_none())
                                    .tooltip(Tooltip::text("Go to Previous Hunk"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if let Some(commit_view) =
                                            this.selected_commit_view.as_ref()
                                        {
                                            commit_view.update(cx, |commit_view, cx| {
                                                commit_view.go_to_previous_hunk(window, cx);
                                            });
                                        }
                                    })),
                            )
                            .child(
                                IconButton::new("git-graph-next-diff-hunk", IconName::ArrowDown)
                                    .icon_size(IconSize::Small)
                                    .disabled(editor.is_none())
                                    .tooltip(Tooltip::text("Go to Next Hunk"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if let Some(commit_view) =
                                            this.selected_commit_view.as_ref()
                                        {
                                            commit_view.update(cx, |commit_view, cx| {
                                                commit_view.go_to_next_hunk(window, cx);
                                            });
                                        }
                                    })),
                            )
                            .child(
                                IconButton::new(
                                    "git-graph-toggle-diff-soft-wrap",
                                    IconName::TextWrap,
                                )
                                .icon_size(IconSize::Small)
                                .disabled(editor.is_none())
                                .tooltip(Tooltip::text("Toggle Soft Wrap"))
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        if let Some(commit_view) =
                                            this.selected_commit_view.as_ref()
                                        {
                                            commit_view.update(cx, |commit_view, cx| {
                                                commit_view.toggle_soft_wrap(window, cx);
                                            });
                                        }
                                    },
                                )),
                            )
                            .children(editor.clone().map(editor::DiffStyleControls::new))
                            .child(
                                IconButton::new("open-commit-in-tab", IconName::ArrowUpRight)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Open Commit in Tab"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_selected_commit_view(window, cx);
                                    })),
                            ),
                    ),
            )
            .child(div().flex_1().min_h_0().overflow_hidden().map(|this| {
                if let Some(editor) = editor {
                    this.child(editor)
                } else if let Some(error) = error {
                    this.child(
                        h_flex()
                            .size_full()
                            .justify_center()
                            .child(Label::new(error).color(Color::Error)),
                    )
                } else if has_no_changes {
                    this.child(
                        h_flex()
                            .size_full()
                            .justify_center()
                            .child(Label::new("No changes").color(Color::Muted)),
                    )
                } else {
                    this.child(
                        h_flex()
                            .size_full()
                            .gap_1()
                            .justify_center()
                            .child(Label::new("Loading changes…").color(Color::Muted))
                            .child(self.render_loading_spinner(cx)),
                    )
                }
            }))
            .into_any_element()
    }

    fn render_commit_detail_content(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .size_full()
            .min_h_0()
            .on_drag_move::<DraggedDetailContentSplitHandle>(cx.listener(
                |this, event, window, cx| {
                    this.detail_content_split_state.update(cx, |state, cx| {
                        state.on_drag_move(event, window, cx);
                    });
                },
            ))
            .on_drop::<DraggedDetailContentSplitHandle>(cx.listener(|this, _event, _window, cx| {
                this.detail_content_split_state.update(cx, |state, _cx| {
                    state.commit_ratio();
                });
                cx.emit(ItemEvent::Edit);
                cx.notify();
            }))
            .child(self.render_commit_detail_panel(window, cx))
            .child(self.render_detail_content_resize_handle(cx))
            .child(self.render_commit_diff_panel(cx))
            .into_any_element()
    }

    fn render_commit_message(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(DetailPanelCommitMessage {
            message,
            scroll_handle,
            ..
        }) = self.selected_commit_message.as_ref()
        else {
            return Empty.into_any_element();
        };

        let message_style = editor::hover_markdown_style(window, cx);
        let rem_size = window.rem_size();
        let line_height = message_style
            .base_text_style
            .line_height_in_pixels(rem_size);

        div()
            // Using grid over flexbox because the structure of this side
            // panel prvents taffy from calculating a concrete width correctly,
            // which causes problems with text reflow when using flexbox.
            // grid, on the other hand, doesn't appear to give taffy the same
            // problems.
            .w_full()
            .py_2()
            .pl_2()
            .grid()
            .grid_cols(1)
            .gap_1()
            .child(
                div()
                    .relative()
                    .w_full()
                    .child(
                        div()
                            .id("commit-message")
                            .text_sm()
                            .w_full()
                            .max_h(line_height * 12.)
                            .overflow_y_scroll()
                            .track_scroll(scroll_handle)
                            .child(MarkdownElement::new(message.clone(), message_style)),
                    )
                    .vertical_scrollbar_for(scroll_handle, window, cx),
            )
            .into_any_element()
    }
}

impl Render for GitGraphNext {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // This happens when we changed branches, we should refresh our search as well
        if let QueryState::Pending(query) = &mut self.search_state.state {
            let query = std::mem::take(query);
            self.search_state.state = QueryState::Empty;
            self.search(query, cx);
        }
        self.initialize_pending_commit_view(window, cx);
        let (commit_count, is_loading) = self.commit_count_and_loading_state(cx);

        let error = self.get_repository(cx).and_then(|repo| {
            repo.read(cx)
                .get_graph_data(self.log_source.clone(), self.log_order)
                .and_then(|data| data.error.clone())
        });

        let content = if commit_count == 0 {
            let message = if let Some(error) = &error {
                format!("Error loading: {}", error)
            } else if is_loading {
                "Loading".to_string()
            } else {
                "No commits found".to_string()
            };
            let label = Label::new(message)
                .color(Color::Muted)
                .size(LabelSize::Large);

            h_flex()
                .size_full()
                .gap_1()
                .justify_center()
                .child(label)
                .when(is_loading && error.is_none(), |this| {
                    this.child(self.render_loading_spinner(cx))
                })
        } else {
            let is_path_history = matches!(self.log_source, LogSource::Path(_));
            let header_resize_info =
                HeaderResizeInfo::from_redistributable(&self.column_widths, cx);

            let column_filter = self.column_visibility.clone();

            // The graph column (index 0) only exists in the non-path-history layout and is
            // rendered as a separate canvas outside the table.
            let graph_visible =
                is_path_history || !column_filter.get(0usize).copied().unwrap_or(false);

            let table_offset = if is_path_history { 0 } else { 1 };
            let table_filter = column_filter
                .as_slice()
                .get(table_offset..table_offset + TABLE_COLUMN_COUNT)
                .map(|slice| TableRow::from_vec(slice.to_vec(), TABLE_COLUMN_COUNT))
                .unwrap_or_else(|| TableRow::from_element(false, TABLE_COLUMN_COUNT));
            let header_widths = redistribute_hidden_widths(
                &self.column_widths.read(cx).widths_to_render(),
                Some(&column_filter),
            );
            let header_context = TableRenderContext::for_column_widths(Some(header_widths), true)
                .with_column_filter(Some(column_filter));

            let [
                graph_fraction,
                description_fraction,
                date_fraction,
                author_fraction,
                commit_fraction,
            ] = self.preview_column_fractions(window, cx);
            let table_fraction =
                description_fraction + date_fraction + author_fraction + commit_fraction;
            let table_width_config = self.table_column_width_config(window, cx);

            let table_collapsed = table_fraction <= f32::EPSILON;
            let graph_content_width = self.graph_canvas_content_width();

            let has_detail = self.selected_entry_idx.is_some();
            let top_ratio = self.graph_detail_split_state.read(cx).visible_top_ratio();
            let bottom_ratio = self.graph_detail_split_state.read(cx).bottom_ratio();

            v_flex()
                .size_full()
                .on_drag_move::<DraggedGraphDetailSplitHandle>(cx.listener(
                    |this, event, window, cx| {
                        this.graph_detail_split_state.update(cx, |state, cx| {
                            state.on_drag_move(event, window, cx);
                        });
                    },
                ))
                .on_drop::<DraggedGraphDetailSplitHandle>(cx.listener(
                    |this, _event, _window, cx| {
                        this.graph_detail_split_state.update(cx, |state, _cx| {
                            state.commit_ratio();
                        });
                        cx.emit(ItemEvent::Edit);
                        cx.notify();
                    },
                ))
                .child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .min_h_0()
                        .when(has_detail, |this| {
                            this.flex_basis(DefiniteLength::Fraction(top_ratio))
                        })
                        .when(!has_detail, |this| this.flex_1())
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                        this.deploy_header_context_menu(event.position, window, cx);
                                        cx.stop_propagation();
                                    }),
                                )
                                .child(render_table_header(
                                    if !is_path_history {
                                        TableRow::from_vec(
                                            vec![
                                                Label::new("Graph")
                                                    .color(Color::Muted)
                                                    .truncate()
                                                    .into_any_element(),
                                                Label::new("Description")
                                                    .color(Color::Muted)
                                                    .into_any_element(),
                                                Label::new("Date")
                                                    .color(Color::Muted)
                                                    .into_any_element(),
                                                Label::new("Author")
                                                    .color(Color::Muted)
                                                    .into_any_element(),
                                                Label::new("Commit")
                                                    .color(Color::Muted)
                                                    .into_any_element(),
                                            ],
                                            5,
                                        )
                                    } else {
                                        TableRow::from_vec(
                                            vec![
                                                Label::new("Description")
                                                    .color(Color::Muted)
                                                    .into_any_element(),
                                                Label::new("Date")
                                                    .color(Color::Muted)
                                                    .into_any_element(),
                                                Label::new("Author")
                                                    .color(Color::Muted)
                                                    .into_any_element(),
                                                Label::new("Commit")
                                                    .color(Color::Muted)
                                                    .into_any_element(),
                                            ],
                                            4,
                                        )
                                    },
                                    header_context,
                                    Some(header_resize_info),
                                    Some(self.column_widths.entity_id()),
                                    cx,
                                )),
                        )
                        .child({
                            let row_height = Self::row_height(window, cx);
                            let selected_entry_idx = self.selected_entry_idx;
                            let hovered_entry_idx = self.hovered_entry_idx;
                            let context_menu_target_index = self
                                .context_menu
                                .as_ref()
                                .and_then(|menu| menu.target_entry_index);
                            let weak_self = cx.weak_entity();
                            let focus_handle = self.focus_handle.clone();
                            let table_focus_handle =
                                self.table_interaction_state.read(cx).focus_handle.clone();

                            let graph_canvas = div()
                                .id("graph-canvas")
                                .size_full()
                                .overflow_hidden()
                                .cursor_pointer()
                                .child(
                                    div()
                                        .size_full()
                                        .child(self.render_graph_canvas(window, cx)),
                                )
                                .on_scroll_wheel(cx.listener(Self::handle_graph_scroll))
                                .on_mouse_move(cx.listener(Self::handle_graph_mouse_move))
                                .on_click(cx.listener(Self::handle_graph_click))
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(Self::handle_graph_secondary_mouse_down),
                                )
                                .on_hover(cx.listener(|this, &is_hovered: &bool, _, cx| {
                                    if !is_hovered && this.hovered_entry_idx.is_some() {
                                        this.hovered_entry_idx = None;
                                        cx.notify();
                                    }
                                }));

                            let commits_table = Table::new(4)
                                .interactable(&self.table_interaction_state)
                                .hide_row_borders()
                                .hide_row_hover()
                                .width_config(table_width_config)
                                .column_filter(table_filter)
                                .map_row(move |(index, row), window, cx| {
                                    let is_selected = selected_entry_idx == Some(index);
                                    let is_hovered = hovered_entry_idx == Some(index);
                                    let is_context_menu_target =
                                        context_menu_target_index == Some(index);
                                    let table_focus_handle = table_focus_handle.clone();
                                    let is_focused = focus_handle.is_focused(window)
                                        || table_focus_handle.is_focused(window);
                                    let weak = weak_self.clone();
                                    let weak_for_hover = weak.clone();
                                    let weak_for_context_menu = weak.clone();

                                    let hover_bg = cx.theme().colors().element_hover.opacity(0.6);
                                    let selected_bg = if is_focused {
                                        cx.theme().colors().element_selected
                                    } else {
                                        cx.theme().colors().element_hover
                                    };

                                    row.h(row_height)
                                        .cursor_pointer()
                                        .when(is_selected || is_context_menu_target, |row| {
                                            row.bg(selected_bg)
                                        })
                                        .when(
                                            is_hovered && !is_selected && !is_context_menu_target,
                                            |row| row.bg(hover_bg),
                                        )
                                        .on_hover(move |&is_hovered, _, cx| {
                                            weak_for_hover
                                                .update(cx, |this, cx| {
                                                    if is_hovered {
                                                        if this.hovered_entry_idx != Some(index) {
                                                            this.hovered_entry_idx = Some(index);
                                                            cx.notify();
                                                        }
                                                    } else if this.hovered_entry_idx == Some(index)
                                                    {
                                                        this.hovered_entry_idx = None;
                                                        cx.notify();
                                                    }
                                                })
                                                .ok();
                                        })
                                        .on_click(move |event, window, cx| {
                                            weak.update(cx, |this, cx| {
                                                this.handle_entry_click(
                                                    index,
                                                    event,
                                                    ScrollStrategy::Center,
                                                    Some(&table_focus_handle),
                                                    window,
                                                    cx,
                                                );
                                            })
                                            .ok();
                                        })
                                        .on_mouse_down(
                                            MouseButton::Right,
                                            move |event: &MouseDownEvent, window, cx| {
                                                weak_for_context_menu
                                                    .update(cx, |this, cx| {
                                                        this.handle_entry_secondary_mouse_down(
                                                            index, event, window, cx,
                                                        );
                                                    })
                                                    .ok();
                                            },
                                        )
                                        .into_any_element()
                                })
                                .uniform_list(
                                    "git-graph-commits",
                                    commit_count,
                                    cx.processor(Self::render_table_rows),
                                );

                            bind_redistributable_columns(
                                div()
                                    .relative()
                                    .flex_1()
                                    .w_full()
                                    .overflow_hidden()
                                    .child(
                                        h_flex()
                                            .size_full()
                                            .when(!is_path_history && graph_visible, |this| {
                                                this.child(
                                                    div()
                                                        .map(|this| {
                                                            if table_collapsed {
                                                                this.w(graph_content_width)
                                                            } else {
                                                                this.w(DefiniteLength::Fraction(
                                                                    graph_fraction,
                                                                ))
                                                            }
                                                        })
                                                        .h_full()
                                                        .min_w_0()
                                                        .overflow_hidden()
                                                        .child(graph_canvas),
                                                )
                                            })
                                            .child(
                                                div()
                                                    .tab_index(2)
                                                    .tab_group()
                                                    .tab_stop(false)
                                                    .map(|this| {
                                                        if table_collapsed {
                                                            this.flex_1()
                                                        } else {
                                                            this.w(DefiniteLength::Fraction(
                                                                table_fraction,
                                                            ))
                                                        }
                                                    })
                                                    .h_full()
                                                    .min_w_0()
                                                    .child(commits_table),
                                            ),
                                    )
                                    .child(render_redistributable_columns_resize_handles(
                                        &self.column_widths,
                                        Some(&self.column_visibility),
                                        window,
                                        cx,
                                    )),
                                self.column_widths.clone(),
                                Some(self.column_visibility.clone()),
                            )
                        }),
                )
                .when(has_detail, |this| {
                    this.child(self.render_graph_detail_resize_handle(cx))
                        .child(
                            div()
                                .w_full()
                                .min_h_0()
                                .flex_basis(DefiniteLength::Fraction(bottom_ratio))
                                .child(self.render_commit_detail_content(window, cx)),
                        )
                })
        };

        div()
            .key_context("GitGraph")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .on_action(cx.listener(|this, _: &OpenCommitView, window, cx| {
                this.open_selected_commit_view(window, cx);
            }))
            .on_action(cx.listener(Self::copy_selected_commit_sha))
            .on_action(cx.listener(Self::copy_selected_commit_tag))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.search_state
                    .editor
                    .update(cx, |editor, cx| editor.focus_handle(cx).focus(window, cx));
                this.activate_search_editor_if_focused(window, cx);
            }))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::scroll_up))
            .on_action(cx.listener(Self::scroll_down))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::toggle_changed_files_view))
            .on_action(cx.listener(Self::focus_next_tab_stop))
            .on_action(cx.listener(Self::focus_previous_tab_stop))
            .on_action(
                cx.listener(|this, _: &crate::git_graph::FocusNextTabStop, window, cx| {
                    window.focus_next(cx);
                    this.activate_search_editor_if_focused(window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_action(cx.listener(
                |this, _: &crate::git_graph::FocusPreviousTabStop, window, cx| {
                    window.focus_prev(cx);
                    this.activate_search_editor_if_focused(window, cx);
                    cx.stop_propagation();
                    cx.notify();
                },
            ))
            .on_action(cx.listener(|this, _: &SelectNextMatch, _window, cx| {
                this.select_next_match(cx);
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousMatch, _window, cx| {
                this.select_previous_match(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleCaseSensitive, _window, cx| {
                this.search_state.case_sensitive = !this.search_state.case_sensitive;
                this.search_state.state.next_state();
                cx.emit(ItemEvent::Edit);
                cx.notify();
            }))
            .child(
                v_flex()
                    .size_full()
                    .child(self.render_search_bar(cx))
                    .child(div().flex_1().child(content)),
            )
            .children(self.context_menu.as_ref().map(|context_menu| {
                deferred(
                    anchored()
                        .position(context_menu.position)
                        .anchor(Anchor::TopLeft)
                        .child(context_menu.menu.clone()),
                )
                .with_priority(1)
            }))
            .on_action(cx.listener(|this, _: &buffer_search::Deploy, window, cx| {
                let diff_is_focused =
                    this.selected_commit_view
                        .as_ref()
                        .is_some_and(|commit_view| {
                            commit_view
                                .read(cx)
                                .editor()
                                .focus_handle(cx)
                                .contains_focused(window, cx)
                        });
                if !diff_is_focused {
                    window.dispatch_action(Box::new(FocusSearch), cx);
                    cx.stop_propagation();
                }
            }))
    }
}

impl EventEmitter<ItemEvent> for GitGraphNext {}

impl Focusable for GitGraphNext {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search_state.editor.read(cx).focus_handle(cx)
    }
}

impl Item for GitGraphNext {
    type Event = ItemEvent;

    fn act_as_type<'a>(
        &'a self,
        type_id: std::any::TypeId,
        self_handle: &'a Entity<Self>,
        cx: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == std::any::TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == std::any::TypeId::of::<editor::SplittableEditor>() {
            self.selected_commit_view
                .as_ref()
                .map(|commit_view| commit_view.read(cx).editor().into())
        } else if type_id == std::any::TypeId::of::<Editor>() {
            self.selected_commit_view.as_ref().map(|commit_view| {
                commit_view
                    .read(cx)
                    .editor()
                    .read(cx)
                    .rhs_editor()
                    .clone()
                    .into()
            })
        } else {
            None
        }
    }

    fn as_searchable(
        &self,
        _: &Entity<Self>,
        cx: &App,
    ) -> Option<Box<dyn workspace::searchable::SearchableItemHandle>> {
        self.selected_commit_view.as_ref().map(|commit_view| {
            Box::new(commit_view.read(cx).editor())
                as Box<dyn workspace::searchable::SearchableItemHandle>
        })
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::GitGraph))
    }

    fn tab_tooltip_content(&self, cx: &App) -> Option<TabTooltipContent> {
        let repo_name = self.get_repository(cx).and_then(|repo| {
            repo.read(cx)
                .work_directory_abs_path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        });
        let path_history_path = match &self.log_source {
            LogSource::Path(path) => Some(path.as_unix_str().to_string()),
            _ => None,
        };

        Some(TabTooltipContent::Custom(Box::new(Tooltip::element({
            move |_, _| {
                v_flex()
                    .child(Label::new(if path_history_path.is_some() {
                        "Path History"
                    } else {
                        "Git Graph Next"
                    }))
                    .when_some(path_history_path.clone(), |this, path| {
                        this.child(Label::new(path).color(Color::Muted).size(LabelSize::Small))
                    })
                    .when_some(repo_name.clone(), |this, name| {
                        this.child(Label::new(name).color(Color::Muted).size(LabelSize::Small))
                    })
                    .into_any_element()
            }
        }))))
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        if let LogSource::Path(path) = &self.log_source {
            return path
                .as_ref()
                .file_name()
                .map(|name| SharedString::from(name.to_string()))
                .unwrap_or_else(|| SharedString::from(path.as_unix_str().to_string()));
        }

        self.get_repository(cx)
            .and_then(|repo| {
                repo.read(cx)
                    .work_directory_abs_path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
            })
            .map_or_else(|| "Git Graph Next".into(), |name| SharedString::from(name))
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn split_ratios_restore_and_clamp() {
        let mut graph_detail = super::GraphDetailSplitState::new();
        assert_eq!(graph_detail.visible_top_ratio(), 0.25);
        assert_eq!(graph_detail.bottom_ratio(), 0.75);

        graph_detail.restore_ratio(0.95);
        assert_eq!(graph_detail.visible_top_ratio(), 0.8);
        graph_detail.on_double_click();
        assert_eq!(graph_detail.visible_top_ratio(), 0.25);

        let mut detail_content = super::DetailContentSplitState::new();
        assert_eq!(detail_content.visible_left_ratio(), 0.2);
        assert_eq!(detail_content.right_ratio(), 0.8);

        detail_content.restore_ratio(0.05);
        assert_eq!(detail_content.visible_left_ratio(), 0.15);
        detail_content.on_double_click();
        assert_eq!(detail_content.visible_left_ratio(), 0.2);
    }
}
