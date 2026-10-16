//! Private Unix worker transport for sequential, read-only flow rendering.
//! No terminal acquisition, public listener, persistent config snapshots or API tokens.
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    args::Args,
    diff_render::{ColorMode, Layout, RenderSession},
};

const VERSION: u32 = 1;
const MAX_REQUEST: usize = 64 * 1024;
const MAX_OUTPUT: usize = 64 * 1024 * 1024;
const OUTPUT: u8 = 0;
const ERROR: u8 = 1;
const STALE: u8 = 2;
const FALLBACK: u8 = 3;
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Serialize, Deserialize)]
enum Request {
    Render {
        version: u32,
        fingerprint: String,
        paths: [PathBuf; 2],
        width: u16,
        context: usize,
        inline: bool,
        color: bool,
        theme: Option<String>,
    },
    Stop,
}

struct Scope {
    root: PathBuf,
    base: String,
    slot: String,
    fingerprint: String,
}

fn uid() -> u32 {
    // SAFETY: geteuid has no pointer arguments or preconditions.
    unsafe { libc::geteuid() }
}

fn trace(message: impl std::fmt::Display) {
    if std::env::var_os("HELIX_DIFF_WORKER_TRACE").is_some() {
        eprintln!("diff worker: {message}");
    }
}

fn hash_path(hash: &mut Sha256, path: &Path) {
    hash_field(hash, path.as_os_str().as_bytes());
}
fn hash_field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn finish_hash(hash: Sha256) -> String {
    hash.finalize()
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect()
}

fn metadata(hash: &mut Sha256, meta: &fs::Metadata) {
    for value in [
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime() as u64,
        meta.mtime_nsec() as u64,
        meta.ctime() as u64,
        meta.ctime_nsec() as u64,
        meta.mode() as u64,
    ] {
        hash.update(value.to_le_bytes());
    }
}

fn private_root() -> Result<PathBuf> {
    let root = std::env::var_os("HELIX_DIFF_WORKER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/neohelix-diff-{}", uid())));
    ensure!(root.is_absolute(), "worker directory must be absolute");
    match fs::DirBuilder::new().mode(0o700).create(&root) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    let meta = fs::symlink_metadata(&root)?;
    ensure!(
        meta.is_dir() && meta.uid() == uid() && meta.mode() & 0o077 == 0,
        "unsafe worker directory"
    );
    Ok(root)
}

/// Directory mtimes are not dependencies: listing the files detects additions,
/// while ignoring generated Steel stubs must not invalidate every startup.
fn stamp_tree(
    path: &Path,
    hash: &mut Sha256,
    visited: &mut BTreeSet<PathBuf>,
    remaining: &mut usize,
) -> Result<()> {
    ensure!(
        *remaining > 0,
        "too many worker dependencies; use HELIX_DIFF_WORKER=0"
    );
    *remaining -= 1;
    let link = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if link.file_type().is_symlink() {
        hash_path(hash, path);
        metadata(hash, &link);
        hash_path(hash, &fs::read_link(path)?);
    }
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if meta.is_dir() {
        let canonical = fs::canonicalize(path)?;
        if !visited.insert(canonical) {
            return Ok(());
        }
        let mut children = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        children.sort();
        for child in children {
            let name = child.file_name().unwrap();
            if [".git", ".jj", "target", "node_modules", ".cache"]
                .iter()
                .any(|skip| name == OsStr::new(skip))
            {
                continue;
            }
            if path.file_name() == Some(OsStr::new("grammars")) && name == "sources" {
                continue;
            }
            // These are generated from this executable, not imported by the VM.
            if path.file_name() == Some(OsStr::new("cogs")) && name == "helix" {
                continue;
            }
            stamp_tree(&child, hash, visited, remaining)?;
        }
    } else {
        hash_path(hash, path);
        metadata(hash, &link);
        metadata(hash, &meta);
        if link.file_type().is_symlink() {
            hash_path(hash, &fs::read_link(path)?);
        }
    }
    Ok(())
}

fn fingerprint() -> Result<String> {
    let mut hash = Sha256::new();
    let workspace = helix_loader::find_workspace().0;
    hash_path(&mut hash, &workspace);
    let mut roots = vec![
        helix_loader::config_dir(),
        helix_loader::config_file(),
        helix_loader::lang_config_file(),
        workspace.join(".helix"),
        helix_loader::data_dir().join("workspace_trust"),
    ];
    if let Some(path) = std::env::var_os("HELIX_STEEL_CONFIG") {
        roots.push(path.into());
    }
    #[cfg(feature = "steel")]
    if let Some(path) = steel::compiler::modules::steel_home() {
        roots.push(PathBuf::from(path).join("cogs"));
    }
    if let Some(paths) = std::env::var_os("HELIX_DIFF_WORKER_WATCH") {
        roots.extend(std::env::split_paths(&paths));
    }
    for runtime in helix_loader::runtime_dirs() {
        for child in [
            "languages.toml",
            "queries",
            "themes",
            "icons",
            "plugins",
            "grammars",
            "helix",
        ] {
            roots.push(runtime.join(child));
        }
    }
    let mut visited = BTreeSet::new();
    let mut remaining = 30_000;
    for root in roots {
        hash_path(&mut hash, &root);
        stamp_tree(&root, &mut hash, &mut visited, &mut remaining)?;
    }
    Ok(finish_hash(hash))
}

impl Scope {
    fn capture(width: u16) -> Result<Self> {
        let root = private_root()?;
        let mut hash = Sha256::new();
        hash.update(VERSION.to_le_bytes());
        hash_path(&mut hash, &std::env::current_dir()?);
        hash_path(&mut hash, &helix_loader::config_file());
        hash_path(&mut hash, &helix_loader::log_file());
        let exe = std::env::current_exe()?;
        hash_path(&mut hash, &exe);
        metadata(&mut hash, &fs::metadata(exe)?);
        let mut environment = std::env::vars_os().collect::<Vec<_>>();
        environment.sort();
        for (name, value) in environment {
            if [
                "_",
                "SHLVL",
                "OLDPWD",
                "HELIX_DIFF_WORKER",
                "HELIX_DIFF_WORKER_TRACE",
            ]
            .iter()
            .any(|skip| name == OsStr::new(skip))
            {
                continue;
            }
            hash_field(&mut hash, name.as_bytes());
            hash_field(&mut hash, value.as_bytes());
        }
        // This is a private filesystem name, not authentication. A 128-bit
        // digest keeps sockets within Darwin's short sockaddr_un path limit.
        let base = finish_hash(hash)[..32].to_owned();
        let slot = format!("{base}.{width}");
        ensure!(
            root.join(format!("{slot}.sock"))
                .as_os_str()
                .as_bytes()
                .len()
                < 104,
            "worker socket path is too long"
        );
        Ok(Self {
            root,
            base,
            slot,
            fingerprint: fingerprint()?,
        })
    }
    fn socket(&self) -> PathBuf {
        self.root.join(format!("{}.sock", self.slot))
    }
    fn lock(&self, kind: &str) -> Result<File> {
        let path = self.root.join(format!("{}.{}.lock", self.slot, kind));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let meta = file.metadata()?;
        ensure!(
            meta.is_file() && meta.uid() == uid() && meta.mode() & 0o077 == 0 && meta.nlink() == 1,
            "unsafe worker lock"
        );
        Ok(file)
    }
}

fn try_lock(file: &File) -> io::Result<bool> {
    // SAFETY: the file descriptor remains open through this call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error)
    }
}

fn connect(path: &Path) -> Result<UnixStream> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.file_type().is_socket() && meta.uid() == uid() && meta.mode() & 0o077 == 0,
        "unsafe worker socket"
    );
    let stream = UnixStream::connect(path)?;
    configure_peer(&stream, TIMEOUT)?;
    Ok(stream)
}

fn configure_peer(stream: &UnixStream, read_timeout: Duration) -> Result<()> {
    check_peer(stream)?;
    // Darwin/BSD accept() can inherit O_NONBLOCK from our listener. Framed
    // protocol reads/writes require blocking sockets with bounded timeouts;
    // otherwise an ordinary fragmented request is mistaken for a disconnect.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(read_timeout))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    Ok(())
}

fn check_peer(stream: &UnixStream) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let peer_uid = {
        let mut credentials = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut size = std::mem::size_of_val(&credentials) as libc::socklen_t;
        // SAFETY: credentials and size are valid writable getsockopt buffers.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut size,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error().into());
        }
        ensure!(
            size as usize == std::mem::size_of_val(&credentials),
            "invalid peer credentials"
        );
        credentials.uid
    };
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    let peer_uid = {
        let mut user = 0;
        let mut group = 0;
        // SAFETY: both IDs are valid writable buffers for this live socket.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut user, &mut group) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        user
    };
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    let peer_uid: u32 = { anyhow::bail!("peer credentials unavailable on this platform") };
    ensure!(
        peer_uid == uid(),
        "diff worker peer belongs to another user"
    );
    Ok(())
}

fn open_worker(scope: &Scope, width: u16) -> Result<UnixStream> {
    if let Ok(stream) = connect(&scope.socket()) {
        return Ok(stream);
    }
    let lock = scope.lock("start")?;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Ok(stream) = connect(&scope.socket()) {
            return Ok(stream);
        }
        if try_lock(&lock)? {
            break;
        }
        ensure!(Instant::now() < deadline, "timed out starting diff worker");
        std::thread::sleep(Duration::from_millis(10));
    }
    if let Ok(stream) = connect(&scope.socket()) {
        return Ok(stream);
    }
    // Bound background processes. Existing workers remain usable above the cap.
    let live = fs::read_dir(&scope.root)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension() == Some(OsStr::new("sock")))
        .filter(|entry| connect(&entry.path()).is_ok())
        .count();
    ensure!(live < 8, "diff worker limit reached");
    Command::new(std::env::current_exe()?)
        .arg("--internal-diff-worker")
        .arg("--config")
        .arg(helix_loader::config_file())
        .arg("--log")
        .arg(helix_loader::log_file())
        .arg("--width")
        .arg(width.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    loop {
        if let Ok(stream) = connect(&scope.socket()) {
            return Ok(stream);
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for diff worker"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn send_request(stream: &mut UnixStream, request: &Request) -> Result<()> {
    let bytes = serde_json::to_vec(request)?;
    ensure!(bytes.len() <= MAX_REQUEST, "diff worker request too large");
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}

fn read_request(stream: &mut UnixStream) -> Result<Request> {
    let mut size = [0; 4];
    stream.read_exact(&mut size)?;
    let size = u32::from_le_bytes(size) as usize;
    ensure!(size <= MAX_REQUEST, "diff worker request too large");
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn reply(stream: &mut UnixStream, status: u8, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_OUTPUT {
        return Err(io::Error::other("diff worker reply too large"));
    }
    stream.write_all(&[status])?;
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(bytes)
}

/// Return false only before writing any output, so fallback cannot duplicate a diff.
pub fn try_render(args: &Args, output: &mut impl Write, is_terminal: bool) -> Result<bool> {
    if args.patch
        || !args.render_diff
        || std::env::var_os("HELIX_DIFF_WORKER").is_some_and(|value| value == "0")
    {
        return Ok(false);
    }
    let paths: Vec<_> = args.files.keys().cloned().collect();
    let [left, right] = paths.as_slice() else {
        return Ok(false);
    };
    let width = args.diff_width.unwrap_or(100);
    for _ in 0..3 {
        let exchange = || -> Result<(u8, Vec<u8>)> {
            let scope = Scope::capture(width)?;
            let mut stream = open_worker(&scope, width)?;
            let request = Request::Render {
                version: VERSION,
                fingerprint: scope.fingerprint,
                paths: [left.clone(), right.clone()],
                width,
                context: args.diff_context.unwrap_or(3),
                inline: args.diff_layout == Some(Layout::Inline),
                color: match args.diff_color.unwrap_or_default() {
                    ColorMode::Always => true,
                    ColorMode::Never => false,
                    ColorMode::Auto => is_terminal,
                },
                theme: args.diff_theme.clone(),
            };
            send_request(&mut stream, &request)?;
            let mut status = [0];
            stream.read_exact(&mut status)?;
            let mut size = [0; 4];
            stream.read_exact(&mut size)?;
            let size = u32::from_le_bytes(size) as usize;
            ensure!(size <= MAX_OUTPUT, "diff worker response too large");
            let mut bytes = vec![0; size];
            stream.read_exact(&mut bytes)?;
            Ok((status[0], bytes))
        };
        match exchange() {
            Ok((OUTPUT, bytes)) => {
                output.write_all(&bytes)?;
                trace("retained renderer");
                return Ok(true);
            }
            Ok((ERROR, bytes)) => anyhow::bail!("{}", String::from_utf8_lossy(&bytes)),
            Ok((STALE, _)) => {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Ok(_) => break,
            Err(error) => {
                trace(format!("one-shot fallback: {error:#}"));
                break;
            }
        }
    }
    Ok(false)
}

struct SocketGuard {
    socket: PathBuf,
    pid: Option<PathBuf>,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
        if let Some(pid) = &self.pid {
            let _ = fs::remove_file(pid);
        }
    }
}

fn peer_open(stream: &UnixStream) -> io::Result<()> {
    let mut byte = [0u8];
    // SAFETY: the live descriptor and one-byte writable buffer are valid. PEEK
    // leaves protocol bytes untouched and DONTWAIT never blocks the renderer.
    let read = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            byte.as_mut_ptr().cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    if read == 0 {
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "diff client disconnected",
        ));
    }
    if read < 0 {
        let error = io::Error::last_os_error();
        if !matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        ) {
            return Err(error);
        }
    }
    Ok(())
}

struct OutputBuffer<'a> {
    stream: &'a UnixStream,
    bytes: Vec<u8>,
    unchecked: usize,
    overflow: bool,
}
impl Write for OutputBuffer<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.unchecked >= 16 * 1024 {
            self.flush()?;
        }
        if self.bytes.len().saturating_add(bytes.len()) > MAX_OUTPUT {
            self.overflow = true;
            return Err(io::Error::other("diff worker output limit"));
        }
        self.bytes.extend_from_slice(bytes);
        self.unchecked += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.unchecked = 0;
        peer_open(self.stream)
    }
}

fn ready(listener: &UnixListener, remaining: Duration) -> io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: descriptor points to one valid pollfd for the live listener.
    let result = unsafe {
        libc::poll(
            &mut descriptor,
            1,
            remaining.as_millis().clamp(1, i32::MAX as u128) as i32,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(result > 0)
}

pub fn serve(args: &Args) -> Result<()> {
    let scope_width = args.diff_width.unwrap_or(100);
    let scope = Scope::capture(scope_width)?;
    let lock = scope.lock("serve")?;
    if !try_lock(&lock)? {
        return Ok(());
    }
    let socket = scope.socket();
    if let Ok(meta) = fs::symlink_metadata(&socket) {
        ensure!(
            meta.file_type().is_socket() && meta.uid() == uid(),
            "refusing to replace non-worker socket"
        );
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    let pid_path = scope.root.join(format!("{}.pid", scope.slot));
    let mut socket_guard = SocketGuard {
        socket: socket.clone(),
        pid: None,
    };
    if let Ok(meta) = fs::symlink_metadata(&pid_path) {
        ensure!(
            meta.is_file() && meta.uid() == uid() && meta.nlink() == 1,
            "unsafe worker pid file"
        );
        fs::remove_file(&pid_path)?;
    }
    let mut pid_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&pid_path)?;
    socket_guard.pid = Some(pid_path);
    writeln!(pid_file, "{}", std::process::id())?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let idle = std::env::var("HELIX_DIFF_WORKER_IDLE_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(120)
        .clamp(1, 3600);
    let idle = Duration::from_secs(idle);
    let mut last = Instant::now();
    let mut session: Option<RenderSession> = None;
    loop {
        let remaining = idle.saturating_sub(last.elapsed());
        if remaining.is_zero() {
            return Ok(());
        }
        match ready(&listener, remaining) {
            Ok(false) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
            Ok(true) => (),
        }
        let (mut stream, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error.into()),
        };
        if configure_peer(&stream, Duration::from_secs(5)).is_err() {
            continue;
        }
        let request = match read_request(&mut stream) {
            Ok(request) => request,
            Err(_) => continue,
        };
        if matches!(request, Request::Stop) {
            let _ = reply(&mut stream, OUTPUT, &[]);
            return Ok(());
        }
        let Request::Render {
            version,
            fingerprint: requested,
            paths,
            width,
            context,
            inline,
            color,
            theme,
        } = request
        else {
            unreachable!()
        };
        if version != VERSION
            || width != scope_width
            || !(20..=4096).contains(&width)
            || context > 1000
            || !paths.iter().all(|path| path.is_absolute())
            || paths[0] == paths[1]
        {
            let _ = reply(&mut stream, ERROR, b"invalid diff worker request");
            continue;
        }
        if peer_open(&stream).is_err() {
            continue;
        }
        let current = match fingerprint() {
            Ok(value) => value,
            Err(_) => {
                let _ = reply(&mut stream, STALE, &[]);
                return Ok(());
            }
        };
        if current != scope.fingerprint {
            let _ = reply(&mut stream, STALE, &[]);
            return Ok(());
        }
        if requested != current {
            let _ = reply(&mut stream, STALE, &[]);
            continue;
        }
        let mut request_args = Args {
            render_diff: true,
            diff_width: Some(width),
            diff_context: Some(context),
            diff_layout: Some(if inline {
                Layout::Inline
            } else {
                Layout::SideBySide
            }),
            diff_color: Some(if color {
                ColorMode::Always
            } else {
                ColorMode::Never
            }),
            diff_theme: theme,
            ..Args::default()
        };
        for path in paths {
            request_args.files.insert(path, Vec::new());
        }
        if session.is_none() {
            match RenderSession::new(&request_args) {
                Ok(value) => session = Some(value),
                Err(error) => {
                    let message = format!("{error:#}");
                    let _ = reply(&mut stream, ERROR, message.as_bytes());
                    return Ok(());
                }
            }
            // Initialization can generate module stubs or race a config edit.
            if fingerprint()? != scope.fingerprint {
                let _ = reply(&mut stream, STALE, &[]);
                return Ok(());
            }
        }
        let mut output = OutputBuffer {
            stream: &stream,
            bytes: Vec::new(),
            unchecked: 16 * 1024,
            overflow: false,
        };
        let result = session
            .as_mut()
            .unwrap()
            .render(&request_args, &mut output, false);
        let overflow = output.overflow;
        let bytes = output.bytes;
        if peer_open(&stream).is_ok() {
            match result {
                Ok(()) => {
                    let _ = reply(&mut stream, OUTPUT, &bytes);
                }
                Err(_) if overflow => {
                    let _ = reply(&mut stream, FALLBACK, &[]);
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    let _ = reply(&mut stream, ERROR, message.as_bytes());
                }
            }
        }
        last = Instant::now();
    }
}

pub fn stop() -> Result<()> {
    let scope = Scope::capture(100)?;
    for entry in fs::read_dir(&scope.root)? {
        let path = entry?.path();
        if path.extension() != Some(OsStr::new("sock"))
            || !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("{}.", scope.base))
        {
            continue;
        }
        if let Ok(mut stream) = connect(&path) {
            send_request(&mut stream, &Request::Stop)?;
            let mut header = [0; 5];
            stream.read_exact(&mut header)?;
            ensure!(
                header == [OUTPUT, 0, 0, 0, 0],
                "invalid worker stop response"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_nonblocking_mode_is_cleared_before_fragmented_protocol_reads() -> Result<()> {
        let (mut client, mut server) = UnixStream::pair()?;
        // Emulate Darwin's inherited flag even when the test runs on Linux.
        server.set_nonblocking(true)?;
        configure_peer(&server, Duration::from_secs(2))?;
        let writer = std::thread::spawn(move || -> Result<()> {
            let request = serde_json::to_vec(&Request::Stop)?;
            let size = (request.len() as u32).to_le_bytes();
            std::thread::sleep(Duration::from_millis(30));
            client.write_all(&size[..1])?;
            std::thread::sleep(Duration::from_millis(30));
            client.write_all(&size[1..])?;
            for byte in request {
                client.write_all(&[byte])?;
            }
            Ok(())
        });
        assert!(matches!(read_request(&mut server)?, Request::Stop));
        writer.join().unwrap()?;
        Ok(())
    }

    #[test]
    fn dependency_stamp_changes_on_edit_add_remove_and_symlink_target() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source.scm");
        fs::write(&source, "first")?;
        let stamp = || -> Result<Vec<u8>> {
            let mut hash = Sha256::new();
            stamp_tree(root.path(), &mut hash, &mut BTreeSet::new(), &mut 100)?;
            Ok(hash.finalize().to_vec())
        };
        let initial = stamp()?;
        fs::write(&source, "changed")?;
        assert_ne!(initial, stamp()?);
        let edited = stamp()?;
        fs::write(root.path().join("new.scm"), "new")?;
        assert_ne!(edited, stamp()?);
        fs::remove_file(root.path().join("new.scm"))?;
        let target = tempfile::tempdir()?;
        fs::write(target.path().join("external.scm"), "outside")?;
        std::os::unix::fs::symlink(target.path(), root.path().join("linked"))?;
        let linked = stamp()?;
        fs::write(target.path().join("external.scm"), "new outside")?;
        assert_ne!(linked, stamp()?);
        Ok(())
    }

    #[test]
    fn generated_stubs_do_not_invalidate_but_user_modules_do() -> Result<()> {
        let root = tempfile::tempdir()?;
        let cogs = root.path().join("cogs");
        fs::create_dir_all(cogs.join("helix"))?;
        let stamp = || -> Result<Vec<u8>> {
            let mut hash = Sha256::new();
            stamp_tree(&cogs, &mut hash, &mut BTreeSet::new(), &mut 100)?;
            Ok(hash.finalize().to_vec())
        };
        let initial = stamp()?;
        fs::write(cogs.join("helix/configuration.scm"), "generated")?;
        assert_eq!(initial, stamp()?);
        fs::write(cogs.join("plugin.scm"), "user")?;
        assert_ne!(initial, stamp()?);
        Ok(())
    }

    #[test]
    fn protocol_limits_and_disconnected_writer() -> Result<()> {
        let (mut client, mut server) = UnixStream::pair()?;
        check_peer(&client)?;
        check_peer(&server)?;
        client.write_all(&((MAX_REQUEST + 1) as u32).to_le_bytes())?;
        assert!(read_request(&mut server).is_err());
        drop(client);
        let mut writer = OutputBuffer {
            stream: &server,
            bytes: Vec::new(),
            unchecked: 0,
            overflow: false,
        };
        assert!(writer.flush().is_err());
        Ok(())
    }
}
