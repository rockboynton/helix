# Shared LSP instances: baseline and design

Status: measurement tooling and transport groundwork only. **Helix still starts an
independent server per editor process. No broker is enabled or implemented.**
The target is independent Neohelix processes, including separate Zellij panes;
sharing must not depend on the terminal multiplexer.

## First measured baseline

2026-10-17, Linux, Helix workspace, two independent rust-analyzer processes kept
alive together. One pilot run, not a multi-trial acceptance benchmark. The
server was Nix's `rust-analyzer-preview` 1.101.0-nightly-2026-10-11. Cargo was
offline; checks, build scripts and procedural macros were disabled deliberately.
This is not the user's full everyday analyzer configuration.

The harness opened `helix-lsp/src/client.rs` and requested a nonempty hover on
`Client` (zero-based line 55, character 12), then allowed two seconds to settle:

| Launched instance | Initialize response | First nonempty hover | Main-process RSS after settling |
| --- | ---: | ---: | ---: |
| First | 76.62 ms | 7479.17 ms | 2,845,704 KiB |
| Second, first still alive | 15.97 ms | 9009.86 ms | 2,827,448 KiB |

After both were running, their combined proportional set size (PSS) was
5,626,429 KiB, approximately 5.37 GiB. The first alone had measured
2,843,638 KiB PSS, approximately 2.71 GiB. RSS double-counts shared mappings;
PSS apportions those mappings. These numbers exclude descendant processes and
do not measure two complete Helix sessions. A first hover does not prove the
entire workspace is indexed. OS caches were not flushed.

There is meaningful duplication to investigate here, but no measured broker
savings yet. Sharing will still need per-client state and may increase the one
server's retained document set. Repeat with real editing sessions and normal
configuration before promising a particular memory or startup reduction.

Reproduce with the same analyzer and Rust toolchain on `PATH`:

```sh
CARGO_NET_OFFLINE=true python contrib/bench-lsp-instances.py \
  --root "$PWD" --file "$PWD/helix-lsp/src/client.rs" --language-id rust \
  --line 55 --character 12 --settle 2 \
  --init-options '{"cargo":{"buildScripts":{"enable":false}},"procMacro":{"enable":false},"check":{"enable":false}}' \
  --server rust-analyzer
```

The source position must still select `Client` if the file changes. For other
projects, select a known hoverable symbol and supply that server's normal
arguments/configuration. The harness writes no workspace text, rejects server
workspace-edit requests, discards notifications instead of retaining an
unbounded log, and shuts down/reaps its own children. Servers may still create
their normal caches or run tools according to their configuration. This is a
protocol baseline, not a substitute for testing Helix itself.

## Integration boundary

`helix-lsp/src/client.rs::Client::start` currently resolves the server executable,
spawns a `kill_on_drop` child, and owns that child. `Transport::start` now accepts
generic asynchronous readers/writers rather than child-pipe-specific types.
The ordinary client still supplies the same buffered child pipes, with unchanged
process ownership and protocol behavior. Each Client has its own numeric request-ID
counter. Initialization sends the editor PID, capabilities, effective settings,
root URI and workspace folders. These are separate per editor today.

A shared client still needs process ownership distinct from a connection.
The first transport refactor removes the I/O type restriction; it does not attach
an editor to a broker. Do not simply replace stdio with a socket: request IDs, document versions,
server-to-client requests, settings and shutdown are also session state.

Transport regressions cover fragmented headers/payloads (including UTF-8),
initialization and delayed requests, response routing, server-to-client requests,
disconnect errors, shutdown-time registration acknowledgements and exit flushing.
Linux tests also exercise an actual Unix socket pair and a child-pipe echo
process. Socket-pair testing proves the I/O boundary works; it does not verify
discovery, peer authentication or multi-editor correctness. Native macOS remains
untested.

## Safety contract before enabling sharing

- **Opt-in and conservative identity.** Start Unix-only, with direct servers as
  the default. Partition by resolved executable and its identity, arguments,
  effective environment/configuration, canonical workspace roots/folders,
  trust, client capabilities and broker/editor protocol version. Incompatible
  instances stay separate. Credentials must not appear in logs or disk metadata.
- **Private broker discovery.** Same-user authentication on Linux and macOS,
  private socket directories, short Darwin socket paths, atomic startup and
  locks, bounded frames/queues, stale-socket handling, and idle cleanup. A peer
  PID alone is not authentication. Do not reuse the diff worker's wire protocol.
- **One upstream lifecycle.** The broker owns initialize/shutdown/exit and the
  child process. A frontend disconnect must not kill another frontend's server.
  Test last-client exit, crashes, cancellation and restart explicitly.
- **Route both directions.** Remap client request IDs and cancellation, route
  responses only to their originating client, and scope progress tokens. Route
  server requests with an explicit policy; do not broadcast side-effecting
  requests such as workspace edits or prompts to every editor.
- **Document ownership is the hard boundary.** A server has one authoritative
  text/version stream per URI. Two clients' incremental edits cannot be
  interleaved safely just because they opened the same disk path. The first
  prototype should share disjoint documents only. Before accepting an overlap,
  an explicit control-plane lease must either establish safe ownership or move
  the affected frontend to an independent server. No silent overwrite or forced
  save is acceptable. Fallback after attachment needs replay of that frontend's
  own open buffers, not rereading disk; ownership negotiation must finish before
  sending a conflicting `didOpen`/`didChange` upstream.
- **Diagnostics and workspace state.** Route document diagnostics to relevant
  clients with correct version mapping. Deduplicate watcher registrations and
  define settings/configuration responses centrally. Unsupported multi-client
  features must be rejected safely rather than appearing to work incorrectly.

The lifecycle and document constraints follow the [LSP specification](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/).
The exclusive-lease/fallback policy is our proposed implementation, not a claim
that LSP itself provides multi-editor sharing.

## Next implementation milestones

1. **Implemented locally:** generalize the existing transport streams while
   retaining direct-server behavior. Sharing remains disabled; the editor still
   starts independent children. Complete before/after editor-level testing when
   a shared-client attachment path is introduced.
2. Add a private broker/control handshake and compatibility identity. Test
   startup races and separate configuration/workspaces without a real server.
3. Prove two clients with disjoint files use one server. Include overlapping
   numeric IDs, cancellation, diagnostics, disconnect and hostile/malformed
   peers. Use a deterministic instrumented test server before rust-analyzer.
4. Implement and test document-lease conflicts and safe direct-server fallback,
   including divergent unsaved buffers and workspace edits. This is required
   before offering an everyday opt-in, not optional later polish.
5. Repeat matched real-project measurements: one versus two editor processes,
   direct versus shared, time to useful hover/diagnostics and aggregate memory.
   Test native macOS and Zellij pane lifetimes separately.
