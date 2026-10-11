use super::helpers;
use std::time::Duration;

/// Polling is disabled so this exercises native Filesentry delivery all the way
/// through the Helix event registry and editor job queue.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[tokio::test(flavor = "multi_thread")]
async fn native_watcher_reloads_external_writes_and_atomic_replacement() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.txt");
    std::fs::write(&path, "original\n")?;
    // FSEvents canonicalizes /var to /private/var on macOS. Exercise the same
    // alias-to-canonical mapping on every Unix host, including Linux CI.
    #[cfg(unix)]
    let path = {
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink(directory.path(), &alias)?;
        alias.join("source.txt")
    };
    let mut config = helpers::test_config();
    config.editor.file_watcher.enable = true;
    config.editor.auto_reload.poll.enable = false;
    config.editor.auto_reload.prompt_if_modified = false;
    let mut app = helpers::AppBuilder::new()
        .with_config(config)
        .with_file(&path, None)
        .build()?;
    let id = app.editor.document_id_by_path(&path).unwrap();
    app.editor.file_watcher.add_root(directory.path());
    let (_input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut input = tokio_stream::wrappers::UnboundedReceiverStream::new(input_rx);

    for atomic in [false, true] {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut attempt = 0;
        loop {
            attempt += 1;
            let content = format!("external {atomic} attempt {attempt}\n");
            if atomic {
                let replacement = directory.path().join("replacement.txt");
                std::fs::write(&replacement, &content)?;
                std::fs::rename(replacement, &path)?;
            } else {
                std::fs::write(&path, &content)?;
            }
            // Wait beyond the native settle interval, then drain editor jobs.
            tokio::time::sleep(Duration::from_millis(400)).await;
            anyhow::ensure!(
                app.event_loop_until_idle(&mut input).await,
                "editor exited during file watching"
            );
            if app.editor.document(id).unwrap().text().to_string() == content {
                break;
            }
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "native watch did not reload the buffer (atomic={atomic})"
            );
        }
    }
    helpers::test_key_sequence(&mut app, Some(":qa!<ret>"), None, true).await?;
    Ok(())
}
