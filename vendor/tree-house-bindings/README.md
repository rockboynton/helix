# Neohelix query-analysis cache patch

Source: crates.io `tree-house-bindings` 0.3.2 (upstream MPL-2.0); vendored
Tree-sitter C sources retain their MIT license in `vendor/LICENSE`.

The opt-in `query-cache` feature adds a private, thread-local disk cache for
validated grammar-pattern analysis. The embedding application supplies a
namespace binding its executable, grammars, queries and configuration identity.
The current Neohelix implementation invalidates conservatively.

Cached data contains checked counts, three derived boolean flags per query
step, one derived boolean per pattern, and sorted rootless-repeat symbol IDs.
It contains no pointers, parse tables, bytecode, source text or rendered output.
The query is still parsed from source, capture/field names resolved, and Rust
predicates checked. Missing, corrupt or incompatible data falls back to the
normal grammar-validating analysis. Only successful analysis is stored.

Changes are localized to `vendor/src/query.c`, `src/query.rs`, `src/grammar.rs`,
the new `src/query_cache.rs`, feature/dependency wiring, and this document.
Default behavior without configuring a cache remains unchanged.
