# Steel configuration and plugins

Neohelix includes Steel by default. TOML remains supported, but neither a global
`config.toml` nor `languages.toml` is required for a Steel-only setup. Your config
normally lives in `~/.config/helix/` (`%AppData%\helix\` on Windows), outside the checkout.

## A minimal setup

Put reusable definitions in `helix.scm`, with an exported installer:

```scheme
;; ~/.config/helix/helix.scm
(require "helix/configuration.scm")
(provide configure!)

(define (configure!)
  (mouse #f)
  (line-number 'relative)
  (cursor-shape #:normal 'block #:insert 'bar #:select 'underline)
  (lsp (hash 'display-inlay-hints #t)))
```

Call it from `init.scm`:

```scheme
;; ~/.config/helix/init.scm
(require "helix.scm")
(configure!)
```

This separates importing definitions from applying settings. Larger configs can
split editing, appearance, languages and keybindings into modules, export an
installer from each, and call them from the entry point. Explicit imports and
`prefix-in` help avoid name collisions.

## Named helpers and structured settings

The public `helix/configuration.scm` module provides helpers such as `mouse`,
`statusline`, `cursor-shape`, `define-lsp` and `define-language`. Structured
settings still use hashes; Scheme does not require everything to be flat or
top-level. The additional examples below belong inside `configure!`, with the
module's `helix/configuration.scm` import in scope. For options without a suitable
helper, use the generic API:

```scheme
(set-option! "auto-reload" #t)
(set-option! "inline-blame.show" "never")
```

Some helpers construct whole sections with defaults. To change only one field
while preserving other configured fields, use `set-option!` on that field.
For example, `statusline` has its own default separator, and the whitespace
helper fills in omitted render settings.

[Shared LSP](shared-lsp.md) defaults on for compatible Unix editor processes.
Opt out with `(lsp (hash 'shared #f))`. Changes affect newly started servers;
restart the editor to change existing connections.

## Languages, servers and formatters

```scheme
(define-lsp "steel-language-server"
  (command "steel-language-server")
  (args '()))

(define-language "scheme"
  (language-servers '("steel-language-server")))
```

Language overrides preserve unrelated built-in fields, including grammars and
roots. A formatter is another language field: for example,
`(formatter (command "typstyle") (args '("--wrap-text")))` inside a
`define-language` form for `"typst"`.

Server initialization JSON is different: supplying `config` to `define-lsp` or
`set-lsp-config!` replaces the existing initialization JSON. Read `get-lsp-config`
and explicitly deep-merge it to retain built-in server options. Do not
unintentionally replace rust-analyzer's initialization settings.

`--health` loads user and trusted-project Steel configuration headlessly, so its
language-server, formatter and clipboard checks reflect the effective settings.
It does not start language servers or enter the terminal UI. Configuration errors
are reported instead of silently checking the built-in defaults.

## Keybindings and plugins

Load plugin commands in `helix.scm` and export commands that should be available
in command mode. Install global bindings separately in your keybindings module.
[GitLens](gitlens.md) is bundled under `plugins/gitlens/`, but importing it installs
no global shortcut. It needs Neohelix's native bindings, not just any Steel fork.

Use `keymap` for declarative mappings or `add-global-keybinding` with a hash for
programmatically composed maps and command sequences. The [optional user
keymap](keybindings.md) is not the fork's built-in default map.

## Startup layers, trust and reload

Startup loads user `helix.scm` and `init.scm` first, then a trusted project's
`.helix/helix.scm` and `.helix/init.scm` when both project files exist. Relative
imports resolve within their own layer. Explicit project settings override user
settings rather than replacing the entire user configuration.

`HELIX_STEEL_CONFIG` selects a different user config directory; it is unnecessary
for the default location and does not bypass project trust. Use `:config-open`
and `:config-reload` to edit/reload configuration. Refreshes triggered by
`:workspace-trust` also work without a placeholder TOML file. Existing server
connections may require an editor restart to adopt new initialization settings.

Trusted Scheme can execute code and perform I/O. Workspace trust still gates
project configuration; read-only pager/diff modes are not a plugin sandbox.
