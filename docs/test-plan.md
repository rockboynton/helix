# Neohelix acceptance walkthrough

Mark each checkbox **pass / fail / skipped**, and record failures as
`build + OS + terminal/Zellij version + action + expected/actual`. Record
`hx --version` and your config revision first. This is a manual acceptance plan,
not a claim that every platform has passed it.

## Setup: disposable data, real config

Run this in Bash (`bash` first if your login shell is Fish), from the Neohelix
checkout, using the binary you want to test:

```sh
HX=hx                         # or an absolute checkout release-binary path
TESTROOT=$(python3 contrib/neohelix-test-fixture.py --large-mib 16)
printf '%s\n' "$TESTROOT"      # keep this path for every terminal used below
"$HX" --version
cd "$TESTROOT/git"
"$HX" src/lib.rs
```

Use `--without-jj` on the fixture helper if JJ is unavailable. It creates only
temporary local repos, directory snapshots, Git/JJ patches, three-way variants,
a CSV and a PNG. No real repo, remote, user config or bookmark is modified.
Commands below run from `$TESTROOT/git` unless another directory is specified.
Keep all editing/history experiments inside these fixtures. Use a fresh fixture
when an earlier test changed inputs. **No failure should cost real work.**

Keys assume the [optional user keymap](keybindings.md); commands work without it.
Test once in direct Ghostty/Kitty, then repeat relevant checks in Zellij. Use
Zellij locked mode when Alt keys are intercepted. A checkout binary needs its
matching runtime; installed/Nix binaries already supply that runtime.

**For unsaved-edit tests, disable autosave in every editor:**
`:set auto-save.focus-lost false`, then `:set auto-save.after-delay.enable false`.
Repeat after config reloads. Do not mistake autosave + filesystem reload for live
shared-buffer synchronization.

## 1. Startup, configuration and ordinary editing

- [ ] **Semantic colors:** in Rust, wait for rust-analyzer, then run
  `:toggle lsp.semantic-tokens`. Types, fields and resolved symbols use your
  theme; syntax colors remain immediate with the option off or the server busy.
  Edit and restart the server; old token positions must not color unrelated text.
- [ ] **Commit gutters:** edit and save a fixture file, leave another buffer
  dirty, then `jj commit -m test` externally. Saved changes lose their gutter
  markers automatically; unsaved text and cursor positions remain untouched.

- [ ] **Steel/trust:** decline any initial trust prompt. Before trusting, `:get text-width` shows your
  user value. Run `:workspace-trust`; it becomes **88** from the fixture's project
  Steel layer. `:workspace-untrust` restores the user layer; trust again for later
  tests. Neither command demands a TOML file. `:config-open` opens your active config.
- [ ] **Reload/display:** `:config-reload` retains your theme, icons and bindings.
  Check relative numbers, cursor shapes, indent guides, search-position count,
  and transparent background in Zellij. Re-disable autosave afterward.
- [ ] **Selections:** select a subword in `my_http_value`, try your subword motions,
  use the command picker to switch case (snake/camel/Pascal) in the README, and
  try closest-pair selection on `((nested))`.
  Only the intended text/range changes; undo restores it.
- [ ] **Trim/history:** select the trailing-whitespace comment and run `:trim`;
  only selected trailing spaces disappear. Run several commands, open command
  history (`Space :`), then yank different selections and open register history
  (`Space H` in your config). Entries can be selected/reused.
- [ ] **Language overrides:** open Rust, Scheme and `data.csv`. Normal syntax,
  CSV quoted-field highlights and your configured language servers/formatters
  apply. On a Rust symbol, your `Space K` opens hover docs in a buffer.

## 2. Pickers, tree and safe external reload

- [ ] **Previews:** rapidly move through file-picker entries, including Rust,
  Markdown and CSV; scroll the preview. Syntax colors remain theme-correct; no
  plaintext-then-highlight flash or stale previous-file result. A temporary loading
  placeholder is allowed, not a zero-latency guarantee.
- [ ] **Diagnostics:** introduce an unsaved Rust type error; open diagnostics and
  narrow the list. The selected message wraps inside its column. `Alt-]`/`Alt-[`
  scroll a tall message without covering the preview; selection resets scrolling.
  Undo the error.
- [ ] **Tree:** `Space e` toggles visibility; `Ctrl-w e` toggles focus both ways;
  `Space E` still opens the current-buffer-directory explorer. Test icons, expand/
  collapse, `/lib` filtering, `n`/`p`, Enter, Escape, `H`/`L`, and `?`. Selection
  and expansion survive hiding/showing; clearing a filter restores browsing state.
- [ ] **Watcher:** in another terminal, edit or atomically replace a clean fixture
  file. Helix updates promptly and undo can restore prior text. Repeat with an
  unsaved edit in Helix: local text must survive. Test external deletion too.
  Use normal mode and autosave off; do not wait 30 seconds as the expected latency.
- [ ] **Refactoring:** `:global-refactor NEOHELIX_SAMPLE`; edit the matching text,
  keeping result lines intact; `:refactor-apply` updates source buffers without
  saving. Undo there. Repeat after externally changing an affected source:
  stale results must be rejected, not silently applied.

## 3. Editable diffs, complete reviews and folds

- [ ] **Two-file split:** `:ds ../before/src/lib.rs src/lib.rs`. Expect two columns,
  your normal syntax/gutter colors, muted line fills, stronger changed-word fills,
  aligned blank rows and synchronized scrolling. Repeat `:ds`: no third pane.
  Compare `../before/bindings.kdl` with `bindings.kdl`: swapped actions emphasize
  changed words, not the unchanged Ctrl/Mod key names or whole multi-line prefixes.
- [ ] **Transfers:** move to a hunk, use `:diff-get`, undo, then `:diff-put` and
  undo in the destination. The intended hunk changes; neither action saves.
- [ ] **Inline:** `:diff-off`, then launch
  `"$HX" --diff --diff-layout inline ../before/src/lib.rs src/lib.rs`.
  Edit working text, navigate hunks and undo. Deleted baseline rows stay virtual;
  saving the working file must not insert deleted text or alignment rows.
- [ ] **Changeset:** `:dr` (`Space g r`) shows changed/added/deleted/renamed paths.
  Switch files, change inline/split layout and edit multiple working files.
  Edits remain when switching; baselines stay read-only. Git rename entries may
  appear as add/delete; JJ uses native rename handling.
- [ ] **Refresh/exit:** with a review open, externally add/change a fixture file.
  Catalog/clean files refresh; dirty buffers survive. `:diff-off` keeps the working
  file on the left, removes comparison panes, restores the prior sidebar state
  and retains all unsaved buffers/unrelated panes.
- [ ] **Three-way:** launch `"$HX" --diff3 ../three/left.rs ../three/base.rs ../three/right.rs`.
  Base is centered; outer panes align with it. Transfer a hunk between a focused
  outer pane and base; undo. Exit while an outer pane is focused: that variant survives.
- [ ] **Diff folds:** `:diff-fold 0` hides unchanged context in split, inline and
  three-way views. `:fold-open` expands paired context; search into hidden text
  reveals it. Editing/undo opens invalidated folds; saving contains full source.
- [ ] **Code folds:** outside comparisons, use `z f t`, `z f T`, open an enclosing/
  nested fold, search into it, and try `:fold-selection` on plain text. Folding is
  view-local; editing invalidates stale folds, and saves never omit hidden text.

## 4. Pager, patches, JJ and jjui

- [ ] **Pager:** `"$HX" --pager src/lib.rs`, then
  `git show HEAD:src/lib.rs | "$HX" --pager --language rust`. Both highlight;
  movement/search/yank work. Insert, undo, write/save-as and move cannot mutate
  files. `q` exits cleanly. Piped input waits for EOF; follow/ANSI-man support is not expected.
- [ ] **Large file:** `"$HX" --pager ../large.txt`; record first-paint latency/RSS,
  search near EOF and exit. It loads the complete input, not bounded streaming
  storage; do not confuse that limit with failed small-file pager behavior.
- [ ] **Both patch formats:** open `../patches/git.patch` and `../patches/jj.patch`
  with `"$HX" --patch PATH`, in inline and `--diff-layout side-by-side` layouts.
  Navigate files/hunks; all patch text stays read-only and referenced source paths
  are never opened for editing. Also pipe each producer to `"$HX" --patch`.
- [ ] **Native JJ:** from `$TESTROOT/jj`, open `src/lib.rs`; trust the workspace.
  `Space g g` finds changed files, including when Helix starts outside the repo
  and opens an absolute file path. `:jjr` selects revisions: `@` is editable,
  the parent is read-only; switching revisions retains layout and unsaved buffers.
  Sidebar `r` reopens the revision picker. No checkout/history rewrite occurs.
- [ ] **jjui display:** with your configured tools, browse several previously unseen
  revisions. Preview is inline, full diff is split; colors match your theme and
  long lines wrap. No binary garbage, broken terminal state or stale prior revision.
- [ ] **jjui editing:** only in the JJ fixture, use Shift-E / `jj diffedit` through
  your configured Neohelix editor. Save a deliberate edit, exit and confirm jjui
  returns and JJ imports it once. This **can rewrite revisions/rebase descendants**.
- [ ] **Performance:** compare matched Delta/Neohelix layouts, widths and revisions
  using the [benchmark harnesses](diff-render-performance.md). Record stopped-worker/
  empty-cache startup, first unseen diff after initialization, and revisit separately.
  The target is within 50 ms of Delta in the stated matched case, not a universal
  guarantee. Use private benchmark caches; do not clear your normal caches.

## 5. GitLens, blame and media

- [ ] **Local blame:** on a committed fixture line, `Space B` opens compact details.
  Near the screen bottom it expands upward. `y` copies the hash; `d`, then `d`
  returns from full diff with scroll/selection retained. This repo has no remote:
  local details should work without a token. `Space g b` independently toggles a
  theme-colored blame gutter that remains aligned while scrolling and editing.
- [ ] **Inline blame:** temporarily enable `inline-blame.auto-fetch`, then test
  `:set inline-blame.show cursor-line`, `all-lines` and `never`. Placement follows
  the selected mode; record scrolling lag rather than assuming fetching is free.
  Restore `never` and your prior auto-fetch value afterward.
- [ ] **GitHub/GHE:** on a known committed line in a trusted real repo, inspect
  verified PR details; Tab/Shift-Tab select commit/PR links, `c` copies and Enter
  opens the selected URL. A fork must not redirect validated upstream PR URLs to
  origin. With an environment token, no Keychain prompt should be needed. Offline/
  failed lookup keeps local details and labels unverified hints honestly.
- [ ] **Cache invalidation:** revisit a line after its responsible commit changes
  in the fixture; blame reflects the new commit, not the old PR. Unsaved/uncommitted
  text is not falsely attributed to an unrelated commit. `:gitlens-clear-cache`
  clears saved PR details.
- [ ] **Media:** launch Helix from `$TESTROOT/media`; picker-preview `sample.png`
  and a multipage PDF you copy there.
  With `pdftoppm` installed, pages fit, page scrolling works and rapid selection
  changes do not leave old images behind. Repeat in patched Zellij. Unsupported
  terminals/missing tools show a fallback, not garbage. Ordinary document opening
  is not a dedicated image/PDF viewer yet.

## 6. Shared LSP: two actual editor processes

Open the **same fixture workspace and file** in two panes using the same binary,
config and effective environment. Trust it in both; disable both autosave modes.
Rust-analyzer must be installed. Use OS process inspection to distinguish this
workspace's backend from other projects and Neohelix's small proxy/broker helpers.

- [ ] **Sharing/default:** both have completion, hover and diagnostics, but only
  **one rust-analyzer backend**. The second editor avoids a second full index.
- [ ] **Unsaved sync:** type in A; B updates **before any disk write**, with independent
  cursor/scroll. Type in B, undo, and try interleaved inserts. Both converge; use
  `git diff`/disk inspection to verify this is not filesystem reload.
- [ ] **Workspace edits/save:** LSP-rename `greet` across the two Rust files while
  one has unsaved peer edits. Both update once without losing those edits. Move a
  file using Helix: the peer path follows without saving its contents. Save matching
  text: the peer's modified marker clears only if its text matches that save.
- [ ] **Lifetime/isolation:** close A; B's LSP still works. `:lsp-restart` in B starts
  a private backend without disturbing another editor. In a fresh session with
  `lsp.shared = false` in both configs, each editor gets an independent backend.
  Restore your config afterward; closing all fixture editors reaps their backends.

Skip optional external integrations explicitly rather than marking them passed.
Keep performance numbers separate from functional results. Use the linked guides
for limits: workspace edits are not globally atomic, arbitrary external renames
are not tracked, and native macOS is not validated merely by a Linux test run.
