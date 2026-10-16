# Reusable flow-diff worker

On Unix, file or directory comparisons with `hx --render-diff` automatically
reuse a private background renderer.
The first request still initializes Steel and parses its documents. Later
requests reuse evaluated configuration, language resources and unchanged parsed
documents, including when jj exports them under different temporary paths.

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

- `HELIX_DIFF_WORKER=0`: use the original one-shot renderer.
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

Arbitrary external I/O performed by Scheme configuration cannot be inferred.
Add external imports to `HELIX_DIFF_WORKER_WATCH`, stop the worker after changing
other external state, or opt out. Like a running editor, configuration is not
reevaluated for every unchanged request.

Input files are reread for every request. Parsed-document reuse verifies exact
source bytes and detected language, not only metadata or a digest. Its LRU is
bounded to 64 documents and 16 MiB of source text; syntax trees and other process
allocations require additional memory. Rendered output and diff results are not
cached.

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
