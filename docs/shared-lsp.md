# Shared LSP instances

Status: experimental Unix implementation, **enabled by default**. Set
`editor.lsp.shared = false` to use ordinary independent language servers.
Review the restrictions below for your workflow. Native macOS runtime testing is pending.
The target is independent Neohelix processes, including separate Zellij panes;
sharing must not depend on the terminal multiplexer.

In Steel configuration:

```scheme
(require "helix/configuration.scm")
(lsp (hash 'shared #f)) ; opt out of sharing
```

Or in TOML:

```toml
[editor.lsp]
shared = false # opt out of sharing
```

Changes apply to newly started language servers; already attached servers are
not migrated. Close and reopen the editor to switch an existing workspace.
`:lsp-restart` deliberately starts a private server without disturbing peers.
The former `HELIX_SHARED_LSP` opt-in is no longer used.

## Behavior and restrictions

Compatible Neohelix processes opening **the same or different files in a workspace**
can share a server. Each editor owns a small stdio proxy; a private Unix-socket
broker owns the actual language server. These helpers skip configuration/Steel
initialization and use single-threaded asynchronous I/O runtimes.

- The broker remaps request IDs, cancellation and progress tokens. Document
  diagnostics go to synchronized views, with their local versions translated;
  diagnostics for unopened files can go to all attached editors. Watcher
  notifications come from one frontend at a time;
  registrations are replayed to peers and pending registrations survive leader
  departure.
- A file has one authoritative unsaved document. Typing in either editor updates
  the other through version-checked transactions, without saving or reloading
  from disk. Cursors, selections and scroll positions remain independent.
  Incremental edits are transformed using their actual operations; full-text
  changes use Unicode-aware differences. Simultaneous
  typing can interleave characters. This is text coordination, not semantic merging.
  A clean newly opened buffer joins the existing unsaved contents. A preexisting
  divergent edited buffer instead isolates, preserving both versions.
- Peer changes do not echo back into the same server. Each view has at most one
  pending synchronization request per file; newer changes coalesce behind it.
  Saving clears a peer's modified marker only if its version/text still match
  what was saved. Moving a file or directory inside Helix rebinds peer buffers
  while retaining text and selections. This does not promise tracking arbitrary
  external filesystem renames.
- Normal navigation, completion, diagnostics, formatting, semantic tokens,
  symbols, folding, rename, code actions and execute-command requests are routed.
  Workspace edits carry one-shot, expiring version-check tokens. The requesting
  editor applies its files; the broker applies edits for peer-only open files to
  their shared unsaved documents. Closed files use the requesting editor's normal
  workspace-edit path. Server-initiated workspace edits choose one driver, not
  every window. File creation and rename operations use that driver too.
- Stale versions, overlapping edits, repeated sequential edits to one shared
  file, creating/deleting an open shared file, and destination-buffer collisions
  are rejected. Workspace edits are not globally atomic: if a peer changes after
  preparation, local edits may already be applied. Inspect or undo those before
  retrying. A driver disconnect never replays a potentially applied edit to a peer.
  Unknown extensions still isolate before execution; retry the cancelled request
  after isolation. Mutating requests are never automatically replayed. User
  prompts/show-document requests are not broadcast.
- Changed effective settings also cause isolation. Backend failure cancels
  pending requests and starts independent servers with each frontend's unsaved
  buffers. Requests need retrying. Fallback requires unchanged capabilities;
  otherwise restart the LSP explicitly. Buffer text remains in Helix.
- `:lsp-restart` starts a fresh **private** server for the current editor. It
  neither reattaches to the old backend nor restarts another editor's server.
  Exiting one editor leaves surviving peers running. Exiting the last attached
  editor shuts down/reaps the backend and removes the socket (with a bounded
  two-second graceful shutdown before forced cleanup); no
  unwatched workspace index is retained for a future session.

This is an editor-aware protocol, not a transparent multiplexer for arbitrary
LSP clients. Read-only views that cannot accept synchronization and other
incompatible frontends fall back rather than pretending their text is synchronized.
Custom servers with pane-dependent environments may not share; disable sharing
if independent editor-local state is required.

## Compatibility, discovery and bounds

Compatibility hashes include the resolved executable and its filesystem
identity, arguments, canonical workspace directory, effective server environment,
complete initialize parameters except editor PID, and broker/editor identity.
Capabilities, initialization options, root URI and workspace folders must match.
Settings are also checked when delivered after initialization. Mutable scripts
or server dependencies referenced by arguments are not recursively fingerprinted;
restart after changing those. No configuration or credentials are written to
socket metadata or logs.

Presentation-only `_`, `SHLVL`, `OLDPWD`, `COLUMNS`, `LINES`, and
`ZELLIJ_PANE_ID` are removed from both identity and the server environment.
`PWD` is the canonical workspace directory. Other environment differences
conservatively prevent sharing. Workspace trust stays enforced by normal Helix
server startup; the broker does not run a second workspace-config loader.

Sockets default to `/tmp/neohelix-lsp-<uid>`; `HELIX_SHARED_LSP_DIR` can select
another short absolute directory. Directories must be owned by the user with
mode 0700, sockets use 0600, peers must have the same effective UID, lock files
reject symlinks/hardlinks/public permissions, and startup uses exclusive locks.
The shared wire protocol is separate from the diff worker. It supports Linux
and macOS peer-credential APIs; native macOS remains unverified.

Limits are 8 brokers per discovery directory, 8 clients per broker, 16 MiB
messages/control frames, bounded queues, 1024 outstanding routed requests,
256 registrations and 128 pending server requests. Replay retains up to 512
documents/8 MiB of current document snapshots per frontend; exceeding replay capacity
switches that frontend to a private backend without dropping the triggering
notification. Incremental and peer changes are compacted into replayable full-text
opens, so ordinary typing does not exhaust a growing edit log. Workspace-edit
tokens are bounded to 128 and expire after 60 seconds. An oversized/malformed wire
message closes the affected transport; Helix retains its buffers, but the LSP
may need restarting. Slow readers are disconnected, upstream writes time out,
and initialization has a deadline. Never treat these limits as support for
arbitrarily large documents.

An unused startup broker expires within the default 120-second idle budget
(`HELIX_SHARED_LSP_IDLE_SECS`, bounded to 1–3600); initialization has its own
60-second deadline. These do not extend the last-attached-client lifecycle.

## Verification and reproduction

`contrib/test-shared-lsp.py` exercises real `hx` proxy/broker processes with a
deterministic instrumented server, plus actual PTY-backed editor processes.
Coverage includes simultaneous startup, disjoint sharing, overlapping unsaved
buffers, UTF-16 incremental replay, large-buffer fallback, malformed peers,
unsafe discovery permissions, backend crashes, restart, opt-out and cleanup.
`HX=/path/to/hx` selects another build. Rust routing/wire tests additionally
cover colliding request IDs, cancellation, diagnostics, compatibility,
configuration, progress and registration success/failure/leader transfer.
The live-document rewrite adds actual two-editor tests for unsaved joining,
bidirectional/concurrent typing, peer-save state, cross-file rename, file moves,
rename-then-edit resource operations and crash replay. The optional real-analyzer
test exercises cross-file rename as well as useful hover and backend process count.
This is not native macOS or live Zellij validation.

Final Linux checks for the live-document rewrite: 38 LSP/transport tests both
with and without Steel, 24 release-build process/editor tests including real
rust-analyzer, 231 editor integration tests, 16 serial diff-worker regressions,
strict Clippy and a build without default features. Test concurrency was bounded
to avoid exhausting worker/thread limits. The no-default-features build retains
preexisting unused-variable warnings outside this subsystem.

```sh
python contrib/test-shared-lsp.py -v
RUST_ANALYZER=/absolute/path/to/rust-analyzer python contrib/test-shared-lsp.py -v
cargo test -p helix-lsp --features steel
cargo test -p helix-lsp
```

For matched real-server measurements, run without concurrent builds/tests:

```sh
CARGO_NET_OFFLINE=true python contrib/bench-shared-lsp.py \
  --root "$PWD" --files helix-lsp/src/client.rs helix-lsp/src/lib.rs \
  --server /absolute/path/to/rust-analyzer --hx target/release/hx \
  --settle 5 --requests 500 --edit-requests 100
```

This opens two disjoint source files and measures initialization, first useful
hover and aggregate Linux PSS including proxies/broker/server descendants. It
does not measure editor startup or prove full indexing has finished. The default
options disable checks, build scripts and procedural macros. Use `--options`
for a different configuration; native macOS has no `/proc` PSS measurement.
Each client gets 20 warmup hovers and `--requests` measured hovers (default 200), reporting median,
p95, p99 and maximum round-trip latency. Increase `--requests` for longer runs.
The acceptance goal is no more than a few milliseconds of added request latency
versus the matched direct server; initialization/attachment is reported separately.
`--edit-requests` adds in-memory newline edits followed by useful-hover barriers
(default 100 per file). Use `--reverse` to repeat both modes in reverse order.

## Live-document release measurements

2026-10-17, Linux, implementation `6ed8cdc1`, Rust 1.99.0 thin-LTO release;
Nix rust-analyzer 1.101.0-nightly-2026-10-11. Same Helix workspace and two files
as the command above. Two trials ran direct/shared, then shared/direct using
`--reverse`, with no concurrent builds or tests. Each file received 20 warmup
hovers, 500 measured hovers and 100 in-memory newline edits, each followed by a
useful hover barrier. Totals per mode: **2,000 warm requests and 400 edit barriers**.
Checks, build scripts and procedural macros were disabled. OS caches were not
flushed. This is protocol latency, not full editor startup or rendering latency.

Ranges across the four per-file samples:

| Operation / mode | Median | p95 | p99 |
| --- | ---: | ---: | ---: |
| Warm hover, direct | 0.344–0.495 ms | 0.577–0.672 ms | 0.658–1.231 ms |
| Warm hover, shared | 0.347–0.371 ms | 0.414–0.438 ms | 0.437–0.500 ms |
| Edit + hover, direct | 8.133–10.799 ms | 11.339–13.682 ms | 16.052–26.729 ms |
| Edit + hover, shared | 8.940–11.407 ms | 10.529–13.213 ms | 11.042–16.097 ms |

Comparing matching files within each trial, shared edit-barrier medians added
**0.479, 0.807, 0.608 and 0.657 ms**. Warm-hover median overhead was at most
0.006 ms. Shared p95 values did not exceed their paired direct values for either
operation. This meets the few-millisecond overhead target in this workload, not
a universal bound: edit-barrier maxima reached 255.0 ms direct and 188.6 ms
shared. Lower sampled shared percentiles do not prove the proxy makes analysis
intrinsically faster.

| Mode | First initialize | Second initialize | Second useful hover | Aggregate PSS |
| --- | ---: | ---: | ---: | ---: |
| Two direct analyzers | 11.30–12.04 ms | 15.49–15.81 ms | 730.87–1356.27 ms | 5,686,622–5,688,268 KiB |
| One shared analyzer and three helpers | 26.64–32.25 ms | 5.87–6.03 ms | 9.46–9.69 ms | 2,883,991–2,890,220 KiB |

First useful hover remained about 6.6 seconds in both modes. Starting the first
broker added 15.34–20.21 ms to initialization; subsequent clients reused the
loaded analyzer. Aggregate PSS fell by **49.2–49.3%**, approximately 5.42 GiB to
2.75–2.76 GiB, including 16.6–16.8 MiB for the three helpers. Memory was sampled
after edit barriers, includes live descendants, and is not a claim of complete
indexing or the same savings in another workspace. Native macOS measurements
are still pending.

## Earlier single-owner release measurements

These results predate the live-document/workspace-edit rewrite. Do not assume
they establish the rewritten implementation's latency; rerun the matched harness.

2026-10-17, Linux, Helix workspace, Rust 1.99.0 release build with thin LTO;
Nix `rust-analyzer-preview` 1.101.0-nightly-2026-10-11. Two trials reversed order:
direct/shared, then shared/direct. Each mode/trial opened the two files above,
waited five seconds after useful hovers, then measured 500 hovers per file after
20 warmups: **2,000 measured requests per mode**. No builds/tests ran concurrently.
The analyzer's own schema confirms `checkOnSave=false`; build scripts and
procedural macros were also disabled. OS filesystem caches were not flushed.

Ranges below are across the four per-client samples in each mode. These are
whole protocol round trips through the same Python harness, not estimated IPC
costs or an editor-rendering benchmark.

| Mode | Median | p95 | p99 | Maximum |
| --- | ---: | ---: | ---: | ---: |
| Direct analyzer | 0.400–0.471 ms | 0.580–0.673 ms | 0.676–0.803 ms | 4.375 ms |
| Shared analyzer | 0.338–0.373 ms | 0.406–0.465 ms | 0.432–0.601 ms | 1.759 ms |

The shared path stays within the few-millisecond request-overhead target here;
no added latency was observed in these samples. Its slightly lower times are
not a claim that multiplexing inherently accelerates a cached analyzer query.
This covers warm hovers, not every LSP method, operating system or workload.

| Mode | First initialize | Second initialize | Second useful hover | Aggregate sampled PSS |
| --- | ---: | ---: | ---: | ---: |
| Two independent analyzers | 11.85–11.89 ms | 15.58–32.96 ms | 1251.78–1411.46 ms | 5,556,810–5,565,811 KiB |
| One shared analyzer plus three helpers | 27.45–28.13 ms | 5.55–5.75 ms | 9.10–10.99 ms | 2,861,181–2,861,520 KiB |

**Starting the first broker adds about 16 ms to initialization** in this setup;
that is separate from warm request latency. First useful hover remained about
6.6 seconds in both modes because the analyzer still has to load the workspace.
The measured aggregate PSS saving was about 48.5%, including all three helper
processes; the helpers themselves consumed roughly 15 MiB combined. These are
sampled process-memory results, not proof that indexing is complete or a promise
of the same ratio for another project. PSS apportions shared memory mappings.

The earlier independent-only pilot is retained in Git history. It supplied an
unrecognized `check.enable=false` option, so its claim of disabled checks should
not be used for comparison. The corrected matched results above supersede it.
Neither harness writes source files, but servers may create normal caches or run
tools according to their configuration. Native macOS still needs the process
tests and latency measurements run on an actual Mac.

The ownership and lifecycle constraints follow the
[LSP specification](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/).
Shared documents, versioned peer transactions and coordinated workspace edits
are Neohelix policies, not synchronization features supplied by LSP itself.
