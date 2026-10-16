# Neohelix user keymap

These are the author's optional Steel keybindings in the NixOS configuration,
not new hard-coded Helix defaults. `z` remains the normal view menu; `z f` is
its folding submenu. Existing centering and alignment keys are preserved.

## Folding (`z f`)

| Key | Command |
| --- | --- |
| `t` | `:fold-toggle` (fold at cursor) |
| `T` | `:fold-toggle-all` (all syntax folds) |
| `c` / `o` | `:fold-close` / `:fold-open` |
| `s` | `:fold-selection` |
| `d` | `:diff-fold` (unchanged comparison context) |

`T` opens syntax folds if any are closed; otherwise it closes them all. It does
not open unrelated manually selected regions. There are no separate close-all
or open-all shortcuts, but `:fold-all` and `:fold-open-all` remain available.

## Git and diffs (`Space g`)

| Key | Action |
| --- | --- |
| `g` | Changed-file picker (previously `Space g`) |
| `r` | Review working-copy changes inside Helix |
| `[` / `]` | Previous / next changed file |
| `l` | Toggle inline / side-by-side review layout |
| `G` | Get the other side's hunk |
| `p` | Put this side's hunk into the other side |
| `q` | Finish comparison and restore previous sidebar |
| `R` | Reset VCS diff change |
| `i` / `s` | External inline / split `jj diff` in a floating Zellij pane |
| `t` | Open jjui in Zellij, then reload buffers on return |
| `b` | Toggle the full-file blame gutter |
| `u` / `o` | Copy / open source browser link (existing `gh` commands) |
| `c` | Search conflict markers using the existing Zellij action |

This replaces the old `' g` and `' g d` menus. Hunk navigation remains `[g` /
`]g`; `Space v` remains the built-in review shortcut. `Space B` still opens
GitLens, with its own popup keys. The existing link-copy command uses `pbcopy`
and is unchanged; it requires that command to be available.

## Sidebar and refactoring

| Key | Action |
| --- | --- |
| `Space E` | Toggle sidebar visibility, retaining expansion, selection and review state |
| `Space e` | Original file explorer |
| `Space Alt-r` | Open interactive workspace refactoring |
| `Space Alt-R` | Apply refactoring results to source buffers, without saving |
| `Space W` | Trim trailing whitespace in current selections |

`Space R` keeps Helix's clipboard-replacement action. The new Alt-r / Alt-R
chords do not collide with the checked Zellij config or inherited defaults.
Alt-w is reserved by Zellij for floating panes, so trim deliberately uses `W`.
Space does not prevent Zellij from intercepting the next modified key.

Existing picker Alt-j/k and diagnostic Alt-[/] shortcuts are also intercepted
by this Zellij configuration. Use Shift-Up/Down or Page-Up/Down for preview
scrolling, and Zellij locked mode (`Ctrl-g`) when those Alt keys must reach Helix.
These pre-existing multiplexer bindings have not been changed.

## Short path commands

Aliases share the original command's argument rules and path completion.

| Alias | Full command | Example |
| --- | --- | --- |
| `:ds` | `:diff-split` | `:ds before.rs after.rs` |
| `:d3` | `:diff-three` | `:d3 left.rs base.rs right.rs` |
| `:dr` | `:diff-review` | `:dr before-dir after-dir` |
| `:drp` | `:diff-review-patch` | `:drp changes.patch` |
| `:ft` | `:file-tree` | `:ft src` |

`:ds other.rs` compares against the current file. `:dr` without paths reviews
the current workspace. `:ft` opens/focuses the sidebar; `Space E` toggles it.
