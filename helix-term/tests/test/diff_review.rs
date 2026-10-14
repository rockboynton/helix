use super::helpers::*;
use helix_term::{diff_render::Layout, ui::EditorView};
use helix_view::current_ref;

fn tree(app: &helix_term::application::Application) -> &helix_term::ui::file_tree::FileTree {
    app.test_component::<EditorView>()
        .unwrap()
        .file_tree
        .as_ref()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn directory_review_navigates_layouts_keeps_edits_and_refreshes_new_files(
) -> anyhow::Result<()> {
    let left = tempfile::tempdir()?;
    let right = tempfile::tempdir()?;
    for root in [left.path(), right.path()] {
        std::fs::create_dir(root.join("src"))?;
    }
    std::fs::write(left.path().join("src/a.rs"), "fn old_a() {}\n")?;
    std::fs::write(right.path().join("src/a.rs"), "fn new_a() {}\n")?;
    std::fs::write(left.path().join("src/b.rs"), "fn old_b() {}\n")?;
    std::fs::write(right.path().join("src/b.rs"), "fn new_b() {}\n")?;
    let mut app = AppBuilder::new()
        .with_file(left.path(), None)
        .with_file(right.path(), None)
        .with_review(Layout::SideBySide)
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                None,
                Some(&|app| {
                    assert_eq!(tree(app).review.as_ref().unwrap().entries.len(), 2);
                    assert_eq!(app.editor.tree.views().count(), 2);
                    assert!(app.editor.tree.views().all(|(view, _)| view.area.x > 0));
                }),
            ),
            (
                Some("A // edited<esc>:diff-file-next<ret>"),
                Some(&|app| {
                    assert!(current_ref!(app.editor)
                        .1
                        .text()
                        .to_string()
                        .contains("new_b"));
                    assert_eq!(tree(app).review.as_ref().unwrap().active, Some(1));
                }),
            ),
            (
                Some(":diff-file-prev<ret>:diff-layout inline<ret>"),
                Some(&|app| {
                    let (_, doc) = current_ref!(app.editor);
                    assert!(doc.is_read_only());
                    assert!(doc.text().to_string().contains("// edited"));
                    assert_eq!(app.editor.tree.views().count(), 1);
                }),
            ),
            (
                Some(":diff-layout side-by-side<ret>"),
                Some(&|app| {
                    assert!(current_ref!(app.editor).1.is_modified());
                    assert!(current_ref!(app.editor)
                        .1
                        .text()
                        .to_string()
                        .contains("// edited"));
                    assert_eq!(app.editor.tree.views().count(), 2);
                    std::fs::write(right.path().join("src/a.rs"), "fn old_a() {}\n").unwrap();
                    std::fs::write(right.path().join("src/b.rs"), "fn refreshed_b() {}\n").unwrap();
                    std::fs::write(right.path().join("src/c.rs"), "fn added() {}\n").unwrap();
                }),
            ),
            (
                Some(":diff-review-refresh<ret>"),
                Some(&|app| {
                    assert_eq!(tree(app).review.as_ref().unwrap().entries.len(), 3);
                    assert!(current_ref!(app.editor)
                        .1
                        .text()
                        .to_string()
                        .contains("// edited"));
                    assert!(current_ref!(app.editor).1.is_modified());
                }),
            ),
            (
                Some(":diff-file-next<ret>"),
                Some(&|app| {
                    assert!(current_ref!(app.editor)
                        .1
                        .text()
                        .to_string()
                        .contains("refreshed_b"));
                }),
            ),
            (
                Some(":diff-file-prev<ret>"),
                Some(&|app| {
                    assert!(current_ref!(app.editor).1.is_modified());
                }),
            ),
            (
                Some(":file-tree<ret>"),
                Some(&|app| {
                    assert!(tree(app).focused);
                }),
            ),
            (
                Some("hh"),
                Some(&|app| {
                    assert!(tree(app).focused);
                    assert_eq!(tree(app).visible().len(), 1);
                }),
            ),
            (
                Some("l<esc>:file-tree-close<ret>"),
                Some(&|app| {
                    assert!(app
                        .test_component::<EditorView>()
                        .unwrap()
                        .file_tree
                        .is_none());
                    assert!(app.editor.documents().any(|doc| doc.is_modified()));
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await?;
    assert_eq!(
        std::fs::read_to_string(right.path().join("src/a.rs"))?,
        "fn old_a() {}\n"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn expandable_filesystem_tree_opens_files_without_covering_editor() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join("src"))?;
    std::fs::write(root.path().join("src/main.rs"), "fn main() {}\n")?;
    let mut app = AppBuilder::new().build()?;
    let command = format!(":file-tree {}<ret>", root.path().display());
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(&command),
                Some(&|app| {
                    assert!(tree(app).focused);
                    assert_eq!(tree(app).visible().len(), 1);
                    assert!(app.editor.tree.views().all(|(view, _)| view.area.x > 0));
                }),
            ),
            (
                Some("lj<ret>"),
                Some(&|app| {
                    assert!(!tree(app).focused);
                    let (_, doc) = current_ref!(app.editor);
                    assert_eq!(doc.language_name(), Some("rust"));
                    assert_eq!(doc.path().unwrap(), &root.path().join("src/main.rs"));
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_file_patch_review_is_read_only_and_switches_layouts() -> anyhow::Result<()> {
    let input = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -10 +20 @@\n-fn old_a() {}\n+fn new_a() {}\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -30 +40 @@\n-fn old_b() {}\n+fn new_b() {}\n";
    let file = temp_file_with_contents(input)?;
    let mut app = AppBuilder::new()
        .with_file(file.path(), None)
        .with_patch(Layout::Inline)
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                None,
                Some(&|app| {
                    assert_eq!(tree(app).review.as_ref().unwrap().entries.len(), 2);
                    assert!(current_ref!(app.editor)
                        .1
                        .text()
                        .to_string()
                        .contains("old_a"));
                }),
            ),
            (
                Some(":diff-file-next<ret>:diff-layout side-by-side<ret>"),
                Some(&|app| {
                    assert_eq!(app.editor.tree.views().count(), 2);
                    assert!(current_ref!(app.editor)
                        .1
                        .text()
                        .to_string()
                        .contains("new_b"));
                    assert!(app.editor.tree.views().all(|(view, _)| app
                        .editor
                        .document(view.doc)
                        .unwrap()
                        .is_read_only()));
                }),
            ),
            (Some("q"), None),
        ],
        true,
    )
    .await?;
    assert_eq!(std::fs::read_to_string(file.path())?, input);
    Ok(())
}
