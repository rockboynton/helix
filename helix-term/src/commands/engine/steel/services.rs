//! Small asynchronous services for plugins. UI policy stays in Scheme.
use std::{future::Future, process::Stdio, time::Duration};

use serde_json::{json, Value};
use steel::{gc::Gc, rvals::FutureResult, SteelVal};
use tokio::io::AsyncReadExt;

use super::Context;

fn future_json(future: impl Future<Output = Value> + Send + 'static) -> SteelVal {
    SteelVal::FutureV(Gc::new(FutureResult::new(Box::pin(async move {
        SteelVal::try_from(future.await)
    }))))
}

pub(super) fn github_pulls(
    cx: &mut Context,
    repo: String,
    commit: String,
) -> anyhow::Result<SteelVal> {
    let (_, doc) = current_ref!(cx.editor);
    anyhow::ensure!(
        cx.editor
            .workspace_trust
            .query(
                doc.workspace_root(),
                helix_loader::workspace_trust::TrustQuery::Git
            )
            .is_trusted(),
        "Trust this workspace before requesting GitHub details (:workspace-trust)"
    );
    Ok(future_json(super::github::lookup(repo, commit)))
}

/// Snapshot cursor identity before scheduling any expensive history work.
pub(super) fn line_blame(cx: &mut Context) -> anyhow::Result<SteelVal> {
    let (view, doc) = current_ref!(cx.editor);
    anyhow::ensure!(
        cx.editor
            .workspace_trust
            .query(
                doc.workspace_root(),
                helix_loader::workspace_trust::TrustQuery::Git
            )
            .is_trusted(),
        "Trust this workspace before requesting blame (:workspace-trust)"
    );
    let path = doc
        .path()
        .ok_or_else(|| anyhow::anyhow!("Save this buffer before requesting blame"))?
        .to_path_buf();
    let line = doc.cursor_line(view.id) as u32;
    let offsets = doc
        .blame_line_offsets(line)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut identity = json!({
        "document-id": helix_view::extension::document_id_to_usize(&doc.id()),
        "version": doc.version(), "line": line, "path": path.to_string_lossy(),
        "directory": path.parent().map(|parent| parent.to_string_lossy()),
    });
    let cached = (!doc.is_blame_potentially_out_of_date)
        .then_some(doc.file_blame.as_ref())
        .flatten()
        .and_then(|value| value.as_ref().ok())
        .map(|blame| blame_data(blame, line, offsets));
    Ok(future_json(async move {
        let result = match cached {
            Some(value) => Ok(value),
            None => tokio::task::spawn_blocking(move || {
                helix_vcs::FileBlame::try_new(path).map(|blame| blame_data(&blame, line, offsets))
            })
            .await
            .map_err(anyhow::Error::from)
            .and_then(|value| value),
        };
        match result {
            Ok(data) => identity["data"] = data,
            Err(error) => identity["error"] = Value::String(error.to_string()),
        }
        identity
    }))
}

fn blame_data(blame: &helix_vcs::FileBlame, line: u32, offsets: (u32, u32)) -> Value {
    let details = blame.blame_for_line(line, offsets.0, offsets.1).details();
    json!({"commit": details.commit, "author": details.author, "email": details.email,
        "datetime": details.datetime, "title": details.title, "body": details.body,
        "remotes": blame.remote_urls()})
}

/// Discard replies after switching documents, moving lines, or editing the buffer.
pub(super) fn blame_current(cx: &mut Context, result: SteelVal) -> anyhow::Result<bool> {
    let result = Value::try_from(result)?;
    let (view, doc) = current_ref!(cx.editor);
    Ok(result["document-id"].as_f64()
        == Some(helix_view::extension::document_id_to_usize(&doc.id()) as f64)
        && result["version"].as_f64() == Some(f64::from(doc.version()))
        && result["line"].as_f64() == Some(doc.cursor_line(view.id) as f64))
}

/// No shell interpolation; bounded output/time; a dropped/timed-out request kills its child.
pub(super) fn run_command(program: String, args: Vec<String>, timeout_ms: u64) -> SteelVal {
    future_json(async move { command_output(program, args, timeout_ms).await })
}

async fn command_output(program: String, args: Vec<String>, timeout_ms: u64) -> Value {
    let result = tokio::time::timeout(Duration::from_millis(timeout_ms.clamp(1, 30_000)), async {
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdout = child.stdout.take().unwrap().take(1_048_577);
        let mut stderr = child.stderr.take().unwrap().take(65_537);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (out_result, err_result) =
            tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        out_result?;
        err_result?;
        anyhow::ensure!(
            out.len() <= 1_048_576 && err.len() <= 65_536,
            "Command output exceeded limit"
        );
        let status = child.wait().await?;
        Ok::<_, anyhow::Error>(
            json!({"ok": status.success(), "stdout": String::from_utf8_lossy(&out),
            "stderr": String::from_utf8_lossy(&err), "code": status.code()}),
        )
    })
    .await;
    match result {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => json!({"ok": false, "error": "Command unavailable or failed", "stdout": ""}),
        Err(_) => json!({"ok": false, "error": "Command timed out", "stdout": ""}),
    }
}

fn plugin_cache_path(name: &str) -> anyhow::Result<std::path::PathBuf> {
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)),
        "Cache names must contain only ASCII letters, digits, hyphens, or underscores"
    );
    Ok(helix_loader::cache_dir()
        .join("plugins")
        .join(format!("{name}.json")))
}

pub(super) fn load_cache(name: String) -> anyhow::Result<SteelVal> {
    let path = plugin_cache_path(&name)?;
    Ok(future_json(async move {
        tokio::task::spawn_blocking(move || read_cache(&path))
            .await
            .unwrap_or(Value::Bool(false))
    }))
}

fn read_cache(path: &std::path::Path) -> Value {
    use std::io::Read;
    let read = || -> anyhow::Result<Value> {
        let metadata = std::fs::symlink_metadata(path)?;
        anyhow::ensure!(
            metadata.is_file() && metadata.len() <= 1_048_576,
            "Invalid plugin cache"
        );
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(1_048_577)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 1_048_576, "Plugin cache too large");
        Ok(serde_json::from_slice(&bytes)?)
    };
    read().unwrap_or(Value::Bool(false))
}

pub(super) fn store_cache(name: String, value: SteelVal) -> anyhow::Result<SteelVal> {
    let path = plugin_cache_path(&name)?;
    let bytes = serde_json::to_vec(&Value::try_from(value)?)?;
    anyhow::ensure!(bytes.len() <= 1_048_576, "Plugin cache too large");
    Ok(future_json(async move {
        Value::Bool(
            tokio::task::spawn_blocking(move || write_cache(&path, &bytes))
                .await
                .is_ok_and(|result| result.is_ok()),
        )
    }))
}

fn write_cache(path: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::{
        io::Write,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
    let directory = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cache has no directory"))?;
    let mut directories = std::fs::DirBuilder::new();
    directories.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directories.mode(0o700);
    }
    directories.create(directory)?;
    let temporary = directory.join(format!(
        ".cache-{}-{}.tmp",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut created = false;
    let mut write = || -> anyhow::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        created = true;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        Ok(())
    };
    let result = write();
    // This exact temporary file was created by this call; no glob/broad cleanup.
    if result.is_err() && created {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_plugin_formats_details_and_rejects_unsafe_remotes() {
        use steel::{rvals::FromSteelVal, steel_vm::engine::Engine};
        let mut engine = Engine::new();
        engine.register_steel_module("helix/misc.scm".into(), r#"
            (provide line-blame-at-cursor line-blame-current? run-command-async github-commit-pulls github-clear-clients!
                     set-status! set-error! await-callback pop-last-component-by-name! push-component!
                     load-plugin-cache store-plugin-cache!)
            (define (line-blame-at-cursor) #f)
            (define (line-blame-current? x) #t)
            (define (run-command-async . args) #f)
            (define (github-commit-pulls . args) args)
            (define (github-clear-clients!) #f)
            (define (set-status! x) #f)
            (define (set-error! x) #f)
            (define (await-callback . args) #f)
            (define (pop-last-component-by-name! x) #f)
            (define (push-component! x) #f)
            (define (load-plugin-cache x) #f)
            (define (store-plugin-cache! . args) #f)
        "#.into());
        engine.register_steel_module(
            "helix/components.scm".into(),
            r#"
            (provide markdown-popup update-markdown-popup! markdown-popup-keybinding! markdown-code-block copy-text!
                     switch-markdown-popup! restore-markdown-popup! test-popup-action test-restore-count)
            (define (markdown-popup . args) args)
            (define (update-markdown-popup! x) #f)
            (define test-actions (hash))
            (define restores 0)
            (define (markdown-popup-keybinding! popup key action)
              (set! test-actions (hash-insert test-actions key action)))
            (define (test-popup-action key) (hash-ref test-actions key))
            (define (test-restore-count) restores)
            (define (copy-text! . args) #f)
            (define (switch-markdown-popup! . args) #f)
            (define (restore-markdown-popup!) (set! restores (+ restores 1)))
            (define (markdown-code-block language body) (string-append "````" language "\n" body "\n````"))
        "#
            .into(),
        );
        engine.add_search_directory(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtime"),
        );
        engine
            .run(
                r#"
                (require "plugins/gitlens/gitlens.scm")
                (require "plugins/gitlens/gitlens-cache.scm")
                "#,
            )
            .unwrap();
        // The runtime import above exercises the public plugin entry point.
        // Load its source in the test scope as well to test private policies and
        // popup state without exporting those implementation details to users.
        engine
            .compile_and_run_raw_program_with_path(
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../runtime/plugins/gitlens/gitlens.scm"
                )),
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../runtime/plugins/gitlens/gitlens.scm"),
            )
            .unwrap();
        for (url, expected) in [
            (
                "https://THE.COMPANY.DEV/Owner/Repo.git",
                Some("the.company.dev/Owner/Repo"),
            ),
            (
                "git@github.com:owner/repo.git",
                Some("github.com/owner/repo"),
            ),
            (
                "https://github.com/owner/repo.git",
                Some("github.com/owner/repo"),
            ),
            (
                "ssh://git@github.com/owner/repo",
                Some("github.com/owner/repo"),
            ),
            (
                "git@the.company.dev:owner/repo.git",
                Some("the.company.dev/owner/repo"),
            ),
            (
                "https://the.company.dev/owner/repo.git",
                Some("the.company.dev/owner/repo"),
            ),
            (
                "ssh://git@the.company.dev/owner/repo",
                Some("the.company.dev/owner/repo"),
            ),
            ("https://user:secret@the.company.dev/owner/repo", None),
            ("https://-the.company.dev/owner/repo", None),
            ("https://the..company.dev/owner/repo", None),
            ("https://the.company.dev/owner/repo#fragment", None),
            ("git@the.company.dev:owner/repo;evil", None),
            ("https://github.com/owner/../repo", None),
            ("https://github.com/owner/repo?token=secret", None),
        ] {
            let value = engine
                .call_function_by_name_with_args(
                    "github-repository",
                    vec![SteelVal::StringV(url.into())],
                )
                .unwrap();
            match expected {
                Some(expected) => assert_eq!(String::from_steelval(&value).unwrap(), expected),
                None => assert_eq!(value, SteelVal::BoolV(false)),
            }
        }
        let result = engine
            .run(
                r#"
            (format-blame (hash 'author "A *name*" 'email "a@example.com"
                                'datetime "2026-10-11T12:00:00-07:00" 'commit "abcdef"
                                'title "Title" 'body "Body")
                          (list (hash 'number "123" 'title "PR title" 'state "closed")) "")
        "#,
            )
            .unwrap();
        let text = String::from_steelval(result.last().unwrap()).unwrap();
        assert!(text.contains("Oct 11, 2026 at 12:00 (UTC-07:00)"));
        assert!(text.contains("PR #123"));
        assert!(text.contains("A \\*name\\*"));
        assert!(text.contains("Body"));
        assert!(!text.contains("**Line:**"));
        assert!(text.contains(">\n**Authored:**"));
        assert!(text.contains("abcdef\n## Title\n\nBody\n**PR #123"));
        let hints = engine
            .run(
                r#"
            (list (inferred-pulls "Fix watcher (#14544)" "owner/repo")
                  (inferred-pulls "Merge pull request #123 from owner/branch" "owner/repo")
                  (inferred-pulls "Fix #123" "owner/repo")
                  (inferred-pulls "Fix (#0)" "owner/repo")
                  (inferred-pulls "Fix (#123/evil)" "owner/repo"))
        "#,
            )
            .unwrap();
        let value = Value::try_from(hints.last().unwrap().clone()).unwrap();
        assert_eq!(value[0][0]["number"], "14544");
        assert_eq!(value[0][0]["inferred"], true);
        assert_eq!(value[1][0]["number"], "123");
        for index in 2..5 {
            assert!(value[index].as_array().unwrap().is_empty());
        }
        let cache = engine
            .run(
                r#"
            (remember-prs! "owner/repo" "abc" (list (hash 'number "123" 'title "Saved")))
            (cached-pr-entry "owner/repo" "abc")
        "#,
            )
            .unwrap();
        let cached = Value::try_from(cache.last().unwrap().clone()).unwrap();
        assert_eq!(cached[0][0]["title"], "Saved");
        assert_eq!(cached[1], true);
        let formatting = engine.run(r#"
            (list (human-datetime "Unknown")
                  (human-datetime "2024-12-05T11:13:00-06:00")
                  (repository-for (hash 'body "Source: helix-editor/helix#8675, abc."
                                        'remotes (list "https://github.com/helix-editor/helix.git"
                                                       "git@github.com:rockboynton/helix.git")))
                  (local-hints (hash 'body "Source: helix-editor/helix#8675, abc.") "rockboynton/helix")
                  (format-diff "diff --git a/test b/test\n+```\n")
                  (unpublished-commit? (hash 'unpublished #t))
                  (unpublished-commit? (hash 'ok #f)))
        "#).unwrap();
        let formatting = Value::try_from(formatting.last().unwrap().clone()).unwrap();
        assert_eq!(formatting[0], "Unknown");
        assert_eq!(formatting[1], "Dec 5, 2024 at 11:13 (UTC-06:00)");
        assert_eq!(formatting[2], "github.com/rockboynton/helix");
        assert_eq!(
            formatting[3][0]["html_url"],
            "https://github.com/helix-editor/helix/pull/8675"
        );
        assert!(formatting[4].as_str().unwrap().contains("````diff"));
        assert_eq!(formatting[5], true);
        assert_eq!(formatting[6], false);
        let canonical = engine
            .run(
                r#"
            (safe-pulls "rockboynton/helix"
              (list (hash 'number "8675" 'title "Upstream PR"
                          'html_url "https://github.com/helix-editor/helix/pull/8675")
                    (hash 'number "123" 'title "Fork PR"
                          'html_url "https://github.com/rockboynton/helix/pull/123")
                    (hash 'number "123" 'title "Wrong number"
                          'html_url "https://github.com/helix-editor/helix/pull/124")
                    (hash 'number "123" 'title "Unsafe host"
                          'html_url "https://github.com.evil.test/helix-editor/helix/pull/123")
                    (hash 'number "123" 'title "Unsafe path"
                          'html_url "https://github.com/../helix/pull/123")
                    (hash 'number "123" 'title "Unsafe query"
                          'html_url "https://github.com/o/r/pull/123?token=secret")))
        "#,
            )
            .unwrap();
        let canonical = Value::try_from(canonical.last().unwrap().clone()).unwrap();
        assert_eq!(canonical.as_array().unwrap().len(), 2);
        assert_eq!(
            canonical[0]["html_url"],
            "https://github.com/helix-editor/helix/pull/8675"
        );
        assert_eq!(
            canonical[1]["html_url"],
            "https://github.com/rockboynton/helix/pull/123"
        );
        let enterprise = engine.run(r#"
            (define enterprise-repo "the.company.dev/owner/repo")
            (define enterprise-data (hash 'commit "abc" 'title "Fix (#123)" 'body ""
                'remotes (list "git@the.company.dev:upstream/repo.git" "https://the.company.dev/owner/repo.git")))
            (remember-prs! enterprise-repo "abc" (list (hash 'number "123" 'title "Enterprise")))
            (remember-prs! "github.com/owner/repo" "abc" (list (hash 'number "123" 'title "Public")))
            (list (repository-for enterprise-data)
                  (popup-links enterprise-data enterprise-repo (local-hints enterprise-data enterprise-repo))
                  (github-commit-pulls enterprise-repo "abc")
                  (safe-pulls enterprise-repo
                    (list (hash 'number "123" 'title "Upstream" 'html_url "https://the.company.dev/upstream/repo/pull/123")
                          (hash 'number "123" 'title "Other host" 'html_url "https://github.com/owner/repo/pull/123")
                          (hash 'number "123" 'title "Lookalike" 'html_url "https://the.company.dev.evil/owner/repo/pull/123")
                          (hash 'number "123" 'title "Query" 'html_url "https://the.company.dev/owner/repo/pull/123?token=secret")))
                  (cached-pr-entry enterprise-repo "abc")
                  (cached-pr-entry "github.com/owner/repo" "abc"))
        "#).unwrap();
        let enterprise = Value::try_from(enterprise.last().unwrap().clone()).unwrap();
        assert_eq!(enterprise[0], "the.company.dev/owner/repo");
        assert_eq!(
            enterprise[1][0][1],
            "https://the.company.dev/owner/repo/commit/abc"
        );
        assert_eq!(
            enterprise[1][1][1],
            "https://the.company.dev/upstream/repo/pull/123"
        );
        assert_eq!(enterprise[2], json!(["the.company.dev/owner/repo", "abc"]));
        assert_eq!(enterprise[3].as_array().unwrap().len(), 1);
        assert_eq!(enterprise[4][0][0]["title"], "Enterprise");
        assert_eq!(enterprise[5][0][0]["title"], "Public");
        let deferred = engine.run(r#"
            (define test-data (hash 'commit "abc" 'remotes (list "https://github.com/rockboynton/helix")))
            (set! showing-diff? #t)
            (show-blame! request-id (hash 'data test-data) "rockboynton/helix" '() "PR: verified")
            (define queued? (not (equal? pending-blame-update #f)))
            (diff-popup "Diff body" test-data)
            ((test-popup-action #\d))
            (list queued? showing-diff? pending-blame-update (test-restore-count))
        "#).unwrap();
        let deferred = Value::try_from(deferred.last().unwrap().clone()).unwrap();
        assert_eq!(deferred, json!([true, false, false, 1]));
    }

    #[test]
    fn plugin_cache_is_atomic_private_and_tolerates_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("plugins/test.json");
        assert_eq!(read_cache(&path), Value::Bool(false));
        write_cache(&path, br#"{"version":"1","entries":{}}"#).unwrap();
        assert_eq!(read_cache(&path)["version"], "1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        write_cache(&path, br#"{"version":"2"}"#).unwrap();
        assert_eq!(read_cache(&path)["version"], "2");
        std::fs::write(&path, "invalid json").unwrap();
        assert_eq!(read_cache(&path), Value::Bool(false));
        std::fs::write(&path, vec![b' '; 1_048_577]).unwrap();
        assert_eq!(read_cache(&path), Value::Bool(false));
        assert!(plugin_cache_path("../escape").is_err());
        assert!(plugin_cache_path("").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_is_async_bounded_and_reports_failures() {
        let output = command_output(
            "printf".into(),
            vec!["%s".into(), "a;echo injected".into()],
            1000,
        )
        .await;
        assert_eq!(output["stdout"], "a;echo injected");
        assert_eq!(output["ok"], true);
        let timeout = command_output("sleep".into(), vec!["5".into()], 10).await;
        assert_eq!(timeout["error"], "Command timed out");
        let missing =
            command_output("/nonexistent/helix-plugin-command".into(), vec![], 1000).await;
        assert_eq!(missing["ok"], false);
    }
}
