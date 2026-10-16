# Local range-aware parsing patch

Base: crates.io `tree-house` 0.4.0, MPL-2.0, upstream
<https://github.com/helix-editor/tree-house>. The upstream Rust source,
manifest, README and license are retained here for a reproducible local build.
No new external fork has been published.
The crate's recorded upstream revision is
`75d213c4033ccf3ea98a5539d7d7c4005d8fb2ea` (repository path `highlighter`).

Local API additions:

- `Syntax::new_for_ranges` and `Syntax::update_for_ranges` build a rendering-only
  forest with complete outer syntax and complete local-scope analysis.
- Injection queries still inspect complete parsed parent trees. Selected
  injections are identified by their syntax-derived matched-node ranges.
- A selected injection layer keeps all its source ranges. Combined injections
  are never clipped to a hunk; omitted ranges may contain required fences,
  definitions or other context.
- Unselected injection layers are not parsed and are pruned from the resulting
  forest. Descendant injections are selected recursively under the same rule.
- Existing `new`/`update` calls retain their full-forest behavior and traversal
  path. The range API is not used for interactive editing.

Callers must not query the selective forest outside the requested ranges. When
coverage changes, update with the new source's ranges, including when no text
edits occurred. Failed selective/incremental construction falls back to full
parsing in Neohelix's read-only renderer.

These additions are isolated in `src/lib.rs` and `src/parse.rs`. A future pinned
dependency fork or upstream release can replace this vendored crate without
changing the editor-side contract.
