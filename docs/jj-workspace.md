# Native JJ review workspace

The first slice is a native revision selector connected to Neohelix's existing
changed-file sidebar and inline/split diff editor. It runs inside the editor,
using `jj-lib`; no jj subprocess, terminal embedding, or renderer daemon is used.
It is not yet a full jjui replacement or a persistent revision-graph panel.

## Try it

Open a file inside a trusted JJ workspace, then run:

```text
:jj-review
```

The picker shows the change ID, commit ID, bookmarks, author, and description.
`@` marks the current working revision. Search and select with the normal picker
keys; Enter opens its changed files in the review sidebar. Full commit IDs, not
ambiguous change IDs, identify historical snapshots, including divergent changes.

An optional revset narrows the list, for example:

```text
:jj-review @
:jj-review ancestors(@, 20)
:jjr bookmarks()
```

The default is `@ | ancestors(@, 50) | bookmarks()`. The list is limited to the
first 200 revisions in JJ's reverse-topological order. Configured revset aliases
are supported. An invalid revset or an untrusted/non-JJ workspace reports an
error without replacing the existing review.

The active file supplies repository context even if Helix started outside that
repository. When revisiting the picker from a review snapshot, the sidebar's
repository supplies context instead. In a JJ review sidebar, `r` reopens the
default revision selector.

## Editing and navigation

- Selecting `@` opens the existing working-copy review: the working side is
  editable and reuses unsaved buffers. It includes working changes not yet
  recorded by a separate jj invocation.
- Selecting a historical commit opens immutable before/after snapshots.
  They are deliberately read-only: selecting history neither checks out the
  revision nor writes historical contents into today's working files.
- Existing file navigation, hunk navigation, word coloring, syntax highlighting,
  wrapping, and `:diff-layout inline` / `:diff-layout side-by-side` remain available.
  Switching revisions preserves the layout and reuses the review panes.
- `:diff-off` exits the review and restores the previous filesystem sidebar.

Historical comparisons use a commit's merged parent tree as the baseline, as
native JJ diffs do. Renames and conflict materialization use the existing native
JJ provider. Historical views do not follow working-file changes. Reopening the
revision selector reads the current operation head afresh; it does not reconcile
divergent operations, rewrite commits, or change bookmarks.

## Next steps

A persistent revision graph/list beside the file tree, selection-driven previews,
and explicit JJ edit/new/rebase operations are not implemented yet. Those need
buffer-safety checks, transactional error handling, and refresh/selection rules.
Keeping the editor open amortizes configuration and grammar/query initialization;
it does not eliminate parsing costs for previously unseen source contents.
