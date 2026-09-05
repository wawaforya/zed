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
│ Author, time, refs  │             Diff              │
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

Opening from the Title Bar initially filters to the current branch, or HEAD when detached. The Graph column starts hidden. Branch selection filters history without checking out a branch. Clearing the branch filter shows all branches and restores the Graph column; File History retains its path filter and has no Graph column. Empty repositories open without a branch filter.

Default split ratios:

- graph height: 25%
- detail height: 75%
- commit information width: 20%
- diff width: 80%

The graph ratio is clamped to 20%–80%. The information ratio is clamped to 15%–60%. Double-clicking either divider restores its default. With no selected commit, the graph occupies all available space.

## Selection and loading

Selecting a commit:

1. Selects and scrolls to the commit row.
2. Loads commit metadata and the commit diff.
3. Uses the same loaded diff for file statistics, the changed-file list, and the embedded diff.
4. Selects the first changed file in ordinary history. In File History, selects the history target on every commit switch and scrolls its row into view, including in the expanded directory tree.
5. Verifies the selected repository, log source, commit, and file before applying asynchronous results.

When the original File History path is absent from a commit's diff, the repository resolves its historical path with `git log --follow --name-status -z`. Resolved paths are cached in the view until history is invalidated. This also works through the remote commit-diff request. If the target cannot be found, the diff pane prompts the user to select a file instead of silently selecting an unrelated change.

Single-clicking a changed file displays it in the embedded diff. Double-clicking it opens the filtered official Commit View. Double-clicking a commit opens the complete official Commit View.

Click a commit to choose a base, then Ctrl-click (Cmd-click on macOS) or Shift-click another commit to choose a target. Both endpoints are highlighted and labeled Base/Target. Right-click either endpoint and choose Compare to display the net changes between their snapshots, equivalent to `git diff BASE TARGET`. This excludes changes introduced by the base commit itself and does not use the merge base. Ordinary clicking returns to single-commit mode. During comparison, single-commit opening buttons are disabled and file double-clicks stay in the embedded diff. Remote comparison requires a host that supports the base-commit request field; older hosts report an error rather than displaying the target's single-commit diff.

## Focus and text search

Opening the tab initially focuses the graph, not Search commits. Reactivating it restores the last focused graph/list or diff region, including the last focused side of a Split diff. Focus is remembered when entering a region, before TabSwitcher takes focus. Hidden or unavailable diff content falls back to the graph. Menus are not remembered as focus targets, and leaving Search commits restores the graph rather than the text field. The commit search field is focused explicitly; Escape returns to the graph without clearing the selected commit.

The embedded diff keeps a stable `SplittableEditor` for the lifetime of the graph. Switching files or commits replaces its contents, not the searchable entity. Diff search actions propagate to the native pane search bar; commit search actions remain scoped to the graph/search field. Unified search includes deleted text, while Split search uses the focused side. Search covers the current excerpts; use Show All Lines to search the complete file. Pending content loads are cancelled and generation-checked before applying results.

The diff file header embeds the owning pane's native `BufferSearchBar`; it does not create a second search instance. Use the header's search button, Ctrl+F in the diff, or Vim `/` and `?`. While searching, the single-row header keeps the file path on the left and all file actions on the right, with a fixed 480px search control centered between them. The path is no longer reduced to a 6rem slot. Very narrow headers scroll horizontally rather than overlapping the path, search, and actions. File folding is available from the more menu and remains separate from Show All Lines. Closing search restores the normal file controls and focuses the diff.

Only Git Graph Next opts into embedded search and suppresses its pane toolbar. Search registrations and keyboard context remain on the pane so native search and Vim continue to use the same instance. Graph confirmation propagates when the diff or its search field is focused, allowing Enter to submit native/Vim search instead of opening the selected commit. Switching to an ordinary editor restores its standard toolbar; moving the graph tab to another pane binds the header to that pane's search bar.

## Show All Lines

Show Changes Only sets the current file's excerpts to its diff hunk ranges with the configured context line count.

Show All Lines replaces those excerpts with the complete file range and zero additional context lines. Switching modes updates the existing diff data and does not reload the Git commit diff.

The vertical scrollbar remains available in both modes. Git diff scrollbar markers are enabled in Show All Lines, where they represent positions in the complete file, and disabled in Show Changes Only, where excerpts compress file coordinates. The diff gutter remains visible in both modes.

Git Graph Next does not display a minimap.

## Isolation

The official Git Graph, Git Panel, Commit View, Editor, and SplittableEditor implementations remain unchanged. Git Graph Next has a distinct item type and is reused only when its repository and log source match. Official and Next graph tabs can coexist.

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
