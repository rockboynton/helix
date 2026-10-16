# Editable side-by-side file comparison

For full changesets and the reusable expandable sidebar, see
[changeset review and file tree](diff-review.md).

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

Replacement hunks also emphasize changed words with a stronger, muted background,
while retaining Tree-sitter foreground colors. This applies to split, three-way,
inline and patch views, including `--render-diff` output. Themes can override
`ui.diff.minus.word` and `ui.diff.plus.word` with background colors.
Unicode word boundaries are used. Pure additions/deletions keep the line fill;
replacement blocks above 128 lines, 16,384 characters on either side, or 2,048
tokens fall back to line-only highlighting to bound refinement work. This is
textual word diffing, not compiler-aware semantic equivalence.

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
| `:diff-fold [N]` | Collapse unchanged context, retaining N lines at each edge (default 3) |
| `:fold-open` | Expand the summary row at the cursor, in all compared panes |
| `:fold-open-all` | Expand all folds in the current comparison |
| `:diff-off` | Keep working file in the left pane; close extra comparison panes, retaining buffers |

Transfers are ordinary undoable edits, not writes to disk. Save each file with
`:write`, or both with `:write-all`. Nothing is automatically saved. An empty
side of a hunk is addressed at the following line, or at EOF for trailing changes.
If the diff is still updating, transfer/navigation commands ask you to retry.

### Folding unchanged context

Use `:diff-fold` to focus on changes without changing the underlying files.
`:diff-fold 5` retains more context; `:diff-fold 0` shows just changes and fold
summaries. Runs shorter than two hidden lines remain expanded. Collapsing is
explicit, not enabled by default. It works in two-pane, three-way and editable
inline comparisons, and read-only inline patch views. Multi-file reviews use
the same commands for the current file; collapse again after switching files.

Summary rows retain the real source line number. Normal up/down movement skips
folded text, and `:fold-open` opens the summary at the cursor. Search/goto targets
inside hidden lines reveal their fold. Compared panes open/collapse together,
preserving alignment and inline deleted-row highlighting. Folds containing an
existing cursor stay open. Edits (including undo, reload and hunk transfers)
currently open all folds in the comparison; use `:diff-fold` again after the
updated diff is ready. This conservative invalidation prevents stale ranges
from hiding newly changed text.

Outside diff mode, select lines and run `:fold-selection` to use the same
view-local folding mechanism; `:fold-open` and `:fold-open-all` also work there.
Fold commands do not modify text, and save writes the complete file. Selections,
operators and clipboard actions still address real text, including hidden lines
when a selection spans a fold. Normal views also support nested Tree-sitter
[code folding](folding.md). No default keys are taken over; the commands can be
bound in TOML or Steel like other commands.

One comparison session (two or three panes) is supported per editor. Closing a
compared pane or switching its buffer ends the comparison. Comparison panes temporarily disable soft-wrap
and inline diagnostic rows to preserve logical-line alignment; other views keep
their settings. Long lines can be scrolled horizontally. Arbitrary Git/JJ revision
inputs and GitLens integration are not implemented.

## Three-way comparison

```sh
hx --diff3 left.rs base.rs right.rs
hx --diff3 --pager left.rs base.rs right.rs
```

Inside Helix, use `:diff-three left.rs base.rs right.rs` (alias `:diff3`), or
give `:diff-split` three paths. Existing columns in the focused row are reused;
only missing columns are created. Files are ordered **left | base | right**.
All three retain normal editing, syntax highlighting, undo and explicit saves;
`--pager` makes all three read-only and `q` exits.

Both variants are compared independently against the middle base. Alignment
combines insertion gaps from both comparisons; scrolling any pane synchronizes
the other two. Base lines changed by either variant have removal backgrounds;
changed variant lines have addition backgrounds. Normal Helix gutter icons
remain in use. `:diff-next` / `:diff-prev` navigate the focused variant's changes,
or the union of both comparisons when focused on base.

From an outer pane, `:diff-get` copies the base hunk into that variant;
`:diff-put` copies that variant's hunk into base. These are undoable edits,
never automatic writes. Transfer commands in the base pane require choosing
an outer pane first, rather than guessing a variant. Editing base recomputes
both comparisons. `:diff-off` keeps the focused outer variant (or the right
variant when base is focused) in the leftmost comparison pane and closes the
other comparison panes. All buffers, including unsaved edits, remain available.
This is a three-way comparison/editor, not an automatic merge or conflict
resolver, and requires three complete files (not partial patch input).

## Interactive inline viewing

`hx --diff --diff-layout inline before.rs after.rs` opens the real `after.rs`
document for editing, with removed baseline lines rendered as virtual, read-only
rows before the corresponding working lines. Movement, selections, search,
LSP, undo and saves operate on the working file, not synthetic patch text.
Deleted rows retain syntax highlighting from the complete baseline source tree
and use the normal configured gutter icons. They are not cursor/edit targets.
`:diff-next` / `:diff-prev` navigate hunks; `:diff-get` restores a baseline hunk
into the working file as an undoable edit. `:diff-put` is rejected because the
inline baseline is read-only. Nothing is saved until `:write`.

Add `--pager` for a strictly read-only synthetic presentation in which both old
and new text are searchable/selectable and `q` exits. Raw patch input remains
read-only too: partial patches do not provide a complete save target.

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
changes and missing final newlines are reported explicitly. Input files are never written.

The formatter loads the normal built-in/TOML settings, then user Steel
configuration followed by trusted project Steel configuration. Themes (including
dynamic themes), icons and language overrides therefore match the editor.
`--diff-theme <name>` overrides the configured theme. No terminal is acquired and
no editor event loop, LSP or file-watching service is started. Ordinary Scheme
display output goes to stderr, not into the diff stream. Normal Steel cache/module
initialization and trusted configuration side effects still apply; this is not a
sandbox for startup scripts. Set `editor.enable-steel = false` in TOML to disable
Steel configuration just as in the editor.

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
diff-do-chdir = false
diff-args = ["--render-diff", "--color", "always", "--width", "$width", "$left", "$right"]
diff-expected-exit-codes = [0]
```

Try `jj diff --tool neohelix` first. To make it the default, set
`ui.diff-formatter = "neohelix"`; add `"--diff-layout", "inline"` to the args
for inline output. For checkout testing, use the absolute path to
`target/release/hx` as `program` and point `HELIX_RUNTIME` at that checkout's
`runtime` directory; an older installed `hx` will not have this flag.
This integrates with jjui's **read-only** diff view without a jjui fork.
`diff-do-chdir = false` preserves the workspace for configuration and worker
reuse; apply it to a separate inline formatter too. See the
[worker guide](diff-render-worker.md) for lifecycle and opt-out controls.
It is a formatter integration, not an embedded editable Helix terminal.
Your existing jj/jjui/NixOS configuration is not changed automatically.
If a jjui preview command contains `--config ui.diff-formatter=delta`, that
explicit override wins over jj's default; remove it or change it to `neohelix`
when you want the preview to use this renderer too.

Validation on Linux: the renderer was invoked by `jj 0.46.0` with real exported
snapshot directories in both layouts, and its output was displayed by jjui
0.10.11's built-in diff screen in a PTY. Navigation/exit and terminal restoration
were checked with isolated temporary repositories/configuration.

### Interactive JJ diff editing and jjui handoff

Let JJ resolve revisions/ranges and export complete snapshot directories rather
than duplicating its revision-selection UI in Helix. Extend the tool above with:

```toml
[ui]
diff-formatter = "neohelix"
diff-editor = "neohelix"
diff-instructions = false

[merge-tools.neohelix]
program = "hx"
diff-args = ["--render-diff", "--color", "always", "--width", "$width", "$left", "$right"]
diff-invocation-mode = "dir"
diff-do-chdir = false
edit-args = ["--review", "--diff-layout", "inline", "$left", "$right"]
edit-invocation-mode = "dir"
```

Use `jj diff -r REV` (or `--from`/`--to`) for formatted output and
`jj diffedit -r REV` for interactive editing. Switch layouts with `:diff-layout`,
save working files with `:write`/`:write-all`, then exit with `:quit-all` (or use
`:write-quit-all`). JJ reads back the exported right directory after Helix exits.
That can rewrite the selected revision and rebase descendants; it is not merely
a viewer. Raw patch streams are intentionally not used as editable save targets.

jjui's [unified action system (#515)](https://github.com/idursun/jjui/issues/515)
was implemented in [merged PR #533](https://github.com/idursun/jjui/pull/533).
In jjui 0.10.11, the default **Shift-E** binding invokes `revisions.diff_edit`,
which runs `jj diffedit` interactively and refreshes when the subprocess returns.
Setting JJ's `ui.diff-editor` is therefore sufficient; a custom action is optional.
Do not use the unmerged `[diff]` configuration proposal from PR #453.
Embedded read-only previews still use the formatter path, separately from this
full-terminal editor handoff.

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
The formatter contract is implemented by `--render-diff`; full-terminal handoff
uses jjui's existing interactive runner. In-screen interactive terminal embedding
would still require host support. Current non-terminal `--pager` output is still
byte-preserving passthrough (except explicit patch formatting). Interactive
raw-patch views are read-only; full-file split and inline comparison are editable
unless `--pager` is specified.
