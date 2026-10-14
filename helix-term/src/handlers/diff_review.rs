//! Coalesce filesystem notifications before refreshing a changeset catalog.
use helix_core::file_watcher::{EventType, FileSystemDidChange};
use helix_event::{register_hook, AsyncHook};
use std::{collections::BTreeSet, path::PathBuf, time::Duration};
use tokio::time::Instant;

#[derive(Default)]
struct Handler {
    paths: BTreeSet<PathBuf>,
}

impl AsyncHook for Handler {
    type Event = Vec<PathBuf>;
    fn handle_event(&mut self, paths: Self::Event, _deadline: Option<Instant>) -> Option<Instant> {
        self.paths.extend(paths);
        Some(Instant::now() + Duration::from_millis(250))
    }
    fn finish_debounce(&mut self) {
        let paths = std::mem::take(&mut self.paths);
        crate::job::dispatch_blocking_jobs(move |editor, compositor, jobs| {
            crate::diff_review::refresh_changed(editor, compositor, jobs, &paths);
        });
    }
}
pub(super) fn register_hooks() {
    let tx = Handler::default().spawn();
    register_hook!(move |event: &mut FileSystemDidChange| {
        let paths = event
            .fs_events
            .iter()
            .filter(|event| event.ty != EventType::Tempfile)
            .map(|event| event.path.as_std_path().to_path_buf())
            .collect::<Vec<_>>();
        if !paths.is_empty() {
            helix_event::send_blocking(&tx, paths);
        }
        Ok(())
    });
}
