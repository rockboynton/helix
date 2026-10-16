# Reusable diff renderer measurements

## End-to-end worker verification: 2026-10-16

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
