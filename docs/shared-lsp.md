# Shared LSP instances

Status: experimental Unix implementation, **disabled by default**. Start editors
with `HELIX_SHARED_LSP=1 hx` to opt in. Editors without that variable retain
ordinary independent language servers. Do not enable this globally until its
restrictions below fit your workflow. Native macOS runtime testing is pending.
The target is independent Neohelix processes, including separate Zellij panes;
sharing must not depend on the terminal multiplexer.

## Behavior and restrictions

Compatible editor processes opening **different files in the same workspace**
can share a server. Each editor owns a small stdio proxy; a private Unix-socket
broker owns the actual language server. These helpers skip configuration/Steel
initialization and use single-threaded asynchronous I/O runtimes.

- The broker remaps request IDs, cancellation and progress tokens. Document
  diagnostics go to the owning editor; diagnostics for unopened files can go to
  all attached editors. Watcher notifications come from one frontend at a time;
  registrations are replayed to peers and pending registrations survive leader
  departure.
- A document has one owner. Opening an already-owned URI isolates the second
  editor onto a new private server **before** forwarding its conflicting open.
  Both editors keep their own buffers. The fallback replays the affected
  editor's actual open/change notifications, including unsaved incremental
  changes, rather than loading saved text from disk. It never forces a save.
- Normal navigation, completion, diagnostics, formatting, semantic tokens,
  symbols, folding and related read-oriented requests are routed. Rename,
  code actions, execute-command and unknown extensions instead isolate the
  requesting editor. **Their triggering request is cancelled: retry it after
  isolation.** Mutating requests are never automatically replayed, avoiding
  duplicate execution. Server-initiated workspace edits are rejected while
  shared; user prompts/show-document requests are not broadcast.
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

This is intentionally conservative, not a general transparent multi-client LSP
implementation. Two editors with the same file open will not share indefinitely.
URI ownership is not a filesystem lock or cross-editor editing synchronization.
Custom servers with pane-dependent environments are unsuitable for this opt-in.

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
documents/8 MiB of open/change history per frontend; exceeding replay capacity
switches that frontend to a private backend without dropping the triggering
notification. Incremental history is not compacted. An oversized/malformed wire
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
Linux validation includes 28 LSP unit/transport tests with and without Steel,
17 process/editor tests with the real-analyzer check enabled, 230 editor
integration tests, 16 renderer-worker regressions, strict Clippy and a build
without default features. This is not native macOS or live Zellij validation.

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
  --settle 5 --requests 500
```

This opens two disjoint source files and measures initialization, first useful
hover and aggregate Linux PSS including proxies/broker/server descendants. It
does not measure editor startup or prove full indexing has finished. The default
options disable checks, build scripts and procedural macros. Use `--options`
for a different configuration; native macOS has no `/proc` PSS measurement.
Each client also gets 20 warmup hovers and 200 measured hovers, reporting median,
p95, p99 and maximum round-trip latency. Increase `--requests` for longer runs.
The acceptance goal is no more than a few milliseconds of added request latency
versus the matched direct server; initialization/attachment is reported separately.

## Release measurements

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
Exclusive document leases and private fallback are Neohelix policies, not a
multi-editor synchronization feature supplied by LSP itself.
