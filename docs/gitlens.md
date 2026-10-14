# GitLens-style blame

### Try it

Load the [GitLens modules](../runtime/plugins/gitlens/) through your Steel configuration
as described below. Importing the plugin does not install keybindings.

1. Launch this build with `hx path/to/file`.
2. Trust the repository with `:workspace-trust`.
3. Put the cursor on a committed line and press **Shift-B** (`B`) in normal mode.
4. Use **Tab** to select a link, **Enter** to open it, or **d** to inspect the commit diff.
5. Press **d** again to return to blame details, or **Esc** to close the popup.

`B` overrides Helix's default backward-WORD motion. `Space B` still invokes
native statusline blame when retained in your keymap. The popup is independent
of inline blame; it does not require enabling it.

### Installation and configuration

Neohelix packages GitLens under `runtime/plugins/gitlens/`. Steel searches the
runtime automatically, so no copy or symlink in your user config is needed.
It is optional: your configuration must import it and install its shortcut.
For a separate installation, copy or symlink that directory into
`~/.config/helix/plugins/gitlens/`, keeping both Scheme files together. These
modules require Neohelix's native Steel bindings; they are not standalone plugins
for an unmodified upstream build. Your own configuration stays outside this repo.

In your user `helix.scm`, import and export the commands:

```scheme
(require "plugins/gitlens/gitlens.scm")
(provide gitlens-blame gitlens-clear-cache)
```

In your separate keybindings module, add `"B" ":gitlens-blame"` to the normal-mode
keymap. A minimal standalone binding installer would be:

```scheme
(require "helix/keymaps.scm")
(provide configure-keybindings!)

(define (configure-keybindings!)
  (add-global-keybinding
    (hash "normal" (hash "B" ":gitlens-blame"))))
```

Call that installer from your user startup configuration. Your existing full
keybindings module can keep the binding alongside all your other shortcuts.
The plugin retains popup-local controls such as `d` and `y`.

Steel startup runs user configuration first, then a trusted project's
`.helix/helix.scm` and `.helix/init.scm` when both exist. Each layer resolves its
own relative imports; a project does not need to import the user's absolute path.
`HELIX_STEEL_CONFIG`, when set, selects the user layer rather than bypassing
project trust. Missing TOML files use built-in defaults. `--health` reports the
built-in/TOML layer before Steel initialization, not runtime Steel overrides.

### What the popup shows

- The current line's commit author and email.
- A human-readable authored date and time, including the original timezone
  (for example, `Oct 11, 2026 at 01:49 (UTC-07:00)`).
- The full commit SHA, commit title, and commit-message body.
- Associated GitHub PR numbers, titles, and open/closed/merged status.
- Selectable commit and PR links, with the commit selected initially when available.
- Immediate PR hints from recognizable `Title (#123)` or merge-commit messages.
  These are explicitly unverified until GitHub confirms the association.
- Original upstream PR hints from `Source:` provenance in ported commits.
  `Source:` is part of the commit message, not an extra metadata service.

There is no separate line-number section or keybinding hint line in the popup.
The layout is compact, with blank rows before headings and between headings and
their body. Height fits the content; long content scrolls. Near the bottom of the
screen, the popup prefers to expand upward when that offers more room.

### Controls

| Key / command | Action |
| --- | --- |
| `B` in normal mode | Open blame details for the current line |
| `Tab` / `Shift-Tab` | Select the next / previous commit or PR link |
| `Enter` | Open the selected link in your default browser |
| `c` | Copy the selected link |
| `y` | Copy the full commit hash, including from the diff view |
| `d` | Open the full commit diff; press again to return to blame details |
| `Ctrl-d` / `Ctrl-u`, `Page Down` / `Page Up` | Scroll the popup |
| `Esc` | Dismiss the popup |
| `:gitlens-blame` | Open the popup without using the keybinding |
| `:gitlens-clear-cache` | Clear saved PR metadata |

Both copy actions write Helix's default yank register and the system clipboard.
If the clipboard is unavailable, the value remains available for `p`. Links are
keyboard-actionable, not mouse-click/OSC-8 terminal hyperlinks; only HTTPS links
can be opened.

### Commit diff

**d** opens the full local commit patch in a syntax-highlighted diff block,
independent of GitHub. This is a unified Git patch, not a Delta-style side-by-side
or word-level diff renderer.

Press **d** again, even while loading, to restore the original GitLens popup's
selected link and scroll position without repeating the PR lookup. PR enrichment
can finish in the background while the diff is open and appears on return.

The patch is loaded through asynchronous `git show`, with a five-second timeout
and a 1 MiB output limit. External diff commands and textconv are disabled.
Oversized patches display an explicit error. Merge commits compare against their
first parent; binary changes use Git's summary.

### Performance and caching

Local metadata is native and loaded in a background worker. GitHub enrichment uses
native Octocrab HTTP with your existing GitHub authentication, a five-second timeout, no shell
interpolation, bounded output, and a five-minute/128-entry persistent cache. Only
Shift-B initiates a plugin lookup; no plugin network work runs on startup or render.
Saved metadata survives editor restarts; stale details remain visible while
refreshing, for up to seven days. The cache is lazy-loaded on the first request,
stored in Helix's cache directory under `plugins/gitlens-pr-v1.json` (normally
`$XDG_CACHE_HOME/helix/plugins/gitlens-pr-v1.json`, or
`~/.cache/helix/plugins/gitlens-pr-v1.json` on Linux), and contains metadata only,
never authentication tokens. Unix files are mode `0600`; private repository titles
may be present. Run `:gitlens-clear-cache` to discard saved details.
The existing `inline-blame.auto-fetch` setting is independent of this plugin.
Enrichment preserves selection and scroll position. Replies for changed
documents/versions/cursor lines are ignored, and late replies cannot reopen
dismissed popups or replace a diff view you have already left.

### Forks and upstream PR links

Origin is preferred for actual commit lookups and commit links. Upstream provides
original commit-message PR hints in forks. Up to 20 associated PRs are displayed
using the [commit-to-PR API](https://docs.github.com/en/rest/commits/commits#list-pull-requests-associated-with-a-commit).

Ported commits with `Source:` provenance use the fork for commit association
checks and retain the upstream PR as a separate unverified message hint.
Verified PR links retain GitHub's canonical repository, which can differ from
the commit lookup repository. Bare message references remain unverified hints.
Older cache records with rewritten links are invalidated automatically.
Confirmed unpublished commits omit the dead commit link; publishing the branch
makes that hash available on GitHub.

### Requirements and limitations

- The repository must be trusted. Uncommitted lines have no historical blame.
- GitHub enrichment uses a pooled native Octocrab client—no `gh` subprocess is
  needed for requests or credential lookup. Offline/authentication failures and
  non-GitHub repositories still show local commit metadata.
- GitHub.com and GitHub Enterprise hosts are derived from repository remotes;
  no hostname configuration is needed. HTTPS, `git@host:owner/repo.git`, and
  `ssh://git@host/owner/repo.git` forms are supported. Lookups use
  the host's native HTTPS API and that host's existing authentication.
  Public and Enterprise PR caches are isolated by hostname. Canonical PR URLs
  may point to another repository on the same host, but never another host.
  A Git remote alone cannot identify its forge; non-GitHub hosts fall back to
  local details when the GitHub API lookup fails. SSH aliases must resolve to
  the actual web/API hostname in the remote; ports and credential-bearing URLs
  are not currently supported.
- Authentication follows the GitHub CLI's token precedence: `GH_TOKEN` then
  `GITHUB_TOKEN` for GitHub.com/Enterprise Cloud; `GH_ENTERPRISE_TOKEN` then
  `GITHUB_ENTERPRISE_TOKEN` for Enterprise Server. Otherwise the client reads
  the selected host/account from `hosts.yml` and its native credential store
  (Linux Secret Service, macOS Keychain, Windows Credential Manager). No tokens
  enter Steel, popups, logs, or the PR cache. `GH_CONFIG_DIR`/`XDG_CONFIG_HOME`
  are respected. You can still use `gh auth login` to set up a saved login.
- HTTP clients are reused for up to eight hosts. After changing credentials,
  run `gitlens-clear-cache` or restart Helix to rebuild clients. Requests have a
  five-second total deadline and a 1 MiB response limit; redirects and retries
  are disabled. System TLS roots are used; certificate verification stays on.
- The full commit patch requires the `git` executable and local commit history.
- This is line/commit inspection, not the entire VS Code GitLens feature set:
  there is no file-history browser or general issue-reference resolver.

### Implementation and disabling

The core Steel bindings provide asynchronous native metadata, generic bounded
process execution, native Octocrab HTTP/authentication, private JSON cache
storage, and a linked Markdown popup. Remote selection, cache policy,
formatting, and popup-local controls live in the plugin module. The global `B`
binding lives in `keybindings.scm`; importing GitLens does not install keybindings.
To disable the shortcut, remove or remap `"B" ":gitlens-blame"` in that keymap.
To remove the plugin entirely, also remove its import and command re-exports from
`helix.scm`.

The PR cache holds 128 repository–commit pairs total, not 128 lines. Lines
sharing a commit share an entry. Adding a 129th distinct commit evicts the
oldest-fetched entry; reading an entry does not update its timestamp. Revisiting
an evicted commit shows local blame first and fetches PR metadata again. A rebase
that changes the blamed commit hash uses a new cache key on the next request.
