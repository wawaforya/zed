# Git Graph Next

## Goals

Git Graph Next is an isolated Git graph implementation with this layout:

```text
┌────────────────────────────────────────────────────┐
│ Search / Repository / Branch Filter                │
├────────────────────────────────────────────────────┤
│                                                    │
│                    Git Graph                       │
│                                                    │
├─────────────── draggable horizontal divider ────────┤
│                    │                               │
│ Commit information │                               │
│ Author, time       │             Diff              │
│ SHA and message     │                               │
│ Changed files       │                               │
│                    │                               │
└──────── draggable vertical divider ────────────────┘
```

The Title Bar graph button and File History actions open Git Graph Next. The Git Panel Open Git Graph button and other existing Git Graph entry points continue to open the official Git Graph.

## Entry points

The actions are kept separate:

```text
Title Bar graph button
  → zed_actions::git_graph_next::Open
  → GitGraphNext

File History actions
  → LogSource::Path
  → GitGraphNext

Git Panel Open Git Graph button
  → crate::git_graph::Open
  → GitGraph
```

File History from the Editor, Git Panel, Project Panel, and workspace paths opens Git Graph Next. Show in Git Graph, the Git Panel Open Git Graph button, and OpenAtCommit continue to use the official Git Graph.

## Title Bar button

The Title Bar Git controls contain the worktree picker, branch picker, and Git Graph Next button. The button uses the existing Title Bar button style and displays the Git Graph icon followed by the current HEAD's eight-character SHA and commit subject. Long subjects are truncated within a maximum width of 28 rems. The tooltip includes the full SHA and commit message. Repositories without a loaded HEAD display `No commits`.

The button is available whenever the current project has an active Git repository and Git integration is enabled. Its visibility does not depend on the project-name, worktree-name, or branch-name settings. The controls are arranged as follows:

```text
worktree / branch / [Graph icon] short SHA + commit subject
```

Separators are only rendered between controls that are present. If both the worktree and branch controls are hidden, the Graph button is displayed without a separator. Non-Git projects and projects with Git integration disabled do not display it.

The button uses the element ID `git_graph_next_trigger`, shows the `Open Git Graph Next` action tooltip, and dispatches `zed_actions::git_graph_next::Open`.

## Code structure

### `git_graph_next.rs`

Owns:

- `GitGraphNext` workspace item
- graph loading and lane calculation
- commit table, search, selection, and context menus
- commit metadata and changed-file list
- graph/detail and detail/diff split states
- `open_or_reuse_graph_next`
- Git Graph Next action registration

### `git_graph_next_diff.rs`

Owns the embedded read-only commit diff:

- `MultiBuffer` and `SplittableEditor`
- commit-file buffers and `BufferDiff` instances
- selected-file switching
- Show Changes Only and Show All Lines
- vertical scrollbar and diff markers
- previous/next hunk, soft wrap, and split/unified controls
- loading, error, empty, deleted-file, and binary-file states

Opening a commit or file in a separate tab continues to call the existing `CommitView::open` API. `CommitView` itself is not embedded or modified.

## Layout behavior

Opening from the Title Bar initially selects `Current`, which follows HEAD across branch checkouts and also works when detached. `Current · <branch>` and `All Branches` are pinned above concrete branches in the filter picker. Selecting a concrete branch fixes the history to that branch without checking it out. The Graph column starts hidden. Selecting `All Branches` in the picker shows all branches and restores the Graph column; there is no separate clear button. File History retains its path filter and has no Graph column. Empty repositories open without a branch filter.

The `All Authors` button opens a text filter for the code author's name or email (not the committer). Enter applies a case-insensitive literal substring match; punctuation is not interpreted as a regular expression. Choose `All Authors` in the popup to remove the author condition, or Escape to leave the current filter unchanged. Author and branch filters are independent and intersect: switching branches retains the author condition, and removing it retains the branch. Filtering covers the complete source history, including File History, and commit message/SHA searches use that same filtered source. Changing the author clears commit selection and the old diff. The author condition is local to the current graph tab and is not persisted to disk. Remote filtering requires a host that understands the author-filter log source; older hosts reject it rather than returning unfiltered history. File History keyword search retains Git's existing `--follow`/`--grep` limitation: commits before a rename can be omitted when the rename commit does not match the keyword, even without an author filter.

The `Stashes` button shows the stash count for the graph's repository and opens its stash picker. Enter views the selected stash in the official Commit View; Apply, Pop, and Drop remain explicit actions. Other stash picker entry points retain their existing default behavior.

Commit details display the author as `Name (email)`, with the email clickable to copy and its full value available in a tooltip. The local commit date and time (including seconds) appear below the author. Branch/tag chips are shown only in the commit list, not repeated in the details, and the details omit hosting-provider links such as `View on GitHub`. Message is collapsed by default and preserves its disclosure state when selecting another commit. Its header displays `Message (short SHA)`; clicking the SHA copies the full commit hash, which is also available in a tooltip, even while the message is collapsed.

Default split ratios:

- graph height: 25%
- detail height: 75%
- commit information width: 20%
- diff width: 80%

The graph ratio is clamped to 20%–80%. The information ratio is clamped to 15%–60%. Double-clicking either divider restores its default. The displayed graph/detail height ratio is remembered as it changes during dragging, without depending on a drop event reaching the parent container. It is shared across Git Graph Next tabs and windows for the current Zed session; newly opened tabs reuse it without changing already-open tabs. Double-clicking the horizontal divider also resets the remembered ratio. No ratio is written to disk, so restarting Zed restores the default. With no selected commit, the graph occupies all available space.

## Selection and loading

Selecting a commit:

1. Selects and scrolls to the commit row.
2. Loads commit metadata and the commit diff.
3. Uses the same loaded diff for file statistics, the changed-file list, and the embedded diff.
4. Selects the first changed file in ordinary history. In File History, selects the history target on every commit switch and scrolls its row into view, including in the expanded directory tree.
5. Verifies the selected repository, log source, commit, and file before applying asynchronous results.

When the original File History path is absent from a commit's diff, the repository resolves its historical path with `git log --follow --name-status -z`. Resolved paths are cached in the view until history is invalidated. This also works through the remote commit-diff request. If the target cannot be found, the diff pane prompts the user to select a file instead of silently selecting an unrelated change.

Single-clicking a changed file displays it in the embedded diff. Double-clicking it opens the filtered official Commit View. Double-clicking a commit opens the complete official Commit View.

Clicking a directory toggles its expanded state, including sticky directory headers at the top of the changed-file tree. Collapsing a directory preserves the selected file and displayed diff. Scrolling over sticky headers continues to scroll the tree.

Ordinary clicking selects only that commit. Ctrl-click (Cmd-click on macOS) toggles an individual commit. Shift-click selects the inclusive range in the current displayed list order, not Git ancestry. The range anchor is the most recent ordinary or Ctrl/Cmd click; repeated Shift-clicks keep that anchor so the range can grow or shrink. Right-clicking a selected row preserves the selection; right-clicking an unselected row selects only that commit. Ref-chip menus remain ref-specific. Selection uses commit SHAs and is cleared when the graph is invalidated or its repository/filter changes.

Exactly two selected commits enable Compare, regardless of whether they were selected with Ctrl/Cmd or Shift. The first selected commit (the anchor for a Shift range) is Base and the other is Target. Both endpoints are highlighted and labeled Base/Target. The context menu displays both comparison directions. Comparing shows the net changes between snapshots, equivalent to `git diff BASE TARGET`; it excludes changes introduced by Base itself and does not use the merge base. Three or more selected commits offer batch operations instead of an ambiguous multi-commit diff.

Multi-selection menus can copy full SHAs, titles, short SHAs with titles, full messages, or a Markdown list. Output always follows the displayed list from top to bottom, not click order. Full messages are separated and identified by their full SHA. Unloaded metadata is fetched asynchronously; the clipboard is updated only when every requested commit is available, with failures reported rather than silently omitting commits. A newer copy request or opening another commit context menu cancels an unfinished copy, so a delayed batch copy cannot overwrite a subsequent single-commit copy. The Copy Commit SHA action also copies all selected SHAs.

The toolbar shows the multi-selection count. Selecting multiple commits does not automatically compare them: details identify the commit or explicit comparison currently displayed, independently of the selection. Ordinary clicking returns to single-commit mode. During comparison, single-commit opening buttons are disabled and file double-clicks stay in the embedded diff. Remote comparison requires a host that supports the base-commit request field; older hosts report an error rather than displaying the target's single-commit diff.

## Focus and text search

Opening the tab initially focuses the graph, not Search commits. Reactivating it restores the last focused graph/list or diff region, including the last focused side of a Split diff. Focus is remembered when entering a region, before TabSwitcher takes focus. Hidden or unavailable diff content falls back to the graph. Menus are not remembered as focus targets, and leaving Search commits restores the graph rather than the text field. The commit search field is focused explicitly; Escape returns to the graph without clearing the selected commit.

The embedded diff keeps a stable `SplittableEditor` for the lifetime of the graph. Switching files or commits replaces its contents, not the searchable entity. Diff search actions propagate to the native pane search bar; commit search actions remain scoped to the graph/search field. Unified search includes deleted text, while Split search uses the focused side. Search covers the current excerpts; use Show All Lines to search the complete file. Pending content loads are cancelled and generation-checked before applying results.

The diff file header embeds the owning pane's native `BufferSearchBar`; it does not create a second search instance. Use the header's search button, Ctrl+F in the diff, or Vim `/` and `?`. While searching, the single-row header keeps the file path on the left and all file actions on the right, with a fixed 480px search control centered between them. The path is no longer reduced to a 6rem slot. Very narrow headers scroll horizontally rather than overlapping the path, search, and actions. File folding is available from the more menu and remains separate from Show All Lines. Closing search restores the normal file controls and focuses the diff.

Only Git Graph Next opts into embedded search and suppresses its pane toolbar. Search registrations and keyboard context remain on the pane so native search and Vim continue to use the same instance. Graph confirmation propagates when the diff or its search field is focused, allowing Enter to submit native/Vim search instead of opening the selected commit. Switching to an ordinary editor restores its standard toolbar; moving the graph tab to another pane binds the header to that pane's search bar.

## Show All Lines

Show Changes Only sets the current file's excerpts to its diff hunk ranges with the configured context line count.

Show All Lines replaces those excerpts with the complete file range and zero additional context lines. Returning to Show Changes Only removes the full-file excerpts before rebuilding hunk excerpts, because incremental excerpt updates preserve overlapping context. Switching modes keeps the existing editor, buffers, and diff data and does not reload the Git commit diff.

The vertical scrollbar remains available in both modes. Git diff scrollbar markers are enabled in Show All Lines, where they represent positions in the complete file, and disabled in Show Changes Only, where excerpts compress file coordinates. The diff gutter remains visible in both modes.

Git Graph Next does not display a minimap.

## Isolation

The behavior of the official Git Graph, Git Panel, Commit View, Editor, and SplittableEditor remains unchanged. Author filtering is opt-in through a wrapped log source; shared repository caches and remote requests distinguish it from unfiltered history. Git Graph Next has a distinct item type and is reused only when its repository and log source match. Official and Next graph tabs can coexist.

The first version does not restore Git Graph Next tabs or split ratios after restarting the workspace. Persistence can be added later with a distinct serialized item kind and storage.

## Existing-file changes

- `crates/zed_actions/src/lib.rs`: add `git_graph_next::Open`
- `crates/git_ui/src/git_ui.rs`: declare and initialize the new modules and route the shared File History opener to Next
- `crates/git_ui/src/git_graph.rs`: route contextual File History actions to Next while preserving official Graph actions
- `crates/title_bar/src/title_bar.rs`: render the Git Graph Next button whenever Git controls are available and dispatch the Next action

No default keymap changes are required. `git_ui` depends on `search` to embed the native search bar; workspace items opt into this presentation via `Item::embeds_buffer_search`.

## Acceptance criteria

1. The Title Bar graph button opens only `GitGraphNext`.
2. File History actions open `GitGraphNext` with `LogSource::Path`.
3. The Git Panel Open Git Graph button opens only the official `GitGraph`.
4. Both graph types can be open simultaneously.
5. Reopening Next for the same repository and log source reuses its tab.
6. The graph is above the detail area; commit information is left of the diff.
7. Changed-file selection updates the embedded diff.
8. Show All Lines and Show Changes Only switch correctly without reloading the commit diff.
9. Show All Lines displays Git diff markers in the vertical scrollbar.
10. Stale asynchronous results cannot replace the currently selected commit or file.
11. Existing official Git Graph, Commit View, and Editor behavior remains unchanged.
