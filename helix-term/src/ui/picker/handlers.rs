use std::{
    path::Path,
    sync::{atomic, Arc},
    time::Duration,
};

use helix_event::AsyncHook;
use tokio::time::Instant;

// Shared across picker instances, including parsers finishing after a picker
// closes. Never queue speculative parses behind these workers: a pending
// selection is retried on the redraw triggered by a completed parse.
static PREVIEW_PARSE_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

pub(super) fn preview_parse_slot_available() -> bool {
    PREVIEW_PARSE_SLOTS.available_permits() != 0
}

use crate::{job, ui::overlay::Overlay};

use super::{CachedPreview, DynQueryCallback, Picker};

struct PreviewParseGuard(Arc<atomic::AtomicBool>);
impl Drop for PreviewParseGuard {
    fn drop(&mut self) {
        self.0.store(false, atomic::Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abandoned_preview_callback_allows_retry() {
        let running = Arc::new(atomic::AtomicBool::new(true));
        let guard = PreviewParseGuard(running.clone());
        let callback = move || drop(guard);
        // Closing the picker can discard the completion callback entirely.
        drop(callback);
        assert!(!running.load(atomic::Ordering::Acquire));
    }

    #[test]
    fn saturated_parsers_do_not_trigger_a_redraw_retry_loop() {
        let first = PREVIEW_PARSE_SLOTS.try_acquire().unwrap();
        let second = PREVIEW_PARSE_SLOTS.try_acquire().unwrap();
        assert!(!preview_parse_slot_available());
        drop(first);
        assert!(preview_parse_slot_available());
        drop(second);
    }

    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn preview_highlighting_dispatches_immediately_without_a_timer() {
        let mut jobs = job::Jobs::new();
        let mut handler = PreviewHighlightHandler::<(), ()>::default();
        let path: Arc<Path> = Path::new("preview.rs").into();
        // Even an existing distant timer must not defer preview work.
        let deadline = Instant::now() + Duration::from_secs(60);
        assert!(handler.handle_event(path.clone(), Some(deadline)).is_none());
        assert!(jobs.callbacks.try_recv().is_ok());
        // Repeated selection/redraw events are immediate too. The callback's
        // running flag prevents duplicate parses, not a debounce timer.
        assert!(handler.handle_event(path, None).is_none());
        assert!(jobs.callbacks.try_recv().is_ok());
        assert!(jobs.callbacks.try_recv().is_err());
    }
}

pub(super) struct PreviewHighlightHandler<T: 'static + Send + Sync, D: 'static + Send + Sync> {
    phantom_data: std::marker::PhantomData<(T, D)>,
}

impl<T: 'static + Send + Sync, D: 'static + Send + Sync> Default for PreviewHighlightHandler<T, D> {
    fn default() -> Self {
        Self {
            phantom_data: Default::default(),
        }
    }
}

impl<T: 'static + Send + Sync, D: 'static + Send + Sync> AsyncHook
    for PreviewHighlightHandler<T, D>
{
    type Event = Arc<Path>;

    fn handle_event(
        &mut self,
        path: Self::Event,
        _timeout: Option<tokio::time::Instant>,
    ) -> Option<tokio::time::Instant> {
        job::dispatch_blocking(move |editor, compositor| {
            let Some(Overlay {
                content: picker, ..
            }) = compositor.find::<Overlay<Picker<T, D>>>()
            else {
                return;
            };

            let Some(CachedPreview::PendingSyntax(doc, running)) =
                picker.preview_cache.get_mut(&path)
            else {
                return;
            };

            if doc.syntax().is_some() {
                return;
            }

            let Some(language) = doc.language_config().map(|config| config.language()) else {
                return;
            };
            let Ok(permit) = PREVIEW_PARSE_SLOTS.try_acquire() else {
                return;
            };
            if running.swap(true, atomic::Ordering::AcqRel) {
                return;
            }
            let guard = PreviewParseGuard(running.clone());

            let loader = editor.syn_loader.load();
            let text = doc.text().clone();
            let generation = picker.preview_generation.clone();

            tokio::task::spawn_blocking(move || {
                let syntax = match helix_core::Syntax::new(text.slice(..), language, &loader) {
                    Ok(syntax) => Some(syntax),
                    Err(err) => {
                        log::info!("highlighting picker preview failed: {err}");
                        None
                    }
                };
                // Release before publishing: that callback redraws the picker
                // and may immediately retry a selection waiting for a slot.
                drop(permit);

                job::dispatch_blocking(move |editor, compositor| {
                    let _guard = guard;
                    let Some(Overlay {
                        content: picker, ..
                    }) = compositor.find::<Overlay<Picker<T, D>>>()
                    else {
                        log::info!("picker closed before syntax highlighting finished");
                        return;
                    };
                    if !Arc::ptr_eq(&generation, &picker.preview_generation) {
                        return;
                    }
                    let Some(CachedPreview::PendingSyntax(mut doc, _)) =
                        picker.preview_cache.remove(&path)
                    else {
                        return;
                    };
                    let diagnostics = helix_view::Editor::doc_diagnostics(
                        &editor.language_servers,
                        &editor.diagnostics,
                        &doc,
                    );
                    doc.replace_diagnostics(diagnostics, &[], None);
                    doc.syntax = syntax;
                    picker
                        .preview_cache
                        .insert(path, CachedPreview::Document(doc));
                });
            });
        });
        None
    }

    fn finish_debounce(&mut self) {}
}

pub(super) struct DynamicQueryChange {
    pub query: Arc<str>,
    pub is_paste: bool,
}

pub(super) struct DynamicQueryHandler<T: 'static + Send + Sync, D: 'static + Send + Sync> {
    callback: Arc<DynQueryCallback<T, D>>,
    // Duration used as a debounce.
    // Defaults to 100ms if not provided via `Picker::with_dynamic_query`. Callers may want to set
    // this higher if the dynamic query is expensive - for example global search.
    debounce: Duration,
    last_query: Arc<str>,
    query: Option<Arc<str>>,
}

impl<T: 'static + Send + Sync, D: 'static + Send + Sync> DynamicQueryHandler<T, D> {
    pub(super) fn new(callback: DynQueryCallback<T, D>, duration_ms: Option<u64>) -> Self {
        Self {
            callback: Arc::new(callback),
            debounce: Duration::from_millis(duration_ms.unwrap_or(100)),
            last_query: "".into(),
            query: None,
        }
    }
}

impl<T: 'static + Send + Sync, D: 'static + Send + Sync> AsyncHook for DynamicQueryHandler<T, D> {
    type Event = DynamicQueryChange;

    fn handle_event(&mut self, change: Self::Event, _timeout: Option<Instant>) -> Option<Instant> {
        let DynamicQueryChange { query, is_paste } = change;
        if query == self.last_query {
            // If the search query reverts to the last one we requested, no need to
            // make a new request.
            self.query = None;
            None
        } else {
            self.query = Some(query);
            if is_paste {
                self.finish_debounce();
                None
            } else {
                Some(Instant::now() + self.debounce)
            }
        }
    }

    fn finish_debounce(&mut self) {
        let Some(query) = self.query.take() else {
            return;
        };
        self.last_query = query.clone();
        let callback = self.callback.clone();

        job::dispatch_blocking(move |editor, compositor| {
            let Some(Overlay {
                content: picker, ..
            }) = compositor.find::<Overlay<Picker<T, D>>>()
            else {
                return;
            };
            // Increment the version number to cancel any ongoing requests.
            picker.version.fetch_add(1, atomic::Ordering::Relaxed);
            picker.matcher.restart(false);
            let injector = picker.injector();
            let get_options = (callback)(&query, editor, picker.editor_data.clone(), &injector);
            tokio::spawn(async move {
                if let Err(err) = get_options.await {
                    log::info!("Dynamic request failed: {err}");
                }
                // NOTE: the Drop implementation of Injector will request a redraw when the
                // injector falls out of scope here, clearing the "running" indicator.
            });
        })
    }
}
