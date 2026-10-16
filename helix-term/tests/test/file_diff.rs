use super::helpers::*;
use helix_view::current_ref;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_file_picker_uses_open_jj_file_when_cwd_is_outside_repository() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let init = std::process::Command::new("jj")
        .args(["git", "init", "--colocate"])
        .current_dir(&root)
        .output()?;
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let nested = root.join("nested");
    std::fs::create_dir(&nested)?;
    let initial = nested.join("initial.txt");
    let target = root.join("pick_me.txt");
    std::fs::write(&initial, "initial\n")?;
    std::fs::write(&target, "target contents\n")?;
    assert!(!helix_stdx::env::current_working_dir().starts_with(&root));
    let mut app = AppBuilder::new().with_file(&initial, None).build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("<space>g"),
                Some(&|app| {
                    let surface = app
                        .test_surface()
                        .content
                        .iter()
                        .map(|cell| cell.symbol.as_str())
                        .collect::<String>();
                    assert!(
                        surface.contains("pick_me.txt"),
                        "picker did not discover external JJ workspace: {surface}"
                    );
                }),
            ),
            (
                Some("pick_me"),
                Some(&|app| {
                    let surface = app
                        .test_surface()
                        .content
                        .iter()
                        .map(|cell| cell.symbol.as_str())
                        .collect::<String>();
                    assert!(
                        surface.contains("target contents"),
                        "preview must use the absolute path: {surface}"
                    );
                }),
            ),
            (
                Some("<ret>"),
                Some(&|app| {
                    let (_, doc) = current_ref!(app.editor);
                    assert_eq!(doc.path(), Some(target.as_path()));
                    assert_eq!(doc.text().to_string(), "target contents\n");
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_file_blame_toggle_is_view_local_and_scrolls_in_the_gutter() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let file = root.path().join("main.rs");
    std::fs::write(
        &file,
        (0..100)
            .map(|line| format!("fn line_{line}() {{}}\n"))
            .collect::<String>(),
    )?;
    let git = |args: &[&str]| {
        let result = std::process::Command::new("git")
            .args(args)
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=Blame Test",
        "-c",
        "user.email=blame@example.com",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-qm",
        "baseline",
    ]);
    let mut app = AppBuilder::new().with_file(&file, None).build()?;
    use helix_view::graphics::{Color, Style};
    for (scope, color) in [
        ("ui.virtual.blame.commit", Color::Rgb(200, 100, 100)),
        ("ui.virtual.blame.date", Color::Rgb(100, 200, 100)),
        ("ui.virtual.blame.author", Color::Rgb(100, 100, 200)),
    ] {
        app.editor
            .theme
            .set(scope.into(), Style::default().fg(color));
    }
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(":blame-toggle<ret>"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert!(view
                        .gutters
                        .layout
                        .contains(&helix_view::editor::GutterType::Blame));
                    assert!(doc.file_blame.as_ref().unwrap().is_ok());
                    let area = view.area;
                    let row = (area.x..view.inner_area(doc).x)
                        .map(|x| app.test_surface()[(x, area.y)].symbol.as_str())
                        .collect::<String>();
                    assert!(row.contains("Blame Test"), "{row:?}");
                    let surface = app.test_surface();
                    let author_column = row.find("Blame Test").unwrap();
                    let date_column = row.find(' ').unwrap() + 1;
                    assert_eq!(surface[(area.x, area.y)].fg, Color::Rgb(200, 100, 100));
                    assert_eq!(
                        surface[(area.x + date_column as u16, area.y)].fg,
                        Color::Rgb(100, 200, 100),
                    );
                    assert_eq!(
                        surface[(area.x + author_column as u16, area.y)].fg,
                        Color::Rgb(100, 100, 200),
                    );
                }),
            ),
            (
                Some("80j"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert!(doc.cursor_line(view.id) >= 80);
                    assert!(view.inner_area(doc).x > 30);
                }),
            ),
            (
                Some(":blame-toggle<ret>"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert!(!view
                        .gutters
                        .layout
                        .contains(&helix_view::editor::GutterType::Blame));
                    assert!(view.inner_area(doc).x < 10);
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editable_inline_uses_real_working_document_and_virtual_source_highlighting(
) -> anyhow::Result<()> {
    use std::io::Write;
    let mut left = tempfile::Builder::new().suffix(".rs").tempfile()?;
    let mut right = tempfile::Builder::new().suffix(".rs").tempfile()?;
    left.write_all(b"/*\nfn old() {}\n*/\n")?;
    right.write_all(b"fn new() {}\n")?;
    let mut app = AppBuilder::new()
        .with_file(left.path(), None)
        .with_file(right.path(), None)
        .with_editable_inline_diff()
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                None,
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert!(!doc.is_read_only());
                    assert_eq!(doc.path().unwrap(), right.path());
                    assert!(doc.syntax().is_some());
                    assert!(
                        doc.inline_diff.is_none(),
                        "not a synthetic editable patch buffer"
                    );
                    assert_eq!(doc.text().to_string(), "fn new() {}\n");
                    let area = view.inner_area(doc);
                    let surface = app.test_surface();
                    let old = &surface[(area.x, area.y + 1)];
                    let new = &surface[(area.x, area.y + 3)];
                    assert_eq!(old.symbol.as_str(), "f");
                    assert_eq!(new.symbol.as_str(), "f");
                    assert_ne!(old.fg, new.fg, "deleted comment uses baseline syntax");
                    assert!(view
                        .text_pos_at_screen_coords(
                            doc,
                            area.y + 1,
                            area.x,
                            view.text_format(doc, area.width, None),
                            &view.text_annotations(doc, None),
                            true
                        )
                        .is_none());
                }),
            ),
            (
                Some("A // edited<esc>:w<ret>"),
                Some(&|app| {
                    let (_, doc) = current_ref!(app.editor);
                    assert_eq!(doc.text().to_string(), "fn new() {} // edited\n");
                    assert!(!doc.is_modified());
                    assert_eq!(
                        std::fs::read_to_string(right.path()).unwrap(),
                        "fn new() {} // edited\n"
                    );
                }),
            ),
            (
                Some(":diff-get<ret>"),
                Some(&|app| {
                    assert_eq!(
                        current_ref!(app.editor).1.text().to_string(),
                        "/*\nfn old() {}\n*/\n"
                    );
                }),
            ),
            (
                Some("u"),
                Some(&|app| {
                    assert_eq!(
                        current_ref!(app.editor).1.text().to_string(),
                        "fn new() {} // edited\n"
                    );
                }),
            ),
            (
                Some(":diff-off<ret>"),
                Some(&|app| {
                    assert!(app.editor.inline_diff_session.is_none());
                    assert_eq!(app.editor.tree.views().count(), 1);
                    assert_eq!(current_ref!(app.editor).1.path().unwrap(), right.path());
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await?;
    assert_eq!(
        std::fs::read_to_string(left.path())?,
        "/*\nfn old() {}\n*/\n"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn three_way_scrolls_all_columns_and_preserves_alignment_on_focus() -> anyhow::Result<()> {
    let lines = (0..260)
        .map(|i| format!("line-{i:03}\n"))
        .collect::<Vec<_>>();
    let left = temp_file_with_contents(&format!(
        "left\nleft\n{}{}",
        lines[..30].concat(),
        lines[40..].concat()
    ))?;
    let base = temp_file_with_contents(&lines.concat())?;
    let right = temp_file_with_contents(&format!(
        "{}right\nright\nright\n{}",
        lines[..70].concat(),
        lines[70..].concat()
    ))?;
    let mut app = AppBuilder::new()
        .with_file(left.path(), None)
        .with_file(base.path(), None)
        .with_file(right.path(), None)
        .with_three_diff()
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("220j"),
                Some(&|app| {
                    let session = app.editor.three_diff_session.as_ref().unwrap();
                    let surface = app.test_surface();
                    let labels = session.views.map(|id| {
                        let view = app.editor.tree.get(id);
                        let doc = app.editor.document(view.doc).unwrap();
                        let inner = view.inner_area(doc);
                        (inner.x..inner.x + 8)
                            .map(|x| surface[(x, inner.y + 2)].symbol.as_str())
                            .collect::<String>()
                    });
                    assert_eq!(labels[0], labels[1]);
                    assert_eq!(labels[1], labels[2]);
                    assert!(labels[0].starts_with("line-"));
                }),
            ),
            (
                Some("<C-w>h<C-w>h"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert!(doc.cursor_line(view.id) > 150);
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
async fn three_way_diff_aligns_edits_transfers_undo_and_reuses_columns() -> anyhow::Result<()> {
    let left = temp_file_with_contents("head\nleft\nend\n")?;
    let base = temp_file_with_contents("head\nbase\nend\n")?;
    let right = temp_file_with_contents("head\nright\nend\n")?;
    let mut app = AppBuilder::new()
        .with_file(left.path(), None)
        .with_file(base.path(), None)
        .with_file(right.path(), None)
        .with_three_diff()
        .build()?;
    let repeat = format!(
        ":diff3 {} {} {}<ret>",
        left.path().display(),
        base.path().display(),
        right.path().display()
    );
    test_key_sequences(
        &mut app,
        vec![
            (
                None,
                Some(&|app| {
                    let session = app.editor.three_diff_session.as_ref().unwrap();
                    let layout = session.result.load_full().unwrap();
                    assert_eq!(app.editor.tree.views().count(), 3);
                    for side in 0..3 {
                        let view = app.editor.tree.get(session.views[side]);
                        assert_eq!(view.area.y, app.editor.tree.get(session.views[1]).area.y);
                        assert_eq!(layout.sides[side].rows[2], layout.sides[1].rows[2]);
                        let doc = app.editor.document(session.documents[side]).unwrap();
                        let inner = view.inner_area(doc);
                        let expected = if side == 1 {
                            helix_view::graphics::Color::Rgb(80, 24, 24)
                        } else {
                            helix_view::graphics::Color::Rgb(24, 72, 32)
                        };
                        assert_eq!(app.test_surface()[(inner.x, inner.y + 1)].bg, expected);
                    }
                }),
            ),
            (
                Some("j:diff-get<ret>"),
                Some(&|app| {
                    assert_eq!(
                        current_ref!(app.editor).1.text().to_string(),
                        "head\nbase\nend\n"
                    );
                }),
            ),
            (
                Some("u"),
                Some(&|app| {
                    assert_eq!(
                        current_ref!(app.editor).1.text().to_string(),
                        "head\nright\nend\n"
                    );
                }),
            ),
            (
                Some(":diff-put<ret>"),
                Some(&|app| {
                    assert_eq!(
                        app.editor
                            .document_by_path(base.path())
                            .unwrap()
                            .text()
                            .to_string(),
                        "head\nright\nend\n"
                    );
                }),
            ),
            (
                Some("<C-w>hu"),
                Some(&|app| {
                    assert_eq!(
                        current_ref!(app.editor).1.text().to_string(),
                        "head\nbase\nend\n"
                    );
                }),
            ),
            (
                Some(&repeat),
                Some(&|app| {
                    assert_eq!(app.editor.tree.views().count(), 3);
                    assert!(app.editor.three_diff_session.is_some());
                }),
            ),
            (
                Some(":diff-off<ret>"),
                Some(&|app| {
                    assert!(app.editor.three_diff_session.is_none());
                    assert!(app.editor.tree.views().all(|(view, _)| view.diff.is_none()));
                    assert_eq!(app.editor.tree.views().count(), 1);
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await?;
    assert_eq!(std::fs::read_to_string(base.path())?, "head\nbase\nend\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn three_way_diff_can_reuse_two_panes_and_pager_remains_read_only() -> anyhow::Result<()> {
    let left = temp_file_with_contents("l\n")?;
    let base = temp_file_with_contents("b\n")?;
    let right = temp_file_with_contents("r\n")?;
    let mut app = AppBuilder::new()
        .with_file(left.path(), None)
        .with_file(base.path(), None)
        .with_diff()
        .with_pager(None)
        .build()?;
    let command = format!(
        ":diff3 {} {} {}<ret>gg:diff-get<ret>",
        left.path().display(),
        base.path().display(),
        right.path().display()
    );
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(&command),
                Some(&|app| {
                    assert_eq!(app.editor.tree.views().count(), 3);
                    assert_eq!(current_ref!(app.editor).1.text().to_string(), "r\n");
                    let session = app.editor.three_diff_session.as_ref().unwrap();
                    assert!(session.documents.iter().all(|&id| app
                        .editor
                        .document(id)
                        .unwrap()
                        .is_read_only()));
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
async fn repeated_diff_reuses_side_by_side_panes_and_retains_displaced_buffers(
) -> anyhow::Result<()> {
    let a = temp_file_with_contents("a\n")?;
    let b = temp_file_with_contents("b\n")?;
    let c = temp_file_with_contents("c\n")?;
    let mut app = AppBuilder::new()
        .with_file(a.path(), None)
        .with_file(b.path(), None)
        .with_diff()
        .build()?;
    let command = format!(
        ":diff-split {} {}<ret>:diff-split {} {}<ret>",
        a.path().display(),
        c.path().display(),
        a.path().display(),
        b.path().display()
    );
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(&command),
                Some(&|app| {
                    assert_eq!(app.editor.tree.views().count(), 2);
                    assert!(app.editor.document_by_path(c.path()).is_some());
                    let views = app.editor.tree.views().map(|(v, _)| v).collect::<Vec<_>>();
                    assert_eq!(views[0].area.y, views[1].area.y);
                    assert_ne!(views[0].area.x, views[1].area.x);
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
async fn raw_patch_both_layouts_are_read_only_fragments_with_original_coordinates(
) -> anyhow::Result<()> {
    for layout in [
        helix_term::diff_render::Layout::Inline,
        helix_term::diff_render::Layout::SideBySide,
    ] {
        let input = "diff --git a/nonexistent.rs b/nonexistent.rs\n--- a/nonexistent.rs\n+++ b/nonexistent.rs\n@@ -100,2 +200,2 @@\n fn context() {}\n-fn old() {}\n+fn new() {}\n";
        let file = temp_file_with_contents(input)?;
        let mut app = AppBuilder::new()
            .with_file(file.path(), None)
            .with_patch(layout)
            .build()?;
        test_key_sequences(
            &mut app,
            vec![
                (
                    None,
                    Some(&|app| {
                        assert!(app.editor.pager);
                        for (view, _) in app.editor.tree.views() {
                            let doc = app.editor.document(view.doc).unwrap();
                            assert!(doc.is_read_only());
                            assert!(doc.path().is_none());
                            let model = doc.inline_diff.as_ref().unwrap();
                            assert!(model
                                .lines
                                .iter()
                                .any(|line| line.numbers.contains(&Some(100))
                                    || line.numbers.contains(&Some(200))));
                            assert!(model
                                .sources
                                .iter()
                                .any(|source| source.language_name() == Some("rust")));
                            assert!(model.sources.iter().all(|source| source.is_read_only()));
                            assert!(doc.language_servers().next().is_none());
                        }
                    }),
                ),
                (
                    Some("/new<ret>i<esc>:diff-put<ret>:w!<ret>"),
                    Some(&|app| {
                        let (_, doc) = current_ref!(app.editor);
                        assert!(!doc.is_modified());
                        assert!(doc.text().to_string().contains("fn new()"));
                    }),
                ),
                (Some("q"), None),
            ],
            true,
        )
        .await?;
        assert_eq!(std::fs::read_to_string(file.path())?, input);
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn inline_diff_renders_source_syntax_backgrounds_and_gutter() -> anyhow::Result<()> {
    use std::io::Write;
    let mut left = tempfile::Builder::new().suffix(".rs").tempfile()?;
    let mut right = tempfile::Builder::new().suffix(".rs").tempfile()?;
    left.write_all(b"/*\nfn old() {}\n*/\n")?;
    right.write_all(b"fn new() {}\n")?;
    let mut app = AppBuilder::new()
        .with_file(left.path(), None)
        .with_file(right.path(), None)
        .with_inline_diff()
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                None,
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    let model = doc.inline_diff.as_ref().unwrap();
                    assert!(doc.syntax().is_none());
                    assert!(model.sources.iter().all(|source| source.syntax().is_some()));
                    let surface = app.test_surface();
                    let area = view.inner_area(doc);
                    let old = &surface[(area.x, area.y + 1)];
                    let new = &surface[(area.x, area.y + 3)];
                    assert_eq!(old.symbol.as_str(), "f");
                    assert_eq!(new.symbol.as_str(), "f");
                    assert_eq!(old.bg, helix_view::graphics::Color::Rgb(48, 0, 0));
                    assert_eq!(new.bg, helix_view::graphics::Color::Rgb(0, 40, 0));
                    assert_ne!(
                        old.fg, new.fg,
                        "original comment and keyword scopes must remain distinct"
                    );
                    let mut gutter =
                        helix_view::gutter::diff(&app.editor, doc, view, &app.editor.theme, true);
                    let mut text = helix_view::gutter::GutterText::default();
                    assert!(gutter(1, false, true, &mut text).is_some());
                    assert_eq!(
                        text.text,
                        app.editor.icons.diff.deleted.icon_char.to_string()
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
async fn inline_diff_is_searchable_read_only_and_navigates_hunks() -> anyhow::Result<()> {
    let left = temp_file_with_contents("same\nold\nstable\nend\n")?;
    let right = temp_file_with_contents("same\nnew\nstable\nextra\nend\n")?;
    let mut app = AppBuilder::new()
        .with_file(left.path(), None)
        .with_file(right.path(), None)
        .with_inline_diff()
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                None,
                Some(&|app| {
                    let (_, doc) = current_ref!(app.editor);
                    assert_eq!(
                        doc.text().to_string(),
                        "same\nold\nnew\nstable\nextra\nend\n"
                    );
                    assert!(doc.is_read_only());
                    assert!(doc.path().is_none());
                    assert_eq!(doc.inline_diff.as_ref().unwrap().hunks, [1, 4]);
                    assert!(doc.language_servers().next().is_none());
                }),
            ),
            (
                Some(":diff-next<ret>"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert_eq!(doc.cursor_line(view.id), 1);
                }),
            ),
            (
                Some(":diff-next<ret>:diff-prev<ret>/extra<ret>"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert_eq!(doc.cursor_line(view.id), 4);
                }),
            ),
            (
                Some("i<esc>:diff-get<ret>:w!<ret>"),
                Some(&|app| {
                    let (_, doc) = current_ref!(app.editor);
                    assert_eq!(
                        doc.text().to_string(),
                        "same\nold\nnew\nstable\nextra\nend\n"
                    );
                    assert!(!doc.is_modified());
                }),
            ),
            (Some("q"), None),
        ],
        true,
    )
    .await?;
    assert_eq!(
        std::fs::read_to_string(left.path())?,
        "same\nold\nstable\nend\n"
    );
    assert_eq!(
        std::fs::read_to_string(right.path())?,
        "same\nnew\nstable\nextra\nend\n"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn standalone_read_only_diff_blocks_transfers_and_q_exits() -> anyhow::Result<()> {
    let left = temp_file_with_contents("old\n")?;
    let right = temp_file_with_contents("new\n")?;
    let mut app = AppBuilder::new()
        .with_file(left.path(), None)
        .with_file(right.path(), None)
        .with_diff()
        .with_pager(None)
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(":diff-get<ret>:diff-put<ret>i<esc>"),
                Some(&|app| {
                    let session = app.editor.diff_session.as_ref().unwrap();
                    for id in session.documents {
                        let doc = app.editor.document(id).unwrap();
                        assert!(doc.is_read_only());
                        assert!(!doc.is_modified());
                        assert!(doc.language_servers().next().is_none());
                        assert!(doc.diff_handle.is_none());
                    }
                }),
            ),
            (Some("q"), None),
        ],
        true,
    )
    .await?;
    assert_eq!(std::fs::read_to_string(left.path())?, "old\n");
    assert_eq!(std::fs::read_to_string(right.path())?, "new\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_gutter_uses_configured_helix_icons_for_each_hunk_kind() -> anyhow::Result<()> {
    for (before, after, expected) in [
        ("a\nb\n", "a\nextra\nb\n", 'A'),
        ("a\nextra\nb\n", "a\nb\n", 'D'),
        ("a\nb\n", "a\nx\n", 'M'),
    ] {
        let left = temp_file_with_contents(before)?;
        let right = temp_file_with_contents(after)?;
        let mut app = AppBuilder::new()
            .with_file(left.path(), None)
            .with_file(right.path(), None)
            .with_diff()
            .build()?;
        app.editor.icons.diff.added.icon_char = 'A';
        app.editor.icons.diff.deleted.icon_char = 'D';
        app.editor.icons.diff.modified.icon_char = 'M';
        test_key_sequences(
            &mut app,
            vec![
                (
                    None,
                    Some(&|app| {
                        let session = app.editor.diff_session.as_ref().unwrap();
                        for id in session.views {
                            let view = app.editor.tree.get(id);
                            let doc = app.editor.document(view.doc).unwrap();
                            let mut gutter = helix_view::gutter::diff(
                                &app.editor,
                                doc,
                                view,
                                &app.editor.theme,
                                true,
                            );
                            let mut text = helix_view::gutter::GutterText::default();
                            assert!(gutter(0, false, true, &mut text).is_none());
                            assert!(text.text.is_empty());
                            assert!(gutter(1, false, true, &mut text).is_some());
                            assert_eq!(text.text, expected.to_string());
                        }
                    }),
                ),
                (Some(":qa!<ret>"), None),
            ],
            true,
        )
        .await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_scrolls_both_panes_in_aligned_rows() -> anyhow::Result<()> {
    let lines = (0..240)
        .map(|line| format!("line-{line:03}\n"))
        .collect::<Vec<_>>();
    let before = lines.concat();
    let after = format!(
        "{}extra\nextra\nextra\n{}",
        lines[..30].concat(),
        lines[30..].concat()
    );
    let left = temp_file_with_contents(&before)?;
    let right = temp_file_with_contents(&after)?;
    let mut app = AppBuilder::new()
        .with_file(left.path(), None)
        .with_file(right.path(), None)
        .with_diff()
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("200j"),
                Some(&|app| {
                    let session = app.editor.diff_session.as_ref().unwrap();
                    let surface = app.test_surface();
                    let panes = session.views.map(|id| {
                        let view = app.editor.tree.get(id);
                        view.inner_area(app.editor.document(view.doc).unwrap())
                    });
                    for row in 0..20 {
                        let text = panes.map(|pane| {
                            (pane.x..pane.x + 8)
                                .map(|x| surface[(x, pane.y + row)].symbol.as_str())
                                .collect::<String>()
                        });
                        assert_eq!(text[0], text[1]);
                        assert!(text[0].starts_with("line-"));
                    }
                }),
            ),
            (
                Some("<C-w>h"),
                Some(&|app| {
                    assert!(
                        current_ref!(app.editor)
                            .1
                            .cursor_line(app.editor.tree.focus)
                            > 150
                    )
                }),
            ),
            (
                Some("100k"),
                Some(&|app| {
                    assert!(
                        current_ref!(app.editor)
                            .1
                            .cursor_line(app.editor.tree.focus)
                            < 100
                    )
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
async fn diff_renders_aligned_syntax_and_background_and_skips_gap_clicks() -> anyhow::Result<()> {
    use std::io::Write;
    let mut left = tempfile::Builder::new().suffix(".rs").tempfile()?;
    let mut right = tempfile::Builder::new().suffix(".rs").tempfile()?;
    left.write_all(b"fn removed() {}\nfn common() {}\n")?;
    right.write_all(b"fn common() {}\n")?;
    let mut app = AppBuilder::new()
        .with_file(left.path(), None)
        .with_file(right.path(), None)
        .with_diff()
        .build()?;
    // Match the user's transparent-theme regression: vivid gutter colors must
    // not become the text's background when ui.background has no RGB value.
    app.editor.theme.set(
        "ui.background".into(),
        helix_view::graphics::Style::default(),
    );
    app.editor.theme.set(
        "diff.minus".into(),
        helix_view::graphics::Style::default().fg(helix_view::graphics::Color::Rgb(255, 0, 0)),
    );
    test_key_sequences(
        &mut app,
        vec![
            (
                None,
                Some(&|app| {
                    let session = app.editor.diff_session.as_ref().unwrap();
                    let layout = session.result.load_full().unwrap();
                    assert_eq!(layout.sides[0].rows[1], layout.sides[1].rows[0]);
                    let surface = app.test_surface();
                    for side in 0..2 {
                        let view = app.editor.tree.get(session.views[side]);
                        let doc = app.editor.document(session.documents[side]).unwrap();
                        assert_eq!(doc.language_name(), Some("rust"));
                        assert!(doc.syntax().is_some());
                        let inner = view.inner_area(doc);
                        let y = inner.y + 1;
                        let prefix = (inner.x..inner.x + 9)
                            .map(|x| surface[(x, y)].symbol.as_str())
                            .collect::<String>();
                        assert_eq!(prefix, "fn common");
                        if side == 1 {
                            assert!(view
                                .text_pos_at_screen_coords(
                                    doc,
                                    inner.y,
                                    inner.x,
                                    view.text_format(doc, inner.width, None),
                                    &view.text_annotations(doc, None),
                                    true
                                )
                                .is_none());
                            assert_eq!(
                                view.text_pos_at_screen_coords(
                                    doc,
                                    y,
                                    inner.x,
                                    view.text_format(doc, inner.width, None),
                                    &view.text_annotations(doc, None),
                                    true
                                ),
                                Some(0)
                            );
                        } else {
                            assert_eq!(
                                surface[(inner.x + 5, inner.y)].bg,
                                helix_view::graphics::Color::Rgb(48, 0, 0)
                            );
                            assert_ne!(
                                surface[(inner.x + 5, inner.y)].bg,
                                surface[(inner.x + 5, y)].bg
                            );
                            assert_eq!(surface[(inner.x, inner.y)].fg, surface[(inner.x, y)].fg);
                        }
                    }
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
async fn diff_get_handles_empty_insert_delete_and_missing_final_newline() -> anyhow::Result<()> {
    for (before, after) in [
        ("", "extra\n"),
        ("extra\n", ""),
        ("a\n", "extra\na\n"),
        ("a", "a\n"),
        ("a\n", "a\nextra\n"),
    ] {
        let left = temp_file_with_contents(before)?;
        let right = temp_file_with_contents(after)?;
        let mut app = AppBuilder::new()
            .with_file(left.path(), None)
            .with_file(right.path(), None)
            .with_diff()
            .build()?;
        let keys = if after.starts_with("a\nextra") {
            "j:diff-get<ret>"
        } else {
            ":diff-get<ret>"
        };
        test_key_sequences(
            &mut app,
            vec![
                (None, None),
                (
                    Some(keys),
                    Some(&|app| assert_eq!(current_ref!(app.editor).1.text().to_string(), before)),
                ),
                (
                    Some("u"),
                    Some(&|app| assert_eq!(current_ref!(app.editor).1.text().to_string(), after)),
                ),
                (Some(":qa!<ret>"), None),
            ],
            true,
        )
        .await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn editable_diff_get_put_undo_and_stop() -> anyhow::Result<()> {
    let left = temp_file_with_contents("first\nleft\nlast\n")?;
    let right = temp_file_with_contents("first\nright\nlast\n")?;
    let mut app = AppBuilder::new()
        .with_file(left.path(), None)
        .with_file(right.path(), None)
        .with_diff()
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(":diff-next<ret>"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert_eq!(doc.cursor_line(view.id), 1);
                    assert!(!doc.is_read_only());
                    assert!(app.editor.diff_session.is_some());
                }),
            ),
            (
                Some(":diff-get<ret>"),
                Some(&|app| {
                    assert_eq!(
                        current_ref!(app.editor).1.text().to_string(),
                        "first\nleft\nlast\n"
                    );
                    assert!(current_ref!(app.editor).1.is_modified());
                }),
            ),
            (
                Some("u"),
                Some(&|app| {
                    assert_eq!(
                        current_ref!(app.editor).1.text().to_string(),
                        "first\nright\nlast\n"
                    )
                }),
            ),
            (
                Some(":diff-put<ret>"),
                Some(&|app| {
                    let session = app.editor.diff_session.as_ref().unwrap();
                    assert_eq!(
                        app.editor
                            .document(session.documents[0])
                            .unwrap()
                            .text()
                            .to_string(),
                        "first\nright\nlast\n"
                    );
                }),
            ),
            (
                Some(":diff-off<ret>"),
                Some(&|app| {
                    assert!(app.editor.diff_session.is_none());
                    assert!(app.editor.tree.views().all(|(view, _)| view.diff.is_none()));
                    assert_eq!(app.editor.tree.views().count(), 1);
                    assert_eq!(current_ref!(app.editor).1.path().unwrap(), right.path());
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await?;
    assert_eq!(std::fs::read_to_string(left.path())?, "first\nleft\nlast\n");
    assert_eq!(
        std::fs::read_to_string(right.path())?,
        "first\nright\nlast\n"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_split_from_current_file_and_edit_both_sides() -> anyhow::Result<()> {
    let left = temp_file_with_contents("a\nb\n")?;
    let right = temp_file_with_contents("a\nextra\nb\n")?;
    let mut app = AppBuilder::new().with_file(left.path(), None).build()?;
    let command = format!(":diff-split {}<ret>", right.path().display());
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(&command),
                Some(&|app| assert!(app.editor.diff_session.is_some())),
            ),
            (Some("iX<esc><C-w>h"), None),
            (
                Some("iY<esc>"),
                Some(&|app| {
                    let session = app.editor.diff_session.as_ref().unwrap();
                    assert!(app
                        .editor
                        .document(session.documents[0])
                        .unwrap()
                        .text()
                        .to_string()
                        .starts_with('Y'));
                    assert!(app
                        .editor
                        .document(session.documents[1])
                        .unwrap()
                        .text()
                        .to_string()
                        .starts_with('X'));
                }),
            ),
            (
                Some(":buffer-next<ret>"),
                Some(&|app| assert!(app.editor.diff_session.is_none())),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await?;
    Ok(())
}
