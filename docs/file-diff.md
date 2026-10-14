# Editable side-by-side file comparison

```sh
hx --diff before.rs after.rs
```

Inside Helix, `:diff-split other.rs` compares the current file with another;
`:diff-split before.rs after.rs` opens both. Both panes are normal editable
documents, with path-based language detection and Tree-sitter syntax colors.
Use the normal `Ctrl-w h` / `Ctrl-w l` window commands to switch panes.
`:diff-split` reuses existing side-by-side panes in the focused row, creating
only missing columns. Other rows and displaced buffers are preserved.
Add `--pager` to browse both files read-only, with `q` to close the viewer.
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

## Interactive inline viewing

`hx --diff --diff-layout inline before.rs after.rs` opens a single read-only
Helix buffer. Use normal movement, search, selection, yank and horizontal
scrolling; `q` exits. `:diff-next` and `:diff-prev` jump between hunks. The
gutter shows old/new source line numbers and normal Helix change icons.
Removed lines precede added lines; unchanged lines appear once. Syntax is
queried from each original complete source tree, not a synthetic patch tree.
Presentation newlines separate unterminated source lines without modifying
either file. Writes and hunk transfers are rejected, including forced writes.

## Read-only rendering: standalone and jjui

```sh
hx --render-diff before.rs after.rs
hx --render-diff --diff-layout inline before.rs after.rs
hx --render-diff --color always --diff-layout inline before.rs after.rs | less -R
hx --render-diff --width 120 before-directory after-directory
```

This emits flow text without acquiring a terminal. Both layouts use the same
comparison model and Helix's native document renderer/Tree-sitter highlighting
from each complete source document. Inline output shows old/new line numbers
and all removed lines before added lines in a hunk. Output contains only text
and optional SGR color/style codes: never cursor positioning, alternate-screen
commands, OSC clipboard sequences, or controls from input text.

Options: `--color auto|always|never`, `--width 20..4096` (default 100),
`--diff-context 0..1000` (default 3), and `--diff-theme <name>`. Long lines are
clipped with an explicit `…`; increase the width to see more. Directory inputs
are compared recursively in sorted order. Symlinks are reported without
following them; binary files, empty-file additions/removals, executable-bit
changes and missing final newlines are reported explicitly. No files are written.

The formatter loads built-in/TOML languages and an explicit/default theme; it
does **not** execute Steel startup scripts or start LSP/VCS/watcher services.
Use `--diff-theme modified_gruvbox` to match that theme without starting an editor.

## Raw patch input

```sh
git diff --no-ext-diff --color=never | hx --patch
jj diff --git --color never | hx --patch --diff-layout side-by-side
jj diff --color-words --color never | hx --patch
jj diff --color-words --color always | hx --patch
hx --patch changes.patch
git diff --color=never | hx --render-diff --patch --diff-layout inline
```

`hx --diff` with no paths also reads a patch from stdin. `--patch` defaults to
interactive inline viewing; select `--diff-layout side-by-side` for paired
read-only panes. With redirected stdout it emits formatted flow text instead
of acquiring a terminal. `--render-diff --patch` explicitly selects the flow
renderer (both layouts supported) for hosts such as jjui.

Git unified hunks and native jj color-words line-number markers are recognized.
Default ANSI red/green jj word tokens can be separated back into before/after
lines; custom jj token colors may be ambiguous, so use `--color never` or
`--git` with custom palettes. A configured external formatter such as Delta
is not native jj input: explicitly select `--color-words` or `--git`.
Only SGR styling is consumed; other terminal controls remain inert input.

Multiple files/hunks, quoted Git paths, additions/deletions, rename/mode and
binary metadata, and missing-final-newline notices are retained. Omitted
context stays visibly omitted, rather than allocating padding for large
coordinates. Syntax uses path-selected Tree-sitter on each supplied fragment;
missing context can affect grammar accuracy. No referenced paths are opened,
and synthetic buffers cannot be edited or written, even with `:write!`.
Malformed/truncated unified hunks are rejected before terminal setup. Combined
merge patches are rejected explicitly; use separate Git merge diffs or jj Git
output instead. Input is finite UTF-8 and read through EOF, not a live tail.

## jj/jjui external formatter

For jj/jjui, configure a named external formatter in **jj's** configuration:

```toml
[merge-tools.neohelix]
program = "hx"
diff-invocation-mode = "dir"
diff-args = ["--render-diff", "--color", "always", "--width", "$width", "$left", "$right"]
diff-expected-exit-codes = [0]
```

Try `jj diff --tool neohelix` first. To make it the default, set
`ui.diff-formatter = "neohelix"`; add `"--diff-layout", "inline"` to the args
for inline output. For checkout testing, use the absolute path to
`contrib/hx-neohelix` as `program`; an older installed `hx` will not have this flag.
This integrates with jjui's **read-only** diff view without a jjui fork.
It is a formatter integration, not an embedded editable Helix terminal.
Your existing jj/jjui/NixOS configuration is not changed automatically.
If a jjui preview command contains `--config ui.diff-formatter=delta`, that
explicit override wins over jj's default; remove it or change it to `neohelix`
when you want the preview to use this renderer too.

Validation on Linux: the renderer was invoked by `jj 0.46.0` with real exported
snapshot directories in both layouts, and its output was displayed by jjui
0.10.11's built-in diff screen in a PTY. Navigation/exit and terminal restoration
were checked with isolated temporary repositories/configuration.

## Direction: standalone viewer and external hosts

The intended end state is a standalone diff viewer/editor, not just an editor
command: side-by-side **and** inline layouts, a changed-file list, revision/range
inputs, history navigation, and eventually conflict-resolution layouts. These
include development targets beyond the current editable side-by-side UI and
read-only side-by-side/inline formatter.
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
The formatter contract is implemented by `--render-diff`; the interactive host
contract is future work. Current non-terminal `--pager` output is still
byte-preserving passthrough (except explicit patch formatting). Interactive
inline and raw-patch views are read-only; paired full-file comparison remains
editable unless `--pager` is specified.
