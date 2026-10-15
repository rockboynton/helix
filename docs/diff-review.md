# Changeset review and file tree

```sh
hx --review                         # current Git/JJ working copy
hx --review /path/to/repository
hx --review before-directory after-directory
hx --review --diff-layout inline
hx --review --pager                 # strictly read-only
git diff --color=never | hx --patch # multi-file input gets the same sidebar
```

The expandable Changes sidebar selects a file from the changeset. Split review
opens a read-only baseline beside the live working document: edit, undo, search,
use Tree-sitter highlighting and save normally, without launching another editor.
Nothing is automatically saved, staged or committed. Existing adjacent panes
are reused; switching files retains unsaved buffers. `:diff-get` can restore a
baseline hunk into the working file; `:diff-put` cannot modify the baseline.
Native `]c` / `[c` and `:diff-next` / `:diff-prev` navigate hunks.

Inline review is currently a **read-only presentation** of the latest in-memory
sources, including unsaved edits. Switch back to split view to edit. Patch input
and `--pager` remain read-only in either layout; referenced patch paths are never
opened. See [file comparison](file-diff.md) for rendering and patch format details.

## Commands and navigation

| Command | Action |
| --- | --- |
| `Space v`, `:diff-review [repository]` | Review working-copy changes |
| `:diff-review left-directory right-directory` | Compare directory snapshots |
| `:diff-review-patch file.patch` | Review a patch file |
| `:diff-file-next`, `:diff-file-prev` | Select the next/previous changed file |
| `:diff-layout [inline\|side-by-side]` | Select a layout, or toggle with no argument |
| `:diff-review-refresh` | Reload the catalog and clean sources, retaining unsaved edits |
| `Space E`, `:file-tree [directory]` | Focus the current sidebar, or browse a directory |
| `:file-tree-close` | Close the sidebar without closing documents |
| `:diff-off` | Keep working file in the left pane and restore the previous sidebar |

While the sidebar is focused: `j`/`k` or arrows select, `h` collapses or selects
the parent, `l` expands, and Enter opens a file or toggles a directory. `g`/`G`
select first/last; Tab or Escape returns to the editor. `r` refreshes. In a review,
`n`/`p` select next/previous file and `v` toggles layouts. Mouse selection and
wheel scrolling are supported. `Space e` still opens Helix's original file picker.

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

This is the first multi-file review layer, not yet a complete Hunk/Diffview-style
VCS interface. Arbitrary revisions, staging, history browsing, editable inline
deleted-text projections and continuous/folded changeset
views are not implemented. Input is finite and catalogs retain source snapshots
in memory; this is not a streaming viewer for arbitrarily large changesets.
