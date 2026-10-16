# Reusable diff renderer measurements

## Parallel retained renderer: 2026-10-16

Acceptance target: at most **50 ms slower than Delta** for the first view of an
unseen diff after shared initialization. This is not a claim about a completely
cold installation, a stopped worker, or native macOS performance.

Final Linux release, packaged Nix runtime, normal user Steel/Gruvbox config,
width 160, eight adjacent revisions visited once per sequence, three trials.
No concurrent builds or garbage collection. Shared configuration/query disk
resources were explicitly primed; source-style caches started empty per mode
and sequence. `--diff-worker-warm` initialized the worker with unrelated tiny
Rust/Markdown inputs, not the revisions measured below. Temporary snapshot paths
differed for every revision. Worker traces and PIDs confirmed reuse without
fallback. All outputs matched cache-disabled, non-incremental full-forest
reference rendering byte for byte.

Three-run medians, milliseconds:

| Revision | One-shot inline | Worker inline | Delta inline | One-shot split | Worker split | Delta split |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `930c788a5` | 190.74 | 147.59 | 106.30 | 190.38 | 148.07 | 116.80 |
| `e25ef77fa` | 145.60 | 93.39 | 131.20 | 141.73 | 94.92 | 136.59 |
| `f36d57681` | 142.38 | 111.63 | 82.35 | 143.70 | 112.05 | 87.05 |
| `1891050b8` | 87.07 | 63.55 | 59.38 | 88.37 | 63.37 | 61.68 |
| `b99057045` | 212.29 | 156.48 | 166.13 | 214.45 | 158.54 | 173.03 |
| `897feed4e` | 52.29 | 32.39 | 54.02 | 52.53 | 32.15 | 55.18 |
| `469aaa72e` | 69.45 | 47.45 | 52.37 | 70.20 | 47.28 | 52.88 |
| `fce83e615` | 87.53 | 56.33 | 59.71 | 87.51 | 56.47 | 60.82 |

**The initialized worker meets the +50 ms target on all 16 comparisons.**
Its largest median deficit is 41.29 ms inline and 31.27 ms split. This measures
unseen revisions, not repeatedly rendering the same diff. Identical file contents
across adjacent revisions can legitimately reuse syntax/styles; new contents
still undergo complete-source parsing or validated incremental updates.
Worker RSS was approximately 76–115 MiB over these sequences, including trees.
Explicit warmup with already-primed shared disk resources took about 131 ms;
that cost is excluded from the table and is not first-install startup time.
Ordinary editor startup does not yet perform this warmup automatically.

Improvements: parallel file rendering in retained sessions; bounded largest-first
scheduling with ordered output; an independently bounded shared syntax cache;
up to two additional before/after parser threads for medium-sized pairs; and
server-only dependency validation with protected Nix-store metadata summaries.
Trees/source remain in memory, not serialized. Source style disk writes remain
after each file's render. Full outer trees, locals and whole selected injection
groups are retained, rather than parsing isolated patches.

Reproduce (replace `HELIX_RUNTIME` with the packaged runtime or checkout runtime):

```sh
HELIX_RUNTIME="$PWD/runtime" python contrib/bench-diff-worker-sequence.py \
  --hx target/rust-1.99/release/hx --iterations 3 --count 8 --warm-worker --memory
```

Omit `--warm-worker` to include process/renderer initialization in request zero.
The fresh one-shot path still misses this target for some files; do not describe
these retained-worker results as worker-free cold-start parity. Native macOS,
live jjui navigation and automatic session-start initialization remain separate
verification/integration tasks. See [worker usage](diff-render-worker.md).

## Stopped-worker launch

A separate three-trial run with the same packaged runtime, shared disk resources
primed, and no worker warmup measured request zero at **245.69 ms inline versus
105.93 ms for Delta**, and **239.56 ms split versus 114.86 ms for Delta**.
The stopped-worker launch does **not** meet the +50 ms target. The table above
requires explicit prior initialization; neither first-install cache misses nor
normal editor startup are being hidden inside that claim.

## Earlier experiments

The [historical measurements](diff-render-performance-history.md) record the
serial worker, injection selection, Delta source audit, disk-cache experiments
and phase profiling. Those implementation descriptions and timings are historical,
not the current renderer contract.
