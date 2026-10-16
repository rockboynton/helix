//! Bounded, invocation-local parallelism, not a persistent renderer service.
use super::*;
use std::{
    io::Seek,
    sync::{mpsc, Mutex},
};

const MAX_THREADS: usize = 4;
const SOURCE_LIMIT: u64 = 512 * 1024;
const SPOOL_LIMIT: usize = 1024 * 1024;

struct Job {
    left: PathBuf,
    right: PathBuf,
    name: PathBuf,
    reply: mpsc::SyncSender<Result<Rendered>>,
}

struct Rendered {
    output: tempfile::SpooledTempFile,
    profiles: Vec<FileRenderProfile>,
    #[cfg(unix)]
    query_statistics: (usize, usize),
}

impl Renderer {
    /// Capture only immutable, thread-safe resources. Documents and their Rc
    /// ownership stay on the thread that creates them.
    fn thread_renderer(&self) -> impl FnOnce() -> Self + Send + 'static {
        let theme = self.theme.clone();
        let loader = self.loader.clone();
        let config = self.config.clone();
        let icons = self.icons.clone();
        let (width, context, layout, color) = (self.width, self.context, self.layout, self.color);
        let profile = self.profile.is_some();
        let syntax_cache = self.syntax_cache.clone();
        #[cfg(unix)]
        let highlights = self.highlights.clone();
        move || Self {
            theme,
            loader,
            config,
            icons,
            width,
            context,
            layout,
            color,
            profile: profile.then(RefCell::default),
            syntax_cache,
            documents: Rc::new(RefCell::new(DocumentCache {
                max_entries: 4,
                max_bytes: 2 * SOURCE_LIMIT as usize,
                ..DocumentCache::default()
            })),
            #[cfg(unix)]
            highlights,
        }
    }

    pub(super) fn pairs(
        &self,
        left: &Path,
        right: &Path,
        files: Vec<PathBuf>,
        output: &mut impl Write,
    ) -> Result<()> {
        let threads = threads().min(files.len());
        if threads < 2 {
            return self.serial_pairs(left, right, &files, output);
        }
        std::thread::scope(|scope| {
            let (sender, receiver) = mpsc::sync_channel::<Job>(threads - 1);
            let receiver = Arc::new(Mutex::new(receiver));
            let mut spawned = 0;
            for index in 1..threads {
                let receiver = receiver.clone();
                let create = self.thread_renderer();
                #[cfg(unix)]
                let query_context = tree_house_bindings::query_cache::thread_context();
                let result = std::thread::Builder::new()
                    .name(format!("diff-render-{index}"))
                    .spawn_scoped(scope, move || {
                        #[cfg(unix)]
                        query_context.install();
                        let renderer = create();
                        loop {
                            let job = match receiver.lock().unwrap().recv() {
                                Ok(job) => job,
                                Err(_) => break,
                            };
                            let result = renderer.spooled(&job, true);
                            // A cancelled invocation need not retain completed output.
                            if job.reply.send(result).is_err() {
                                continue;
                            }
                        }
                    });
                if result.is_ok() {
                    spawned += 1;
                }
            }
            drop(receiver);
            // Thread limits must not make an otherwise valid diff fail.
            let result = if spawned == 0 {
                self.serial_pairs(left, right, &files, output)
            } else {
                #[cfg(unix)]
                cache::trace(format!("render threads={}", spawned + 1));
                self.scheduled(left, right, &files, spawned + 1, &sender, output)
            };
            // Release sleeping receivers on success, cancellation and errors.
            drop(sender);
            result
        })
    }

    fn spooled(&self, job: &Job, detach: bool) -> Result<Rendered> {
        #[cfg(unix)]
        let before = tree_house_bindings::query_cache::statistics();
        let mut output = tempfile::spooled_tempfile(SPOOL_LIMIT);
        self.pair(&job.left, &job.right, &job.name, &mut output)?;
        output.rewind()?;
        #[cfg(unix)]
        let after = tree_house_bindings::query_cache::statistics();
        Ok(Rendered {
            output,
            profiles: if detach {
                self.profile
                    .as_ref()
                    .map(|profiles| std::mem::take(&mut *profiles.borrow_mut()))
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            #[cfg(unix)]
            query_statistics: if detach {
                (after.0 - before.0, after.1 - before.1)
            } else {
                (0, 0)
            },
        })
    }

    fn scheduled(
        &self,
        left: &Path,
        right: &Path,
        files: &[PathBuf],
        threads: usize,
        sender: &mpsc::SyncSender<Job>,
        output: &mut impl Write,
    ) -> Result<()> {
        let mut start = 0;
        while start < files.len() {
            let mut end = start;
            while end < files.len()
                && small(&left.join(&files[end]))
                && small(&right.join(&files[end]))
            {
                end += 1;
            }
            if end > start {
                self.pipeline(left, right, &files[start..end], threads, sender, output)?;
            }
            if end < files.len() {
                // Large forests remain serial, with no queued background work.
                self.serial_pairs(left, right, &files[end..end + 1], output)?;
            }
            start = end + 1;
        }
        Ok(())
    }

    fn pipeline(
        &self,
        left: &Path,
        right: &Path,
        files: &[PathBuf],
        threads: usize,
        sender: &mpsc::SyncSender<Job>,
        output: &mut impl Write,
    ) -> Result<()> {
        let mut first = true;
        // Start the largest inputs early rather than leave a long parse until
        // the end of a batch. Receivers retain original presentation order.
        // At most 4 * threads results are pending; each spills above 1 MiB.
        for window in files[1..].chunks(threads * 4) {
            let mut replies = Vec::with_capacity(window.len());
            let mut jobs = Vec::with_capacity(window.len());
            for file in window {
                let (reply, receiver) = mpsc::sync_channel(1);
                jobs.push(Job {
                    left: left.join(file),
                    right: right.join(file),
                    name: file.clone(),
                    reply,
                });
                replies.push(receiver);
            }
            let size = |job: &Job| {
                [&job.left, &job.right]
                    .into_iter()
                    .filter_map(|path| std::fs::symlink_metadata(path).ok())
                    .map(|meta| meta.len())
                    .sum::<u64>()
            };
            jobs.sort_by_cached_key(|job| std::cmp::Reverse(size(job)));
            let mut jobs = jobs.into_iter();
            output.flush()?;
            for job in jobs.by_ref().take(threads - 1) {
                sender
                    .send(job)
                    .map_err(|_| anyhow::anyhow!("diff render threads stopped"))?;
            }
            if first {
                self.pair(
                    &left.join(&files[0]),
                    &right.join(&files[0]),
                    &files[0],
                    output,
                )?;
                first = false;
            }
            for job in jobs {
                output.flush()?;
                match sender.try_send(job) {
                    Ok(()) => (),
                    Err(mpsc::TrySendError::Full(job)) => {
                        let result = self.spooled(&job, false);
                        let _ = job.reply.send(result);
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        anyhow::bail!("diff render threads stopped")
                    }
                }
            }
            for reply in replies {
                let mut rendered = reply.recv().context("diff render thread stopped")??;
                output.flush()?;
                std::io::copy(&mut rendered.output, output)?;
                if let Some(profiles) = &self.profile {
                    profiles.borrow_mut().append(&mut rendered.profiles);
                }
                #[cfg(unix)]
                tree_house_bindings::query_cache::add_statistics(rendered.query_statistics);
            }
        }
        if first {
            self.pair(
                &left.join(&files[0]),
                &right.join(&files[0]),
                &files[0],
                output,
            )?;
        }
        Ok(())
    }

    fn serial_pairs(
        &self,
        left: &Path,
        right: &Path,
        files: &[PathBuf],
        output: &mut impl Write,
    ) -> Result<()> {
        for file in files {
            output.flush()?;
            self.pair(&left.join(file), &right.join(file), file, output)?;
        }
        Ok(())
    }
}

fn small(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => meta.len() <= SOURCE_LIMIT,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

fn threads() -> usize {
    // Retained sessions share their initialized loader with invocation-local
    // threads too. Serializing whole directories loses more than retention
    // saves on first views of adjacent revisions.
    let available = std::thread::available_parallelism().map_or(1, usize::from);
    std::env::var("HELIX_DIFF_RENDER_THREADS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(available)
        .clamp(1, MAX_THREADS)
}
