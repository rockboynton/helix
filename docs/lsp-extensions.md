# Semantic highlighting and LSP extensions

## Semantic highlighting

Neohelix overlays language-server semantic tokens on Tree-sitter highlighting.
This lets the server distinguish resolved types, parameters, fields, methods,
constants and other symbols that syntax alone cannot identify reliably. It is
enabled by default with `editor.lsp.semantic-tokens = true`.

In Steel, use `(lsp (hash 'semantic-tokens #t))`, or change just that setting
with `(set-option! "lsp.semantic-tokens" #t)`. Toggle at runtime with
`:toggle lsp.semantic-tokens`. To exclude one server, add `"semantic-tokens"`
to its language entry's `except-features` list.

- Full-document requests are preferred; range-only servers are supported too.
  Delta responses and dynamic semantic-token registration are not negotiated yet.
- Requests run outside rendering and edits are debounced by 100 ms. Responses
  from an old document version, request generation or server are discarded.
  Edits immediately discard old semantic positions; Tree-sitter supplies syntax
  colors while the new response is pending.
- Server `workspace/semanticTokens/refresh` requests refresh affected documents.
  Compatible shared-LSP sessions route semantic-token requests normally.
- Types map to existing theme scopes, including `type`, `function.method`,
  `variable.parameter`, `variable.other.member` and `type.enum.variant`.
  Rust-analyzer-specific token kinds include lifetimes, built-in types, `Self`,
  `self`, escapes and punctuation. Unknown types leave syntax colors untouched.
  `readonly`, `defaultLibrary`, `mutable` and documentation modifiers affect
  scope selection; not every server modifier has a separate visual treatment.
- No LSP runs for finite diff rendering or picker previews: those remain
  Tree-sitter-highlighted. Semantic tokens do not add server startup to jjui.

Test with Rust source containing a struct, enum variants, fields, a trait method
call and local variables. Wait for rust-analyzer initialization, then toggle
semantic tokens; syntax colors must remain present in both modes. Edit while
requests are pending, restart the server, and repeat with shared LSP enabled.

## Protocol audit

Audited against this fork's native client and rust-analyzer's
[extension reference](https://rust-analyzer.github.io/book/contributing/lsp-extensions.html).
LSP types existing in `helix-lsp-types` do **not** imply client/UI support.

Standard support already includes completion/snippets, diagnostics, hover,
navigation, formatting/range formatting, rename and workspace edits, code
actions, document links, inlay hints, document colors and call hierarchy.

Standard gaps include code lenses, type hierarchy, linked editing, on-type
formatting, LSP folding ranges and selection ranges (Neohelix has Tree-sitter
folding and syntax selection instead). Inlay-hint refresh is not advertised.
Semantic-token delta and dynamic registration remain follow-ups.

There is no native rust-analyzer extension capability block or dedicated UI for
the following groups:

| Extension group | Native status / useful next step |
| --- | --- |
| Parent module, external/local docs, Cargo.toml | Missing; small navigation commands |
| Macro expansion | Missing; read-only syntax-highlighted scratch buffer |
| Runnables, related tests, test explorer | Missing; run/debug picker before a persistent test tree |
| Hover actions and hover ranges | Missing; actionable hover and selected-expression types |
| Structural search/replace | Missing; preview workspace edits before applying |
| Join lines, on-enter, move item | Missing; preserve multicursor/snippet behavior |
| Snippet workspace edits and code-action groups | Missing; ordinary completion snippets and flat code actions already work |
| Server status and flycheck controls | Missing; distinguish analysis readiness from generic progress |
| Workspace reload, proc-macro rebuild | No dedicated commands |
| Symbol filtering and dependency tree | Missing; enrich existing symbol/sidebar tools |
| Analyzer internals / memory-layout views | Missing; lower-priority inspection commands |
| Client commands and colored compiler diagnostic payloads | No native extension integration |

Steel can send custom LSP requests/notifications and receive catch-all events,
but that is a transport escape hatch, not implementation of these workflows.
The shared-LSP broker isolates unknown extension operations into a private
server; extension integrations need an audited sharing policy too.

Recommended order after semantic highlighting: macro expansion and navigation;
runnables; actionable hover; snippet-aware refactors/SSR; test explorer and
dependency navigation. Keep general protocol support in Rust and user-specific
bindings/workflows in Steel.
