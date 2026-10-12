//! Small asynchronous services for plugins. Hosting-specific logic stays in Scheme.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_plugin_formats_details_and_rejects_unsafe_remotes() {
        use steel::{rvals::FromSteelVal, steel_vm::engine::Engine};
        let mut engine = Engine::new();
        engine.register_steel_module("helix/misc.scm".into(), r#"
            (provide line-blame-at-cursor line-blame-current? run-command-async
                     set-status! set-error! await-callback pop-last-component-by-name! push-component!)
            (define (line-blame-at-cursor) #f)
            (define (line-blame-current? x) #t)
            (define (run-command-async . args) #f)
            (define (set-status! x) #f)
            (define (set-error! x) #f)
            (define (await-callback . args) #f)
            (define (pop-last-component-by-name! x) #f)
            (define (push-component! x) #f)
        "#.into());
        engine.register_steel_module(
            "helix/components.scm".into(),
            r#"
            (provide markdown-popup update-markdown-popup!)
            (define (markdown-popup . args) args)
            (define (update-markdown-popup! x) #f)
        "#
            .into(),
        );
        engine.register_steel_module(
            "helix/keymaps.scm".into(),
            r#"
            (provide add-global-keybinding)
            (define (add-global-keybinding x) #f)
        "#
            .into(),
        );
        engine
            .run(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../personal-config/gitlens.scm"
            )))
            .unwrap();
        for (url, expected) in [
            ("git@github.com:owner/repo.git", Some("owner/repo")),
            ("https://github.com/owner/repo.git", Some("owner/repo")),
            ("ssh://git@github.com/owner/repo", Some("owner/repo")),
            ("https://github.com.evil/owner/repo", None),
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
                          0 (list (hash 'number "123" 'title "PR title" 'state "closed")) "")
        "#,
            )
            .unwrap();
        let text = String::from_steelval(result.last().unwrap()).unwrap();
        assert!(text.contains("2026-10-11T12:00:00-07:00"));
        assert!(text.contains("PR #123"));
        assert!(text.contains("A \\*name\\*"));
        assert!(text.contains("Body"));
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
