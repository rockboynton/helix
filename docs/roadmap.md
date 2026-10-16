# Neohelix development queue

## Active: cold flow-diff performance

Target: within 50 ms of Delta on matched file/revision/layout/width comparisons
for the first view of unseen source after shared initialization. The user now
allows a retained renderer; report stopped-worker startup and cold-cache misses
separately, not as unseen-diff timings after initialization. Preserve Steel-defined rendering configuration,
Tree-sitter highlighting, word diffs and wrapping.

The relevant target is the first view of previously unseen source contents after
shared initialization, not fast revisits. Bounded invocation-local parallelism,
incremental before/after parsing and syntax-derived injection selection improve
this. Injection selection retains complete outer
trees/local scopes and whole selected injection groups; it does not parse hunks
in isolation. Measured first-source inline gains from selection are 10–20%.
The [Delta source audit and matched-input measurements](diff-render-performance.md)
separate complete-source parsing from the existing patch-fragment frontend.
Normal editor startup does not yet populate every formatter configuration/query
cache; benchmark priming must not be described as that integration being done.

The opt-in worker now retains parallel rendering and a bounded, cross-revision
syntax cache independent of displayed coverage. Largest-first scheduling and
bounded pair parsing improve the critical path. Explicit `--diff-worker-warm`
initializes shared resources without preloading the revision being tested.
Keep the default worker-free; opting in is explicit. See the performance notes
for the current matched sequence, startup distinction and reproducible harness.

## Queued

- Native JJ workspace: the revision picker now drives the existing review UI;
  see [usage and boundaries](jj-workspace.md). Next are a persistent graph/list,
  selection-driven previews, and explicit, buffer-safe history operations.

- Share compatible LSP instances between independent Helix processes, including
  separate Zellij panes. Design workspace/server/config identity, document
  ownership and version coordination, process lifetime, and isolation first.

## Implemented locally, pending release

- Full-file Git/JJ blame gutter highlights author, date, commit and uncommitted
  metadata separately, with theme-aware defaults. Source syntax is unchanged.
