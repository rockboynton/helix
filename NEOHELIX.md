# Neohelix

Inline review: `hx --diff --diff-layout inline before.rs after.rs`.
Three-way editing/review: `hx --diff3 left.rs base.rs right.rs`.
Raw Git or native jj patches: `git diff | hx --patch` or
`jj diff --color-words --color never | hx --patch`. Both patch layouts are
strictly read-only; see [file diff guide](docs/file-diff.md).

## Why this fork exists

I want Helix with the features I use, without waiting for upstream to merge
every PR. Neohelix is my maintained feature stack: upstream contributions are
ported and adapted, unfinished features are completed where needed, and
fork-specific improvements are added on top.

This is not an automated merge of PR heads, and it is no longer a Patchy-managed
branch. The `neohelix` bookmark is the development line. The original `patchy`
branch is separate and is not the target for these changes.

The aim is to keep Helix's editing model while adding better history inspection,
Jujutsu integration, external-file watching, workspace refactoring, and Steel
configuration/plugins. Steel remains useful for reusable community plugins and
user customization; native Rust services handle work that needs editor state,
background execution, or reusable UI primitives.

## Feature inventory

The stack is based on [upstream Helix](https://github.com/helix-editor/helix).
Its original integration base was
`ba40e547426b0f9896c8bdc699a4ab11f2b37dbc`, fetched on October 11, 2026.
Feature commits record source PRs and imported revisions, with later fixes and
adaptations maintained in this fork. That base is a historical integration point,
not a claim that the branch always matches today's upstream master.

The links below identify provenance, not the PRs' current merge status or exact
parity with their source branches.

| Feature | Source PR / issue |
| --- | --- |
| Inline Git blame | [#13133](https://github.com/helix-editor/helix/pull/13133) |
| Per-view search location and total matches in the statusline | [#11700](https://github.com/helix-editor/helix/pull/11700) |
| Preview scrolling | [#11441](https://github.com/helix-editor/helix/pull/11441) |
| `:trim` trailing whitespace | [#8366](https://github.com/helix-editor/helix/pull/8366) |
| Toggle-case commands | [#12043](https://github.com/helix-editor/helix/pull/12043) |
| Consistent inside/around closest-pair matching | [#10611](https://github.com/helix-editor/helix/pull/10611) |
| Subword text object | [#12026](https://github.com/helix-editor/helix/pull/12026) |
| Command-history picker | [#8354](https://github.com/helix-editor/helix/pull/8354) |
| Register-history picker | [#11184](https://github.com/helix-editor/helix/pull/11184) |
| Picker titles | [#12520](https://github.com/helix-editor/helix/pull/12520) |
| Paths in the file picker and explorer | [#12806](https://github.com/helix-editor/helix/pull/12806) |
| External file watching and auto-reload | [#14544](https://github.com/helix-editor/helix/pull/14544); originally [issue #1125](https://github.com/helix-editor/helix/issues/1125) |
| Steel plugin system and configuration | [#8675](https://github.com/helix-editor/helix/pull/8675) |
| View-based refactoring | [#4381](https://github.com/helix-editor/helix/pull/4381) |
| Icon support | [#2869](https://github.com/helix-editor/helix/pull/2869) |
| Jujutsu diff provider | [#12022](https://github.com/helix-editor/helix/pull/12022) |
| Goto-hover action | [#15187](https://github.com/helix-editor/helix/pull/15187) |
| Transparent backgrounds in Zellij | [#16217](https://github.com/helix-editor/helix/pull/16217) |

## What goes beyond simply importing the PRs

### Pager mode

`hx --pager` views files or finite piped input read-only, with normal Helix
navigation, search, selection/yank, and Tree-sitter syntax highlighting. `q`
exits cleanly. Use `--language rust` for piped Rust code; file inputs use normal
language detection. This establishes a per-document editing policy for future
editable diff sessions. See the [pager guide](docs/pager.md) for examples, safety
boundaries, and the remaining streaming/ANSI limitations.

### Editable side-by-side diff

`hx --review` opens a full Git/JJ working-copy changeset with an expandable file
sidebar and directly editable working files. Two-directory and multi-file patch
reviews support split and read-only inline layouts. `Space E` opens the same
reusable tree for ordinary filesystem browsing. See the
[changeset review guide](docs/diff-review.md) for commands and boundaries.

`hx --diff before.rs after.rs` compares two editable files using their normal
Tree-sitter syntax highlighting, diff backgrounds/gutters, virtual alignment
rows, and synchronized scrolling. `:diff-split` starts a comparison inside the
editor. Hunk navigation and undoable `:diff-get` / `:diff-put` transfers never
save automatically. `--pager --diff` provides read-only browsing.
`--render-diff` renders files or snapshot directories as read-only side-by-side
or inline flow text using the native Tree-sitter renderer; it can be registered
as a jj external formatter for jjui's built-in diff view. See the
[file comparison guide](docs/file-diff.md) for usage and current boundaries.

### On-demand GitLens

[GitLens](docs/gitlens.md) is a Steel plugin built on native asynchronous editor
services. It inspects the commit behind the current line and enriches it with
GitHub PR metadata, without doing commit lookups on the rendering path.

It provides author/email, a readable timestamp and timezone, the full commit hash,
commit title/body, associated PR titles/status, selectable browser links, copying,
and a full local commit diff. PR URLs retain GitHub's canonical repository instead
of being blindly rewritten to a fork's origin. Commit-message hints are marked
unverified; offline failures still leave local metadata available.

With the documented normal-mode `B` binding installed:

| Key | Action |
| --- | --- |
| `B` | Inspect the current line |
| `Tab` / `Shift-Tab` | Choose a commit or PR link |
| `Enter` | Open the selected link |
| `c` | Copy the selected link |
| `y` | Copy the full commit hash |
| `d` | Toggle between commit diff and blame details |
| `Ctrl-d` / `Ctrl-u`, Page Down / Up | Scroll |
| `Esc` | Close |

The popup fits its content and prefers the roomier side of the cursor, including
upward expansion near the bottom of the screen. Diff round-trips preserve selection
and scrolling. GitHub enrichment uses bounded asynchronous `gh api` requests and
a private persistent cache: 128 repository–commit pairs total, five-minute freshness,
and stale fallback for seven days. `:gitlens-clear-cache` clears saved PR details.

The reusable plugin source is in [runtime/plugins/gitlens/](runtime/plugins/gitlens/); global
bindings belong to user keymaps, not plugin imports. GitLens ships with the runtime
but remains an optional standalone module: importing it does not activate a shortcut.
See the
[complete GitLens guide](docs/gitlens.md) for installation, limits, fork routing,
and caching behavior. This is not a recreation of every VS Code GitLens feature.

### Native external-file watching

File watching is based on PR #14544 at
`30993172445062d356810d3ab77d953f15c36ae4`, adapted to this fork's trust and VCS
APIs. It uses [rockboynton/filesentry](https://github.com/rockboynton/filesentry)
at `5778fa774ce6ae460bb649380a9289341d145999`, published on its
`macos-backend` branch, with Linux, macOS FSEvents, and Windows backends.

Native notifications drive reloads; the 30-second polling interval is a backstop,
not the expected update latency. It covers unwatched files and native events
deferred during insert sessions or saves. Filesystem events also feed LSP watches
and Git HEAD refreshes.

Clean buffers reload with undo history. Dirty or deleted buffers are preserved,
rather than overwriting unsaved work. Native macOS runtime validation remains
pending; backend implementation is not the same as testing on a Mac.

### Native Jujutsu integration

The diff/status provider uses `jj-lib` 0.46.0, rather than shelling out to `jj`.
Neither the `jj` executable nor external diff/copy tools are required for the
provider itself. User-configured external commands can still require those tools.

Standard Git and Simple backends are supported; custom backends and future
repository formats need a compatible library update. Parent-tree reads, bookmark
formatting, rename detection, and discarded status snapshots are native.

Queries do not advance operations, save working-copy state, change refs, reconcile
concurrent operations, or run configured signing/filter tools. Status may cache
immutable file/tree objects and briefly lock the working copy. Workspace trust is
required. User, repository, workspace, and scoped JJ configuration are read without
migrating secure-config metadata.

The native JJ library requires Rust 1.97.1; the repository toolchain and CI MSRV
pins were updated accordingly.

### Editable workspace refactoring

PR #4381 was unfinished. This fork provides an editable search-results buffer:

1. Run `:global-refactor 'pattern'` to search the workspace, respecting ignore
   settings and the current file extension.
2. Edit matching lines without adding or deleting result lines.
3. Run `:refactor-apply`.
4. Review the changes in source buffers and save explicitly with `:write-all`.

All affected disk and buffer snapshots are validated before any source edit.
Stale inputs abort the application rather than applying edits to changed content.
Applied changes are undoable in the source buffers; applying is not saving.

### Steel configuration and plugins

Steel is included in the default build and pinned to
`24cd21598c091fb88bc10a6375a1ded25e677c37`. User configuration normally lives in
`~/.config/helix/`, outside this source checkout.

Startup runs the user `helix.scm` and `init.scm` first, then a trusted project's
`.helix/helix.scm` and `.helix/init.scm` when both project files exist. Each layer
resolves its own relative imports. Project settings override explicitly configured
user settings; they do not replace the whole user configuration.

`HELIX_STEEL_CONFIG`, when set, selects the user layer. It does not bypass project
trust. No global TOML file is required for a Steel-only setup, including config
refreshes triggered by `:workspace-trust`. TOML configuration remains supported.

Steel can configure editor options, keymaps, languages, language servers, and
formatters. Language updates preserve unrelated built-in fields. Initialization
JSON supplied through `set-lsp-config!` replaces existing JSON; merge it explicitly
when you want to retain built-in server options. The language regression fixtures
are test data, not a bundled user configuration.

`--health` runs before Steel initialization, so it reports the built-in/TOML
language layer rather than runtime Steel overrides.

## Build and try it

From a checkout:

```sh
nix develop --command cargo build --release --locked -p helix-term
./contrib/hx-neohelix [files...]
```

The launcher selects this checkout's runtime and the release binary, but uses
your normal user configuration. It does not install or redirect your config.
A Nix-packaged build already has its packaged runtime and can run as plain `hx`.

Trust the workspace with `:workspace-trust` before using gated local scripts
or VCS features. The GitLens plugin additionally uses `gh` for GitHub enrichment
and `git` for commit patches; see its guide for installation.

## Useful configuration and commands

Features do not all enable their display or install bindings by default. Keep
configuration in your own repository or normal Helix config directory.

For a Steel setup, editor options can be applied from a function called by
`init.scm`:

```scheme
(require "helix/configuration.scm")
(provide configure-editing!)

(define (configure-editing!)
  (set-option! "auto-reload" #t)
  (set-option! "inline-blame.show" "never")
  (set-option! "inline-blame.auto-fetch" #t))
```

This keeps inline blame off while allowing on-demand GitLens inspection.
Inline blame's `show` options are `never`, `cursor-line`, and `all-lines`;
background fetching is independent of the GitLens plugin's GitHub lookup.

TOML users can configure the same optional features, for example:

```toml
icons = "nerdfonts" # requires a compatible terminal font

[editor]
auto-reload = true
enable-steel = true

[editor.statusline]
right = ["search-position", "file-type-icon", "position"]

[editor.inline-blame]
show = "never"
auto-fetch = true

[keys.normal]
H = "goto_hover"

[keys.normal.space]
R = "select_register_history"
```

These are examples, not the fork author's full keymap. `Space :` opens command
history; `Space B` requests native line blame. GitLens's `B` binding is installed
separately. Picker previews scroll with `Alt-j/k`, `Alt-d/u`, `Alt-f/b`, or
Page Down/Up; `Ctrl-d/u` continues to page picker results. Case conversion
commands are in backtick mode. `:trim` / `:trim-trailing-whitespace` removes
trailing whitespace.

## Validation and known limits

The following are recorded development results, not a promise that every
platform and external tool has been tested at every revision:

- Linux validation has covered default builds, no-default-features checks,
  workspace library tests, editor integration tests, and strict Clippy.
- Regression coverage includes Unicode/CRLF trimming, search behavior, icon
  loading, external reload safety, refactoring validation/undo, Steel language
  overrides, config layering/trust, and GitLens popup interactions.
- The native JJ update passed 437 workspace library tests and 182 editor
  integration tests at eight-way concurrency. Its 16 native tests also ran
  without executables on PATH and with the standalone JJ feature. They cover
  disk changes, renames, merged-parent conflicts, bookmarks, ignores, linked
  workspaces, trust, and unchanged operations/refs/working-copy state.
- Native file delivery was tested with a symlink-aliased buffer path and polling
  disabled, including external writes and atomic replacements.
- Filesentry's Linux suite passed 19 tests with `queue_overflow` excluded. The
  overflow stress test twice failed to trigger the queue overflow it expected;
  no lost-event assertion failed. Filesentry Clippy reported an existing
  `unnecessary_sort_by` warning in a test.
- A picker regression reproduced SIGABRT when a native thread tried to resolve
  a Tokio-runtime-local redraw notification, followed by a second panic in a
  drop guard. Capturing the editor's notification before spawning work fixed
  that regression and passed the eight-way suite. This demonstrated an
  off-runtime redraw bug, not a worker-limit diagnosis; the earlier Rayon abort
  was not independently traced.
- Interactive Linux checks have exercised Steel initialization, icons, search
  status, trimming, shutdown, config lookup without wrapper overrides, and
  Steel/Nix language-server initialization.
- Native macOS/Windows behavior is not verified here. The Linux Steel language
  server has also crashed while indexing the Helix source tree; basic NixOS
  config-file initialization succeeded. That is separate from editor startup.

The editor integration command used during development was:

```sh
cargo test -p helix-term --features integration \
  --test integration -- --test-threads=8
```

## Maintaining the fork

Keep upstream imports and fork adaptations in understandable commits. When
advancing to a newer upstream master, rebase the owned stack and resolve API
changes within the affected feature, then rerun the checks above.

For a conventional Git checkout, the equivalent workflow is:

```sh
git fetch upstream master
git switch neohelix
git rebase upstream/master
```

For a Jujutsu-managed checkout, rebase the feature stack onto the fetched upstream
revision using JJ and move the `neohelix` bookmark to the validated tip. Preserve
unrelated working-copy changes.

Do not regenerate this branch by blindly merging all source heads. Several
sources were drafts against older APIs, issue #1125 has no PR head, and this fork
contains fixes and behavioral decisions beyond the original patches.
