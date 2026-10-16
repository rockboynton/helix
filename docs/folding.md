# Code folding

Folds are local to each view and never remove text from the document. Opening a
file does not collapse anything automatically. Syntax folds use the existing
Tree-sitter parse trees, including injected languages, without an LSP request.

| Command | Action |
| --- | --- |
| `:fold-close` | Close the innermost open syntax region containing the cursor |
| `:fold-toggle` | Open a closed fold on its heading/summary, otherwise close a syntax region |
| `:fold-all` | Close all syntax regions, retaining nested closed state |
| `:fold-toggle-all` | Open all syntax folds if any are closed, otherwise close all; preserve unrelated manual folds |
| `:fold-open` | Open the fold on its heading/summary; closed children remain collapsed |
| `:fold-open-all` | Open all folds, including hidden descendants |
| `:fold-selection` | Manually fold selected lines, even without a language query |

The first line of a syntax capture stays visible as its heading, followed by a
summary row replacing at least two complete lines. A final unterminated line
stays visible. Repeated `:fold-close` closes enclosing regions. Search/goto into
the hidden interior reveals enclosing folds; ordinary vertical movement visits
the summary and then skips its hidden lines. As with manual folds, selections
and operators still address real text: a selection spanning a fold includes its
hidden contents. Saving always writes the complete file.

Edits, undo and reload invalidate this view's folds rather than risk concealing
changed text. Run a fold command again afterward. Folds are not persisted across
restarts. Other views of the same document can have different folds.

## Languages and queries

Bundled queries cover Rust, Python, JavaScript, TypeScript/TSX, JSON, Scheme and
Nix. Add `queries/<language>/folds.scm` under a runtime directory to support more
languages. The usual query inheritance and runtime precedence apply. Capture
foldable nodes with `@fold`, for example:

```scheme
[(block) (declaration_list)] @fold
```

Single-line captures are ignored; duplicate regions are coalesced. Properly
nested ranges are supported; crossing captures are discarded. Queries are
compiled lazily and cached per language. Discovery runs only on explicit fold
commands, not during rendering. `cargo xtask query-check` also validates fold
queries. There is no indentation fallback or LSP folding-range provider yet.

## Keybindings and diffs

No existing keys are replaced. Bind commands in TOML or Steel as usual; for
example, in a Steel keymap hash:

```scheme
"C-f" ":fold-toggle"
```

The author's [optional keymap](keybindings.md) uses `z f t` for a single fold
and `z f T` for all syntax folds.

Syntax folding currently applies to normal views, including read-only pager
files. In comparisons, use `:diff-fold [N]` instead: independent syntax folds
would destroy pane alignment. See [diff context folding](file-diff.md#folding-unchanged-context).
