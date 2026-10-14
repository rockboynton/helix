use super::helpers::*;
use helix_view::current_ref;

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
                    let mut text = String::new();
                    assert!(gutter(1, false, true, &mut text).is_some());
                    assert_eq!(text, app.editor.icons.diff.deleted.icon_char.to_string());
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
                            let mut text = String::new();
                            assert!(gutter(0, false, true, &mut text).is_none());
                            assert!(text.is_empty());
                            assert!(gutter(1, false, true, &mut text).is_some());
                            assert_eq!(text, expected.to_string());
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
                    assert_eq!(app.editor.tree.views().count(), 2);
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
