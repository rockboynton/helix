# Reusable diff renderer measurements

## Actual jjui startup and navigation

Final local implementation, packaged Nix runtime, normal user Steel/Gruvbox,
three-trial Linux medians in milliseconds, shared config/query resources primed:

| Painted view | Delta | Neohelix automatic worker | Difference |
| --- | ---: | ---: | ---: |
| Startup, `930c788a5` | 348.69 | 401.90 | +53.21 |
| Next unseen, `e25ef77fa` | 297.36 | 256.44 | -40.92 |
| Next unseen, `f36d57681` | 243.57 | 276.92 | +33.35 |
| Next unseen, `1891050b8` | 206.71 | 211.15 | +4.44 |
| Full split view, `1891050b8` | 165.61 | 134.58 | -31.03 |

**Automatic launch is close but still misses +50 ms by 3.21 ms in this run.**
Subsequent unseen revisions meet that target. Do not claim a universal or native
macOS guarantee from three Linux PTY trials. The optimization removes redundant
initial dependency scans, reusing the request-start snapshot for native-profile
restoration. Executed configuration is still revalidated after initialization;
every render still starts with a live dependency/trust check. A concurrent edit
during a data-only profile hit is observed on the next request, like subsequent
retained-session requests.

`contrib/bench-jjui-startup.py` measures an actual jjui 0.10.11 process in a
160-column, 40-row PTY: launch to first painted inline preview (80 columns),
then `j` to each previously unseen revision, then `d` to the final split view
(160 columns). Unlike the initialized-worker table below, **launch timings
include concurrent background warmup**. The preview and full view must use one
PID without fallback; the harness checks that and stops it afterward.

The instrumented formatter captures output and prepends a unique marker for
both tools. A `pyte` screen checks that marker after terminal redraws, not just
its appearance in the raw escape stream. Measurements therefore include common
Python instrumentation, jj export/capture, jjui debounce and painting overhead.
They are not bare formatter timings or native Ghostty/Zellij/macOS validation.
The harness uses a private shared-object clone, isolated JJ/jjui settings and
diff caches, and the normal user Steel/Gruvbox configuration. No OS cache flush.
Each mode/trial visits each revision once; no measured revision is preloaded.

Before redundant initialization scans were removed, three-trial launch medians
were 398.19 ms with checkout runtime versus Delta's 348.52 ms, but **452.35 ms
with packaged runtime versus Delta's 346.98 ms**. Thus the checkout result was
borderline (+49.67 ms), and the packaged result failed the +50 ms target
(+105.37 ms). Later unseen previews were within 35 ms of Delta or faster;
the wider full view reused the same worker. Do not substitute checkout results
for packaged-runtime results.

Removing the first two redundant dependency scans reduced packaged startup to
414.34 ms versus Delta's 348.64 ms (+65.70 ms). Configuration-only warmup then
regressed it to 481.38 ms versus 349.39 ms and was rejected: loading syntax
resources before the real diff helps more than avoiding disposable warm trees.

With the final implementation and empty diff-profile/query/highlight caches,
packaged startup was **898.82 ms versus Delta's 349.28 ms**. Later previews
measured 262.97/276.69/211.97 ms versus Delta's 292.75/238.94/208.39 ms; full
view was 134.46 versus 165.80 ms. Cold-cache startup still fails the target.
This case does not flush global Steel AOT or OS caches and does not represent
a completely fresh installation. The pre-optimization startup was 963.78 ms.

The initialized-cache comparison primes only native rendering configuration and
Rust/Markdown query resources with unrelated tiny files; it then removes source
style entries. It does not assume ordinary editor startup already does this.

Reproduce with Python containing `pyte` (for example a Nix Python environment
with `python3Packages.pyte`), Delta, Git, JJ and the real **unwrapped** jjui:

```sh
HELIX_RUNTIME=/path/to/packaged/runtime python contrib/bench-jjui-startup.py \
  --hx target/rust-1.99/release/hx --jjui /path/to/real/jjui \
  --launcher /path/to/nixos/jjui/launch.sh \
  --iterations 3 --count 4 --full-view --modes delta auto-worker
```

Add `--cold-resources` for empty diff caches. The launcher comes from the
companion NixOS configuration; its real executable paths are supplied by the
harness. See the [shared-viewport contract](diff-render-worker.md#starting-alongside-jjui).

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
live macOS navigation remain separate verification tasks. The PTY measurements
above cover automatic jjui session-start initialization on Linux. See
[worker usage](diff-render-worker.md).

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
