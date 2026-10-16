//! Bound the critical path of a large, previously unseen file pair. At most two
//! extra parser threads exist process-wide; file rendering stays bounded.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

struct Permit;
impl Drop for Permit {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::Release);
    }
}

impl Renderer {
    pub(super) fn prefetch_pair_syntax(
        &self,
        bytes: [&[u8]; 2],
        paths: [&Path; 2],
        sources: &[Rope; 2],
        coverage: &[Vec<Range<usize>>; 2],
        key: &Path,
    ) -> bool {
        if !bytes
            .iter()
            .all(|bytes| (128 * 1024..=512 * 1024).contains(&bytes.len()))
            || std::env::var_os("HELIX_DIFF_RENDER_THREADS").is_some_and(|value| value == "1")
            || [
                "HELIX_DIFF_CACHE",
                "HELIX_DIFF_SYNTAX_CACHE",
                "HELIX_DIFF_INCREMENTAL_PARSE",
            ]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| value == "0"))
        {
            return false;
        }
        let configs = [0, 1].map(|side| {
            // Use ordinary document detection, including configured globs and
            // modelines, rather than guessing languages from the logical key.
            let mut doc = Document::from(
                sources[side].clone(),
                None,
                self.config.clone(),
                self.loader.clone(),
            );
            doc.set_path(Some(paths[side]));
            doc.detect_language_config(&self.loader.load())
        });
        let [Some(left), Some(right)] = configs else {
            return false;
        };
        let configs = [left, right];
        #[cfg(unix)]
        if let Some(cache) = &self.highlights {
            for side in 0..2 {
                let language = format!("{}@{:?}", configs[side].language_id, coverage[side]);
                if cache
                    .load(&language, bytes[side], sources[side].len_chars())
                    .is_some()
                {
                    return false;
                }
            }
        }
        if ACTIVE
            .try_update(Ordering::Acquire, Ordering::Relaxed, |active| {
                (active < 2).then_some(active + 1)
            })
            .is_err()
        {
            return false;
        }
        let _permit = Permit;
        let cache = self.syntax_cache.clone();
        let loader = self.loader.clone();
        let ranges = [0, 1].map(|side| {
            (!std::env::var_os("HELIX_DIFF_SYNTAX_RANGES").is_some_and(|value| value == "0")).then(
                || {
                    coverage[side]
                        .iter()
                        .map(|range| range.start as u32..range.end as u32)
                        .collect::<Vec<_>>()
                },
            )
        });
        let parse = |side: usize| {
            let source = &sources[side];
            let config = &configs[side];
            let loader = loader.load();
            let cached = cache.lock().unwrap().get(key, &config.language_id, source);
            let syntax = cached
                .and_then(|mut cached| {
                    if cached.source == *source
                        && (cached.coverage.is_none() || cached.coverage == ranges[side])
                    {
                        return Some(cached.syntax);
                    }
                    let changes = helix_core::diff::compare_ropes(&cached.source, source);
                    let result = match &ranges[side] {
                        Some(ranges) => cached.syntax.update_for_ranges(
                            cached.source.slice(..),
                            source.slice(..),
                            changes.changes(),
                            &loader,
                            ranges,
                        ),
                        None => cached.syntax.update(
                            cached.source.slice(..),
                            source.slice(..),
                            changes.changes(),
                            &loader,
                        ),
                    };
                    result.ok().map(|()| cached.syntax)
                })
                .or_else(|| match &ranges[side] {
                    Some(ranges) => syntax::Syntax::new_for_ranges(
                        source.slice(..),
                        config.language(),
                        &loader,
                        ranges,
                    )
                    .ok(),
                    None => syntax::Syntax::new(source.slice(..), config.language(), &loader).ok(),
                });
            if let Some(syntax) = syntax {
                cache.lock().unwrap().insert(
                    key,
                    &config.language_id,
                    source,
                    &syntax,
                    ranges[side].as_deref(),
                );
            }
        };
        std::thread::scope(|scope| {
            let parse = &parse;
            #[cfg(unix)]
            let context = tree_house_bindings::query_cache::thread_context();
            let worker = std::thread::Builder::new()
                .name("diff-parse-pair".into())
                .spawn_scoped(scope, move || {
                    #[cfg(unix)]
                    context.install();
                    parse(0);
                    #[cfg(unix)]
                    return tree_house_bindings::query_cache::statistics();
                });
            let Ok(worker) = worker else {
                return false;
            };
            parse(1);
            let result = worker
                .join()
                .unwrap_or_else(|error| std::panic::resume_unwind(error));
            #[cfg(unix)]
            tree_house_bindings::query_cache::add_statistics(result);
            true
        })
    }
}
