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

Modules export only their configuration functions. Importing a module does not
apply settings; the entry point does that explicitly. Keep related settings in
their owning module and avoid introducing helpers for one-off options. Keymaps
merge into Helix's defaults, so unspecified bindings retain their default behavior.
External shell commands remain literal to preserve Helix's `%` expansions.

Editing settings are applied before appearance so true-color is enabled before
loading the theme and icons. The migrated blame settings remain `never` with
background fetching enabled; the original `Space B` requests line blame.
Nothing installs or replaces your normal `hx` executable.
