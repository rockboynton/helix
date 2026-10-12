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
interpolation, bounded output, and a five-minute/128-entry session cache. Only
Shift-B initiates a plugin lookup; no plugin network work runs on startup or render.
The existing `inline-blame.auto-fetch` setting is independent of this plugin.
Missing `gh`, offline/authentication failures, and non-GitHub repositories still
show local metadata. GitHub.com is supported, not GitHub Enterprise hosts. Upstream
is preferred over origin in forks, and up to 20 associated PRs are displayed using
the [commit-to-PR API](https://docs.github.com/en/rest/commits/commits#list-pull-requests-associated-with-a-commit).

Links are keyboard-actionable: **Tab/Shift-Tab** selects a PR or commit link;
**Enter** opens it in your default browser. **Ctrl-d/u** or Page Down/Up scrolls,
and **Esc** dismisses/cancels the popup. These are not mouse-click/OSC-8 terminal
hyperlinks. Replies for changed documents/versions/cursor lines are ignored, and
late replies cannot reopen dismissed popups. Only HTTPS links can be opened.

The core Steel bindings provide asynchronous native metadata, generic bounded
process execution, and a linked Markdown popup. GitHub endpoint choice, caching,
formatting, and the binding live in this Scheme module. To disable the plugin,
remove `(configure-gitlens!)` from `helix.scm` and restore your desired `B` binding.
