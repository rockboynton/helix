# Pager mode

Neohelix can browse files and finite piped input using its normal editor renderer,
Tree-sitter syntax highlighting, navigation, search, selections, and yank commands.

```sh
hx --pager src/main.rs
cat src/main.rs | hx --pager --language rust
git show --color=never | hx --pager --language diff
```

`--pager` reads stdin when no file arguments are supplied. An explicit `-` also
selects stdin; do not combine it with file arguments. Files can use the normal
`file:line:column` syntax and `--vsplit`/`--hsplit` options. Directories and missing
input files are errors rather than opening a picker or a new file.

File inputs use normal Helix language detection. For piped code, select a language
with `--language <name>` or `:set-language`; `--language text` disables syntax
highlighting. Language names match Helix's language configuration. There is no
external syntax-highlighting engine.

## Controls and protection

- Use normal Helix movement, scrolling, search, selection, and yank commands.
- Press `q` in normal or select mode to close the whole pager session without
  writing or asking about modified buffers. Escape still dismisses prompts/popups.
- Read-only buffers show the normal `[readonly]` statusline indicator when that
  element is included in your statusline.
- Text-changing transactions, undo/redo, writes (including `:write!` and save-as),
  and `:move` are blocked. Selection-only transactions remain allowed.
- Documents opened later in the pager session are also read-only. The editing
  policy is separate from filesystem permissions, so it can be reused for an
  immutable pane beside an editable pane in a future diff session.
- Pager documents do not start language servers or VCS diff providers, or register
  document-open hooks for automatic watching. Your usual theme, syntax definitions,
  keymaps, and trusted Steel startup configuration remain available.

Read-only protects editor buffer mutations and saves. It is not a sandbox for
explicit shell commands or trusted plugins that perform their own filesystem I/O.

When stdout is redirected, `--pager` copies input bytes directly to stdout instead
of starting the terminal UI. This preserves encoding and avoids emitting UI escape
sequences into a downstream pipeline.

## First-milestone limits

Piped input is loaded to EOF before browsing starts. Live streams/follow mode,
bounded streaming storage, ANSI color interpretation, and man-page overstrike
handling are future work. This is not yet a drop-in replacement for every `less`
use case.

Terminal controls in read-only document text are rendered visibly rather than
executed; the underlying text and Tree-sitter offsets are unchanged. For best
results, request uncolored output from producers such as Git in this milestone.

An example opt-in shell setting for finite, plain-text command output is:

```sh
export PAGER='hx --pager'
```

Do not set `MANPAGER` or a terminal scrollback pager expecting ANSI/overstrike
support yet.
