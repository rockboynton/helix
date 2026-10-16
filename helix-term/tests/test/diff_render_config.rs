//! Exercise the flow frontend in a subprocess: no terminal or inherited user config.
use std::process::{Command, Output};

#[test]
fn flow_diff_uses_steel_theme_and_languages_without_terminal_output() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let user = root.path().join("config/helix");
    std::fs::create_dir_all(&user)?;
    std::fs::write(
        user.join("helix.scm"),
        r#"
        (require "helix/configuration.scm")
        (require (prefix-in hx. "helix/commands.scm"))
        (provide configure!)
        (define (configure!)
          (set-option! "true-color" #t)
          (update-language-config! "rust" (hash "name" "rust" "file-types" (list "neo-test")))
          (hx.theme "gruvbox_dark_hard"))
    "#,
    )?;
    std::fs::write(
        user.join("init.scm"),
        r#"
        (require "helix.scm")
        (configure!)
        (displayln "configuration ran")
    "#,
    )?;
    // A repository config must still be gated by normal workspace trust.
    let git = Command::new("git")
        .args(["init", "-q"])
        .current_dir(root.path())
        .output()?;
    assert!(git.status.success());
    let project = root.path().join(".helix");
    std::fs::create_dir_all(&project)?;
    std::fs::write(
        project.join("helix.scm"),
        "(provide unused) (define unused #t)",
    )?;
    std::fs::write(
        project.join("init.scm"),
        "(error \"untrusted project executed\")",
    )?;
    std::fs::write(root.path().join("before.neo-test"), "fn old() {}\n")?;
    std::fs::write(root.path().join("after.neo-test"), "fn new() {}\n")?;

    let render = |layout: &str, theme: Option<&str>| -> anyhow::Result<Output> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hx"));
        command
            .env("HELIX_DIFF_WORKER", "0")
            .env("HELIX_DIFF_CACHE", "0")
            .current_dir(root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("STEEL_HOME", root.path().join("steel"))
            .env_remove("HELIX_STEEL_CONFIG")
            .env("COLORTERM", "truecolor")
            .args([
                "--render-diff",
                "--diff-layout",
                layout,
                "--color",
                "always",
            ]);
        if let Some(theme) = theme {
            command.args(["--diff-theme", theme]);
        }
        Ok(command
            .args(["before.neo-test", "after.neo-test"])
            .output()?)
    };
    for layout in ["inline", "side-by-side"] {
        let implicit = render(layout, None)?;
        assert!(
            implicit.status.success(),
            "{}",
            String::from_utf8_lossy(&implicit.stderr)
        );
        let stdout = String::from_utf8(implicit.stdout.clone())?;
        let stderr = String::from_utf8(implicit.stderr)?;
        assert!(stderr.contains("configuration ran"), "{stderr}");
        assert!(!stdout.contains("configuration ran"));
        assert!(!stderr.contains("untrusted project executed"), "{stderr}");
        assert!(
            stdout.contains("38;2;251;73;52"),
            "Steel's Rust file type and Gruvbox keyword color must apply: {stdout:?}"
        );
        let explicit = render(layout, Some("gruvbox_dark_hard"))?;
        assert!(
            explicit.status.success(),
            "{}",
            String::from_utf8_lossy(&explicit.stderr)
        );
        assert_eq!(implicit.stdout, explicit.stdout);
        let overridden = render(layout, Some("base16_default_dark"))?;
        assert!(
            overridden.status.success(),
            "{}",
            String::from_utf8_lossy(&overridden.stderr)
        );
        assert_ne!(
            implicit.stdout, overridden.stdout,
            "CLI theme must override Steel"
        );
        // The host receives SGR colors, never terminal acquisition/cursor controls.
        let mut rest = stdout.as_str();
        while let Some((_, control)) = rest.split_once('\u{1b}') {
            let control = control.strip_prefix('[').expect("only CSI SGR is allowed");
            let (parameters, tail) = control.split_once('m').expect("SGR terminator");
            assert!(
                parameters.chars().all(|c| c.is_ascii_digit() || c == ';'),
                "unexpected terminal control: {parameters:?}"
            );
            rest = tail;
        }
    }
    Ok(())
}
