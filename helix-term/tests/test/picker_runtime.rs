use helix_term::ui::picker::{Column, Picker};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_message_wraps_inside_the_picker_without_being_covered_by_preview(
) -> anyhow::Result<()> {
    use super::helpers::*;
    use helix_term::{
        compositor::{Component, Context},
        job::Jobs,
    };
    use helix_view::graphics::Rect;
    let file = temp_file_with_contents("PREVIEW_MARKER\n")?;
    let message = format!("first line\n{}TAIL_MARKER", "first middle last ".repeat(12));
    let mut app = AppBuilder::new().build()?;
    let mut picker = Picker::new(
        [
            Column::new("severity", |_: &(String, std::path::PathBuf), _: &()| {
                "ERROR".into()
            }),
            Column::new("message", |item: &(String, std::path::PathBuf), _: &()| {
                item.0.as_str().into()
            }),
        ],
        1,
        [(message, file.path().to_path_buf())],
        (),
        |_, _, _| {},
    )
    .with_wrapped_selected_column(1)
    .with_preview(|_, item| Some((item.1.as_path().into(), None)));
    let mut surface = tui::buffer::Buffer::empty(Rect::new(0, 0, 100, 24));
    let mut jobs = Jobs::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        picker.render(
            surface.area,
            &mut surface,
            &mut Context {
                editor: &mut app.editor,
                jobs: &mut jobs,
                scroll: None,
            },
        );
        let left = (0..24)
            .map(|y| {
                (0..50)
                    .map(|x| surface[(x, y)].symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let right = (0..24)
            .map(|y| {
                (50..100)
                    .map(|x| surface[(x, y)].symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        if left.contains("TAIL_MARKER") {
            assert!(right.contains("PREVIEW_MARKER"));
            assert!(!right.contains("TAIL_MARKER"));
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "message tail was truncated: {left:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

/// Picker injectors and matcher completion notifications run on threads outside
/// Tokio (directory walkers and Rayon). They must notify their owning editor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_picker_notifications_use_the_owning_runtime() {
    let (mut matcher, injector) = Picker::<String, ()>::stream(
        [Column::new("name", |item: &String, _: &()| {
            item.as_str().into()
        })],
        (),
    );
    std::thread::spawn(move || {
        // Cloning and dropping the redraw-on-drop guard must also be safe off-runtime.
        let cloned = injector.clone();
        cloned.push("entry".into()).unwrap();
    })
    .join()
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        matcher.tick(10);
        if matcher.snapshot().matched_item_count() == 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "picker did not complete matching"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
