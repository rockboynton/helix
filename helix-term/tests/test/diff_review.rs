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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tree_icons_use_configured_glyphs_styles_and_picker_visibility() -> anyhow::Result<()> {
    use helix_term::ui::file_tree::FileTree;
    use helix_view::{
        graphics::{Color, Rect, Style},
        icons::{Icon, IconStyle},
    };
    use std::{collections::HashMap, path::PathBuf};
    for enabled in [true, false] {
        let mut config = helix_term::config::Config::default();
        config.editor = test_editor_config();
        config.editor.icons.picker = enabled;
        let mut app = AppBuilder::new().with_config(config).build()?;
        let icon = |icon_char| Icon {
            icon_char,
            style: Some(IconStyle::Custom(Style::default().fg(Color::Magenta))),
        };
        app.editor.icons.ui = Some(HashMap::from([
            ("folder".into(), icon('📁')),
            ("folder_opened".into(), icon('📂')),
        ]));
        app.editor.icons.mime_type = Some(HashMap::from([("rs".into(), icon('🦀'))]));
        let mut sidebar = FileTree::from_paths(
            PathBuf::from("/virtual"),
            [(PathBuf::from("src/main.rs"), false, None)],
        );
        let area = Rect::new(0, 0, 40, 6);
        let mut surface = tui::buffer::Buffer::empty(area);
        sidebar.render(area, &mut surface, &app.editor);
        assert_eq!(
            surface
                .content
                .iter()
                .any(|cell| cell.symbol.as_str() == "📁"),
            enabled
        );
        sidebar.expand_all();
        sidebar.render(area, &mut surface, &app.editor);
        for glyph in ["📂", "🦀"] {
            let cell = surface
                .content
                .iter()
                .find(|cell| cell.symbol.as_str() == glyph);
            assert_eq!(cell.is_some(), enabled);
            if let Some(cell) = cell {
                assert_eq!(cell.fg, Color::Magenta);
            }
        }
        let text = surface
            .content
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        assert!(text.contains("main.rs"));
        for width in 0..9 {
            sidebar.render(Rect::new(0, 0, width, 6), &mut surface, &app.editor);
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn window_binding_toggles_review_tree_focus_without_hiding_or_changing_panes(
) -> anyhow::Result<()> {
    let left = tempfile::tempdir()?;
    let right = tempfile::tempdir()?;
    std::fs::write(left.path().join("file.rs"), "fn old() {}\n")?;
    std::fs::write(right.path().join("file.rs"), "fn new() {}\n")?;
    let mut config = helix_term::config::Config::default();
    config.editor = test_editor_config();
    config.keys = toml::from_str::<helix_term::config::ConfigRaw>(
        "[keys.normal.C-w]\ne = ':file-tree-focus'\n[keys.select.C-w]\ne = ':file-tree-focus'\n",
    )?
    .keys
    .unwrap();
    let mut app = AppBuilder::new()
        .with_config(config)
        .with_file(left.path(), None)
        .with_file(right.path(), None)
        .with_review(Layout::SideBySide)
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("<C-w>e"),
                Some(&|app| {
                    assert!(tree(app).focused);
                    assert!(tree(app).visible);
                    assert_eq!(app.editor.tree.views().count(), 2);
                }),
            ),
            (
                Some("<C-w>e"),
                Some(&|app| {
                    assert!(!tree(app).focused);
                    assert!(tree(app).visible);
                    assert_eq!(app.editor.tree.views().count(), 2);
                }),
            ),
            (
                Some(":file-tree-toggle<ret>"),
                Some(&|app| assert!(!tree(app).visible)),
            ),
            (
                Some("<C-w>e"),
                Some(&|app| {
                    assert!(tree(app).visible);
                    assert!(tree(app).focused);
                }),
            ),
            (Some("<C-w>e"), Some(&|app| assert!(!tree(app).focused))),
            (
                Some("v"),
                Some(&|app| assert_eq!(app.editor.mode(), helix_view::document::Mode::Select)),
            ),
            (
                Some("<C-w>e"),
                Some(&|app| {
                    assert!(tree(app).focused);
                    assert_eq!(app.editor.mode(), helix_view::document::Mode::Select);
                }),
            ),
            (Some("<C-w>e"), Some(&|app| assert!(!tree(app).focused))),
            (Some("<esc>:qa!<ret>"), None),
        ],
        true,
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_jj_revision_selection_reuses_panes_and_keeps_history_read_only(
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let jj = |args: &[&str]| -> anyhow::Result<()> {
        let result = std::process::Command::new("jj")
            .args(args)
            .current_dir(&root)
            .output()?;
        anyhow::ensure!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        Ok(())
    };
    jj(&["git", "init", "--colocate"])?;
    let path = root.join("file.rs");
    std::fs::write(&path, "fn historical() {}\n")?;
    jj(&["describe", "-m", "historical revision"])?;
    jj(&["new"])?;
    std::fs::write(&path, "fn working() {}\n")?;
    jj(&["describe", "-m", "working revision"])?;
    let mut app = AppBuilder::new().with_file(&path, None).build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(":jj-review @<ret>"),
                Some(&|app| {
                    let surface = app
                        .test_surface()
                        .content
                        .iter()
                        .map(|cell| cell.symbol.as_str())
                        .collect::<String>();
                    assert!(surface.contains("working revision"), "{surface}");
                }),
            ),
            (
                Some("<ret>"),
                Some(&|app| {
                    assert_eq!(app.editor.tree.views().count(), 2);
                    assert!(!current_ref!(app.editor).1.is_read_only());
                    assert_eq!(current_ref!(app.editor).1.path(), Some(path.as_path()));
                }),
            ),
            (Some("A // unsaved<esc>:diff-layout inline<ret>"), None),
            (Some(":jj-review @-<ret>"), None),
            (
                Some("<ret>"),
                Some(&|app| {
                    assert_eq!(app.editor.tree.views().count(), 1);
                    assert_eq!(tree(app).review.as_ref().unwrap().layout, Layout::Inline);
                    assert!(matches!(
                        tree(app).review.as_ref().unwrap().origin,
                        helix_term::diff_review::Origin::JjRevision { .. }
                    ));
                    assert!(current_ref!(app.editor).1.is_read_only());
                    assert!(current_ref!(app.editor)
                        .1
                        .text()
                        .to_string()
                        .contains("historical"));
                    assert!(app
                        .editor
                        .documents()
                        .any(|doc| doc.path() == Some(path.as_path()) && doc.is_modified()));
                }),
            ),
            // Snapshot buffers have no path; the command must reuse sidebar context
            // rather than accidentally browsing the test runner's own repository.
            (Some(":jjr @<ret>"), None),
            (
                Some("<ret>"),
                Some(&|app| {
                    assert_eq!(tree(app).root, root);
                    assert!(app.editor.inline_diff_session.is_some());
                    assert!(current_ref!(app.editor)
                        .1
                        .text()
                        .to_string()
                        .contains("unsaved"));
                }),
            ),
            (Some(":diff-off<ret>:qa!<ret>"), None),
        ],
        true,
    )
    .await?;
    assert_eq!(std::fs::read_to_string(path)?, "fn working() {}\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_expands_changed_folders_and_restores_collapsed_browsing_tree() -> anyhow::Result<()>
{
    use std::path::Path;
    let left = tempfile::tempdir()?;
    let right = tempfile::tempdir()?;
    for name in ["aaa/one.rs", "zzz/deep/two.rs"] {
        for (root, contents) in [(left.path(), "old\n"), (right.path(), "new\n")] {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(path, contents)?;
        }
    }
    let mut app = AppBuilder::new().build()?;
    let browse = format!(":ft {}<ret>", right.path().display());
    let review = format!(
        ":dr {} {}<ret>",
        left.path().display(),
        right.path().display()
    );
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(&browse),
                Some(&|app| assert!(tree(app).expanded.is_empty())),
            ),
            (
                Some(&review),
                Some(&|app| {
                    let tree = tree(app);
                    for folder in ["aaa", "zzz", "zzz/deep"] {
                        assert!(tree.expanded.contains(Path::new(folder)), "{folder}");
                    }
                    assert!(tree
                        .visible()
                        .iter()
                        .any(|&i| tree.nodes[i].path == Path::new("zzz/deep/two.rs")));
                }),
            ),
            (Some(":diff-layout inline<ret>"), None),
            (Some(":ft<ret>"), Some(&|app| assert!(tree(app).focused))),
            (
                Some("Ghh<esc>"),
                Some(&|app| {
                    assert!(!tree(app).expanded.contains(Path::new("zzz/deep")));
                    assert!(tree(app).expanded.contains(Path::new("aaa")));
                    let path = right.path().join("new/deep/three.rs");
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(path, "new file\n").unwrap();
                }),
            ),
            (
                Some(":diff-review-refresh<ret>"),
                Some(&|app| {
                    let tree = tree(app);
                    assert!(!tree.expanded.contains(Path::new("zzz/deep")));
                    assert!(tree.expanded.contains(Path::new("new")));
                    assert!(tree.expanded.contains(Path::new("new/deep")));
                    assert!(tree
                        .visible()
                        .iter()
                        .any(|&i| tree.nodes[i].path == Path::new("new/deep/three.rs")));
                }),
            ),
            (
                Some(":diff-off<ret>"),
                Some(&|app| {
                    assert!(tree(app).review.is_none());
                    assert_eq!(tree(app).root, right.path());
                    assert!(tree(app).expanded.is_empty());
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await
}

#[test]
fn path_command_aliases_resolve_with_arguments() {
    use helix_term::commands::MappableCommand;
    for (alias, name) in [
        ("ds", "diff-split"),
        ("d3", "diff-three"),
        ("dr", "diff-review"),
        ("drp", "diff-review-patch"),
        ("ft", "file-tree"),
    ] {
        let command: MappableCommand = format!(":{alias} 'path with spaces'").parse().unwrap();
        let MappableCommand::Typable {
            name: resolved,
            args,
            ..
        } = command
        else {
            panic!("expected typable command")
        };
        assert_eq!(resolved, name);
        assert_eq!(args, "'path with spaces'");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sidebar_toggle_preserves_expansion_and_returns_editor_space() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join("src"))?;
    std::fs::write(root.path().join("src/main.rs"), "fn main() {}\n")?;
    let mut app = AppBuilder::new().build()?;
    let open = format!(":ft {}<ret>", root.path().display());
    test_key_sequences(
        &mut app,
        vec![
            (Some(&open), Some(&|app| assert!(tree(app).visible))),
            (
                Some("l<space>E"),
                Some(&|app| {
                    assert!(!tree(app).visible);
                    assert!(!tree(app).focused);
                    assert!(tree(app).expanded.contains(std::path::Path::new("src")));
                    assert_eq!(current_ref!(app.editor).0.area.x, 0);
                }),
            ),
            (
                Some("<space>E"),
                Some(&|app| {
                    assert!(tree(app).visible && tree(app).focused);
                    assert!(tree(app).expanded.contains(std::path::Path::new("src")));
                    assert!(current_ref!(app.editor).0.area.x > 0);
                }),
            ),
            (Some("<space>E"), Some(&|app| assert!(!tree(app).visible))),
            (Some(":ft<ret>"), Some(&|app| assert!(tree(app).visible))),
            (Some(":file-tree-close<ret>:qa!<ret>"), None),
        ],
        true,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hidden_review_refresh_keeps_comparison_and_sidebar_hidden() -> anyhow::Result<()> {
    let left = tempfile::tempdir()?;
    let right = tempfile::tempdir()?;
    std::fs::write(left.path().join("main.rs"), "fn before() {}\n")?;
    std::fs::write(right.path().join("main.rs"), "fn after() {}\n")?;
    let mut app = AppBuilder::new().build()?;
    let review = format!(
        ":dr {} {}<ret>",
        left.path().display(),
        right.path().display()
    );
    test_key_sequences(
        &mut app,
        vec![
            (Some(&review), Some(&|app| assert!(tree(app).visible))),
            (
                Some("<space>E:diff-review-refresh<ret>"),
                Some(&|app| {
                    assert!(!tree(app).visible);
                    assert!(tree(app).review.is_some());
                    assert!(app.editor.diff_session.is_some());
                    assert_eq!(app.editor.tree.views().count(), 2);
                }),
            ),
            (Some("<space>E"), Some(&|app| assert!(tree(app).visible))),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_off_restores_sidebar_and_keeps_dirty_working_file_on_left() -> anyhow::Result<()> {
    check_diff_off(false).await
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_off_from_inline_restores_sidebar_and_keeps_dirty_working_file() -> anyhow::Result<()>
{
    check_diff_off(true).await
}

async fn check_diff_off(inline: bool) -> anyhow::Result<()> {
    let left = tempfile::tempdir()?;
    let right = tempfile::tempdir()?;
    std::fs::create_dir(left.path().join("src"))?;
    std::fs::create_dir(right.path().join("src"))?;
    std::fs::write(left.path().join("src/main.rs"), "fn old() {}\n")?;
    std::fs::write(right.path().join("src/main.rs"), "fn new() {}\n")?;
    let mut app = AppBuilder::new().build()?;
    let browse = format!(":file-tree {}<ret>", left.path().display());
    let review = format!(
        ":diff-review {} {}<ret>",
        left.path().display(),
        right.path().display()
    );
    test_key_sequences(
        &mut app,
        vec![
            (Some(&browse), None),
            (
                Some("l<esc>"),
                Some(&|app| {
                    assert!(tree(app).expanded.contains(std::path::Path::new("src")));
                }),
            ),
            (
                Some(&review),
                Some(&|app| assert!(tree(app).review.is_some())),
            ),
            (Some("A // unsaved<esc>"), None),
            (
                Some(if inline {
                    ":diff-layout inline<ret>"
                } else {
                    "<esc>"
                }),
                None,
            ),
            (
                Some(":diff-off<ret>"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert_eq!(app.editor.tree.views().count(), 1);
                    assert!(app.editor.diff_session.is_none());
                    assert_eq!(doc.path().unwrap(), &right.path().join("src/main.rs"));
                    assert!(doc.is_modified());
                    assert!(doc.text().to_string().contains("// unsaved"));
                    assert!(view.diff.is_none());
                    assert_eq!(tree(app).root, left.path());
                    assert!(tree(app).review.is_none());
                    assert!(!tree(app).focused);
                    assert!(tree(app).expanded.contains(std::path::Path::new("src")));
                    assert!(app.editor.documents().any(|doc| doc
                        .name
                        .as_deref()
                        .is_some_and(|name| name.starts_with("Before:"))));
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await?;
    assert_eq!(
        std::fs::read_to_string(right.path().join("src/main.rs"))?,
        "fn new() {}\n"
    );
    Ok(())
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
                    assert!(!doc.is_read_only());
                    assert!(app.editor.inline_diff_session.is_some());
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

#[tokio::test(flavor = "multi_thread")]
async fn watcher_refreshes_catalog_and_inline_snapshot_without_a_command() -> anyhow::Result<()> {
    let left = tempfile::tempdir()?;
    let right = tempfile::tempdir()?;
    std::fs::write(left.path().join("a.txt"), "old\n")?;
    std::fs::write(right.path().join("a.txt"), "new\n")?;
    let mut config = helix_term::config::Config::default();
    config.editor = test_editor_config();
    // Inject real FileSystemDidChange events deterministically; the native
    // backend is verified separately in a PTY rather than racing initial crawl.
    let mut app = AppBuilder::new()
        .with_config(config)
        .with_file(left.path(), None)
        .with_file(right.path(), None)
        .with_review(Layout::Inline)
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                None,
                Some(&|app| {
                    assert!(current_ref!(app.editor)
                        .1
                        .text()
                        .to_string()
                        .contains("new"));
                    std::fs::write(right.path().join("a.txt"), "externally updated\n").unwrap();
                    std::fs::write(right.path().join("b.txt"), "added\n").unwrap();
                    helix_event::dispatch(helix_core::file_watcher::FileSystemDidChange {
                        fs_events: helix_core::file_watcher::events_from_paths([
                            right.path().join("a.txt"),
                            right.path().join("b.txt"),
                        ]),
                    });
                }),
            ),
            (
                Some("<esc>"),
                Some(&|app| {
                    assert_eq!(tree(app).review.as_ref().unwrap().entries.len(), 2);
                    assert!(current_ref!(app.editor)
                        .1
                        .text()
                        .to_string()
                        .contains("externally updated"));
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await?;
    Ok(())
}
