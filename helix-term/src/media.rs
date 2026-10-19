//! Reusable asynchronous media provider. PDF parsing stays in a bounded child.
use helix_view::graphics::{Rect, Style};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use tokio::{io::AsyncReadExt, task::JoinHandle};
use tui::{buffer::Buffer, image::Image};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Png,
    Pdf,
}
impl Kind {
    pub fn for_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    path: PathBuf,
    page: usize,
    modified: Option<SystemTime>,
    len: u64,
}
type Response = Result<Arc<Image>, String>;

#[derive(Default)]
pub struct Preview {
    selected: Option<Key>,
    task: Option<JoinHandle<()>>,
    response: Arc<Mutex<Option<Response>>>,
    cache: VecDeque<(Key, Arc<Image>)>,
}
impl Drop for Preview {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Preview {
    pub fn path(&self) -> Option<&Path> {
        self.selected.as_ref().map(|key| key.path.as_path())
    }

    pub fn cancel(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(key) = self.selected.take() {
            if let Some(Ok(image)) = self.response.lock().unwrap().take() {
                self.cache.push_back((key, image));
                while self.cache.len() > 8
                    || self
                        .cache
                        .iter()
                        .map(|(_, image)| image.byte_size())
                        .sum::<usize>()
                        > 32 * 1024 * 1024
                {
                    self.cache.pop_front();
                }
            }
        }
    }

    fn request(&mut self, path: &Path, page: usize) -> Result<(), String> {
        // Stable previews do not stat or reopen the file on every frame.
        if self
            .selected
            .as_ref()
            .is_some_and(|key| key.path == path && key.page == page)
        {
            return Ok(());
        }
        let meta = path
            .metadata()
            .map_err(|_| "Media file unavailable".to_owned())?;
        if !meta.is_file() || meta.len() > 64 * 1024 * 1024 {
            return Err("Media preview limit: regular files up to 64 MiB".into());
        }
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()
                .map_err(|err| err.to_string())?
                .join(path)
        };
        let key = Key {
            path,
            page,
            modified: meta.modified().ok(),
            len: meta.len(),
        };
        if self.selected.as_ref() == Some(&key) {
            return Ok(());
        }
        self.cancel();
        self.selected = Some(key.clone());
        self.response = Arc::new(Mutex::new(None));
        if let Some(index) = self.cache.iter().position(|(cached, _)| *cached == key) {
            let (_, image) = self.cache.remove(index).unwrap();
            *self.response.lock().unwrap() = Some(Ok(image));
            return Ok(());
        }
        let response = self.response.clone();
        *response.lock().unwrap() = None;
        self.task = Some(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            // Bound CPU/process use even if multiple pickers are retained.
            static WORKERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
            let _permit = WORKERS.acquire().await.unwrap();
            let started = std::time::Instant::now();
            let result = load(&key.path, key.page)
                .await
                .map_err(|err| err.to_string());
            match &result {
                Ok(image) => log::debug!(
                    "Media preview ready: {} × {}, {} encoded bytes in {:?}",
                    image.width,
                    image.height,
                    image.byte_size(),
                    started.elapsed()
                ),
                Err(error) => log::debug!(
                    "Media preview failed after {:?}: {error}",
                    started.elapsed()
                ),
            }
            *response.lock().unwrap() = Some(result);
            helix_event::request_redraw();
        }));
        Ok(())
    }

    pub fn render(
        &mut self,
        path: &Path,
        page: usize,
        area: Rect,
        surface: &mut Buffer,
        text: Style,
        background: Style,
    ) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let request = self.request(path, page);
        if request.is_err() {
            self.cancel();
        }
        let response = self.response.lock().unwrap();
        let message = match (&request, response.as_ref()) {
            (Err(err), _) | (_, Some(Err(err))) => err.as_str(),
            (_, Some(Ok(image))) => {
                image.render(area.clip_bottom(1), surface, background);
                let label = match Kind::for_path(path) {
                    Some(Kind::Pdf) => format!("Page {} · Alt-j/k: next/previous page", page + 1),
                    _ => format!("PNG · {} × {}", image.width, image.height),
                };
                surface.set_stringn(
                    area.x,
                    area.bottom().saturating_sub(1),
                    &label,
                    area.width as usize,
                    text,
                );
                return;
            }
            _ => "Loading media…",
        };
        surface.set_stringn(
            area.x,
            area.y + area.height / 2,
            message,
            area.width as usize,
            text,
        );
    }
}

async fn bounded_read(
    reader: impl tokio::io::AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(std::io::Error::other(
            "Media renderer output exceeded limit",
        ));
    }
    Ok(bytes)
}

/// Argument-based invocation, never a shell. Dropping this future kills its child.
pub async fn load(path: &Path, page: usize) -> anyhow::Result<Arc<Image>> {
    let path = std::path::absolute(path)?;
    let metadata = tokio::fs::metadata(&path).await?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= 64 * 1024 * 1024,
        "Media preview limit: regular files up to 64 MiB"
    );
    let bytes = match Kind::for_path(&path) {
        Some(Kind::Png) => {
            bounded_read(tokio::fs::File::open(&path).await?, 16 * 1024 * 1024).await?
        }
        Some(Kind::Pdf) => {
            anyhow::ensure!(page < 10_000, "PDF page limit: 10000");
            let mut command = tokio::process::Command::new("pdftoppm");
            command
                .args([
                    "-f",
                    &(page + 1).to_string(),
                    "-l",
                    &(page + 1).to_string(),
                    "-singlefile",
                    "-scale-to",
                    "1600",
                    "-png",
                ])
                .arg(&path);
            renderer_output(command, Duration::from_secs(10)).await?
        }
        None => anyhow::bail!("Unsupported media format"),
    };
    // Base64 encoding and header validation stay off the UI thread too.
    tokio::task::spawn_blocking(move || Image::png(&bytes).map_err(anyhow::Error::msg)).await?
}

async fn renderer_output(
    mut command: tokio::process::Command,
    timeout: Duration,
) -> anyhow::Result<Vec<u8>> {
    tokio::time::timeout(timeout, async {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| {
                anyhow::anyhow!("Could not start pdftoppm (install Poppler tools): {err}")
            })?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (out, _, status) = tokio::try_join!(
            bounded_read(stdout, 16 * 1024 * 1024),
            bounded_read(stderr, 16 * 1024),
            child.wait()
        )?;
        anyhow::ensure!(
            status.success(),
            "Could not render PDF page (page missing, encrypted, or invalid PDF)"
        );
        Ok::<_, anyhow::Error>(out)
    })
    .await
    .map_err(|_| anyhow::anyhow!("PDF preview timed out"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn renderer_timeout_and_cancellation_kill_child_and_ignore_stderr() -> anyhow::Result<()>
    {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "printf success; printf '\\033]52;bad' >&2"]);
        assert_eq!(
            renderer_output(command, Duration::from_secs(2)).await?,
            b"success"
        );
        let dir = tempfile::tempdir()?;
        let pidfile = dir.path().join("pid");
        let mut command = tokio::process::Command::new("sh");
        command
            .args(["-c", "echo $$ > \"$1\"; exec sleep 60", "test"])
            .arg(&pidfile);
        let task = tokio::spawn(renderer_output(command, Duration::from_secs(10)));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while !pidfile.exists() {
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "test child did not start"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let pid: i32 = std::fs::read_to_string(pidfile)?.trim().parse()?;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        while unsafe { libc::kill(pid, 0) } == 0 {
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "cancelled renderer child still alive"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "exec sleep 60"]);
        assert!(renderer_output(command, Duration::from_millis(20))
            .await
            .unwrap_err()
            .to_string()
            .contains("timed out"));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn png_loading_is_bounded_and_cache_tracks_content_identity() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("image.PNG");
        std::fs::write(&path, include_bytes!("../../contrib/helix.png"))?;
        let image = load(&path, 0).await?;
        assert!(image.width > 0 && image.height > 0);
        assert!(bounded_read(&b"12345"[..], 4).await.is_err());
        assert_eq!(bounded_read(&b"1234"[..], 4).await?, b"1234");
        let meta = path.metadata()?;
        let key = Key {
            path: path.clone(),
            page: 0,
            modified: meta.modified().ok(),
            len: meta.len(),
        };
        let mut preview = Preview {
            selected: Some(key.clone()),
            response: Arc::new(Mutex::new(Some(Ok(image.clone())))),
            task: None,
            cache: VecDeque::new(),
        };
        preview.cancel();
        preview.request(&path, 0).unwrap();
        assert!(preview.task.is_none(), "cache hit must not start a worker");
        assert!(Arc::ptr_eq(
            preview
                .response
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap(),
            &image
        ));
        preview.cancel();
        std::fs::write(&path, b"not a png")?;
        preview.request(&path, 0).unwrap();
        assert_ne!(preview.selected.as_ref(), Some(&key));
        assert!(preview.task.is_some());
        preview.cancel();
        assert!(preview.task.is_none());
        Ok(())
    }
}
