use super::helpers::*;
use helix_view::{current_ref, document::Mode};

#[tokio::test(flavor = "multi_thread")]
async fn pager_blocks_edit_save_and_move_and_quits_without_prompt() -> anyhow::Result<()> {
    let source = temp_file_with_contents("fn main() { println!(\"hello\"); }\n")?;
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("must-not-be-created.rs");
    let mut app = AppBuilder::new()
        .with_file(source.path(), None)
        .with_pager(Some("rust"))
        .build()?;
    let commands = format!(
        "i<esc>dp<esc>:w! {}<ret>:mv! {}<ret>",
        destination.display(),
        destination.display()
    );
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(&commands),
                Some(&|app| {
                    let (_, doc) = current_ref!(app.editor);
                    assert!(doc.is_read_only());
                    assert!(!doc.is_modified());
                    assert_eq!(
                        doc.text().to_string(),
                        "fn main() { println!(\"hello\"); }\n"
                    );
                    assert_eq!(doc.language_name(), Some("rust"));
                    assert!(
                        doc.syntax().is_some(),
                        "use Helix's normal Tree-sitter parser"
                    );
                    assert!(doc.language_servers().next().is_none());
                    assert!(doc.diff_handle.is_none());
                    assert_eq!(app.editor.mode(), Mode::Normal);
                }),
            ),
            (Some("q"), None),
        ],
        true,
    )
    .await?;
    assert!(!destination.exists());
    assert_eq!(
        std::fs::read_to_string(source.path())?,
        "fn main() { println!(\"hello\"); }\n"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn pager_preserves_search_selection_and_yank() -> anyhow::Result<()> {
    let source = temp_file_with_contents("hello world\nsecond line\n")?;
    let mut app = AppBuilder::new()
        .with_file(source.path(), None)
        .with_pager(None)
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("/world<ret>y"),
                Some(&|app| {
                    let (_, doc) = current_ref!(app.editor);
                    assert!(!doc.is_modified());
                    assert_eq!(
                        doc.selection(app.editor.tree.focus)
                            .fragments(doc.text().slice(..))
                            .next()
                            .unwrap(),
                        "world"
                    );
                }),
            ),
            (Some("q"), None),
        ],
        true,
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn pager_supports_plain_text_and_rejects_unknown_language() -> anyhow::Result<()> {
    let source = temp_file_with_contents("hello\n")?;
    let mut app = AppBuilder::new()
        .with_file(source.path(), None)
        .with_pager(Some("text"))
        .build()?;
    let (_, doc) = current_ref!(app.editor);
    assert!(doc.language_name().is_none());
    assert!(doc.syntax().is_none());
    test_key_sequences(&mut app, vec![(Some("q"), None)], true).await?;
    assert!(AppBuilder::new()
        .with_file(source.path(), None)
        .with_pager(Some("not-a-language"))
        .build()
        .is_err());
    Ok(())
}
