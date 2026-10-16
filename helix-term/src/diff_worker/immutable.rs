//! Process-local summaries for protected, immutable Nix-store directories.
//! Symlinks are validated every request, including chains leaving and reentering
//! the store. Their final canonical target alone does not prove immutability.
use super::*;
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

#[derive(Clone)]
pub(super) struct Snapshot {
    pub digest: [u8; 32],
    pub directories: BTreeSet<(u64, u64)>,
    pub outside: Vec<PathBuf>,
    pub nodes: usize,
}

type Key = (PathBuf, u64, u64, i64, i64);
static CACHE: OnceLock<Mutex<HashMap<Key, Snapshot>>> = OnceLock::new();

pub(super) fn get(
    path: &Path,
    meta: &fs::Metadata,
    budget: usize,
    follow_link: bool,
) -> Result<Option<Snapshot>> {
    if uid() == 0
        || std::env::var_os("HELIX_DIFF_IMMUTABLE_CACHE").is_some_and(|value| value == "0")
    {
        return Ok(None);
    }
    let prefix = Path::new("/nix/store");
    if !follow_link && !path.starts_with(prefix) {
        return Ok(None);
    }
    let path = fs::canonicalize(path)?;
    if !path.starts_with(prefix) {
        return Ok(None);
    }
    let store = fs::metadata(prefix)?;
    if store.uid() != 0 || store.mode() & 0o022 != 0 {
        return Ok(None);
    }
    let key = (
        path.clone(),
        meta.dev(),
        meta.ino(),
        meta.ctime(),
        meta.ctime_nsec(),
    );
    let mut cache = CACHE.get_or_init(Mutex::default).lock().unwrap();
    if let Some(snapshot) = cache.get(&key) {
        return Ok(Some(snapshot.clone()));
    }
    let Some(snapshot) = compute(&path, uid(), budget)? else {
        return Ok(None);
    };
    if cache.len() >= 128 {
        cache.clear();
    }
    cache.insert(key, snapshot.clone());
    Ok(Some(snapshot))
}

fn compute(root: &Path, caller: u32, budget: usize) -> Result<Option<Snapshot>> {
    fn walk(
        path: &Path,
        caller: u32,
        budget: usize,
        hash: &mut Sha256,
        snapshot: &mut Snapshot,
    ) -> Result<bool> {
        ensure!(
            snapshot.nodes < budget,
            "too many immutable worker dependencies"
        );
        snapshot.nodes += 1;
        let meta = fs::symlink_metadata(path)?;
        hash_path(hash, path);
        metadata(hash, &meta);
        if meta.file_type().is_symlink() {
            hash_path(hash, &fs::read_link(path)?);
            // Defer even links whose final target is inside the store: an
            // intermediate out-of-store link could be retargeted later.
            snapshot.outside.push(path.to_owned());
            return Ok(true);
        }
        // Never assume user-owned or group/world-writable trees are immutable,
        // even if they happen to live underneath a directory named /nix/store.
        if meta.uid() == caller || meta.mode() & 0o022 != 0 {
            return Ok(false);
        }
        if meta.is_dir() {
            if !snapshot.directories.insert((meta.dev(), meta.ino())) {
                return Ok(true);
            }
            let mut children = fs::read_dir(path)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<io::Result<Vec<_>>>()?;
            children.sort();
            for child in children {
                if super::skip_child(path, child.file_name().unwrap()) {
                    continue;
                }
                if !walk(&child, caller, budget, hash, snapshot)? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
    let mut snapshot = Snapshot {
        digest: [0; 32],
        directories: BTreeSet::new(),
        outside: Vec::new(),
        nodes: 0,
    };
    let mut hash = Sha256::new();
    if !walk(root, caller, budget, &mut hash, &mut snapshot)? {
        return Ok(None);
    }
    snapshot.digest = hash.finalize().into();
    Ok(Some(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn out_of_store_links_remain_live_dependencies_including_retargeting() -> Result<()> {
        let store = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        fs::write(store.path().join("fixed.scm"), "immutable")?;
        fs::write(outside.path().join("one.scm"), "one")?;
        fs::write(outside.path().join("two.scm"), "two")?;
        let alias = outside.path().join("current");
        symlink(store.path().join("fixed.scm"), &alias)?;
        symlink(&alias, store.path().join("live.scm"))?;
        // The private computation helper models a protected store owned by a
        // different uid; production eligibility always checks the actual store.
        let snapshot = compute(store.path(), uid().wrapping_add(1), 100)?.unwrap();
        assert_eq!(snapshot.outside, [store.path().join("live.scm")]);
        let fingerprint = || -> Result<String> {
            let mut hash = Sha256::new();
            hash_field(&mut hash, &snapshot.digest);
            let mut visited = snapshot.directories.clone();
            let mut remaining = 100;
            for path in &snapshot.outside {
                stamp_tree(path, &mut hash, &mut visited, &mut remaining)?;
            }
            Ok(finish_hash(hash))
        };
        let initial = fingerprint()?;
        fs::remove_file(&alias)?;
        symlink(outside.path().join("one.scm"), &alias)?;
        assert_ne!(fingerprint()?, initial);
        fs::write(outside.path().join("one.scm"), "changed in place")?;
        let changed = fingerprint()?;
        assert_ne!(changed, initial);
        fs::remove_file(&alias)?;
        symlink(outside.path().join("two.scm"), &alias)?;
        assert_ne!(fingerprint()?, changed);
        assert!(compute(store.path(), uid(), 100)?.is_none());
        Ok(())
    }
}
