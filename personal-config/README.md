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

`init.scm` applies editor settings and merges your keybindings into Helix's defaults.
`helix.scm` is loaded first as a module: define/export reusable plugin functions
there. The migrated blame settings are `never` with background fetching enabled;
the original `Space B` binding requests line blame. Nothing installs or replaces
your normal `hx` executable.
