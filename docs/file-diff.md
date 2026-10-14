# Editable side-by-side file comparison

```sh
hx --diff before.rs after.rs
```

Inside Helix, `:diff-split other.rs` compares the current file with another;
`:diff-split before.rs after.rs` opens both. Both panes are normal editable
documents, with path-based language detection and Tree-sitter syntax colors.
Use the normal `Ctrl-w h` / `Ctrl-w l` window commands to switch panes.

Deleted/left lines have a removal background; added/right lines have an addition
background. Themes can supply backgrounds in `diff.minus` and `diff.plus`;
otherwise their foreground colors are tinted against the editor background.
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
