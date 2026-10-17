# Changeset review and file tree

```sh
hx --review                         # current Git/JJ working copy
hx --review /path/to/repository
hx --review before-directory after-directory
hx --review --diff-layout inline
hx --review --pager                 # strictly read-only
git diff --color=never | hx --patch # multi-file input gets the same sidebar
```

The Changes sidebar starts with all changed-file directories expanded, so nested
changes are visible immediately. You can collapse folders normally; refreshes
retain those choices and expand newly introduced changed-file directories.
`:diff-off` restores your prior filesystem sidebar state.
The expandable sidebar selects a file from the changeset. Split review
opens a read-only baseline beside the live working document: edit, undo, search,
use Tree-sitter highlighting and save normally, without launching another editor.
Nothing is automatically saved, staged or committed. Existing adjacent panes
are reused; switching files retains unsaved buffers. `:diff-get` can restore a
baseline hunk into the working file; `:diff-put` cannot modify the baseline.
Native `]c` / `[c` and `:diff-next` / `:diff-prev` navigate hunks.

Inline review edits the **real working document** with deleted baseline lines
rendered as virtual, read-only rows. Editing, undo, search, LSP operations and
saves use working-file coordinates; deleted text cannot leak into a save.
`:diff-get` restores the baseline hunk; `:diff-put` cannot edit the hidden baseline.
Layout switches retain the working buffer and its unsaved edits. Patch input
and `--pager` remain read-only in either layout; referenced patch paths are never
opened. See [file comparison](file-diff.md) for rendering and patch format details.

## Commands and navigation

| Command | Action |
| --- | --- |
| `Space v`, `:diff-review [repository]` | Review working-copy changes |
| `:jj-review [revset]`, `:jjr [revset]` | Select a native JJ revision; `@` is editable, history is read-only |
| `:diff-review left-directory right-directory` | Compare directory snapshots |
| `:diff-review-patch file.patch` | Review a patch file |
| `:diff-file-next`, `:diff-file-prev` | Select the next/previous changed file |
| `:diff-layout [inline\|side-by-side]` | Select a layout, or toggle with no argument |
| `:diff-fold [N]` | Collapse unchanged context for the current file (default 3 lines at each edge) |
| `:fold-open`, `:fold-open-all` | Expand the current fold or all folds in this file |
| `:diff-review-refresh` | Reload the catalog and clean sources, retaining unsaved edits |
| `:file-tree [directory]` | Focus the current sidebar, or browse a directory |
| `:file-tree-focus` | Toggle tree/editor focus; reveal or create the tree if needed |
| `:file-tree-toggle` (user config: `Space e`) | Toggle sidebar visibility without losing state |
| `:file-tree-close` | Close the sidebar without closing documents |
| `:diff-off` | Keep working file in the left pane and restore the previous sidebar |

While the sidebar is focused: `j`/`k` or arrows select, `h` collapses or selects
the parent, `l` expands, and Enter opens a file or toggles a directory. `g`/`G`
select first/last; Tab or Escape returns to the editor. In a JJ review, `r` opens
the [native revision selector](jj-workspace.md). In a review,
`n`/`p` select next/previous file and `v` toggles layouts. Mouse selection and
wheel scrolling are supported. In the user's Scheme config, `Space E` opens the
original explorer in the current buffer's directory.

`/` edits a live, case-insensitive name/path substring filter, including files
under collapsed folders. Ancestors remain visible as context. `Enter` accepts
the filter; `n`/`p` or `N` move through matching entries with wraparound, without
opening files. Another Enter opens the selected file. Escape cancels a filter
edit, or clears an accepted filter before leaving the tree. Backspace deletes a
grapheme; Ctrl-u clears the input and Ctrl-w deletes a word. No-match Enter does
nothing. Clearing restores unfiltered expansion, selection and scroll; refresh
retains the query and recomputes matches for the new catalog.

`H`/`L` collapse/expand all, PageUp/PageDown page through entries, Ctrl-u/Ctrl-d
move half a page outside filter input, and `?` shows a key summary. Without a
filter, review `n`/`p` retain their next/previous-file behavior; filesystem `n`/`p`
select next/previous visible files, skipping folders.

The sidebar uses the active icon set's file-type and open/closed folder glyphs
and colors. As in file pickers, `editor.icons.picker = false` disables icons.
In the user's Scheme config, `Ctrl-w e` toggles sidebar/editor focus without
changing visibility; a hidden sidebar is revealed and focused first.

Ending a review with `:diff-off` replaces the baseline in the leftmost comparison
pane with the working file and closes only the extra comparison panes. The
sidebar that preceded the review is restored, including its expansion,
selection, scroll and focus state; if there was no sidebar, it closes. Unsaved
edits and baseline buffers remain available, and nothing is saved automatically.
This works after switching the review to inline layout as well.

The Files sidebar is reusable outside reviews. It respects file-picker ignore,
hidden-file and depth settings, excludes `.git`/`.jj`, and does not descend into
symlinked directories. It reserves real layout space, rather than covering text;
on very narrow terminals it is hidden so the editor remains usable.

## Sources and boundaries

Git reviews compare HEAD with working files and include staged, unstaged,
deleted and untracked paths. Index-only/metadata changes receive a summary.
Git renames currently appear as deletion/addition entries. JJ uses its native
working-copy/parent comparison, including unsnapshotted files and rename paths.
Catalog reads use native VCS APIs, not shell commands, and respect workspace
trust. They do not stage files or change HEAD/the JJ operation history.
JJ repositories require explicit workspace trust: run `:workspace-trust`, then
`:diff-review` if opening an unfamiliar workspace.

Directory review treats the left side as baseline and the right as working files.
Missing files, symlinks, binary and special files get read-only snapshots/summaries;
symlinks and special files are never read as ordinary source files. Added text
files are editable. With file watching enabled, review catalogs and inline
snapshots refresh automatically after a 250 ms debounce. Additions/deletions,
saved patch changes, Git refs/index and JJ operation heads are included;
object stores are excluded. Explicit directory comparisons watch both selected
roots even outside a workspace, without watching the caller's whole directory.
Refresh is bounded to one automatic catalog worker with a pending follow-up;
it preserves dirty buffers, active file, focus and sidebar selection/expansion.
`:diff-review-refresh` remains a fallback when watching is disabled or unavailable.
A piped patch is
immutable; reopen with new input instead of refreshing it.

Unchanged context can be folded per file in split and inline layouts; see
[folding commands and editing behavior](file-diff.md#folding-unchanged-context).
This is the first multi-file review layer, not yet a complete Hunk/Diffview-style
VCS interface. A [native JJ revision selector](jj-workspace.md) now drives the
same sidebar and review panes. A persistent revision graph, history-changing
operations, staging and a continuous multi-file changeset view are not implemented.
Input is finite and catalogs retain source snapshots
in memory; this is not a streaming viewer for arbitrarily large changesets.
Use [JJ's external diff-tool integration](file-diff.md#interactive-jj-diff-editing-and-jjui-handoff)
to select revisions/ranges and pass complete files to Helix.

The generic Files tree implements expansion/collapse, navigation, opening files,
ignore filtering and layout reservation. It does not yet implement file creation,
rename/move/delete, filesystem-tree Git status badges or persistent filesystem-tree
auto-refresh. Those are separate from review-catalog watching and from the
toggleable per-line blame gutter.
