#![cfg(all(unix, feature = "steel"))]

use anyhow::{ensure, Result};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    os::unix::net::UnixListener,
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Fixture {
    root: tempfile::TempDir,
    work: PathBuf,
    sockets: PathBuf,
}

#[test]
fn concurrent_clients_share_one_worker() -> Result<()> {
    let fixture = Fixture::new()?;
    let children = (0..4)
        .map(|_| {
            fixture
                .command()
                .args([
                    "--render-diff",
                    "--width",
                    "160",
                    "--color",
                    "always",
                    "a.txt",
                    "b.txt",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut outputs = Vec::new();
    for child in children {
        let output = child.wait_with_output()?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        ensure!(String::from_utf8_lossy(&output.stderr).contains("retained renderer"));
        outputs.push(output.stdout);
    }
    assert!(outputs.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(fixture.pids()?.len(), 1);
    assert_eq!(outputs[0], fixture.render(false, &[])?.stdout);
    Ok(())
}

#[test]
fn interrupted_reply_falls_back_without_partial_output() -> Result<()> {
    let fixture = Fixture::new()?;
    let expected = fixture.render(true, &[])?.stdout;
    let socket = fs::read_dir(&fixture.sockets)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "sock"))
        .unwrap();
    fixture.stop()?;
    wait_for(|| !socket.exists())?;
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let fake = std::thread::spawn(move || -> std::io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        let mut size = [0; 4];
        stream.read_exact(&mut size)?;
        let mut request = vec![0; u32::from_le_bytes(size) as usize];
        stream.read_exact(&mut request)?;
        stream.write_all(&[0])?;
        stream.write_all(&100_u32.to_le_bytes())?;
        stream.write_all(b"NOT-A-DIFF")?;
        Ok(())
    });
    let output = fixture
        .command()
        .args([
            "--render-diff",
            "--width",
            "160",
            "--color",
            "always",
            "a.txt",
            "b.txt",
        ])
        .output()?;
    fake.join().unwrap()?;
    fs::remove_file(socket)?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("fallback"));
    assert_eq!(output.stdout, expected);
    Ok(())
}

#[test]
fn cancelled_client_does_not_break_worker() -> Result<()> {
    let fixture = Fixture::new()?;
    let expected = fixture.render(true, &[])?.stdout;
    let pids = fixture.pids()?;
    fs::write(fixture.work.join("b.txt"), "changed line\n".repeat(100_000))?;
    let mut child = fixture
        .command()
        .args(["--render-diff", "--width", "160", "a.txt", "b.txt"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::sleep(Duration::from_millis(100));
    if child.try_wait()?.is_none() {
        child.kill()?;
    }
    child.wait()?;
    fs::write(fixture.work.join("b.txt"), "after\n")?;
    assert_eq!(fixture.render(true, &[])?.stdout, expected);
    assert_eq!(fixture.pids()?, pids);
    Ok(())
}

impl Fixture {
    fn new() -> Result<Self> {
        let root = tempfile::Builder::new().prefix("nw-").tempdir_in("/tmp")?;
        let work = root.path().join("work");
        let sockets = root.path().join("s");
        fs::create_dir_all(work.join(".git"))?;
        fs::create_dir(&sockets)?;
        fs::set_permissions(&sockets, fs::Permissions::from_mode(0o700))?;
        let user = root.path().join("config/helix");
        fs::create_dir_all(&user)?;
        fs::write(
            user.join("helix.scm"),
            "(provide configure!) (define (configure!) #t)",
        )?;
        fs::write(
            user.join("init.scm"),
            "(require \"helix.scm\") (configure!)",
        )?;
        fs::write(work.join("a.txt"), "before\n")?;
        fs::write(work.join("b.txt"), "after\n")?;
        Ok(Self {
            work: fs::canonicalize(work)?,
            sockets,
            root,
        })
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hx"));
        command
            .current_dir(&self.work)
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("STEEL_HOME", self.root.path().join("steel"))
            .env("HELIX_DIFF_WORKER_DIR", &self.sockets)
            .env("HELIX_DIFF_WORKER_IDLE_SECS", "3")
            .env("HELIX_DIFF_WORKER_TRACE", "1")
            .env("HELIX_DIFF_WORKER", "1")
            .env_remove("HELIX_STEEL_CONFIG")
            .env_remove("HELIX_DIFF_WORKER_WATCH");
        command
    }
    fn render(&self, worker: bool, extra: &[&str]) -> Result<Output> {
        let result = self
            .command()
            .env("HELIX_DIFF_WORKER", if worker { "1" } else { "0" })
            .args(["--render-diff", "--width", "160", "--color", "always"])
            .args(extra)
            .args(["a.txt", "b.txt"])
            .output()?;
        ensure!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if worker {
            ensure!(
                String::from_utf8_lossy(&result.stderr).contains("retained renderer"),
                "worker was not used: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        Ok(result)
    }
    fn pids(&self) -> Result<Vec<u32>> {
        let mut pids = Vec::new();
        for entry in fs::read_dir(&self.sockets)? {
            let path = entry?.path();
            if path.extension().is_some_and(|value| value == "pid") {
                pids.push(fs::read_to_string(path)?.trim().parse()?);
            }
        }
        pids.sort();
        Ok(pids)
    }
    fn stop(&self) -> Result<()> {
        let output = self.command().arg("--diff-worker-stop").output()?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[test]
fn worker_reuses_process_and_preserves_layout_theme_and_changed_contents() -> Result<()> {
    let fixture = Fixture::new()?;
    let first = fixture.render(true, &[])?;
    let pids = fixture.pids()?;
    assert_eq!(pids.len(), 1);
    assert_eq!(first.stdout, fixture.render(false, &[])?.stdout);
    for extra in [
        vec!["--diff-layout", "inline"],
        vec!["--diff-theme", "gruvbox_dark_hard"],
        vec!["--color", "never"],
    ] {
        assert_eq!(
            fixture.render(true, &extra)?.stdout,
            fixture.render(false, &extra)?.stdout
        );
        assert_eq!(fixture.pids()?, pids);
    }
    fs::write(fixture.work.join("b.txt"), "changed after\n")?;
    let updated = fixture.render(true, &[])?;
    assert_ne!(updated.stdout, first.stdout);
    assert_eq!(updated.stdout, fixture.render(false, &[])?.stdout);
    assert_eq!(fixture.pids()?, pids);
    fixture.stop()?;
    wait_for(|| fixture.pids().is_ok_and(|pids| pids.is_empty()))?;
    Ok(())
}

fn wait_for(mut predicate: impl FnMut() -> bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        ensure!(Instant::now() < deadline, "timed out waiting for worker");
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[test]
fn config_imports_invalidate_and_untrusted_project_never_runs() -> Result<()> {
    let fixture = Fixture::new()?;
    let user = fixture.root.path().join("config/helix");
    fs::write(
        user.join("appearance.scm"),
        "(require (prefix-in hx. \"helix/commands.scm\")) (hx.theme \"gruvbox_dark_hard\")",
    )?;
    fs::write(
        user.join("init.scm"),
        "(require \"helix.scm\") (require \"appearance.scm\")",
    )?;
    let project = fixture.work.join(".helix");
    fs::create_dir(&project)?;
    fs::write(
        project.join("helix.scm"),
        "(error \"untrusted project ran\")",
    )?;
    fs::write(
        project.join("init.scm"),
        "(error \"untrusted project ran\")",
    )?;
    let first = fixture.render(true, &[])?;
    let pids = fixture.pids()?;
    fs::write(
        user.join("appearance.scm"),
        "(require (prefix-in hx. \"helix/commands.scm\")) (hx.theme \"base16_default_dark\")",
    )?;
    let second = fixture.render(true, &[])?;
    assert_ne!(first.stdout, second.stdout);
    assert_eq!(second.stdout, fixture.render(false, &[])?.stdout);
    assert_ne!(pids, fixture.pids()?);
    Ok(())
}

#[test]
fn trust_grant_and_revocation_rotate_the_session() -> Result<()> {
    let fixture = Fixture::new()?;
    let project = fixture.work.join(".helix");
    fs::create_dir(&project)?;
    fs::write(
        project.join("helix.scm"),
        "(provide configure!) (define (configure!) #t)",
    )?;
    fs::write(
        project.join("init.scm"),
        "(require (prefix-in hx. \"helix/commands.scm\")) (hx.theme \"gruvbox_dark_hard\")",
    )?;
    let untrusted = fixture.render(true, &[])?;
    let hash = helix_loader::workspace_trust::compute_workspace_hash(&fixture.work).unwrap();
    let mut digest = Sha256::new();
    digest.update(fixture.work.as_os_str().to_string_lossy().as_bytes());
    let name: String = digest
        .finalize()
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect();
    let directory = fixture.root.path().join("data/helix/workspace_trust");
    fs::create_dir_all(&directory)?;
    let entry = directory.join(name);
    fs::write(
        &entry,
        format!(
            "path = {}\nhash = {hash}\nexcluded = false\n",
            fixture.work.display()
        ),
    )?;
    let trusted = fixture.render(true, &[])?;
    assert_ne!(untrusted.stdout, trusted.stdout);
    fs::remove_file(entry)?;
    assert_eq!(untrusted.stdout, fixture.render(true, &[])?.stdout);
    Ok(())
}

#[test]
fn worker_idle_exit_and_secure_fallback() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.render(true, &[])?;
    wait_for(|| fixture.pids().is_ok_and(|pids| pids.is_empty()))?;
    fs::set_permissions(&fixture.sockets, fs::Permissions::from_mode(0o755))?;
    let output = fixture
        .command()
        .args([
            "--render-diff",
            "--width",
            "160",
            "--color",
            "always",
            "a.txt",
            "b.txt",
        ])
        .output()?;
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("fallback"));
    assert_eq!(output.stdout, fixture.render(false, &[])?.stdout);
    fs::set_permissions(&fixture.sockets, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[test]
fn different_workspaces_get_different_workers() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.render(true, &[])?;
    let first = fixture.pids()?;
    let other = fixture.root.path().join("other");
    fs::create_dir_all(other.join(".git"))?;
    fs::copy(fixture.work.join("a.txt"), other.join("a.txt"))?;
    fs::copy(fixture.work.join("b.txt"), other.join("b.txt"))?;
    let output = fixture
        .command()
        .current_dir(&other)
        .args(["--render-diff", "--width", "160", "a.txt", "b.txt"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pids = fixture.pids()?;
    assert_eq!(pids.len(), 2);
    assert!(pids.contains(&first[0]));
    let stop = fixture
        .command()
        .current_dir(other)
        .arg("--diff-worker-stop")
        .output()?;
    assert!(stop.status.success());
    Ok(())
}
