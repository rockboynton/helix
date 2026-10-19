use super::helpers;
use std::time::Duration;

#[cfg(feature = "git")]
#[tokio::test(flavor = "multi_thread")]
async fn native_git_commit_refreshes_gutters_when_symbolic_head_is_unchanged() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let git = |args: &[&str]| -> anyhow::Result<()> {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args([
                "-c",
                "user.name=Watcher Test",
                "-c",
                "user.email=watcher@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    };
    git(&["init", "-q"])?;
    let path = root.join("source.txt");
    std::fs::write(&path, "original\n")?;
    git(&["add", "."])?;
    git(&["commit", "-qm", "baseline"])?;
    std::fs::write(&path, "committed\n")?;
    let mut config = helpers::test_config();
    config.editor.file_watcher.enable = true;
    config.editor.auto_reload.poll.enable = false;
    let mut app = helpers::AppBuilder::new()
        .with_config(config)
        .with_file(&path, None)
        .build()?;
    let id = app.editor.document_id_by_path(&path).unwrap();
    app.editor.file_watcher.add_root(root);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !app
        .editor
        .file_watcher
        .is_watching(&root.join(".git/refs/heads"))
    {
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "Git native watches not ready"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let head = std::fs::read(root.join(".git/HEAD"))?;
    git(&["commit", "-qam", "external commit"])?;
    assert_eq!(std::fs::read(root.join(".git/HEAD"))?, head);
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut input = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        anyhow::ensure!(
            tokio::time::timeout(
                Duration::from_secs(3),
                app.event_loop_until_idle(&mut input)
            )
            .await?,
            "editor exited"
        );
        let doc = app.editor.document(id).unwrap();
        let diff = doc.diff_handle().unwrap().load();
        if diff.diff_base().to_string() == "committed\n" && diff.is_empty() {
            break;
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "gutter did not refresh after git commit"
        );
    }
    helpers::test_key_sequence(&mut app, Some(":qa!<ret>"), None, true).await?;
    Ok(())
}

#[cfg(feature = "jj")]
fn jj(root: &std::path::Path, args: &[&str]) -> anyhow::Result<()> {
    let output = std::process::Command::new("jj")
        .current_dir(root)
        .args([
            "--config",
            "signing.behavior='drop'",
            "--config",
            "user.name='Watcher Test'",
            "--config",
            "user.email='watcher@example.com'",
        ])
        .args(args)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "jj {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// No synthetic events or polling: an external commit must update gutter bases
/// while preserving unsaved text, selections and the existing documents.
#[cfg(all(feature = "jj", any(target_os = "linux", target_os = "macos")))]
#[tokio::test(flavor = "multi_thread")]
async fn native_jj_commit_refreshes_gutters_without_reloading_buffers() -> anyhow::Result<()> {
    native_jj_commit_refreshes(false).await
}

#[cfg(all(feature = "jj", any(target_os = "linux", target_os = "macos")))]
#[tokio::test(flavor = "multi_thread")]
async fn native_linked_jj_commit_refreshes_gutters_without_reloading_buffers() -> anyhow::Result<()>
{
    native_jj_commit_refreshes(true).await
}

#[cfg(all(feature = "jj", any(target_os = "linux", target_os = "macos")))]
async fn native_jj_commit_refreshes(linked: bool) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let main = directory.path().join("main");
    std::fs::create_dir(&main)?;
    jj(&main, &["git", "init"])?;
    for file in ["clean.txt", "dirty.txt"] {
        std::fs::write(main.join(file), "original\n")?;
    }
    jj(&main, &["commit", "-m", "baseline"])?;
    let root = if linked {
        let root = directory.path().join("linked");
        jj(&main, &["workspace", "add", root.to_str().unwrap()])?;
        root
    } else {
        main
    };
    for file in ["clean.txt", "dirty.txt"] {
        std::fs::write(root.join(file), "committed\n")?;
    }
    jj(&root, &["status"])?;
    let mut config = helpers::test_config();
    config.editor.file_watcher.enable = true;
    config.editor.auto_reload.poll.enable = false;
    config.editor.auto_reload.prompt_if_modified = false;
    let mut app = helpers::AppBuilder::new()
        .with_config(config)
        .with_file(root.join("clean.txt"), None)
        .build()?;
    let clean = app
        .editor
        .document_id_by_path(&root.join("clean.txt"))
        .unwrap();
    let dirty = app.editor.open(
        &root.join("dirty.txt"),
        helix_view::editor::Action::VerticalSplit,
    )?;
    app.editor.file_watcher.add_root(&root);
    let view = app.editor.tree.focus;
    let doc = app.editor.document_mut(dirty).unwrap();
    let edit = helix_core::Transaction::insert(doc.text(), doc.selection(view), "unsaved".into());
    doc.apply(&edit, view);
    let text = app.editor.document(dirty).unwrap().text().clone();
    let view = app.editor.tree.focus;
    let selection = app.editor.document(dirty).unwrap().selection(view).clone();
    let head = app.editor.document(clean).unwrap().version_control_head();
    let paths = app.editor.diff_providers.get_watched_paths(&root);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !paths
        .iter()
        .all(|path| app.editor.file_watcher.is_watching(path))
    {
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "VCS native watches not ready (linked={linked}): {paths:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    jj(&root, &["commit", "-m", "external commit"])?;
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut input = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        anyhow::ensure!(app.event_loop_until_idle(&mut input).await, "editor exited");
        let doc = app.editor.document(clean).unwrap();
        let diff = doc.diff_handle().unwrap().load();
        if diff.diff_base().to_string() == "committed\n" && diff.is_empty() {
            break;
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "gutter did not refresh after jj commit (linked={linked})"
        );
    }
    let doc = app.editor.document(dirty).unwrap();
    assert_eq!(doc.text(), &text);
    assert_eq!(doc.selection(view), &selection);
    assert!(doc.is_modified());
    assert_eq!(
        doc.diff_handle().unwrap().load().diff_base().to_string(),
        "committed\n"
    );
    assert_ne!(
        app.editor.document(clean).unwrap().version_control_head(),
        head
    );
    helpers::test_key_sequence(&mut app, Some(":qa!<ret>"), None, true).await?;
    Ok(())
}

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
