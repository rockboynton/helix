# Maintained Helix feature stack

This branch contains one commit per requested feature, based on upstream master
`ba40e547426b0f9896c8bdc699a4ab11f2b37dbc` (fetched 2026-10-11).
The original feature list in `config.toml` is preserved. Each feature commit
records its upstream PR and imported source revision in its message.

| Source | Feature |
| --- | --- |
| #16217 | Transparent backgrounds in Zellij |
| #10611 | Consistent closest-pair text objects |
| #12026 | Subword text objects |
| #12043 | Case conversion mode |
| #8366 | `:trim` / `:trim-trailing-whitespace` |
| #11700 | Per-view search match position and count |
| #11441 | Scrolling picker previews |
| #12520 | Descriptive picker titles |
| #12806 | Root path in file pickers and explorer |
| #8354 | Command history picker |
| #11184 | Register history picker |
| #15187 | Hover documentation in a scratch buffer |
| #12022 | Jujutsu diff and status provider |
| #13133 | Inline Git blame |
| #8675 | Steel configuration and plugins |
| #2869 | Configurable icon flavors |
| #14544 / Issue #1125 | Native Filesentry watching and external-file auto-reload |
| #4381 | Editable workspace refactoring view |

## Adaptations

- File watching is based on #14544 at `30993172445062d356810d3ab77d953f15c36ae4`,
  adapted to current master and this fork's workspace-trust/VCS APIs. It uses
  `rockboynton/filesentry` at `5778fa774ce6ae460bb649380a9289341d145999` for native
  Linux, macOS (FSEvents), and Windows backends. Clean buffers reload with undo;
  dirty/deleted buffers are preserved. Polling defaults to 30 seconds and covers
  unwatched files plus native events deferred during insert sessions or saves.
  Filesystem events also feed LSP watches and Git HEAD refreshes.
- The Filesentry pin is published on the fork's `macos-backend` branch and can
  be fetched by fresh builds. See its README for native macOS validation commands.
- #4381 was unfinished. `:global-refactor 'pattern'` searches the workspace,
  respecting ignore settings and the current file extension. Edit the matching
  lines without adding/deleting result lines, then run `:refactor-apply`.
  All affected disk and buffer snapshots are validated before any source edit.
  Changes are undoable in source buffers; saving is explicit with `:write-all`.
- Steel is included in the default build, pinned to
  `24cd21598c091fb88bc10a6375a1ded25e677c37`. `init.scm` and `helix.scm` in the
  global Helix configuration directory are supported. Workspace-local scripts
  require workspace trust; `HELIX_STEEL_CONFIG` explicitly selects another directory.
- Jujutsu uses `jj-lib` 0.46.0; neither `jj` nor external diff/copy tools are
  required at runtime. Standard Git and Simple backends are supported; custom
  backends and future repository formats need a library update. Parent-tree
  reads, bookmark formatting, rename detection, and discarded status snapshots
  are native. Queries do not advance operations, save working-copy state, change
  refs, reconcile concurrent operations, or run configured signing/filter tools.
  Status may cache immutable file/tree objects and briefly lock the working copy.
  Workspace trust is still required. User, repository, workspace, and scoped JJ
  configuration are read without migrating secure-config metadata.
- The native JJ library requires Rust 1.97.1; the toolchain and CI MSRV pins are
  updated. Mechanical lint compatibility changes are kept in a maintenance commit.
- Picker redraw callbacks and drop guards capture their editor's notification
  signal before reaching directory-walker/Rayon threads. This prevents off-runtime
  redraw panics and destructor double-panics in integration builds.

## Useful configuration

These features do not all have default bindings or enable their display by default:

```toml
icons = "nerdfonts" # optional; requires a compatible terminal font

[editor]
auto-reload = true
enable-steel = true

[editor.statusline]
right = ["search-position", "file-type-icon", "position"]

[editor.inline-blame]
show = "cursor-line"
auto-fetch = true

[keys.normal]
H = "goto_hover"

[keys.normal.space]
R = "select_register_history"
```

`Space :` opens command history. `Space B` requests line blame. Picker previews
scroll with `Alt-j/k`, `Alt-d/u`, and `Alt-f/b` (or Page Down/Up); `Ctrl-d/u`
continues to page picker results. Case conversion commands are in backtick mode.

## Validation and maintenance

Linux validation covers the default build, a no-default-features check, workspace
library tests, editor integration tests, and Clippy with warnings denied.
Additional regression tests cover Unicode/CRLF trimming, backward search without
wraparound, icon loading, external-file reload safety, and refactoring validation
and undo. An interactive smoke test exercised Steel initialization, Nerd Font
icons, search match status, trimming, and shutdown with isolated config/data paths.
macOS/Windows runtime behavior and live LSP/DAP servers have not been tested here.

The native JJ update passed 437 workspace library tests and all 182 editor
integration tests at eight-way concurrency, plus strict workspace/all-targets
Clippy, the default build, and a no-default-features check. The 16 native JJ tests
also pass with no executables on PATH and as part of the standalone JJ feature
before later features are applied. They cover disk changes, renames, merged-parent
conflicts, bookmarks, ignores, linked workspaces, trust, and unchanged operations,
refs, and working-copy state. Native file delivery is covered with a symlink-aliased
buffer path and polling disabled, including external writes and atomic replacements.
Filesentry's Linux suite passed 19 tests with `queue_overflow` excluded. That
stress test failed twice because it did not actually trigger its expected queue
overflow; no lost-event assertion failed. Filesentry Clippy completed with an
existing `unnecessary_sort_by` warning in a test. macOS runtime checks are pending.
Integration
validation used:

```sh
cargo test -p helix-term --features integration \
  --test integration -- --test-threads=8
```

A new picker regression reproduced SIGABRT before the fix: a native-thread redraw
tried to resolve a Tokio-runtime-local notification, then the redraw-on-drop guard
panicked again during cleanup. The captured notification fix passes that regression
and the eight-way suite. This is a demonstrated off-runtime redraw bug, not evidence
of a worker limit; the precise earlier Rayon abort was not independently traced.

To advance this owned stack, fetch upstream and rebase its individual commits:

```sh
git fetch upstream master
git rebase upstream/master
```

Resolve API changes within the affected feature commit and rerun the checks.
Do not regenerate this branch by blindly merging all source heads: several are
drafts against older APIs, and issue #1125 has no pull-request head to fetch.
