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
fn explicit_warmup_starts_one_worker_without_render_output_or_implicit_opt_in() -> Result<()> {
    let fixture = Fixture::new()?;
    let warm = fixture
        .command()
        .env("HELIX_DIFF_WORKER", "0")
        .args(["--diff-worker-warm", "--width", "160"])
        .output()?;
    ensure!(
        warm.status.success(),
        "{}",
        String::from_utf8_lossy(&warm.stderr)
    );
    assert!(warm.stdout.is_empty());
    let pid = fixture.pids()?;
    assert_eq!(pid.len(), 1);
    let expected = fixture.render(false, &[])?;
    let rendered = fixture.render(true, &[])?;
    assert_eq!(rendered.stdout, expected.stdout);
    assert_eq!(fixture.pids()?, pid);
    Ok(())
}

#[test]
fn parallel_worker_matches_full_parse_across_unseen_snapshots_and_coverage() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut pid = None;
    for version in 0..3 {
        let left = fixture.work.join(format!("snapshot-{version}/left"));
        let right = fixture.work.join(format!("snapshot-{version}/right"));
        fs::create_dir_all(&left)?;
        fs::create_dir_all(&right)?;
        let large = |value: usize| {
            let body: String = (0..3200)
                .map(|line| {
                    let value = if line == 1500 { value } else { 0 };
                    format!("fn item_{line}() {{ let x = vec![{line}, {value}]; }}\n")
                })
                .collect();
            format!("macro_rules! items {{ () => {{\n{body}\n}}; }}\n")
        };
        let before = large(version);
        let after = large(version + 1);
        assert!((128 * 1024..512 * 1024).contains(&before.len()));
        fs::write(left.join("large.rs"), before)?;
        fs::write(right.join("large.rs"), after)?;
        for index in 0..9 {
            let name = format!("{index:02}.rs");
            fs::write(left.join(&name), format!(
                "/// ```rust\n/// let x = {version};\n/// println!(\"{{x}}\");\n/// ```\nfn main() {{ let x = vec![{version}, {index}]; }}\n"
            ))?;
            fs::write(right.join(&name), format!(
                "/* context {version} */\n/// ```rust\n/// let x = {};\n/// println!(\"{{x}} 界\");\n/// ```\nfn main() {{ let x = vec![{version}, {index}, 9]; }}\n", version + 1
            ))?;
        }
        for layout in ["inline", "side-by-side"] {
            let render = |worker: bool| -> Result<Output> {
                Ok(fixture
                    .command()
                    .env("HELIX_DIFF_WORKER", if worker { "1" } else { "0" })
                    .env("HELIX_DIFF_RENDER_THREADS", if worker { "4" } else { "1" })
                    .env("HELIX_DIFF_CACHE", if worker { "1" } else { "0" })
                    .env("HELIX_DIFF_SYNTAX_RANGES", if worker { "1" } else { "0" })
                    .env(
                        "HELIX_DIFF_INCREMENTAL_PARSE",
                        if worker { "1" } else { "0" },
                    )
                    .args([
                        "--render-diff",
                        "--width",
                        "160",
                        "--color",
                        "always",
                        "--diff-layout",
                        layout,
                        "--diff-context",
                        if version == 1 { "0" } else { "3" },
                    ])
                    .args([&left, &right])
                    .output()?)
            };
            let full = render(false)?;
            let retained = render(true)?;
            ensure!(
                full.status.success(),
                "{}",
                String::from_utf8_lossy(&full.stderr)
            );
            ensure!(
                retained.status.success(),
                "{}",
                String::from_utf8_lossy(&retained.stderr)
            );
            assert_eq!(retained.stdout, full.stdout, "snapshot {version}/{layout}");
            assert!(String::from_utf8_lossy(&retained.stderr).contains("retained renderer"));
            let current = fixture.pids()?;
            assert_eq!(current.len(), 1);
            if let Some(pid) = &pid {
                assert_eq!(&current, pid);
            }
            pid = Some(current);
        }
    }
    Ok(())
}

#[test]
fn parallel_directory_render_matches_serial_and_handles_spooling_and_errors() -> Result<()> {
    let fixture = Fixture::new()?;
    let left = fixture.work.join("left");
    let right = fixture.work.join("right");
    fs::create_dir(&left)?;
    fs::create_dir(&right)?;
    for index in 0..9 {
        let name = format!("{index:02}.rs");
        fs::write(
            left.join(&name),
            format!("/// Before 界\nfn before_{index}() {{}}\n"),
        )?;
        fs::write(
            right.join(&name),
            format!("/// After é\nfn after_{index}() {{ let s = \"string\"; }}\n"),
        )?;
    }
    // Changed source fits the parallel source cap but produces more than the
    // spool's in-memory limit once syntax colors and wrapped rows are emitted.
    fs::remove_file(left.join("03.rs"))?;
    fs::remove_file(right.join("03.rs"))?;
    fs::write(left.join("03.txt"), "before\n")?;
    fs::write(
        right.join("03.txt"),
        format!("{}界 é\n", "\t".repeat(20)).repeat(10000),
    )?;
    let render = |threads: &str, cache: &str, layout: &str| -> Result<Output> {
        Ok(fixture
            .command()
            .env("HELIX_DIFF_WORKER", "0")
            .env("HELIX_DIFF_RENDER_THREADS", threads)
            .env("HELIX_DIFF_CACHE", cache)
            .args([
                "--render-diff",
                "--width",
                "100",
                "--color",
                "always",
                "--diff-layout",
                layout,
            ])
            .args([&left, &right])
            .output()?)
    };
    for layout in ["inline", "side-by-side"] {
        let serial = render("1", "0", layout)?;
        ensure!(
            serial.status.success(),
            "{}",
            String::from_utf8_lossy(&serial.stderr)
        );
        for threads in ["2", "4"] {
            let initial = render(threads, "1", layout)?;
            ensure!(
                initial.status.success(),
                "{}",
                String::from_utf8_lossy(&initial.stderr)
            );
            assert_eq!(
                initial.stdout, serial.stdout,
                "threads={threads} layout={layout}"
            );
            assert!(String::from_utf8_lossy(&initial.stderr)
                .contains(&format!("render threads={threads}")));
        }
    }
    assert!(fixture.pids()?.is_empty());
    // An error in a background file must return promptly, not leave threads
    // asleep waiting for further jobs. A FIFO is never opened as a source file.
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(right.join("02.rs").as_os_str().as_bytes())?;
        fs::remove_file(right.join("02.rs"))?;
        // SAFETY: path is a live, NUL-terminated path to a disposable test FIFO.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let failed = render("4", "1", "inline")?;
        assert!(!failed.status.success());
        assert!(String::from_utf8_lossy(&failed.stderr).contains("not a regular file"));
    }
    Ok(())
}

#[test]
fn syntax_event_cache_matches_live_trees_across_layouts_themes_unicode_and_injections() -> Result<()>
{
    let fixture = Fixture::new()?;
    fs::write(
        fixture.root.path().join("config/helix/languages.toml"),
        "[[language]]\nname = 'rust'\ninjection-regex = '^(rust|neo-rust)$'\n",
    )?;
    let cases = [
        ("rs", "/// docs\nfn before() {\n let x = \"界 é\"; // hi\n}\n", "/// new docs\nfn after() {\n let x = \"界 é long text long text long text long text long text long text\"; // hi\n}\n"),
        ("md", "# Old\n\n```neo-rust\nfn old() { let s = \"abc\"; }\n```\n", "# New\n\n```neo-rust\nfn new() { let s = \"def\"; }\n```\n"),
        ("toml", "# old\n[table]\nname = \"old\"\n", "# new\n[table]\nname = \"new\"\n"),
        ("nix", "{ lib }: { name = \"old\"; }\n", "{ lib }: { name = \"new\"; }\n"),
        ("scm", "(define (old x) (list x \"before\"))\n", "(define (new x) (list x \"after\"))\n"),
    ];
    for (extension, before, after) in cases {
        let left = fixture.work.join(format!("before.{extension}"));
        let right = fixture.work.join(format!("after.{extension}"));
        fs::write(&left, before)?;
        fs::write(&right, after)?;
        for (layout, width, theme) in [
            ("inline", "60", "gruvbox_dark_hard"),
            ("side-by-side", "160", "base16_default_dark"),
        ] {
            let render = |cache: bool| -> Result<Output> {
                let result = fixture
                    .command()
                    .env("HELIX_DIFF_WORKER", "0")
                    .env("HELIX_DIFF_CACHE", if cache { "1" } else { "0" })
                    .env("HELIX_DIFF_SYNTAX_RANGES", if cache { "1" } else { "0" })
                    .env(
                        "HELIX_DIFF_INCREMENTAL_PARSE",
                        if cache { "1" } else { "0" },
                    )
                    .args([
                        "--render-diff",
                        "--width",
                        width,
                        "--color",
                        "always",
                        "--diff-layout",
                        layout,
                        "--diff-theme",
                        theme,
                    ])
                    .args([&left, &right])
                    .output()?;
                ensure!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
                Ok(result)
            };
            let initial = render(true)?;
            let cached = render(true)?;
            let fresh = render(false)?;
            assert_eq!(initial.stdout, fresh.stdout, "initial {extension}/{layout}");
            assert_eq!(cached.stdout, fresh.stdout, "cached {extension}/{layout}");
            assert!(
                String::from_utf8_lossy(&cached.stderr).contains("highlight hit"),
                "{}",
                String::from_utf8_lossy(&cached.stderr)
            );
            assert!(fixture.pids()?.is_empty());
        }
    }
    let directory = fixture.root.path().join("cache/helix/diff-highlights-v1");
    for entry in fs::read_dir(&directory)? {
        fs::write(entry?.path(), b"corrupted")?;
    }
    let render = |cache: bool| -> Result<Output> {
        let result = fixture
            .command()
            .env("HELIX_DIFF_WORKER", "0")
            .env("HELIX_DIFF_CACHE", if cache { "1" } else { "0" })
            .args([
                "--render-diff",
                "--width",
                "60",
                "--color",
                "always",
                "--diff-layout",
                "inline",
                "--diff-theme",
                "gruvbox_dark_hard",
                "before.rs",
                "after.rs",
            ])
            .output()?;
        ensure!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        Ok(result)
    };
    let repaired = render(true)?;
    assert!(!String::from_utf8_lossy(&repaired.stderr).contains("highlight hit"));
    assert_eq!(repaired.stdout, render(false)?.stdout);
    assert!(String::from_utf8_lossy(&render(true)?.stderr).contains("highlight hit"));
    for entry in fs::read_dir(&directory)? {
        fs::set_permissions(entry?.path(), fs::Permissions::from_mode(0o644))?;
    }
    let public = render(true)?;
    assert!(!String::from_utf8_lossy(&public.stderr).contains("highlight hit"));
    assert_eq!(public.stdout, repaired.stdout);
    Ok(())
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
fn native_profile_cache_preserves_output_without_starting_steel_or_a_worker() -> Result<()> {
    let fixture = Fixture::new()?;
    let user = fixture.root.path().join("config/helix");
    fs::write(user.join("init.scm"), "(require \"helix.scm\") (displayln \"PROFILE-CONFIG-EVALUATED\") (require (prefix-in hx. \"helix/commands.scm\")) (hx.theme \"gruvbox_dark_hard\")")?;
    let render = || {
        fixture
            .command()
            .env("HELIX_DIFF_WORKER", "0")
            .env("HELIX_DIFF_CACHE_TRACE", "1")
            .args([
                "--render-diff",
                "--width",
                "160",
                "--color",
                "always",
                "a.txt",
                "b.txt",
            ])
            .output()
    };
    let first = render()?;
    ensure!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(stderr.contains("PROFILE-CONFIG-EVALUATED"), "{stderr}");
    assert!(stderr.contains("native profile stored"), "{stderr}");
    let second = render()?;
    ensure!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(first.stdout, second.stdout);
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("native profile hit"), "{stderr}");
    assert!(!stderr.contains("PROFILE-CONFIG-EVALUATED"), "{stderr}");
    assert!(fixture.pids()?.is_empty());
    fs::write(fixture.work.join("b.txt"), "new file contents\n")?;
    let changed = render()?;
    assert!(changed.status.success());
    assert_ne!(changed.stdout, first.stdout);
    assert!(String::from_utf8_lossy(&changed.stderr).contains("native profile hit"));
    let uncached = fixture
        .command()
        .env("HELIX_DIFF_WORKER", "0")
        .env("HELIX_DIFF_CACHE", "0")
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
    assert!(uncached.status.success());
    assert_eq!(changed.stdout, uncached.stdout);
    Ok(())
}

#[test]
fn corrupt_or_public_native_profile_falls_back_to_config_evaluation() -> Result<()> {
    let fixture = Fixture::new()?;
    let first = fixture.render(false, &[])?;
    let directory = fixture.root.path().join("cache/helix/diff-profile-v1");
    let profile = fs::read_dir(directory)?
        .next()
        .unwrap_or_else(|| {
            panic!(
                "profile missing: {}",
                String::from_utf8_lossy(&first.stderr)
            )
        })?
        .path();
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o644))?;
    assert_eq!(fixture.render(false, &[])?.stdout, first.stdout);
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o600))?;
    fs::write(&profile, "not a native profile")?;
    assert_eq!(fixture.render(false, &[])?.stdout, first.stdout);
    let output = fixture
        .command()
        .env("HELIX_DIFF_WORKER", "0")
        .env("HELIX_DIFF_CACHE_TRACE", "1")
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
    assert!(String::from_utf8_lossy(&output.stderr).contains("native profile hit"));
    Ok(())
}

#[test]
fn query_analysis_disk_cache_matches_fresh_highlighting_and_repairs_corruption() -> Result<()> {
    let fixture = Fixture::new()?;
    let cases = [
        ("rs", "/// docs\nfn old(x: Option<u32>) { if let Some(n) = x { println!(\"{n}\"); } }\n", "/// docs\nfn new(x: Option<u32>) { if let Some(n) = x { println!(\"value={n}\"); } }\n"),
        ("md", "# Before\n```rust\nfn old() {}\n```\n", "# After\n```rust\nfn new() {}\n```\n"),
        ("toml", "[section]\nname = \"old\"\nvalues = [1, 2]\n", "[section]\nname = \"new\"\nvalues = [1, 3]\n"),
        ("nix", "{ pkgs }: let value = 1; in { inherit value; }\n", "{ pkgs }: let value = 2; in { inherit value; }\n"),
        ("scm", "(define (old x) (list x \"before\"))\n", "(define (new x) (list x \"after\"))\n"),
    ];
    let statistics = |output: &Output| -> (usize, usize) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let line = stderr
            .lines()
            .find(|line| line.contains("query-analysis hits="))
            .expect("query cache statistics");
        let values = line
            .split_whitespace()
            .filter_map(|part| {
                part.split_once('=')
                    .map(|(_, value)| value.parse::<usize>().unwrap())
            })
            .collect::<Vec<_>>();
        (values[0], values[1])
    };
    for (index, (extension, before, after)) in cases.into_iter().enumerate() {
        let left = fixture.work.join(format!("old.{extension}"));
        let right = fixture.work.join(format!("new.{extension}"));
        fs::write(&left, before)?;
        fs::write(&right, after)?;
        let render = |cache: bool| -> Result<Output> {
            let output = fixture
                .command()
                .env("HELIX_DIFF_WORKER", "0")
                .env("HELIX_DIFF_CACHE", if cache { "1" } else { "0" })
                .env("HELIX_DIFF_HIGHLIGHT_CACHE", "0")
                .env("HELIX_DIFF_CACHE_TRACE", "1")
                .args(["--render-diff", "--width", "160", "--color", "always"])
                .args([&left, &right])
                .output()?;
            ensure!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(output)
        };
        let initial = render(true)?;
        let cached = render(true)?;
        let fresh = render(false)?;
        assert_eq!(
            initial.stdout, fresh.stdout,
            "first {extension} output differs"
        );
        assert_eq!(
            cached.stdout, fresh.stdout,
            "cached {extension} output differs"
        );
        let (hits, total) = statistics(&cached);
        assert!(
            hits > 0 && hits == total,
            "not all queries hit disk cache: {}",
            String::from_utf8_lossy(&cached.stderr)
        );
        assert!(fixture.pids()?.is_empty());
        if index == 0 {
            let directory = fixture.root.path().join("cache/helix/diff-queries-v1");
            let (query, mut data) = fs::read_dir(directory)?
                .map(|entry| -> Result<_> {
                    let path = entry?.path();
                    let data = fs::read(&path)?;
                    Ok((path, data))
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .find(|(_, data)| {
                    data.len() > 48 && u32::from_le_bytes(data[36..40].try_into().unwrap()) > 0
                })
                .expect("a query with steps, not an empty locals query");
            // Keep the checksum valid while making the C payload invalid, so
            // this exercises the C bounds/flag checks, not only Rust hashing.
            data[32 + 16] |= 0x80;
            let checksum = Sha256::digest(&data[32..]);
            data[..32].copy_from_slice(&checksum);
            fs::write(&query, data)?;
            let repaired = render(true)?;
            assert_eq!(repaired.stdout, fresh.stdout);
            let (hits, total) = statistics(&repaired);
            assert!(
                hits < total,
                "corrupt cache must trigger normal validating compilation"
            );
            let healed = render(true)?;
            let (hits, total) = statistics(&healed);
            assert!(hits > 0 && hits == total);
            fs::write(&query, "truncated query analysis")?;
            assert_eq!(render(true)?.stdout, fresh.stdout);
        }
    }
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
            .env("HELIX_DIFF_WORKER_IDLE_SECS", "30")
            .env("HELIX_DIFF_WORKER_TRACE", "1")
            .env("HELIX_DIFF_CACHE_TRACE", "1")
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
    let started = fixture
        .command()
        .env("HELIX_DIFF_WORKER_IDLE_SECS", "1")
        .args(["--render-diff", "--width", "160", "a.txt", "b.txt"])
        .output()?;
    assert!(started.status.success());
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
