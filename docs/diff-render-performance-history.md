# Historical diff renderer experiments

These are development-time measurements, preserved for provenance. For the
current implementation, acceptance target and results, see
[diff renderer performance](diff-render-performance.md).

## Historical serial worker across unseen revisions: 2026-10-16

The following measurements describe the earlier serial implementation, before
the parallel retained-renderer changes above.

Benchmarked eight adjacent first-parent revisions, newest to oldest, starting at
`930c788a5`. Each sequence visits every revision **once**, with a new worker per
trial/layout, three trials and alternating mode order. Complete before/after
snapshots use different directories for every revision, as jj exports do. Normal
user Steel/Gruvbox configuration, width 160, Linux release, no concurrent builds.
Shared disk configuration/query resources were explicitly primed; source-style
caches started empty for each sequence and remained available within it. This is
not an entirely cold installation or an OS-cache-flushed benchmark.

The worker's **first request includes starting its process and renderer**;
subsequent rows are different, previously unseen revisions, not repeat views.
Client traces confirmed worker use with no one-shot fallback, and PID files
confirmed the same process handled every request within a sequence. Each output
matched cache-disabled, non-incremental, full-forest reference output exactly.
All benchmark workers were stopped afterward. Native macOS is unmeasured.

Three-run medians, milliseconds:

| Request / revision | One-shot inline | Worker inline | Delta inline | One-shot split | Worker split | Delta split |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 0 / `930c788a5` | 260.06 | 542.30 | 106.44 | 260.57 | 545.38 | 119.03 |
| 1 / `e25ef77fa` | 219.95 | 281.09 | 130.41 | 220.42 | 281.61 | 136.02 |
| 2 / `f36d57681` | 193.74 | 220.22 | 80.54 | 194.84 | 227.75 | 87.80 |
| 3 / `1891050b8` | 74.55 | 92.23 | 60.07 | 74.28 | 91.55 | 61.99 |
| 4 / `b99057045` | 323.04 | 519.28 | 167.36 | 327.22 | 530.09 | 177.99 |
| 5 / `897feed4e` | 39.09 | 32.32 | 55.38 | 37.41 | 32.61 | 54.90 |
| 6 / `469aaa72e` | 57.27 | 71.49 | 52.60 | 57.20 | 72.89 | 53.47 |
| 7 / `fce83e615` | 73.38 | 82.95 | 59.11 | 73.08 | 84.24 | 61.77 |

**The existing worker is not a performance win for this unseen-revision suite.**
It retains the loader/configuration and a bounded exact-content document cache,
but performs serial file rendering; the one-shot path uses up to four threads.
Document reuse additionally requires matching displayed coverage and theme.
There is no cross-revision, changed-source incremental tree lookup: incremental
updates currently copy only the same request's before tree into its after tree.
Matching source bytes alone do not prove a tree/style-cache hit.

These results do not rule out a persistent parallel renderer with broader AST
reuse, but they do not justify enabling the existing worker by default. Its
serial architecture must not be presented as that future implementation.

### Serial control, without source-style disk caches

A separate three-trial inline run disabled source-style disk caches in all
modes. Shared configuration/query disk caches remained enabled; the worker
retained in-memory documents. Every output again matched the full reference:

| Request / revision | One-shot, four threads | One-shot, serial | Worker, serial | Delta |
| --- | ---: | ---: | ---: | ---: |
| 0 / `930c788a5` | 262.72 | 519.41 | 542.09 | 107.98 |
| 1 / `e25ef77fa` | 218.19 | 314.37 | 288.45 | 129.73 |
| 2 / `f36d57681` | 195.17 | 246.27 | 223.84 | 82.02 |
| 3 / `1891050b8` | 73.19 | 104.01 | 88.23 | 59.77 |
| 4 / `b99057045` | 316.83 | 565.41 | 539.57 | 163.65 |
| 5 / `897feed4e` | 37.03 | 39.12 | 30.02 | 54.22 |
| 6 / `469aaa72e` | 56.16 | 80.15 | 67.77 | 52.27 |
| 7 / `fce83e615` | 72.34 | 92.99 | 80.87 | 59.90 |

After startup, retaining resources/documents saves roughly 9–26 ms versus
serial one-shot rendering here, but not enough to offset losing parallelism.
The process/transport startup is included, not subtracted from request zero.
There is no measured justification yet for claiming daemon-based Delta parity.
The next daemon experiment would need persistent thread-owned renderers and a
separately bounded syntax-tree cache independent of displayed style coverage,
with verified invalidation and incremental updates across changed revisions.

Reproduce with private temporary caches and automatic benchmark-worker cleanup:

```sh
HELIX_RUNTIME="$PWD/runtime" python contrib/bench-diff-worker-sequence.py \
  --hx target/rust-1.99/release/hx --iterations 3
```

Add `--serial-control` to compare serial one-shot rendering. Add
`--no-source-disk-cache` to disable source-style disk caches in every mode while
retaining the worker's in-memory document cache. `--anchor`/`--count` choose an
adjacent sequence, or `--revisions` supplies an explicit unique sequence.

## Syntax-derived injection selection: 2026-10-16

The complete-source renderer now keeps the full outer syntax tree and full
local-scope analysis, but skips injected-language groups unrelated to displayed
hunk/context ranges. Selected groups retain **all** their original ranges, not
just the visible slice: Rustdoc fences, Markdown code blocks and definitions
outside a hunk remain available. This is not isolated-hunk parsing or a
language-specific attempt to guess enclosing-function boundaries. The ordinary
interactive editor still builds complete forests.

Same release executable with `HELIX_DIFF_SYNTAX_RANGES=0` versus its default,
four invocation-local threads, width 160, normal Steel/Gruvbox configuration,
three-run medians. Shared resources were primed; source-style caches were
cleared before every fresh process. No worker, GC or compilation ran during
measurement. Values are milliseconds:

| Revision | Full inline | Selective inline | Full split | Selective split | Delta inline / split |
| --- | ---: | ---: | ---: | ---: | ---: |
| `930c788a` | 328.89 | 261.81 | 329.40 | 262.14 | 107.95 / 118.15 |
| `e25ef77fa` | 256.92 | 219.42 | 261.51 | 221.00 | 126.30 / 136.44 |
| `f36d57681` | 227.11 | 193.50 | 223.21 | 195.72 | 80.79 / 84.61 |
| `1891050b8` | 82.68 | 74.28 | 83.06 | 76.16 | 60.46 / 62.38 |

This improves inline first views by roughly **10–20%**, but **does not achieve
Delta parity**. Every measured result matched serial, cache-disabled,
non-incremental full parsing byte-for-byte in both layouts. The second revision
reused three identical source results within each request; no source styles
survived between requests. Delta values above are from the selective run; its
separate full-run control measured 108.56/116.82, 128.86/137.78, 82.36/86.49
and 59.12/62.14 ms respectively.

The initial resource-prime render of the largest revision took 1143.97 ms
(full-forest control 1308.38 ms). These shared-resource-warm first-source results
do not imply that ordinary editor startup already primes all formatter caches.
Native macOS timings remain unverified.

The tree-house additions are locally vendored and described in
[`vendor/tree-house/NEOHELIX-PATCH.md`](https://github.com/rockboynton/helix/blob/neohelix/vendor/tree-house/NEOHELIX-PATCH.md).
Targeted tests compare styles against full forests inside multiline comments,
raw strings, combined Rustdoc injections, macros, Markdown fences, Scheme
scopes and HTML scripts, including incremental edits. A separate subprocess
corpus compares complete rendered output across layouts, themes, Unicode and
custom injection-language aliases.

## Invocation-local parallelism and incremental parsing: 2026-10-16

Complete-source first views now use bounded parallel directory rendering and
before-to-after incremental syntax reuse. The baseline tree remains unchanged;
an unavailable baseline tree or failed incremental update falls back to fresh
complete parsing. Background output is ordered and spills past 1 MiB to private
temporary storage; at most three background file renders run at once. Inputs
above 512 KiB per side stay on the main thread to avoid concurrent large forests.
The first file still streams directly. There is no persistent process.

Linux release, normal user Steel configuration/Gruvbox, width 160, three-run
medians, no concurrent compilation, warmed configuration/query resources, and
**all source-style snapshots removed before every measured fresh process**:

| Revision, inline | Serial ms | 2 threads ms | 4 threads ms | Delta ms |
| --- | ---: | ---: | ---: | ---: |
| `930c788a` | 631.71 | 422.63 | 324.29 | 107.83 |
| `e25ef77fa` | 377.18 | 303.61 | 256.85 | 131.33 |
| `f36d57681` | 295.23 | 258.01 | 220.64 | 81.87 |
| `1891050b8` | 124.51 | 108.95 | 82.39 | 58.93 |

The separate initial experiment without incremental parsing measured
`930c788a` at 817.42/559.87/429.16 ms for 1/2/4 threads (Delta 111.09 ms).
Thus parallelism and incremental reuse both help, but **complete-source first
views still do not meet Delta parity**. Each output was checked byte-for-byte
against serial, disk-cache-disabled, non-incremental full parsing. Duplicate
contents within one invocation may reuse newly computed styles; nothing is
retained from previous source trials. Native macOS performance is unverified.

### Separate comparison: identical unified patches

Both tools received the same precomputed unified patch bytes on stdin; patch
generation is outside these timings for both tools. Shared resources were primed,
each process was fresh, no worker ran, and source-style caches were cleared.
The patch frontend uses Tree-sitter on supplied fragments, not complete omitted
file contents; these results must not be presented as full-source accuracy.

| Revision | Neohelix inline ms | Delta inline ms | Neohelix split ms | Delta split ms |
| --- | ---: | ---: | ---: | ---: |
| `930c788a` | 114.12 | 94.24 | 123.93 | 108.83 |
| `e25ef77fa` | 121.35 | 115.20 | 129.00 | 122.96 |
| `f36d57681` | 77.50 | 68.94 | 80.01 | 76.26 |
| `1891050b8` | 54.97 | 51.82 | 56.41 | 54.41 |

The patch-sized-work contract is substantially closer to Delta than parsing
entire unseen source files. This is a potential explicit fast-preview mode,
not an automatic semantic downgrade of the complete-source formatter/editor.

## Delta source audit: 2026-10-16

Inspected Delta **0.20.1**, commit `b88d2290468449d75950bfa150ea7c1291fe2977`,
the version used in the comparisons. Its relevant design choices are:

- It consumes the patch stream, not complete source syntax trees. Each hunk
  resets the Syntect highlighter; omitted source is not parsed for context.
  See [hunk headers](https://github.com/dandavison/delta/blob/0.20.1/src/handlers/hunk_header.rs)
  and [line highlighting](https://github.com/dandavison/delta/blob/0.20.1/src/paint.rs).
- Nearby changed lines are buffered for word alignment, with a default buffer
  limit of 32, rather than waiting for an unlimited changed block. See
  [hunk processing](https://github.com/dandavison/delta/blob/0.20.1/src/handlers/hunk.rs).
- Highlighting is capped at 400 characters per line by default. Remaining text
  is rendered without syntax styles; the source explicitly notes possible
  subsequent highlighting errors until the next hunk reset. See
  [defaults](https://github.com/dandavison/delta/blob/0.20.1/src/cli.rs) and
  [the highlighting branch](https://github.com/dandavison/delta/blob/0.20.1/src/paint.rs).
- Syntax/theme assets load from Bat's asset cache or its embedded bundle, and
  styled output accumulates in strings before being emitted. See
  [assets](https://github.com/dandavison/delta/blob/0.20.1/src/utils/bat/assets.rs)
  and [painting](https://github.com/dandavison/delta/blob/0.20.1/src/paint.rs).
- Comparing paths invokes `git diff --no-index` (or `diff` for some process
  substitutions), then processes that resulting patch; it does not build full
  source ASTs. See [path comparison](https://github.com/dandavison/delta/blob/0.20.1/src/subcommands/diff.rs).

Directly reusable without changing syntax semantics: bounded buffering, reused
output storage, and prebuilt/validated resource caches. Neohelix's flow emitter
now appends sanitized cell text into reusable row storage and writes once per
row, avoiding an allocated string and tiny write for every cell. Output remains
byte-identical, including SGR, wide/combining characters and control sanitization.

Not silently adopted: replacing complete-source Tree-sitter context with isolated
hunks, truncating syntax highlighting, or changing word matching merely to meet
a timing target. Neohelix's explicit raw-patch input already has a fragment-based
contract; the benchmark supports `--patch` to compare that separately with Delta
using identical patch bytes, without claiming complete-source syntax accuracy.

Reproduce first views of complete-source revisions with shared resources primed
but **source-style caches cleared before every measured invocation**:

```sh
HELIX_RUNTIME="$PWD/runtime" python contrib/bench-diff-first-view.py \
  --hx target/rust-1.99/release/hx --iterations 3
```

The harness interleaves 1/2/4-thread fresh processes and checks every result
against serial, cache-disabled, non-incremental full parsing. Add `--patch
--threads 1` for the distinct patch-stream comparison. No worker runs and the OS
filesystem cache is not flushed. Exact duplicate sources inside a single
invocation may reuse work; no source styles survive from earlier trials.

## Hunk-only style collection: 2026-10-16

First-view cache population no longer highlights the entire source file. Full
Tree-sitter parsing preserves context, while cached style events cover only the
displayed hunk/context ranges. Those ranges are included in cache identity so a
different comparison cannot use incomplete highlighting. Serialization and disk
writes follow output flushing rather than blocking the first emitted content.

Measured with the same release setup, no worker and no concurrent compilation:

| Revision, inline | First visit ms | Source cache hits | Revisit median ms | Delta ms |
| --- | ---: | ---: | ---: | ---: |
| `930c788a` | 1521.25 | 0 | 74.97 | 105.67 |
| `e25ef77fa` | 530.70 | 1 | 73.66 | 124.76 |
| `f36d57681` | 382.55 | 0 | 59.25 | 82.91 |
| `1891050b8` | 153.54 | 0 | 43.96 | 61.45 |

The first row also fills shared configuration/query resources after the executable
changed. Later rows reuse those resources but encounter unseen source/range
combinations. These are not a controlled before/after comparison with the older
table: range-specific identity changes which unchanged sources can be reused.
All four revisions, both layouts, matched uncached Tree-sitter output byte for
byte. Split followed inline and therefore is not an independent first visit.

This removes avoidable cache-filling work, but **new-diff parity with Delta is
still not achieved**. Moving writes after output alone cannot fix a host which
captures until process exit. The native JJ workspace separately keeps an editor
open for revision navigation, reusing initialization without a background worker;
it does not itself remove the cost of parsing unseen source contents.

## Source highlights and dependency validation: 2026-10-16

Fresh processes with populated, validated configuration/query/source-highlight
caches, no running worker, same Linux release setup as below. Dependency
validation now avoids duplicate `stat` calls and per-directory `realpath` walks;
cache hits use one dependency snapshot taken at request start.

| Input | Layout | Delta ms | Neohelix ms |
| --- | --- | ---: | ---: |
| One text line | Inline | 40.34 | 35.82 |
| One Rust line | Inline | 43.01 | 35.14 |
| 1,000 Rust lines | Inline | 138.28 | 112.42 |
| 1,000 Rust lines | Split | 231.59 | 102.77 |
| 12-file revision `930c788a` | Inline | 106.54 | 84.55 |
| 12-file revision `930c788a` | Split | 118.57 | 88.04 |
| Same revision through `jj diff` | Inline | 146.17 | 119.20 |
| Same revision through `jj diff` | Split | 153.77 | 126.18 |

These are fresh-process timings, **not empty-cache timings**. An isolated empty
disk cache for the small Rust input took 521.00 ms on its first fill, then
34.46 ms median in fresh processes. Previously unseen small Rust contents took
39.56 ms with configuration/query resources already cached, versus 502.85 ms
with all caching disabled.

First visits to other revisions still expose the outstanding parsing cost:

| Revision, inline | First visit ms | Source cache hits | Revisit median ms | Delta ms |
| --- | ---: | ---: | ---: | ---: |
| `e25ef77fa` | 482.10 | 6 | 75.55 | 131.71 |
| `f36d57681` | 504.11 | 2 | 60.81 | 81.44 |
| `1891050b8` | 174.95 | 1 | 44.42 | 60.99 |

Visited in this order after `930c788a`, no worker. Different temporary snapshot
paths do not prevent content reuse. Both layouts of all four revisions matched
uncached full Tree-sitter output byte-for-byte, including the user's theme and
word backgrounds. Split was measured after inline, so its first request already
had the source highlights populated; it is not a second source-cache-miss result.

Conclusion: populated-cache fresh processes now beat Delta in the matched suite,
but **general first-visit/empty-cache parity is not solved**. Whole-document
syntax construction on new source contents remains the next target. Native
macOS performance and jjui terminal drawing are not established by these tests.

## Earlier configuration/query-only disk caches: 2026-10-16

The default renderer now runs without a background worker. Validated native
configuration snapshots skip Steel initialization, and validated query-analysis
snapshots skip Tree-sitter's expensive grammar analysis. Full source parsing
still happens in each process. See the [cache/worker guide](diff-render-worker.md)
for validation, bounds, controls and external-configuration dependencies.

Linux release measurements: five fresh processes per case, median milliseconds,
warm filesystem and validated resource caches, width 160, forced color, user's
Steel configuration and Gruvbox theme; Delta 0.20.1 with paging disabled. No
concurrent compilation. These are **not cold-cache-miss measurements**. Different
inputs are rendered normally; final output is not memoized.

| Input | Layout | Delta | Neohelix |
| --- | --- | ---: | ---: |
| One text line | Inline | 39.77 | 56.68 |
| One Rust line | Inline | 44.93 | 60.27 |
| 1,000 Rust lines | Inline | 147.43 | 213.45 |
| 1,000 Rust lines | Split | 231.16 | 202.52 |
| 12-file revision `930c788a` | Inline | 108.60 | 842.63 |
| 12-file revision `930c788a` | Split | 114.68 | 863.90 |
| Same revision through `jj diff` | Inline | 149.91 | 886.27 |
| Same revision through `jj diff` | Split | 158.43 | 899.53 |

The prior fresh-process renderer took roughly 490 ms for the small Rust case
and 1.5 s for the revision. This improves startup substantially but **does not
meet the target of beating Delta**, especially for multi-file reviews. Full
source parsing remains the main outstanding cost. Native macOS measurements
are still needed.

An isolated empty disk cache for the small Rust case took 543.05 ms on its
first fill; subsequent fresh processes took 58.68 ms median. Changing the source
to previously unseen Rust text took 59.18 ms with the validated resource cache,
versus 482.19 ms with caching disabled. Cached and uncached output matched
byte-for-byte; all three constructed queries used cached analysis. This confirms
the speedup is resource reuse, not replaying a previously rendered diff.

## Historical end-to-end worker verification: 2026-10-16

Linux, Rust 1.99.0, optimized ThinLTO release, actual user Steel configuration,
warm filesystem caches, captured output and no concurrent compilation. Every
worker output matched its one-shot baseline byte-for-byte; trace output was
required to confirm worker use rather than accepting fallback as a success.

Four revisions, newest first: `930c788a5`, `e25ef77fa`, `f36d57681`,
`1891050b8`. One-shot columns are medians of three complete CLI invocations per
revision. Worker columns are individual complete invocations, first visiting
each revision and then revisiting them in reverse order. Width is 160.

| Layout | One-shot range | First cold worker request | Subsequent new revisions | Revisits |
| --- | ---: | ---: | ---: | ---: |
| Inline | 787–1,465 ms | 1,566 ms | 126–345 ms | 54–97 ms |
| Side-by-side | 791–1,484 ms | 1,558 ms | 128–343 ms | 59–106 ms |

Actual `jj diff -r 930c788a --tool neohelix`, including snapshot export and
process overhead, measured 1,502 ms one-shot (median of three). The cold worker
request took 1,600 ms; three subsequent invocations took 145, 147 and 139 ms.
All outputs matched. This verifies the jj formatter path, not terminal drawing
or a measured end-to-end jjui interaction on macOS.

The actual-jj check exposed an essential configuration detail: jj normally
changes into a different temporary directory for every invocation, preventing
workspace-scoped reuse. Set `diff-do-chdir = false` on both inline and split
formatter definitions. With the default setting, repeated jj requests still
took about 1.6 seconds despite exercising the worker transport. We keep workspace
isolation rather than guessing the real repository from inherited `PWD`.

Verification passed: eight subprocess lifecycle/robustness tests, three worker
unit tests, ten renderer unit tests plus the renderer argument-validation test,
optimized release build, and Clippy including tests/examples with existing
unrelated lint exceptions. Lifecycle coverage includes concurrent startup,
cancelled clients, truncated replies without partial stdout, config imports,
trust grant/revocation, workspace isolation, option/content changes, idle exit
and insecure-directory fallback. Native macOS validation remains pending.

## Session experiments

The session experiments below preceded the Unix worker transport. File-based
`hx --render-diff` now automatically uses that transport; see the
[worker guide](diff-render-worker.md). Historical session timings below exclude
IPC and are not end-to-end worker measurements. Set `HELIX_DIFF_WORKER=0` when
reproducing a fresh-process baseline.

`diff_render::RenderSession` retains the evaluated configuration, theme/icon
resources and language loader across sequential requests in one workspace.
Each request still reads the input files. A content-addressed LRU now retains
read-only parsed documents (64 entries, 16 MiB of source bytes; document/tree
allocations add memory beyond that budget). Lookup verifies exact bytes and detected
language, not just a hash or file metadata. This survives jj's temporary snapshot
paths without reusing a tree for changed contents or the wrong language. Failed
syntax parses and entries larger than the byte budget are not retained.
The session does not cache rendered output or diff results. The worker owns its
session and recreates the process when configuration, runtime or trust changes.

Build the production CLI and benchmark with the same release profile:

```sh
cargo build --locked --release -p helix-term --bin hx --example diff-render-bench
HELIX_RUNTIME="$PWD/runtime" target/release/examples/diff-render-bench \
  /absolute/path/before /absolute/path/after inline 20
```

Use the same working directory and environment as the CLI comparison so user
and trusted-project configuration are identical. The example evaluates Steel
once, measures initialization and the first render separately, then reports
the median/minimum/maximum of subsequent renders. All repeated outputs must
match byte-for-byte. Both files and directory pairs are accepted, in `inline`
or `side-by-side` layout. `DIFF_BENCH_OUTPUT=/tmp/session-output` optionally
saves the first output for comparison with the normal CLI.

Compare against multiple fresh CLI invocations using the same width (160),
layout, paths, and `--color always`. Measure elapsed wall time through process
exit; redirect/capture output identically for every run. Report warm filesystem
cache results as such, not as cold starts. Repeated identical inputs isolate
retained resources, including parsed documents; they do not by themselves
demonstrate how the cache behaves when navigating different revisions.

Set `DIFF_BENCH_PROFILE=1` to report average per-file read, document preparation,
syntax, diff and rendering times over the warmed requests. These times cover
changed non-binary text pairs with hunks; totals exclude traversal/setup/cleanup
and other overhead. The `document` total includes both `prepare` and `syntax`.

For navigation, set `DIFF_BENCH_SEQUENCE` to a JSON manifest of
`[["left-directory", "right-directory", "expected-CLI-output-file"], ...]`.
The example then executes each request once, starting with an empty cache, and
checks every output against independently captured CLI output. No requests are
warmed before this sequence. Supply the desired layout through the normal
positional argument. Include reversed requests to measure revisiting revisions.
The example also accepts `DIFF_BENCH_CACHE_ENTRIES` and `DIFF_BENCH_CACHE_BYTES`
to compare limits without rebuilding. Zero entries disables parsed-document
caching. Navigation reports entry/source-byte usage, not total process memory.

The worker now handles session invalidation, workspace isolation, serialized
requests, lifecycle and transport failure separately from this benchmark.
These session measurements exclude jj's snapshot/export work, IPC, and terminal rendering.

## Initial measurements: 2026-10-16 (before parsed-document caching)

Linux, Rust 1.99.0, production release profile, actual user Steel configuration
and `modified_gruvbox`, width 160, forced color, warm filesystem caches. No
compilation ran during this suite. Fresh-process numbers are medians of seven
CLI invocations with captured output. Session numbers are medians of twenty
renders after its first request, writing to a fresh in-memory output buffer.
They exclude process startup and future worker IPC costs.

| Input | Layout | Fresh process | Warm session |
| --- | --- | ---: | ---: |
| One changed plaintext line | Inline | 365.43 ms | 0.15 ms |
| One changed plaintext line | Split | 361.40 ms | 0.16 ms |
| One changed Rust line | Inline | 495.73 ms | 0.31 ms |
| One changed Rust line | Split | 503.07 ms | 0.33 ms |
| 1,000 changed Rust lines | Inline | 648.97 ms | 134.01 ms |
| 1,000 changed Rust lines | Split | 629.02 ms | 127.24 ms |
| 12-file revision | Inline | 1,491.86 ms | 725.86 ms |
| 12-file revision | Split | 1,486.59 ms | 742.17 ms |

Synthetic Rust files contain functions of the form
`fn example_N() { let value = 1; }`, changing every value to `2`. Plaintext uses
the same single line with `.txt` filenames. The revision fixture is the changed
file set between `930c788a515808f0fb8bb0cbba865ba189e70ae9^` and
`930c788a515808f0fb8bb0cbba865ba189e70ae9`, with both complete file versions
materialized into temporary directories. All cases run from the same Helix
workspace to keep configuration/trust selection consistent.

Session initialization measured 347–360 ms. Its first Rust-line render took
120–121 ms; subsequent renders took about 0.3 ms. The first 12-file render took
1,062–1,073 ms *after* initialization; its warm requests still took 721–767 ms.
Retaining initialization therefore helps substantially but is not enough to
make the larger revision instant. The remaining file reading, document parsing,
diff calculation and rendering costs have not been separately profiled here.

Every repeated output matched the first session output byte-for-byte, and every
case matched both the refactored CLI and the previous `930c788a` release binary.
Eight renderer unit tests passed, including repeated-request option isolation
and rereading changed files. Both CLI layouts also retained the user's Steel
theme exactly. These are renderer measurements, not an end-to-end jjui latency
claim; the normal CLI still initializes a new session on each invocation.

## Phase profiling and parsed-document caching

An instrumented repeat of the same revision isolated the remaining cost. These
are per-phase averages over twenty warm inline requests, not a partition of the
request median (traversal, setup and destruction are outside the phase totals):

| Phase | Before parsed-document caching |
| --- | ---: |
| File reads | 2.90 ms |
| Decoding/document preparation | 2.48 ms |
| Tree-sitter detection/parsing | 652.75 ms |
| Diff calculation | 7.88 ms |
| Row rendering and output | 38.73 ms |

The two large Rust files `helix-term/src/commands.rs` and
`helix-term/src/commands/typed.rs` accounted for about 394 ms of parsing. This
motivated retaining parsed documents rather than changing the row renderer.
With a 32-entry parsed cache, the same inline revision measured 49.03 ms warmed;
split measured 60.04 ms. The syntax/cache-lookup portion of the split profile
dropped to 0.43 ms (480 hits across twenty requests), while row rendering remained
48.72 ms. All repeat cases still matched the previous release and CLI exactly.

However, a sequence across four different revisions exposed eviction at that
limit: returning to the first revision took about 621 ms instead of its 49 ms
repeat time. This is why navigation and capacity controls were added to the
benchmark; an unchanged-input repeat alone is not a browsing latency claim.

## Navigation comparison: 32 versus 64 entries

The final binary was tested with both entry limits, keeping the 16 MiB source
budget unchanged. Each sequence started with an empty cache and visited
`930c788a5`, `e25ef77fa`, `f36d57681`, and `1891050b8`, then returned through
them in reverse order. The table uses inline medians from three independent
sessions; fresh CLI comparisons use three independent invocations per revision.
First visits and revisits are timed request bodies, excluding session startup
(about 350 ms), future IPC, jj snapshot export and terminal display.

| Revision | Fresh CLI | First visit, 64 entries | Revisit, 64 entries |
| --- | ---: | ---: | ---: |
| `930c788a5` | 1,454 ms | 1,108 ms | 50 ms |
| `e25ef77fa` | 1,134 ms | 298 ms | 53 ms |
| `f36d57681` | 1,047 ms | 280 ms | 29 ms |
| `1891050b8` | 831 ms | 81 ms | 11 ms |

The first row's first visit must also pay session startup, so its full cold cost
is essentially unchanged. Subsequent unseen revisions benefit from retained
Steel/configuration, loaded grammars/queries, and shared file versions. They
are not required to be repeats of a previously viewed revision. Completely new
contents still need full syntax parsing; the cache does not perform incremental
parsing between different contents.

With 32 entries, the return to `930c788a5` measured 613 ms inline / 631 ms split.
With 64 entries it measured 50 ms / 60 ms, respectively. The larger cache retained
59 documents and 2,388,655 source bytes (2.28 MiB) across this sequence, below both
limits. This supports the 64-entry default for this measured workload, not a
guarantee of zero evictions for other histories. All sequence outputs matched
independently captured CLI output byte-for-byte. Both layouts and all three
independent runs passed.

Ten renderer tests pass, including changed-file freshness, cross-path content
reuse, language isolation, forced hash collisions, source/entry limits, LRU
eviction, disabled-cache mode, and cached-versus-fresh theme changes. Clippy
passes with only the four existing unrelated Rust 1.99 lint allowances used by
this fork. A persistent worker and its lifecycle/invalidation/cancellation
policies remain unimplemented; ordinary `hx --render-diff` still starts a fresh
session each invocation.
