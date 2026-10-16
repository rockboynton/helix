# Reusable flow-diff worker

On Unix, file or directory comparisons with `hx --render-diff` can optionally
reuse a private background renderer by setting `HELIX_DIFF_WORKER=1`.
The default is a fresh process with validated disk caches, without a worker.
The first request still initializes Steel and parses its documents. Later
requests reuse evaluated configuration, language resources and unchanged parsed
documents, including when jj exports them under different temporary paths.

The worker now renders files in parallel and retains syntax trees separately
from displayed styles. Changed revisions can incrementally update those trees
even when hunk coverage or snapshot paths change. Complete outer syntax and
local scopes remain available; selected injection groups retain all their
source ranges. See the [matched measurements](diff-render-performance.md).

To initialize a renderer before the first diff, explicitly run this from the
same workspace, environment and viewport width used by the formatter:

```sh
hx --diff-worker-warm --width 160
HELIX_DIFF_WORKER=1 jj diff
```

Warmup evaluates configuration and renders unrelated tiny Rust/Markdown inputs;
it does not preload the revision you will view. It produces no output and does
not enable worker use globally. Widths have separate workers. Normal editor
startup does not automatically perform this warmup. A stopped worker's first
diff still pays initialization costs; report that separately from unseen diffs
after initialization.

This is read-only flow output, not an embedded interactive editor. Raw patch
input (`--patch`), ordinary editing, and interactive diff modes are unchanged.
Other platforms retain one-shot rendering. Linux is tested; native macOS
validation is still pending. macOS uses Unix-domain sockets with short paths
and `getpeereid` authentication; Linux uses `SO_PEERCRED`.

## jj and jjui

Set `diff-do-chdir = false` on each jj formatter tool using Neohelix:

```toml
[merge-tools.neohelix]
program = "hx"
diff-args = ["--render-diff", "--color", "always", "--width", "$width", "$left", "$right"]
diff-invocation-mode = "dir"
diff-do-chdir = false
```

Do the same for a separate inline formatter. jj otherwise changes into a fresh
temporary diff directory on each invocation, isolating every request into a new
worker and losing workspace configuration. With this setting, jj supplies
absolute snapshot paths while retaining the workspace directory. jjui uses the
same jj formatter settings; no jjui worker or service is needed.

## Controls

- `HELIX_DIFF_WORKER=1`: opt in to the background renderer.
- `HELIX_DIFF_WORKER=0` (or unset): fresh-process rendering with disk caches.
- `HELIX_DIFF_CACHE=0`: bypass native configuration and query-analysis disk
  caches and source highlight events, evaluating configuration and syntax afresh.
- `HELIX_DIFF_HIGHLIGHT_CACHE=0`: bypass only source highlight events, retaining
  native configuration and query-analysis caches for comparison/profiling.
- `HELIX_DIFF_RENDER_THREADS=1`: serial directory rendering for comparison.
  Directory rendering otherwise uses up to four invocation-local
  threads (bounded by available parallelism); `2`/`4` allow explicit experiments.
  These threads exit with the request. Up to two additional bounded parser
  threads overlap before/after parsing for medium-sized file pairs. Large files
  remain serial. Largest-first scheduling retains lexical output order and
  bounds queued output, spilling individual results above 1 MiB to private files.
- `HELIX_DIFF_SYNTAX_CACHE=0`: disable the shared in-memory syntax-tree cache.
- `HELIX_DIFF_IMMUTABLE_CACHE=0`: disable process-local metadata summaries of
  protected, non-user-owned Nix store directories. Mutable configuration and
  symlink chains are still checked live on every request.
- `HELIX_DIFF_INCREMENTAL_PARSE=0`: disable before-to-after incremental syntax
  reuse for comparison against two independent complete parses. Normally the
  working source updates a copy of the baseline tree when available; failures
  fall back to a fresh complete parse. The baseline tree is not modified.
- `HELIX_DIFF_SYNTAX_RANGES=0`: disable range-aware injection selection for
  comparison. Normally new source keeps a complete outer syntax tree and local
  scopes, but parses only injection groups intersecting displayed ranges. Each
  selected group retains all its source ranges, including context outside the
  hunks. Ordinary editing continues to use complete syntax forests.
- `HELIX_DIFF_CACHE_TRACE=1`: report profile hits/misses/rejections and query
  analysis hits to stderr.
- `hx --diff-worker-stop`: stop workers matching the current executable,
  workspace, configuration and environment, across viewport widths.
- `HELIX_DIFF_WORKER_TRACE=1`: report worker use or fallback to stderr.
- `HELIX_DIFF_WORKER_IDLE_SECS`: idle timeout, default 120 seconds, clamped to
  1–3600 seconds.
- `HELIX_DIFF_WORKER_DIR`: override the private socket directory. It must be an
  absolute, same-user-owned directory with no group/other permissions. The
  default is `/tmp/neohelix-diff-<uid>`. Keep the path short for macOS.
- `HELIX_DIFF_WORKER_WATCH`: colon-separated additional configuration dependency
  paths, for example Scheme imports outside the usual user/project/runtime
  directories. These are watched through metadata snapshots on each request,
  not a continuously running filesystem watcher.

## Isolation and invalidation

### Fresh-process disk caches

On Unix the renderer caches effective rendering settings in
`$XDG_CACHE_HOME/helix/diff-profile-v1` and Tree-sitter query analysis in
`$XDG_CACHE_HOME/helix/diff-queries-v1` (normally under `~/.cache`). A valid native
profile avoids starting Steel; cached queries still undergo ordinary query
parsing and predicate validation, but skip expensive grammar analysis.
Effective language snapshots retain injection-regex aliases, not just exact
language names; they omit LSP commands and root matchers unused by rendering.
No source text, syntax trees, final output, API credentials or user commands
are persisted. Editor settings are limited to those needed for rendering.

Both caches are bound to the executable, platform, environment, workspace,
configuration, trust and runtime-resource metadata. Changes invalidate them.
Dependencies are validated at request start. A profile is stored only if the
dependency fingerprint remains unchanged across configuration evaluation.
Configuration edits concurrent with a cache hit are observed on the next
request; cached profiles contain rendering data, not executable configuration.
Directories are private (0700); files must be same-user-owned, private (0600),
regular, non-symlink files with one hard link. SHA-256 checksums catch corruption.
Invalid entries fall back to normal configuration evaluation/query compilation.
Profiles are bounded to 32 entries of at most 20 MiB; query analysis to 256
entries of at most 1 MiB. Files are replaced atomically.

`diff-highlights-v1` holds content-addressed Tree-sitter highlight events:
character offsets and styles, not source text, syntax-tree pointers, terminal
output, or diff results. The key additionally binds exact input bytes, detected
language and the complete effective theme. A cache hit skips parsing that source
but still decodes its current bytes and computes its diff, layout, word changes
and wrapping normally. New source contents require parsing the complete outer
language; unrelated injected-language groups can be skipped. Style collection
is limited to the displayed hunk/context ranges, which also
form part of the cache key. Undisplayed lines are not highlighted to fill a
cache. Disk serialization/writes happen after the file's output is flushed.
Capturing hosts such as jjui still wait for process exit, so deferring writes
alone does not remove their cost. Thus navigating an unseen
revision can reuse unchanged files, but does not guarantee a cache hit for its
new contents.

Highlight files use the same owner/permissions/symlink checks and checksums,
plus embedded key, character-count, ordered-offset and style-index validation.
Corrupt entries fall back to live Tree-sitter rendering. At most 256 entries,
128 MiB in total, are retained; each entry is capped at 24 MiB. Sources larger
than 2 MiB use the original live-tree path instead of materializing cached
events. Ordinary editing and interactive diff panes do not use this cache.

The same limitation on arbitrary Scheme external I/O described below applies
to disk profiles: declare external dependencies with `HELIX_DIFF_WORKER_WATCH`,
or set `HELIX_DIFF_CACHE=0` when configuration depends on undeclared external
state. First cache fills still evaluate Steel and compile queries; cached
resource timings are not cold-cache-miss timings.

### Optional worker

Workers are partitioned by working directory, executable identity, configuration,
environment and initial viewport width. Layout, color, theme and context are
request options. Requests are serialized. Socket directories are mode 0700,
sockets are mode 0600, and both ends verify same-user peer credentials.
Startup is locked and the launcher refuses to start another worker when it
observes eight live workers in its socket directory.

Before each render, dependency metadata is checked for user/project
configuration, workspace trust, runtime resources and installed Steel modules.
Edits, additions, deletions and symlink target changes retire the process;
the replacement reevaluates configuration and starts with an empty cache.
Untrusted project configuration is never executed by the renderer.
The server performs this validation; the client does not duplicate the complete
dependency walk. Protected Nix store directory summaries can be reused within
the process. Store ownership/write permissions are checked, and symlinks are
always followed through the live dependency walk, including links that leave
the store and return through a mutable alias. User-owned stores use the ordinary
walk instead.

Arbitrary external I/O performed by Scheme configuration cannot be inferred.
Add external imports to `HELIX_DIFF_WORKER_WATCH`, stop the worker after changing
other external state, or opt out. Like a running editor, configuration is not
reevaluated for every unchanged request.

Input files are reread for every request. Parsed-document reuse verifies exact
source bytes and detected language, not only metadata or a digest. Its LRU is
bounded to 64 documents and 16 MiB of source text; syntax trees and other process
allocations require additional memory. Rendered output and diff results are not
cached.
The independently bounded shared syntax cache retains at most 64 entries and
8 MiB of source text, with a 512 KiB per-source limit; syntax-tree memory is
additional. It verifies exact source/language identity, updates copied trees
against complete new source, and falls back to a fresh parse on update failure.
Changing recognized highlight scopes clears both in-memory caches.

## Failure and cancellation

A complete response is buffered before any bytes reach stdout. Startup,
transport or worker-limit failures fall back to one-shot rendering without
partial or duplicated output. Configuration/render errors remain errors.
Responses above 64 MiB fall back to the streaming one-shot renderer; use
`HELIX_DIFF_WORKER=0` directly for such unusually large outputs.

Disconnected clients are detected before starting queued work, between file
pairs and document parses, and periodically while writing output. An individual
Tree-sitter parse already in progress is not preempted.

Workers exit after their idle timeout and remove their socket/PID files. Empty
lock files remain in the private socket directory. There is no system service,
network listener, or persistent rendered-content cache.
