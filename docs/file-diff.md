# Editable side-by-side file comparison

```sh
hx --diff before.rs after.rs
```

Inside Helix, `:diff-split other.rs` compares the current file with another;
`:diff-split before.rs after.rs` opens both. Both panes are normal editable
documents, with path-based language detection and Tree-sitter syntax colors.
Use the normal `Ctrl-w h` / `Ctrl-w l` window commands to switch panes.
The gutter uses Helix's configured added/deleted/modified icons and theme
styles, just like normal editing—not literal `+`/`-` signs. Replacements have
the modified marker on both sides; empty hunks retain boundary markers.

Deleted/left lines have a removal background; added/right lines have an addition
background. Defaults are Delta-like dark red/green fills (pale red/green on
light themes), including when the editor background is transparent. Themes can
override them with `"ui.diff.minus" = { bg = "#300000" }` and
`"ui.diff.plus" = { bg = "#002800" }`. Explicit backgrounds in `diff.minus` and
`diff.plus` are also honored; bright foreground/gutter colors are never used as
background fills. Syntax foregrounds remain unchanged.
Blank alignment rows are virtual: they never become file contents. Scrolling
either focused pane synchronizes the other's aligned position. Diff computation
runs in the background and stale results are hidden while edits are recomputed.
Switching panes brings an offscreen single cursor to the corresponding source
location; existing visible cursors and multiple selections are preserved.

| Command | Action |
| --- | --- |
| `:diff-next`, `:diff-prev` | Jump between changed hunks in the focused pane |
| `:diff-get` | Replace the hunk at the cursor with the other pane's version |
| `:diff-put` | Copy the hunk at the cursor into the other pane |
| `:diff-off` | Remove comparison decorations; keep both buffers open |

Transfers are ordinary undoable edits, not writes to disk. Save each file with
`:write`, or both with `:write-all`. Nothing is automatically saved. An empty
side of a hunk is addressed at the following line, or at EOF for trailing changes.
If the diff is still updating, transfer/navigation commands ask you to retry.

This first version supports one pair per editor. Closing a pane or switching
its buffer ends the comparison. Comparison panes temporarily disable soft-wrap
and inline diagnostic rows to preserve logical-line alignment; other views keep
their settings. Long lines can be scrolled horizontally. Character/word-level
highlighting, Git/JJ revision inputs, and GitLens integration are not implemented.

## Direction: standalone viewer and external hosts

The intended end state is a standalone diff viewer/editor, not just an editor
command: side-by-side **and** inline layouts, a changed-file list, revision/range
inputs, history navigation, and eventually conflict-resolution layouts. These
are development targets, not capabilities provided by this first version.
[Diffview.nvim](https://github.com/sindrets/diffview.nvim) is a workflow reference,
not a requirement to reproduce its UI or Git-specific implementation.

Both layouts should share the comparison model and retain Tree-sitter syntax
from the source documents. File inputs can be editable. A patch received over
stdin must remain a read-only review unless complete backing documents have
been explicitly supplied: omitted context is not a complete file and must never
be written back as one.

External integration has two separate contracts:

- **Formatter:** consume files or a patch and emit styled text for a host's
  own read-only diff view; the host handles navigation. This must not initialize
  a terminal UI, issue cursor/alternate-screen commands, or execute input controls.
- **Interactive host:** own a child PTY, feed its output through a terminal
  emulator into a bounded pane, forward focused keyboard/mouse events and
  resize events, and respect unsaved edits on close. Running a pager process
  externally is not the same as hosting its editor inside a pane.

In jjui 0.10.11, the built-in diff viewer captures command output and renders
it as text ([diff source](https://github.com/idursun/jjui/blob/v0.10.11/internal/ui/diff/diff.go),
[command runner](https://github.com/idursun/jjui/blob/v0.10.11/internal/ui/context/command_runner.go)).
Its interactive command runner uses a separate process-launch path. Therefore
read-only styled output can use the formatter route; embedding Helix editing
requires an interactive host integration, not only a `ui.pager` setting.
Neither new contract is implemented yet. Current non-terminal `--pager` output
is still byte-preserving passthrough, and `--diff` still takes two file paths.
