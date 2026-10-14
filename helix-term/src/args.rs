use anyhow::Result;
use helix_core::Position;
use helix_view::tree::Layout;
use indexmap::IndexMap;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Args {
    pub display_help: bool,
    pub display_version: bool,
    pub health: bool,
    pub health_arg: Option<String>,
    pub load_tutor: bool,
    pub fetch_grammars: bool,
    pub build_grammars: bool,
    pub strict: bool,
    pub pager: bool,
    pub diff: bool,
    pub render_diff: bool,
    pub patch: bool,
    pub diff_layout: Option<crate::diff_render::Layout>,
    pub diff_width: Option<u16>,
    pub diff_context: Option<usize>,
    pub diff_theme: Option<String>,
    pub diff_color: Option<crate::diff_render::ColorMode>,
    pub stdin: bool,
    pub language: Option<String>,
    pub split: Option<Layout>,
    pub verbosity: u64,
    pub log_file: Option<PathBuf>,
    pub config_file: Option<PathBuf>,
    pub files: IndexMap<PathBuf, Vec<Position>>,
    pub working_directory: Option<PathBuf>,
}

impl Args {
    pub fn parse_args() -> Result<Args> {
        Self::parse_from(std::env::args())
    }

    #[allow(clippy::too_many_lines)]
    fn parse_from(argv: impl IntoIterator<Item = String>) -> Result<Args> {
        let mut args = Args::default();
        let mut argv = argv.into_iter().peekable();
        let mut line_number = 0;

        let mut insert_file_with_position = |file_with_position: &str| {
            let (filename, position) = parse_file(file_with_position);

            // Before setting the working directory, resolve all the paths in args.files
            let filename = helix_stdx::path::canonicalize(filename);

            args.files
                .entry(filename)
                .and_modify(|positions| positions.push(position))
                .or_insert_with(|| vec![position]);
        };

        argv.next(); // skip the program, we don't care about that

        while let Some(arg) = argv.next() {
            match arg.as_str() {
                "--" => break, // stop parsing at this point treat the remaining as files
                "--version" => args.display_version = true,
                "--help" => args.display_help = true,
                "--strict" => args.strict = true,
                "--pager" => args.pager = true,
                "--diff" => args.diff = true,
                "--render-diff" => args.render_diff = true,
                "--patch" => args.patch = true,
                "--diff-layout" => {
                    args.diff_layout = Some(match argv.next().as_deref() {
                        Some("inline") => crate::diff_render::Layout::Inline,
                        Some("side-by-side") => crate::diff_render::Layout::SideBySide,
                        _ => anyhow::bail!("--diff-layout requires inline or side-by-side"),
                    })
                }
                "--width" => {
                    let width = argv
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--width requires a column count"))?
                        .parse()?;
                    anyhow::ensure!((20..=4096).contains(&width), "--width must be 20..4096");
                    args.diff_width = Some(width);
                }
                "--diff-context" => {
                    let context = argv
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--diff-context requires a line count"))?
                        .parse()?;
                    anyhow::ensure!(context <= 1000, "--diff-context must be 0..1000");
                    args.diff_context = Some(context);
                }
                "--diff-theme" => {
                    args.diff_theme = Some(
                        argv.next()
                            .ok_or_else(|| anyhow::anyhow!("--diff-theme requires a theme name"))?,
                    )
                }
                "--color" => {
                    args.diff_color = Some(match argv.next().as_deref() {
                        Some("auto") => crate::diff_render::ColorMode::Auto,
                        Some("always") => crate::diff_render::ColorMode::Always,
                        Some("never") => crate::diff_render::ColorMode::Never,
                        _ => anyhow::bail!("--color requires auto, always or never"),
                    })
                }
                "-" => args.stdin = true,
                "--language" => {
                    args.language =
                        Some(argv.next().filter(|s| !s.starts_with('-')).ok_or_else(|| {
                            anyhow::anyhow!("--language requires a language name")
                        })?);
                }
                "--tutor" => args.load_tutor = true,
                "--vsplit" => match args.split {
                    Some(_) => anyhow::bail!("can only set a split once of a specific type"),
                    None => args.split = Some(Layout::Vertical),
                },
                "--hsplit" => match args.split {
                    Some(_) => anyhow::bail!("can only set a split once of a specific type"),
                    None => args.split = Some(Layout::Horizontal),
                },
                "--health" => {
                    args.health = true;
                    args.health_arg = argv.next_if(|opt| !opt.starts_with('-'));
                }
                "-g" | "--grammar" => match argv.next().as_deref() {
                    Some("fetch") => args.fetch_grammars = true,
                    Some("build") => args.build_grammars = true,
                    _ => {
                        anyhow::bail!("--grammar must be followed by either 'fetch' or 'build'")
                    }
                },
                "-c" | "--config" => match argv.next().as_deref() {
                    Some(path) => args.config_file = Some(path.into()),
                    None => anyhow::bail!("--config must specify a path to read"),
                },
                "--log" => match argv.next().as_deref() {
                    Some(path) => args.log_file = Some(path.into()),
                    None => anyhow::bail!("--log must specify a path to write"),
                },
                "-w" | "--working-dir" => match argv.next().as_deref() {
                    Some(path) => {
                        args.working_directory = if Path::new(path).is_dir() {
                            Some(PathBuf::from(path))
                        } else {
                            anyhow::bail!(
                                "--working-dir specified does not exist or is not a directory"
                            )
                        }
                    }
                    None => {
                        anyhow::bail!("--working-dir must specify an initial working directory")
                    }
                },
                arg if arg.starts_with("--") => {
                    anyhow::bail!("unexpected double dash argument: {}", arg)
                }
                arg if arg.starts_with('-') => {
                    let arg = arg.get(1..).unwrap().chars();
                    for chr in arg {
                        match chr {
                            'v' => args.verbosity += 1,
                            'V' => args.display_version = true,
                            'h' => args.display_help = true,
                            _ => anyhow::bail!("unexpected short arg {}", chr),
                        }
                    }
                }
                "+" => line_number = usize::MAX,
                arg if arg.starts_with('+') => {
                    match arg[1..].parse::<usize>() {
                        Ok(n) => line_number = n.saturating_sub(1),
                        _ => insert_file_with_position(arg),
                    };
                }
                arg => insert_file_with_position(arg),
            }
        }

        // push the remaining args, if any to the files
        for arg in argv {
            insert_file_with_position(&arg);
        }

        if line_number != 0 {
            if let Some(first_position) = args
                .files
                .first_mut()
                .and_then(|(_, positions)| positions.first_mut())
            {
                first_position.row = line_number;
            }
        }

        if args.stdin && !args.files.is_empty() {
            anyhow::bail!("stdin (-) cannot be combined with file arguments");
        }
        if (args.diff || args.render_diff) && args.files.is_empty() {
            args.patch = true;
        }
        if args.patch {
            anyhow::ensure!(
                args.files.len() <= 1 && !args.load_tutor && args.split.is_none(),
                "--patch takes stdin or one patch file, without tutor or split flags"
            );
            if !args.render_diff {
                args.pager = true;
            }
            if args.diff_layout.is_none() && !args.render_diff {
                args.diff_layout = Some(crate::diff_render::Layout::Inline);
            }
        }
        if args.diff && !args.patch {
            anyhow::ensure!(
                args.files.len() == 2 && !args.stdin && !args.load_tutor && args.split.is_none(),
                "--diff requires exactly two file paths, without stdin, tutor or split flags"
            );
            if args.diff_layout == Some(crate::diff_render::Layout::Inline) {
                args.pager = true;
            } else {
                args.split = Some(Layout::Vertical);
            }
        }
        if args.render_diff {
            anyhow::ensure!(
                (args.files.len() == 2 || args.patch)
                    && (!args.diff || args.patch)
                    && !args.pager
                    && (!args.stdin || args.patch)
                    && !args.load_tutor
                    && args.split.is_none()
                    && args.working_directory.is_none(),
                "--render-diff requires two file/directory paths or patch input, without editor or pager flags"
            );
        } else {
            anyhow::ensure!(
                (args.diff_layout.is_none() || args.diff || args.patch)
                    && args.diff_width.is_none()
                    && args.diff_context.is_none()
                    && args.diff_theme.is_none()
                    && args.diff_color.is_none(),
                "diff layout requires --diff/--patch/--render-diff; width/context/theme/color require --render-diff"
            );
        }
        if args.language.is_some() && !args.pager {
            anyhow::bail!("--language is currently supported with --pager only");
        }
        if args.pager && args.load_tutor {
            anyhow::bail!("--pager cannot be combined with --tutor");
        }
        Ok(args)
    }
}

/// Parse arg into [`PathBuf`] and position.
pub(crate) fn parse_file(s: &str) -> (PathBuf, Position) {
    let def = || (PathBuf::from(s), Position::default());
    if Path::new(s).exists() {
        return def();
    }
    split_path_row_col(s)
        .or_else(|| split_path_row(s))
        .unwrap_or_else(def)
}

/// Split file.rs:10:2 into [`PathBuf`], row and col.
///
/// Does not validate if file.rs is a file or directory.
fn split_path_row_col(s: &str) -> Option<(PathBuf, Position)> {
    let mut s = s.trim_end_matches(':').rsplitn(3, ':');
    let col: usize = s.next()?.parse().ok()?;
    let row: usize = s.next()?.parse().ok()?;
    let path = s.next()?.into();
    let pos = Position::new(row.saturating_sub(1), col.saturating_sub(1));
    Some((path, pos))
}

/// Split file.rs:10 into [`PathBuf`] and row.
///
/// Does not validate if file.rs is a file or directory.
fn split_path_row(s: &str) -> Option<(PathBuf, Position)> {
    let (path, row) = s.trim_end_matches(':').rsplit_once(':')?;
    let row: usize = row.parse().ok()?;
    let path = path.into();
    let pos = Position::new(row.saturating_sub(1), 0);
    Some((path, pos))
}

#[cfg(test)]
mod pager_tests {
    use super::Args;

    fn parse(args: &[&str]) -> anyhow::Result<Args> {
        Args::parse_from(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn read_only_diff_render_options_are_validated_separately_from_editor_modes() {
        for flags in [
            vec!["hx", "--patch"],
            vec!["hx", "--diff", "-"],
            vec!["hx", "--render-diff", "--patch"],
        ] {
            assert!(parse(&flags).unwrap().patch);
        }
        let args = parse(&[
            "hx",
            "--render-diff",
            "--diff-layout",
            "inline",
            "--width",
            "80",
            "--diff-context",
            "0",
            "--color",
            "always",
            "left",
            "right",
        ])
        .unwrap();
        assert!(args.render_diff && !args.diff && !args.pager);
        assert_eq!(args.diff_width, Some(80));
        assert_eq!(args.diff_layout, Some(crate::diff_render::Layout::Inline));
        for args in [
            vec!["hx", "--render-diff", "left"],
            vec!["hx", "--diff", "--render-diff", "left", "right"],
            vec!["hx", "--diff-layout", "inline", "left", "right"],
            vec!["hx", "--render-diff", "--width", "0", "left", "right"],
            vec!["hx", "--render-diff", "--color", "wat", "left", "right"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn pager_flags_and_explicit_stdin() {
        let args = parse(&["hx", "--pager", "--language", "rust", "-"]).unwrap();
        assert!(args.pager && args.stdin);
        assert_eq!(args.language.as_deref(), Some("rust"));
        assert!(args.files.is_empty());
        assert!(!parse(&["hx", "file.rs"]).unwrap().pager);
    }

    #[test]
    fn diff_requires_two_distinct_paths_and_vertical_layout() {
        let inline = parse(&["hx", "--diff", "--diff-layout", "inline", "a", "b"]).unwrap();
        assert!(inline.pager && inline.split.is_none());
        let args = parse(&["hx", "--diff", "left.rs", "right.rs"]).unwrap();
        assert!(args.diff);
        assert!(matches!(args.split, Some(super::Layout::Vertical)));
        assert!(parse(&["hx", "--diff", "--pager", "a", "b"]).unwrap().pager);
        for args in [
            vec!["hx", "--diff", "same", "same"],
            vec!["hx", "--diff", "a", "b", "c"],
            vec!["hx", "--diff", "--hsplit", "a", "b"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn rejects_ambiguous_and_incomplete_pager_arguments() {
        for args in [
            vec!["hx", "--pager", "--language"],
            vec!["hx", "--language", "rust"],
            vec!["hx", "--pager", "-", "file.rs"],
            vec!["hx", "--pager", "--tutor"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }
}
