//! Opt-in, same-user query-analysis cache. No parsed trees or source text.
use crate::Grammar;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

const LIMIT: usize = 1024 * 1024;
#[derive(Clone)]
struct Cache {
    root: PathBuf,
    namespace: String,
    owner: u32,
    grammars: HashMap<Grammar, String>,
    compiled: usize,
    hits: usize,
}
thread_local! { static CACHE: RefCell<Option<Cache>> = const { RefCell::new(None) }; }

/// Validated cache identity and grammar registrations inherited by a new thread.
/// Counters are thread-local and start at zero after installation.
#[derive(Clone)]
pub struct ThreadContext(Option<Cache>);

pub fn thread_context() -> ThreadContext {
    ThreadContext(CACHE.with(|cache| cache.borrow().clone()))
}

impl ThreadContext {
    pub fn install(mut self) {
        if let Some(cache) = &mut self.0 {
            cache.compiled = 0;
            cache.hits = 0;
        }
        CACHE.with(|cache| *cache.borrow_mut() = self.0);
    }
}

/// Account for completed work without importing another thread's mutable state.
pub fn add_statistics((compiled, hits): (usize, usize)) {
    CACHE.with(|cache| {
        if let Some(cache) = cache.borrow_mut().as_mut() {
            cache.compiled += compiled;
            cache.hits += hits;
        }
    });
}

/// Enable a cache on this thread. The namespace must bind the executable,
/// target, grammars and queries; changing it discards all grammar registrations.
pub fn configure(root: Option<(&Path, &str)>) -> std::io::Result<()> {
    CACHE.with(|cache| *cache.borrow_mut() = None);
    let Some((root, namespace)) = root else {
        return Ok(());
    };
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)?;
    let meta = fs::symlink_metadata(root)?;
    // SAFETY: geteuid has no preconditions or pointer arguments.
    let owner = unsafe { libc::geteuid() };
    if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() != owner {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unsafe query cache directory",
        ));
    }
    CACHE.with(|cache| {
        *cache.borrow_mut() = Some(Cache {
            root: root.to_owned(),
            namespace: namespace.to_owned(),
            owner,
            grammars: HashMap::new(),
            compiled: 0,
            hits: 0,
        })
    });
    Ok(())
}

pub(crate) fn register_grammar(grammar: Grammar, name: &str, path: &Path) {
    CACHE.with(|cache| {
        if let Some(cache) = cache.borrow_mut().as_mut() {
            cache
                .grammars
                .insert(grammar, format!("{name}\0{}", path.display()));
        }
    });
}

fn location(grammar: Grammar, source: &str) -> Option<(PathBuf, u32)> {
    CACHE.with(|cache| {
        let state = cache.borrow();
        let cache = state.as_ref()?;
        let grammar = cache.grammars.get(&grammar)?;
        let mut hash = Sha256::new();
        for bytes in [
            cache.namespace.as_bytes(),
            grammar.as_bytes(),
            source.as_bytes(),
        ] {
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        let key: String = hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Some((cache.root.join(format!("{key}.analysis")), cache.owner))
    })
}

pub(crate) fn read(grammar: Grammar, source: &str) -> Option<Vec<u8>> {
    let (path, owner) = location(grammar, source)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file()
        || meta.uid() != owner
        || meta.nlink() != 1
        || meta.mode() & 0o077 != 0
        || meta.len() > (LIMIT + 32) as u64
    {
        return None;
    }
    let mut data = Vec::new();
    file.take((LIMIT + 33) as u64).read_to_end(&mut data).ok()?;
    if data.len() < 32 || data.len() > LIMIT + 32 {
        return None;
    }
    let (checksum, payload) = data.split_at(32);
    if Sha256::digest(payload).as_slice() != checksum {
        return None;
    }
    Some(payload.to_owned())
}

pub fn statistics() -> (usize, usize) {
    CACHE.with(|cache| {
        cache
            .borrow()
            .as_ref()
            .map_or((0, 0), |cache| (cache.compiled, cache.hits))
    })
}

pub(crate) fn record(hit: bool) {
    CACHE.with(|cache| {
        if let Some(cache) = cache.borrow_mut().as_mut() {
            cache.compiled += 1;
            cache.hits += usize::from(hit);
        }
    });
}

pub(crate) fn store(grammar: Grammar, source: &str, payload: &[u8]) {
    let _ = store_inner(grammar, source, payload);
}

fn store_inner(grammar: Grammar, source: &str, payload: &[u8]) -> std::io::Result<()> {
    if payload.len() > LIMIT {
        return Ok(());
    }
    let Some((path, owner)) = location(grammar, source) else {
        return Ok(());
    };
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if !meta.is_file() || meta.uid() != owner || meta.nlink() != 1 || meta.mode() & 0o077 != 0 {
            return Ok(());
        }
    }
    let root = path.parent().unwrap();
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(&Sha256::digest(payload))?;
    file.write_all(payload)?;
    file.persist(&path)?;
    // Bound entries and bytes through a fixed entry cap and payload limit.
    let mut files = fs::read_dir(root)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).ok()?;
            (meta.is_file()
                && meta.uid() == owner
                && meta.mode() & 0o077 == 0
                && meta.nlink() == 1
                && path.extension().is_some_and(|ext| ext == "analysis"))
            .then(|| (meta.modified().ok(), path))
        })
        .collect::<Vec<_>>();
    files.sort();
    let remove = files.len().saturating_sub(256);
    for (_, path) in files.into_iter().take(remove) {
        fs::remove_file(path)?;
    }
    Ok(())
}
