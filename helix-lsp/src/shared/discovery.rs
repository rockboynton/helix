use super::*;
use std::{
    fs::{self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, OpenOptionsExt},
    },
    path::Path,
    time::Duration,
};
use tokio::net::UnixStream;

pub(super) fn uid() -> u32 {
    // SAFETY: geteuid takes no arguments.
    unsafe { libc::geteuid() }
}

pub(super) fn root() -> Result<PathBuf> {
    let root = std::env::var_os("HELIX_SHARED_LSP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/neohelix-lsp-{}", uid())));
    ensure!(root.is_absolute(), "LSP socket directory must be absolute");
    match fs::DirBuilder::new().mode(0o700).create(&root) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    let meta = fs::symlink_metadata(&root)?;
    ensure!(
        meta.is_dir() && meta.uid() == uid() && meta.mode() & 0o077 == 0,
        "unsafe LSP socket directory"
    );
    Ok(root)
}

pub(super) fn lock(socket: &Path, kind: &str) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(socket.with_extension(kind))?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.uid() == uid() && meta.nlink() == 1 && meta.mode() & 0o077 == 0,
        "unsafe LSP lock file"
    );
    Ok(file)
}

pub(super) fn try_lock(file: &File) -> Result<bool> {
    // SAFETY: flock operates on the live File descriptor.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error.into())
    }
}

pub(super) fn authenticate(stream: &UnixStream) -> Result<()> {
    ensure!(
        stream.peer_cred()?.uid() == uid(),
        "LSP socket peer belongs to another user"
    );
    Ok(())
}

pub(super) fn validate_socket(socket: &Path) -> Result<()> {
    let parent = socket.parent().context("socket lacks parent")?;
    ensure!(parent == root()?, "socket outside private directory");
    let stem = socket
        .file_stem()
        .and_then(|s| s.to_str())
        .context("invalid socket name")?;
    ensure!(
        stem.len() == 32
            && stem.bytes().all(|c| c.is_ascii_hexdigit())
            && socket.extension() == Some(OsStr::new("sock")),
        "invalid socket name"
    );
    ensure!(
        socket.as_os_str().len() < 104,
        "LSP socket path too long for macOS"
    );
    Ok(())
}

pub(super) async fn connect(key: &str) -> Result<UnixStream> {
    let socket = root()?.join(format!("{key}.sock"));
    validate_socket(&socket)?;
    let start_lock = lock(&socket, "start")?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(stream) = UnixStream::connect(&socket).await {
            authenticate(&stream)?;
            return Ok(stream);
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "LSP broker startup timed out"
        );
        if try_lock(&start_lock)? {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    if let Ok(stream) = UnixStream::connect(&socket).await {
        authenticate(&stream)?;
        return Ok(stream);
    }
    // Serialize admission across different compatibility keys as well. A lock
    // per socket alone lets simultaneous, incompatible startups exceed the cap.
    let admission = lock(&socket.parent().unwrap().join("brokers.sock"), "admission")?;
    while !try_lock(&admission)? {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "LSP broker admission timed out"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Bound live daemon count; stale socket names are not counted.
    let mut live = 0;
    for entry in fs::read_dir(socket.parent().unwrap())? {
        let path = entry?.path();
        if path.extension() == Some(OsStr::new("sock")) && UnixStream::connect(&path).await.is_ok()
        {
            live += 1;
        }
    }
    ensure!(live < 8, "LSP broker limit reached");
    tokio::process::Command::new(std::env::current_exe()?)
        .arg("--internal-lsp-broker")
        .env(SOCKET_ENV, &socket)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()?;
    loop {
        if let Ok(stream) = UnixStream::connect(&socket).await {
            authenticate(&stream)?;
            return Ok(stream);
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "LSP broker startup timed out"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
