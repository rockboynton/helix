# Octocrab assessment

Researched 2026-10-12. Recommendation: retain `gh api` for this one-endpoint
plugin for now. Revisit Octocrab if adding reviews, checks, issue data, or frequent
uncached requests. No Octocrab dependency was added.

## Fit

Octocrab is a third-party async Rust GitHub SDK, listed among community libraries
by [GitHub](https://docs.github.com/en/rest/using-the-rest-api/libraries-for-the-rest-api).
Its typed [CommitHandler](https://docs.rs/octocrab/latest/octocrab/commits/struct.CommitHandler.html)
supports `associated_pull_requests`, exactly the endpoint used here.

A long-lived client could avoid per-request subprocess startup and reuse HTTP
connections. Its [implementation](https://github.com/XAMPPRocky/octocrab/blob/main/src/lib.rs)
uses Hyper's client and supports personal tokens and configurable timeouts.
That is an architectural benefit, not a measured speedup: no Octocrab prototype
was benchmarked. The remote association lookup still needs GitHub's API; Git
commit messages provide hints, not authoritative PR state or association.

## Local measurements

Three sequential samples using the installed `gh`, a public Helix commit, and
the existing authenticated environment:

| Operation | Samples (ms) | Median (ms) |
| --- | --- | --- |
| `gh --version` | 114, 114, 114 | 114 |
| Commit-to-PR API request | 766, 816, 465 | 766 |

The startup baseline does not isolate API authentication, TLS, network, or server
time. These samples cannot predict an exact native-client improvement.
Persistent-cache smoke testing across two editor processes made only one mocked
API invocation; the restart displayed saved verified details without a request.

## Costs and integration

The [current main manifest](https://github.com/XAMPPRocky/octocrab/blob/main/Cargo.toml)
declares Rust 1.85, compatible with this checkout's toolchain. It brings Hyper,
Tower, and TLS infrastructure absent from this checkout's current Cargo.lock.
Default features also include JWT/AWS-LC support unnecessary for token-based
read-only lookup; a future integration should disable defaults and select only
the needed client, TLS, and timeout features. Dependency count, binary size,
and build-time impact have not been measured.

Authentication is a separate design decision. One option is invoking
[`gh auth token`](https://cli.github.com/manual/gh_auth_token) once per session,
retaining the result only in memory, then issuing native requests. That still
requires `gh` to bootstrap and needs explicit account/token refresh handling.
Fully removing `gh` means providing secure credential discovery/storage instead.
No token was extracted during this research.

If adopted, initialize lazily outside rendering/startup, retain the five-second
total deadline and response bound, reuse the persistent metadata cache, and
keep credentials out of Scheme config, cache files, logs, and command arguments.
GitHub policy should remain plugin-owned rather than spreading into generic
editor or rendering code. For the current scope, the cache and stable async UI
offer clearer value than an SDK dependency swap.
