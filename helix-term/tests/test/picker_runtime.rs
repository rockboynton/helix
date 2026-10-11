use helix_term::ui::picker::{Column, Picker};
use std::time::Duration;

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
