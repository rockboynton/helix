# Other Neohelix editing features

The larger guides cover Steel, diffs, GitLens, folding and media. These smaller
features are also part of the fork. The generated command reference lists their
exact command names and built-in bindings; a user's keymap can override those.

## Watching files and safe reloads

`editor.file-watcher` controls the Filesentry-based native watcher; `auto-reload`
allows clean buffers to reload. Native notifications normally deliver updates
immediately. The 30-second polling interval is a backstop, not an intentional
notification delay. Dirty/deleted documents are retained instead of discarding
unsaved work, and clean reloads retain undo history. Events also feed LSP file
watches and VCS refreshes. Live unsaved updates between editors use the separate
[shared-document protocol](shared-lsp.md), not disk polling.

Commits and checkouts refresh gutter baselines even when source files do not
change. JJ operation heads and checkout state are watched, including linked
workspaces; Git branch/packed refs are watched as well as HEAD. This does not
reload buffer text or discard unsaved edits.

Linux delivery has been tested, including atomic replacements and aliased paths.
macOS FSEvents and Windows backends exist, but their presence is not native
runtime validation on those platforms.

## Editable workspace refactoring

1. Run `:global-refactor 'pattern'` in a trusted workspace.
2. Edit matching result lines without inserting or removing result lines.
3. Run `:refactor-apply`.
4. Review the source buffers, then save explicitly.

Search respects ignore rules and the current file extension. All affected disk
and buffer snapshots are validated before applying: stale results abort. Changes
are undoable in source buffers and are not automatically saved. This is editable
search, not semantic LSP rename.

## Pickers and diagnostics

Preview syntax parsing starts asynchronously as soon as selection needs it,
independently of `idle-timeout`. Open documents and ready cached previews are
reused. At most two syntax previews parse concurrently; the selected file retries
when capacity becomes available. A loading placeholder can still appear until
parsing finishes: zero-latency previews are not guaranteed. Unsupported/failed
parsers fall back to stable plaintext rather than flashing plaintext first.

The active diagnostic expands and wraps within the list instead of hiding under
the preview. `Alt-]`/`Alt-[` scroll messages taller than the list; changing selection
resets that scroll. Other rows remain compact. Preview scrolling and [PNG/PDF
previews](media-previews.md) have separate controls. Multiplexers may intercept
Alt bindings; see the optional keymap's notes.

The fork also includes picker titles, improved path display, command-history
selection (`command_history_picker`) and register-history selection
(`select_register_history`).

## Motions, search and display

- `:trim` removes trailing whitespace in current selections, not the whole file
  unless the whole document is selected.
- Toggle-case commands, subword motions/text objects, and consistent closest-pair
  matching extend Helix's selection model.
- `goto_hover` opens documentation in a buffer rather than only a popup.
- `search-position` in the statusline reports per-view search location/count.
- Icon themes and transparent Zellij backgrounds extend display configuration.
  Full-file [blame annotations](gitlens.md) use a gutter, separate from the tree.

The [feature inventory and PR provenance](https://github.com/rockboynton/helix/blob/neohelix/NEOHELIX.md)
records source contributions. These are maintained adaptations, not a claim that
the upstream PRs are merged or identical to this branch.
