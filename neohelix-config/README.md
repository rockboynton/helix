# Neohelix Steel configuration

This directory is an example configuration and test fixture for Neohelix.
The active daily-driver config lives in the NixOS repository's `helix/`
directory, symlinked by Home Manager into `~/.config/helix`.

Run installed `hx` normally. To run a checkout build with the same active
config, use `./contrib/hx-neohelix [files...]` after building
`nix develop --command cargo build --release --locked -p helix-term`.
That launcher sets only the checkout runtime; it does not redirect your config.

## Organization

- `init.scm`: startup wiring; calls `configure-neohelix!`.
- `helix.scm`: imports modules with prefixes and applies them in an explicit order.
- `editing.scm`: editing, completion, saving, file handling, LSP, and blame settings.
- `languages.scm`: servers, server options, formatters, and file associations.
- `appearance.scm`: display, diagnostics, theme, and icons.
- `keybindings.scm`: named keymaps for modes and Space/tool/VCS submenus.
- `gitlens.scm` and `gitlens-cache.scm`: on-demand blame and cached PR enrichment.

Imports define functions without applying configuration. The entry point applies
editing, languages, appearance, keybindings, then GitLens. True-color is enabled
before loading the theme. Keymaps merge into Helix defaults.

Steel startup runs the user configuration first, then the trusted project's
`.helix/helix.scm` and `.helix/init.scm` when both exist. Each layer resolves its
relative imports in its own directory. Project settings can override user
settings, but do not replace the whole user configuration. `HELIX_STEEL_CONFIG`,
when set, selects the user layer; it does not bypass project trust. Config-open
commands continue to target the user layer. Missing global TOML files leave
built-in defaults in place. Trusted workspace TOML overrides remain supported.

Language overrides preserve unrelated built-in fields. Initialization JSON is
deep-merged explicitly, and document refreshes are batched once. Harper remains
disabled; Scheme uses `steel-language-server`. Install server executables
separately. `--health` runs before Steel initialization and therefore reports
the built-in/TOML layer, not runtime overrides.

See the [GitLens guide](../docs/gitlens.md) for features, controls, caching,
repository routing, and limitations.
