# Personal Steel configuration

From this checkout, run `./contrib/hx-personal [files...]` after building
`nix develop --command cargo build --release --locked -p helix-term`.

The launcher selects this directory with `HELIX_STEEL_CONFIG`, the checkout's
runtime with `HELIX_RUNTIME`, and `/dev/null` as the global editor TOML config.
The regular `~/.config/helix/config.toml` is left untouched for other builds.
Trusted workspace TOML overrides and the regular themes/languages configuration
are still available; this migrates the editor configuration, not language servers
or theme assets. Do not pass another `--config` unless intentionally overriding
the launcher's choice.

## Organization

- `init.scm`: startup wiring; calls `configure-personal!`.
- `helix.scm`: public entry point, imports modules with prefixes and applies them
  in an explicit order.
- `editing.scm`: editing, completion, saving, file handling, LSP, and blame settings.
- `appearance.scm`: display, cursor shapes, statusline, diagnostics, theme, and icons.
- `keybindings.scm`: named keymaps for editor modes and Space/tool/VCS submenus.
- `gitlens.scm`: on-demand native blame inspection and GitHub PR enrichment.
- `gitlens-cache.scm`: lazy, persistent PR metadata cache.

Modules export only their configuration functions. Importing a module does not
apply settings; the entry point does that explicitly. Keep related settings in
their owning module and avoid introducing helpers for one-off options. Keymaps
merge into Helix's defaults, so unspecified bindings retain their default behavior.
External shell commands remain literal to preserve Helix's `%` expansions.

Editing settings are applied before appearance so true-color is enabled before
loading the theme and icons. The migrated blame settings remain `never` with
background fetching enabled; the original `Space B` requests line blame.
Nothing installs or replaces your normal `hx` executable.

## GitLens-style blame

Press **Shift-B** (`B`) in normal mode for a popup with the current line's author,
email, full authored timestamp/timezone, full SHA, commit title/body, and associated
GitHub PR titles and status. This overrides the default backward-WORD motion;
`Space B` retains the native statusline-blame command. Trust the repository first
with `:workspace-trust`. Uncommitted lines have no historical blame.

Local metadata is native and loaded in a background worker. GitHub enrichment uses
`gh api` with your existing GitHub authentication, a five-second timeout, no shell
interpolation, bounded output, and a five-minute/128-entry persistent cache. Only
Shift-B initiates a plugin lookup; no plugin network work runs on startup or render.
Recognizable `Title (#123)` and merge-commit titles provide immediate PR links,
explicitly marked as unverified hints until the API confirms an association.
Saved metadata survives editor restarts; stale details remain visible while
refreshing, for up to seven days. The cache is lazy-loaded on the first request,
stored in Helix's cache directory under `plugins/gitlens-pr-v1.json` (normally
`$XDG_CACHE_HOME/helix/plugins/gitlens-pr-v1.json`, or
`~/.cache/helix/plugins/gitlens-pr-v1.json` on Linux), and contains metadata only,
never authentication tokens. Unix files are mode `0600`; private repository titles
may be present. Run `:gitlens-clear-cache` to discard saved details.
The existing `inline-blame.auto-fetch` setting is independent of this plugin.
Missing `gh`, offline/authentication failures, and non-GitHub repositories still
show local metadata. GitHub.com is supported, not GitHub Enterprise hosts. Upstream
provides original message hints in forks; origin is preferred for actual commit
lookups and links. Up to 20 associated PRs are displayed using
the [commit-to-PR API](https://docs.github.com/en/rest/commits/commits#list-pull-requests-associated-with-a-commit).

Links are keyboard-actionable: **Tab/Shift-Tab** selects a PR or commit link;
**y** copies the full commit hash, and **c** copies the active link (including
after Tab changes selection). Both write Helix's default yank register and the
system clipboard; clipboard failures still leave the value available for `p`.
**d** loads the full local commit patch into the popup, independent of GitHub.
This runs bounded asynchronous `git show` (five seconds, 1 MiB), with external
diff and textconv disabled. Oversized patches display an explicit error.
Merge commits compare against their first parent; binary changes use Git's summary.
Commit is selected initially. Enrichment preserves selection and scroll position
instead of replacing the popup. Height fits the content (long bodies scroll),
with blank rows before headings and between headings and their body.
Popups in the bottom half of the screen prefer to expand upward.
**Enter** opens it in your default browser. **Ctrl-d/u** or Page Down/Up scrolls,
and **Esc** dismisses/cancels the popup. These are not mouse-click/OSC-8 terminal
hyperlinks. Replies for changed documents/versions/cursor lines are ignored, and
late replies cannot reopen dismissed popups. Only HTTPS links can be opened.
Ported commits with `Source:` provenance use the fork for commit association
checks and retain the upstream PR as a separate unverified message hint.
Confirmed unpublished commits omit the dead commit link; publishing the branch
makes that hash available on GitHub.

The core Steel bindings provide asynchronous native metadata, generic bounded
process execution, private JSON cache storage, and a linked Markdown popup.
GitHub endpoint choice, cache policy,
formatting, and the binding live in this Scheme module. To disable the plugin,
remove `(configure-gitlens!)` from `helix.scm` and restore your desired `B` binding.

See [Octocrab research](octocrab-research.md) for the native-client assessment.
