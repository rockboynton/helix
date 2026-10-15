# Neohelix review cheat sheet

## Start a review

```sh
hx --review                         # Git HEAD / JJ parent vs working copy
hx --review before-dir after-dir    # Full directory changeset
hx --diff before.rs after.rs        # Two editable source files
hx --diff3 left.rs base.rs right.rs  # Three editable files, base in the middle
hx --diff --diff-layout inline before.rs after.rs
git diff | hx --patch
jj diff --color-words --color never | hx --patch
```

File paths select the normal Helix language/Tree-sitter configuration. Patches
accept Git unified and native JJ markers, are read-only, and never open paths
named inside the patch. Inline file/review views edit the real working document;
deleted baseline rows are virtual and read-only. `--pager` makes file inputs read-only.

## Inside the editor

| Action | Keys / command |
| --- | --- |
| Working-copy review | `Space v`, `:diff-review [repo]` |
| Directory review | `:diff-review before-dir after-dir` |
| File sidebar | `Space E`, `:file-tree [directory]` |
| Original file picker | `Space e` |
| Switch focused pane | `Ctrl-w h` / `Ctrl-w l` |
| Next / previous hunk | `]c` / `[c`, `:diff-next` / `:diff-prev` |
| Next / previous file | `:diff-file-next` / `:diff-file-prev` |
| Toggle review layout | `:diff-layout` |
| Choose review layout | `:diff-layout inline` / `:diff-layout side-by-side` |
| Copy other side's hunk into this side | `:diff-get` |
| Copy this side's hunk to the other side | `:diff-put` |
| Finish comparison | `:diff-off` |
| Manual refresh fallback | `:diff-review-refresh` |

Transfers are undoable and never save automatically. Review baselines cannot be
edited; working text can. In three-way mode, transfers compare the focused outer
pane with base. Choose an outer pane before transferring.

`diff-off` keeps the working file in the leftmost comparison pane, closes the
extra comparison panes, and restores the sidebar that preceded the review.
Buffers and unsaved edits remain available. In three-way mode it keeps the
focused outer variant (right if base was focused). Unrelated panes remain open.

Split comparisons reuse existing adjacent columns rather than accumulating
panes. File changes refresh reviews automatically when watching is enabled:
catalog updates are debounced, clean files reload, and dirty buffers are retained.
Piped patches are immutable snapshots. Manual refresh is still useful if native
watching is disabled or unavailable.

## Sidebar navigation

`j` / `k` select; `h` collapses or goes to parent; `l` expands; Enter opens a file
or toggles a directory. `g` / `G` select first / last. Tab or Escape returns to
editing. `r` refreshes. In review trees, `n` / `p` change file and `v` toggles layout.
`:file-tree-close` closes the sidebar without closing documents.

## Colors and word changes

Diffs keep normal Helix gutter icons and syntax foreground colors. Muted line
backgrounds show additions/removals; stronger word backgrounds emphasize changed
tokens within replacement blocks. This works in split, inline, three-way and
patch views. Large replacement blocks fall back to line-only coloring.

Theme overrides: `ui.diff.minus`, `ui.diff.plus`, `ui.diff.minus.word`,
`ui.diff.plus.word` (set `bg`, not the syntax foreground).
Word highlighting is textual, not a compiler-semantic analysis.

## Picker readability

The active diagnostic message wraps within its picker column instead of being
hidden behind the preview. `Alt-]` / `Alt-[` scroll long messages; moving to a
different diagnostic resets the scroll. File previews show a loading state until
asynchronous Tree-sitter highlighting resolves, with plaintext fallback if no
parser is available.

## GitLens

With your `B` binding, open on-demand blame details without continuously rendering
inline blame. Within the popup:

| Key | Action |
| --- | --- |
| Tab / Shift-Tab | Select commit / PR link |
| Enter | Open active link |
| `c` | Copy active link |
| `y` | Yank commit hash |
| `d` | Toggle full commit diff / details |
| Escape | Close |

PR lookups use native Octocrab HTTP requests with GitHub Enterprise host support.
Environment tokens are checked before saved `gh` credentials / native credential
storage. For custom Enterprise hosts use `GH_ENTERPRISE_TOKEN` or
`GITHUB_ENTERPRISE_TOKEN`; GitHub.com uses `GH_TOKEN` or `GITHUB_TOKEN`.
Native access to a token saved by `gh` can trigger a macOS Keychain permission
prompt for `hx`. Local blame still works without API authentication. Do not put
tokens in version-controlled config.
See [token creation and required permissions](gitlens.md#creating-a-token-without-keychain)
for GitHub/Enterprise setup and the Keychain-free launch instructions.

## Render output for jjui / other pagers

`--render-diff` writes read-only formatted output, not an embedded interactive
Helix terminal. It supports inline / side-by-side layouts and patch input; the
host controls navigation. See the [full file-diff guide](file-diff.md) for its
width, context and color flags and the [changeset guide](diff-review.md) for
review boundaries. Staging, arbitrary revision browsing and full semantic
changeset analysis are not implemented.
