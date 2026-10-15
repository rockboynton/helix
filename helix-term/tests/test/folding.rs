use super::helpers::*;
use helix_view::current_ref;

fn contents() -> String {
    (0..100).map(|line| format!("line_{line}\n")).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_inline_folding_preserves_the_presentation_and_source_documents(
) -> anyhow::Result<()> {
    let before = contents();
    let after = before.replace("line_25\n", "changed_25\n");
    let left = temp_file_with_contents(&before)?;
    let right = temp_file_with_contents(&after)?;
    let mut app = AppBuilder::new()
        .with_file(left.path(), None)
        .with_file(right.path(), None)
        .with_inline_diff()
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("gg:diff-fold 2<ret>"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert!(doc.is_read_only());
                    assert!(doc.inline_diff.is_some());
                    assert_eq!(view.folds.ranges.len(), 2);
                    assert!(doc.text().to_string().contains("line_25\nchanged_25\n"));
                    assert_eq!(std::fs::read_to_string(left.path()).unwrap(), before);
                    assert_eq!(std::fs::read_to_string(right.path()).unwrap(), after);
                }),
            ),
            (
                Some(":fold-open-all<ret>"),
                Some(&|app| assert!(current_ref!(app.editor).0.folds.ranges.is_empty())),
            ),
            (Some("q"), None),
        ],
        true,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folded_syntax_highlights_visible_lines_beyond_the_original_source_window(
) -> anyhow::Result<()> {
    use std::io::Write;
    let before = (0..100)
        .map(|line| format!("fn line_{line}() {{}}\n"))
        .collect::<String>();
    let after = before
        .replace("line_25", "changed_25")
        .replace("line_70", "changed_70");
    let mut left = tempfile::Builder::new().suffix(".rs").tempfile()?;
    let mut right = tempfile::Builder::new().suffix(".rs").tempfile()?;
    left.write_all(before.as_bytes())?;
    right.write_all(after.as_bytes())?;
    let mut app = AppBuilder::new()
        .with_file(left.path(), None)
        .with_file(right.path(), None)
        .with_diff()
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("gg:diff-fold 2<ret>"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert!(doc.syntax().is_some());
                    let area = view.inner_area(doc);
                    let surface = app.test_surface();
                    assert_eq!(surface[(area.x, area.y + 11)].symbol.as_str(), "f");
                    assert_eq!(
                        surface[(area.x, area.y + 11)].fg,
                        surface[(area.x, area.y + 1)].fg
                    );
                    assert_ne!(
                        surface[(area.x, area.y + 11)].fg,
                        surface[(area.x + 3, area.y + 11)].fg
                    );
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_way_folding_preserves_asymmetric_changes_and_opens_together() -> anyhow::Result<()> {
    let text = contents();
    let left_text = text.replace("line_25\n", "left_25\nextra_left\n");
    let right_text = text.replace("line_70\n", "right_70\n");
    let left = temp_file_with_contents(&left_text)?;
    let base = temp_file_with_contents(&text)?;
    let right = temp_file_with_contents(&right_text)?;
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
                Some("gg:diff-fold 2<ret>"),
                Some(&|app| {
                    assert!(app
                        .editor
                        .tree
                        .views()
                        .all(|(view, _)| view.folds.ranges.len() == 3));
                    for (view, _) in app.editor.tree.views() {
                        let doc = app.editor.document(view.doc).unwrap();
                        let side = view.diff_side(doc).unwrap();
                        assert!(view.folds.ranges.iter().all(|range| {
                            (doc.text().char_to_line(range.start)
                                ..doc.text().char_to_line(range.end))
                                .all(|line| !side.is_changed(line))
                        }));
                    }
                }),
            ),
            (
                Some("2j:fold-open<ret>"),
                Some(&|app| {
                    assert!(app
                        .editor
                        .tree
                        .views()
                        .all(|(view, _)| view.folds.ranges.len() == 2));
                }),
            ),
            (
                Some(":diff-fold 2<ret>50gg"),
                Some(&|app| {
                    assert!(app
                        .editor
                        .tree
                        .views()
                        .all(|(view, _)| view.folds.ranges.len() == 2));
                }),
            ),
            (
                Some(":diff-off<ret>"),
                Some(&|app| {
                    assert_eq!(app.editor.tree.views().count(), 1);
                    assert!(current_ref!(app.editor).0.folds.ranges.is_empty());
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selection_folding_navigation_search_and_save_use_real_text() -> anyhow::Result<()> {
    let text = contents();
    let file = temp_file_with_contents(&text)?;
    let mut app = AppBuilder::new().with_file(file.path(), None).build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("gg2j5x:fold-selection<ret>"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert_eq!(
                        view.folds.ranges,
                        vec![doc.text().line_to_char(2)..doc.text().line_to_char(7)]
                    );
                    assert_eq!(doc.cursor_line(view.id), 2);
                    let area = view.inner_area(doc);
                    let screen = (area.x..area.right())
                        .map(|x| app.test_surface()[(x, area.y + 2)].symbol.as_str())
                        .collect::<String>();
                    assert!(screen.contains("5 lines folded"), "{screen:?}");
                }),
            ),
            (
                Some("j"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert_eq!(doc.cursor_line(view.id), 7);
                    assert_eq!(view.folds.ranges.len(), 1);
                }),
            ),
            (
                Some("k:fold-open<ret>"),
                Some(&|app| {
                    assert!(current_ref!(app.editor).0.folds.ranges.is_empty());
                }),
            ),
            (
                Some("gg2j5x:fold-selection<ret>/line_4<ret>"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert_eq!(doc.cursor_line(view.id), 4);
                    assert!(view.folds.ranges.is_empty());
                }),
            ),
            (
                Some(":w<ret>"),
                Some(&|_app| {
                    assert_eq!(std::fs::read_to_string(file.path()).unwrap(), text);
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_folding_keeps_both_panes_aligned_and_invalidates_after_edit() -> anyhow::Result<()> {
    let before = contents();
    let after = before
        .replace("line_25\n", "changed_25\n")
        .replace("line_70\n", "changed_70\n");
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
                Some("gg:diff-fold 2<ret>"),
                Some(&|app| {
                    let views = app
                        .editor
                        .tree
                        .views()
                        .map(|(view, _)| view)
                        .collect::<Vec<_>>();
                    assert_eq!(views[0].folds.ranges.len(), 3);
                    assert_eq!(views[1].folds.ranges.len(), 3);
                    for view in views {
                        let doc = app.editor.document(view.doc).unwrap();
                        let area = view.inner_area(doc);
                        let screen = (area.x..area.right())
                            .map(|x| app.test_surface()[(x, area.y + 2)].symbol.as_str())
                            .collect::<String>();
                        assert!(screen.contains("21 lines folded"), "{screen:?}");
                    }
                }),
            ),
            (
                Some("3j"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert_eq!(doc.cursor_line(view.id), 23);
                    assert_eq!(view.folds.ranges.len(), 3);
                }),
            ),
            (
                Some("k:fold-open<ret>"),
                Some(&|app| {
                    assert!(app
                        .editor
                        .tree
                        .views()
                        .all(|(view, _)| view.folds.ranges.len() == 2));
                }),
            ),
            (
                Some(":diff-next<ret>A edited<esc>"),
                Some(&|app| {
                    assert!(app
                        .editor
                        .tree
                        .views()
                        .all(|(view, _)| view.folds.ranges.is_empty()));
                    assert!(current_ref!(app.editor)
                        .1
                        .text()
                        .to_string()
                        .contains(" edited"));
                }),
            ),
            (
                Some("u:diff-fold<ret>:fold-open-all<ret>"),
                Some(&|app| {
                    assert!(app
                        .editor
                        .tree
                        .views()
                        .all(|(view, _)| view.folds.ranges.is_empty()));
                    assert_eq!(current_ref!(app.editor).1.text().to_string(), after);
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inline_folding_renders_removed_rows_at_the_compressed_position() -> anyhow::Result<()> {
    let before = contents();
    let after = before.replace("line_25\n", "changed_25\n");
    let left = temp_file_with_contents(&before)?;
    let right = temp_file_with_contents(&after)?;
    let mut app = AppBuilder::new()
        .with_file(left.path(), None)
        .with_file(right.path(), None)
        .with_editable_inline_diff()
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("gg:diff-fold 2<ret>"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    assert_eq!(view.folds.ranges.len(), 2);
                    let area = view.inner_area(doc);
                    let row = |y| {
                        (area.x..area.right())
                            .map(|x| app.test_surface()[(x, area.y + y)].symbol.as_str())
                            .collect::<String>()
                    };
                    assert!(row(5).starts_with("line_25"), "{:?}", row(5));
                    assert!(row(6).starts_with("changed_25"), "{:?}", row(6));
                    assert_eq!(doc.text().to_string(), after);
                }),
            ),
            (
                Some("gg:diff-fold 0<ret>"),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    let area = view.inner_area(doc);
                    let row = |y| {
                        (area.x..area.right())
                            .map(|x| app.test_surface()[(x, area.y + y)].symbol.as_str())
                            .collect::<String>()
                    };
                    assert!(row(1).starts_with("line_25"), "{:?}", row(1));
                    assert!(row(2).starts_with("changed_25"), "{:?}", row(2));
                }),
            ),
            (
                Some(":diff-next<ret>:diff-get<ret>"),
                Some(&|app| {
                    assert_eq!(current_ref!(app.editor).1.text().to_string(), before);
                    assert!(current_ref!(app.editor).0.folds.ranges.is_empty());
                }),
            ),
            (Some(":qa!<ret>"), None),
        ],
        true,
    )
    .await
}
