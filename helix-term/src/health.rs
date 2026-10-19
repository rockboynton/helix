use crate::config::Config;
use helix_core::config::{default_lang_config, user_lang_config};
use helix_core::syntax::config::Configuration;
use helix_loader::grammar::load_runtime_file;
use std::{
    collections::HashSet,
    io::{IsTerminal, Write},
};
use termina::{
    style::{ColorSpec, StyleExt as _, Stylized},
    Terminal as _,
};

/// Resolve configuration exactly once, including user/trusted-project Steel.
/// Health must not report the built-in tools as if they were the active ones.
fn effective_configuration() -> std::io::Result<(Config, Configuration)> {
    let config = Config::load_default().map_err(|err| std::io::Error::other(err.to_string()))?;
    let trust =
        helix_loader::workspace_trust::WorkspaceTrust::new((&config.editor.workspace_trust).into());
    let languages = user_lang_config(&trust).unwrap_or_else(|err| {
        eprintln!("Error parsing user language config: {err}\nUsing default language config");
        default_lang_config()
    });
    #[cfg(feature = "steel")]
    if config.editor.enable_steel {
        use arc_swap::{access::Map, ArcSwap};
        use std::sync::Arc;

        let loader = Arc::new(ArcSwap::from_pointee(
            helix_core::syntax::Loader::new(languages).map_err(std::io::Error::other)?,
        ));
        let mut headless = config;
        headless.editor.file_watcher.enable = false;
        headless.editor.lsp.enable = false;
        let config = Arc::new(ArcSwap::from_pointee(headless));
        let mut dirs = vec![helix_loader::config_dir()];
        dirs.extend(helix_loader::runtime_dirs().iter().cloned());
        let handlers = crate::handlers::setup(config.clone());
        let mut editor = helix_view::Editor::new(
            helix_view::graphics::Rect::new(0, 0, 80, 24),
            Arc::new(helix_view::theme::Loader::new(&dirs)),
            loader.clone(),
            Arc::new(Map::new(config.clone(), |config: &Config| &config.editor)),
            handlers,
            trust,
        );
        editor.pager = true;
        let mut jobs = crate::job::Jobs::new();
        let mut cx = crate::commands::Context {
            register: None,
            count: None,
            editor: &mut editor,
            callback: Vec::new(),
            on_next_key_callback: None,
            jobs: &mut jobs,
        };
        crate::commands::ScriptingEngine::load_headless_configuration(
            &mut cx,
            config.clone(),
            loader.clone(),
        )
        .map_err(std::io::Error::other)?;
        let loader = loader.load();
        let languages = Configuration {
            language: loader.language_configs().cloned().collect(),
            language_server: loader.language_server_configs().clone(),
        };
        return Ok(((*config.load_full()).clone(), languages));
    }
    Ok((config, languages))
}

#[derive(Copy, Clone)]
pub enum TsFeature {
    Highlight,
    TextObject,
    AutoIndent,
    Tags,
    RainbowBracket,
}

impl TsFeature {
    pub fn all() -> &'static [Self] {
        &[
            Self::Highlight,
            Self::TextObject,
            Self::AutoIndent,
            Self::Tags,
            Self::RainbowBracket,
        ]
    }

    pub fn runtime_filename(&self) -> &'static str {
        match *self {
            Self::Highlight => "highlights.scm",
            Self::TextObject => "textobjects.scm",
            Self::AutoIndent => "indents.scm",
            Self::Tags => "tags.scm",
            Self::RainbowBracket => "rainbows.scm",
        }
    }

    pub fn long_title(&self) -> &'static str {
        match *self {
            Self::Highlight => "Syntax Highlighting",
            Self::TextObject => "Treesitter Textobjects",
            Self::AutoIndent => "Auto Indent",
            Self::Tags => "Code Navigation Tags",
            Self::RainbowBracket => "Rainbow Brackets",
        }
    }

    pub fn short_title(&self) -> &'static str {
        match *self {
            Self::Highlight => "Highlight",
            Self::TextObject => "Textobject",
            Self::AutoIndent => "Indent",
            Self::Tags => "Tags",
            Self::RainbowBracket => "Rainbow",
        }
    }
}

/// Display general diagnostics.
pub fn general() -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    let config_file = helix_loader::config_file();
    let lang_file = helix_loader::lang_config_file();
    let log_file = helix_loader::log_file();
    let rt_dirs = helix_loader::runtime_dirs();

    if config_file.exists() {
        writeln!(stdout, "Config file: {}", config_file.display())?;
    } else {
        writeln!(stdout, "Config file: default")?;
    }
    if lang_file.exists() {
        writeln!(stdout, "Language file: {}", lang_file.display())?;
    } else {
        writeln!(stdout, "Language file: default")?;
    }
    writeln!(stdout, "Log file: {}", log_file.display())?;
    writeln!(
        stdout,
        "Runtime directories: {}",
        rt_dirs
            .iter()
            .map(|d| d.to_string_lossy())
            .collect::<Vec<_>>()
            .join(";")
    )?;
    for rt_dir in rt_dirs.iter() {
        if let Ok(path) = std::fs::read_link(rt_dir) {
            let msg = format!(
                "Runtime directory {} is symlinked to: {}",
                rt_dir.display(),
                path.display()
            );
            writeln!(stdout, "{}", msg.yellow())?;
        }
        if !rt_dir.exists() {
            let msg = format!("Runtime directory does not exist: {}", rt_dir.display());
            writeln!(stdout, "{}", msg.yellow())?;
        } else if rt_dir.read_dir().ok().map(|it| it.count()) == Some(0) {
            let msg = format!("Runtime directory is empty: {}", rt_dir.display());
            writeln!(stdout, "{}", msg.yellow())?;
        }
    }

    Ok(())
}

fn clipboard(config: &Config) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    match config.editor.clipboard_provider.name().as_ref() {
        "none" => {
            writeln!(
                stdout,
                "{}",
                "System clipboard provider: Not installed".red()
            )?;
            writeln!(
                stdout,
                "    {}",
                "For troubleshooting system clipboard issues, refer".red()
            )?;
            writeln!(stdout, "    {}",
                "https://github.com/helix-editor/helix/wiki/Troubleshooting#copypaste-fromto-system-clipboard-not-working"
            .red().underlined())?;
        }
        name => writeln!(stdout, "System clipboard provider: {}", name)?,
    }

    Ok(())
}

fn languages_all(config: Configuration) -> std::io::Result<()> {
    languages(None, config)
}

fn languages_selection(config: Configuration) -> std::io::Result<()> {
    let selection = helix_loader::grammar::get_grammar_names().unwrap_or_default();
    languages(selection, config)
}

fn languages(
    selection: Option<HashSet<String>>,
    mut syn_loader_conf: Configuration,
) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    let mut headings = vec!["Language", "Language servers", "Debug adapter", "Formatter"];

    for feat in TsFeature::all() {
        headings.push(feat.short_title())
    }

    let terminal_cols = termina::PlatformTerminal::new()
        .and_then(|terminal| terminal.get_dimensions())
        .map(|size| size.cols)
        .unwrap_or(80);
    let column_width = terminal_cols as usize / headings.len();
    let is_terminal = std::io::stdout().is_terminal();

    let fit = |s: &str| -> Stylized<'static> {
        format!(
            "{:column_width$}",
            s.get(..column_width - 2)
                .map(|s| format!("{}…", s))
                .unwrap_or_else(|| s.to_string())
        )
        .stylized()
    };
    let color = |s: Stylized<'static>, c: ColorSpec| if is_terminal { s.foreground(c) } else { s };
    let bold = |s: Stylized<'static>| if is_terminal { s.bold() } else { s };

    for heading in headings {
        write!(stdout, "{}", bold(fit(heading)))?;
    }
    writeln!(stdout)?;

    syn_loader_conf
        .language
        .sort_unstable_by_key(|l| l.language_id.clone());

    let check_binary_with_name = |cmd: Option<(&str, &str)>| match cmd {
        Some((name, cmd)) => match helix_stdx::env::which(cmd) {
            Ok(_) => color(fit(&format!("✓ {}", name)), ColorSpec::BRIGHT_GREEN),
            Err(_) => color(fit(&format!("✘ {}", name)), ColorSpec::BRIGHT_RED),
        },
        None => color(fit("None"), ColorSpec::BRIGHT_YELLOW),
    };

    let check_binary = |cmd: Option<&str>| check_binary_with_name(cmd.map(|cmd| (cmd, cmd)));

    for lang in &syn_loader_conf.language {
        if selection
            .as_ref()
            .is_some_and(|s| !s.contains(&lang.language_id))
        {
            continue;
        }

        write!(stdout, "{}", fit(&lang.language_id))?;

        let mut cmds = lang.language_servers.iter().filter_map(|ls| {
            syn_loader_conf
                .language_server
                .get(&ls.name)
                .map(|config| (ls.name.as_str(), config.command.as_str()))
        });
        write!(stdout, "{}", check_binary_with_name(cmds.next()))?;

        let dap = lang.debugger.as_ref().map(|dap| dap.command.as_str());
        write!(stdout, "{}", check_binary(dap))?;

        let formatter = lang
            .formatter
            .as_ref()
            .map(|formatter| formatter.command.as_str());
        write!(stdout, "{}", check_binary(formatter))?;

        for ts_feat in TsFeature::all() {
            match load_runtime_file(&lang.language_id, ts_feat.runtime_filename()).is_ok() {
                true => write!(stdout, "{}", color(fit("✓"), ColorSpec::BRIGHT_GREEN))?,
                false => write!(stdout, "{}", color(fit("✘"), ColorSpec::BRIGHT_RED))?,
            }
        }

        writeln!(stdout)?;

        for cmd in cmds {
            write!(stdout, "{}", fit(""))?;
            writeln!(stdout, "{}", check_binary_with_name(Some(cmd)))?;
        }
    }

    if selection.is_some() {
        writeln!(
            stdout,
            "\nThis list is filtered according to the 'use-grammars' option in languages.toml file.\n\
            To see the full list, use the '--health all' or '--health all-languages' option."
        )?;
    }

    Ok(())
}

/// Display diagnostics pertaining to a particular language (LSP,
/// highlight queries, etc).
fn language(lang_str: String, syn_loader_conf: Configuration) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    let lang = match syn_loader_conf
        .language
        .iter()
        .find(|l| l.language_id == lang_str)
    {
        Some(l) => l,
        None => {
            let msg = format!("Language '{}' not found", lang_str);
            writeln!(stdout, "{}", msg.red())?;
            let suggestions: Vec<&str> = syn_loader_conf
                .language
                .iter()
                .filter(|l| l.language_id.starts_with(lang_str.chars().next().unwrap()))
                .map(|l| l.language_id.as_str())
                .collect();
            if !suggestions.is_empty() {
                let suggestions = suggestions.join(", ");
                writeln!(
                    stdout,
                    "Did you mean one of these: {} ?",
                    suggestions.yellow()
                )?;
            }
            return Ok(());
        }
    };

    probe_protocols(
        "language server",
        lang.language_servers.iter().filter_map(|ls| {
            syn_loader_conf
                .language_server
                .get(&ls.name)
                .map(|config| (ls.name.as_str(), config.command.as_str()))
        }),
    )?;

    probe_protocol(
        "debug adapter",
        lang.debugger.as_ref().map(|dap| dap.command.to_string()),
    )?;

    probe_protocol(
        "formatter",
        lang.formatter
            .as_ref()
            .map(|formatter| formatter.command.to_string()),
    )?;

    probe_parser(lang.grammar.as_ref().unwrap_or(&lang.language_id))?;

    for ts_feat in TsFeature::all() {
        probe_treesitter_feature(&lang_str, *ts_feat)?
    }

    Ok(())
}

fn probe_parser(grammar_name: &str) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    write!(stdout, "Tree-sitter parser: ")?;

    match helix_loader::grammar::get_language(grammar_name) {
        Ok(Some(_)) => writeln!(stdout, "{}", "✓".green()),
        Ok(None) | Err(_) => writeln!(stdout, "{}", "None".yellow()),
    }
}

/// Display diagnostics about multiple LSPs and DAPs.
fn probe_protocols<'a, I: Iterator<Item = (&'a str, &'a str)> + 'a>(
    protocol_name: &str,
    server_cmds: I,
) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let mut server_cmds = server_cmds.peekable();

    write!(stdout, "Configured {}s:", protocol_name)?;
    if server_cmds.peek().is_none() {
        writeln!(stdout, "{}", " None".yellow())?;
        return Ok(());
    }
    writeln!(stdout)?;

    for (name, cmd) in server_cmds {
        let (diag, icon) = match helix_stdx::env::which(cmd) {
            Ok(path) => (path.display().to_string().green(), "✓".green()),
            Err(_) => (format!("'{}' not found in $PATH", cmd).red(), "✘".red()),
        };
        writeln!(stdout, "  {} {}: {}", icon, name, diag)?;
    }

    Ok(())
}

/// Display diagnostics about LSP and DAP.
fn probe_protocol(protocol_name: &str, server_cmd: Option<String>) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    write!(stdout, "Configured {}:", protocol_name)?;
    let Some(cmd) = server_cmd else {
        writeln!(stdout, "{}", " None".yellow())?;
        return Ok(());
    };
    writeln!(stdout)?;

    let (diag, icon) = match helix_stdx::env::which(&cmd) {
        Ok(path) => (path.display().to_string().green(), "✓".green()),
        Err(_) => (format!("'{}' not found in $PATH", cmd).red(), "✘".red()),
    };
    writeln!(stdout, "  {} {}", icon, diag)?;

    Ok(())
}

/// Display diagnostics about a feature that requires tree-sitter
/// query files (highlights, textobjects, etc).
fn probe_treesitter_feature(lang: &str, feature: TsFeature) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    let found = match load_runtime_file(lang, feature.runtime_filename()).is_ok() {
        true => "✓".green(),
        false => "✘".red(),
    };
    writeln!(stdout, "{} queries: {}", feature.short_title(), found)?;

    Ok(())
}

pub fn print_health(health_arg: Option<String>) -> std::io::Result<()> {
    let (config, languages) = effective_configuration()?;
    match health_arg.as_deref() {
        Some("languages") => languages_selection(languages)?,
        Some("all-languages") => languages_all(languages)?,
        Some("clipboard") => clipboard(&config)?,
        None => {
            general()?;
            clipboard(&config)?;
            writeln!(std::io::stdout().lock())?;
            languages_selection(languages)?;
        }
        Some("all") => {
            general()?;
            clipboard(&config)?;
            writeln!(std::io::stdout().lock())?;
            languages_all(languages)?;
        }
        Some(lang) => language(lang.to_string(), languages)?,
    }
    Ok(())
}
