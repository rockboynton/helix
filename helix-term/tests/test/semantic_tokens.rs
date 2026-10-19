use super::helpers;
use helix_core::{Rope, Transaction};
use std::{path::PathBuf, time::Duration};

async fn exercise(mode: &str) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("source.rs");
    let log = directory.path().join("requests.jsonl");
    std::fs::write(&source, "fn foo() {}\n")?;
    let server =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../contrib/semantic-token-test-server.py");
    let overrides = format!(
        r#"
[language-server.semantic-test]
command = "python3"
args = [{server:?}, {mode:?}, {log:?}]
[[language]]
name = "rust"
language-servers = ["semantic-test"]
"#
    );
    let mut config = helpers::test_config();
    config.editor.lsp.enable = true;
    config.editor.lsp.shared = false;
    let mut app = helpers::AppBuilder::new()
        .with_config(config)
        .with_lang_loader(helpers::test_syntax_loader(Some(overrides)))
        .with_file(&source, None)
        .build()?;
    let id = app.editor.document_id_by_path(&source).unwrap();
    let color = helix_view::graphics::Color::Rgb(17, 34, 51);
    app.editor.theme.set(
        "type".into(),
        helix_view::graphics::Style::default().fg(color),
    );
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut input = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
    for expected in ["foo", "bar"] {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            anyhow::ensure!(
                tokio::time::timeout(
                    Duration::from_secs(3),
                    app.event_loop_until_idle(&mut input)
                )
                .await?,
                "editor exited"
            );
            let doc = app.editor.document(id).unwrap();
            if doc.semantic_highlights.iter().any(|span| {
                span.scope == "type" && doc.text().slice(span.range.clone()).to_string() == expected
            }) {
                break;
            }
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "no semantic color for {expected} ({mode})"
            );
        }
        let surface = app.test_surface();
        let mut found = false;
        for y in surface.area.top()..surface.area.bottom() {
            for x in surface.area.left()..surface.area.right().saturating_sub(2) {
                let word: String = (0..3)
                    .map(|offset| surface[(x + offset, y)].symbol.as_str())
                    .collect();
                if word == expected {
                    assert_eq!(
                        surface[(x, y)].fg,
                        color,
                        "semantic style must override Tree-sitter on the rendered surface"
                    );
                    found = true;
                }
            }
        }
        assert!(found, "semantic symbol must be visible");
        if expected == "foo" {
            let view = app.editor.tree.focus;
            let doc = app.editor.document_mut(id).unwrap();
            let replacement = Rope::from_str("// 😀\nfn bar() {}\n");
            let edit: Transaction = helix_core::diff::compare_ropes(doc.text(), &replacement);
            doc.apply(&edit, view);
            assert!(
                doc.semantic_highlights.is_empty(),
                "old positions must not survive an edit"
            );
        }
    }
    let messages = std::fs::read_to_string(log)?;
    let messages = messages
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let init = messages
        .iter()
        .find(|msg| msg["method"] == "initialize")
        .unwrap();
    assert_eq!(
        init["params"]["capabilities"]["textDocument"]["semanticTokens"]["augmentsSyntaxTokens"],
        true
    );
    assert_eq!(
        init["params"]["capabilities"]["workspace"]["semanticTokens"]["refreshSupport"],
        true
    );
    let method = format!("textDocument/semanticTokens/{mode}");
    assert!(
        messages
            .iter()
            .filter(|msg| msg["method"] == method)
            .count()
            >= 2
    );
    assert!(messages
        .iter()
        .any(|msg| msg["id"] == "semantic-refresh" && msg.get("result").is_some()));
    helpers::test_key_sequence(&mut app, Some(":qa!<ret>"), None, true).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_tokens_full_round_trip_updates_after_edit_and_refresh() -> anyhow::Result<()> {
    exercise("full").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_tokens_range_only_server_round_trip() -> anyhow::Result<()> {
    exercise("range").await
}

/// Optional native check against a real server, never the developer's project.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_tokens_real_rust_analyzer() -> anyhow::Result<()> {
    let Ok(server) = std::env::var("RUST_ANALYZER") else {
        eprintln!("set RUST_ANALYZER to run the real-server semantic-token check");
        return Ok(());
    };
    let directory = tempfile::tempdir()?;
    std::fs::create_dir(directory.path().join("src"))?;
    std::fs::write(
        directory.path().join("Cargo.toml"),
        "[package]\nname='semantic-fixture'\nversion='0.1.0'\nedition='2021'\n",
    )?;
    let source = directory.path().join("src/main.rs");
    std::fs::write(&source, "struct Thing { value: i32 }\nfn main() { let item = Thing { value: 1 }; let result = item.value; }\n")?;
    let overrides = format!(
        r#"
[language-server.semantic-real]
command = {server:?}
config = {{ cargo = {{ extraArgs = ["--offline"], buildScripts = {{ enable = false }} }}, procMacro = {{ enable = false }}, checkOnSave = false }}
[[language]]
name = "rust"
roots = ["Cargo.toml"]
language-servers = ["semantic-real"]
"#
    );
    let mut config = helpers::test_config();
    config.editor.lsp.enable = true;
    config.editor.lsp.shared = false;
    let mut app = helpers::AppBuilder::new()
        .with_config(config)
        .with_lang_loader(helpers::test_syntax_loader(Some(overrides)))
        .with_file(&source, None)
        .build()?;
    let id = app.editor.document_id_by_path(&source).unwrap();
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut input = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        anyhow::ensure!(
            tokio::time::timeout(
                Duration::from_secs(3),
                app.event_loop_until_idle(&mut input)
            )
            .await?,
            "editor exited"
        );
        let doc = app.editor.document(id).unwrap();
        let has = |name: &str, scope: &str| {
            doc.semantic_highlights.iter().any(|span| {
                span.scope == scope && doc.text().slice(span.range.clone()).to_string() == name
            })
        };
        if has("Thing", "type") && has("value", "variable.other.member") {
            break;
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "real rust-analyzer did not classify struct and field: {:?}",
            doc.semantic_highlights
        );
    }
    helpers::test_key_sequence(&mut app, Some(":qa!<ret>"), None, true).await?;
    Ok(())
}
