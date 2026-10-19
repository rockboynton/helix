use std::borrow::Borrow;
use std::mem::replace;
use std::path::{Path, PathBuf};
use std::slice;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

// Re-export filesentry types (available on all platforms)
pub use filesentry::{CanonicalPathBuf, Event, EventType, Events, Filter, ShutdownOnDrop};

use helix_event::{dispatch, events};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::{Deserialize, Serialize};

events! {
    FileSystemDidChange {
        fs_events: Events
    }
}

/// Create an Events collection from an iterator of paths.
/// All paths will have EventType::Modified.
pub fn events_from_paths(paths: impl IntoIterator<Item = PathBuf>) -> Events {
    use filesentry::CanonicalPathBuf;
    let events: Vec<Event> = paths
        .into_iter()
        .map(|path| {
            // Deleted paths cannot be canonicalized through the filesystem.
            let canonical = path
                .canonicalize()
                .unwrap_or_else(|_| helix_stdx::path::canonicalize(path));
            Event {
                path: CanonicalPathBuf::assert_canonicalized(&canonical),
                ty: EventType::Modified,
            }
        })
        .collect();
    Events::from(events)
}

/// Config for file watching
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct Config {
    /// Enable file watching enable by default
    pub enable: bool,
    pub watch_vcs: bool,
    /// Only enable the file watcher inside helix workspaces (VCS repos and directories with .helix
    /// directory) this prevents watching large directories like $HOME by default
    ///
    /// Defaults to `true`
    pub require_workspace: bool,
    /// Enables ignoring hidden files.
    /// Whether to hide hidden files in file picker and global search results. Defaults to true.
    pub hidden: bool,
    /// Enables reading `.ignore` files.
    /// Whether to hide files listed in .ignore in file picker and global search results. Defaults to true.
    pub ignore: bool,
    /// Enables reading `.gitignore` files.
    /// Whether to hide files listed in .gitignore in file picker and global search results. Defaults to true.
    pub git_ignore: bool,
    /// Enables reading global .gitignore, whose path is specified in git's config: `core.excludefile` option.
    /// Whether to hide files listed in global .gitignore in file picker and global search results. Defaults to true.
    pub git_global: bool,
    // /// Enables reading `.git/info/exclude` files.
    // /// Whether to hide files listed in .git/info/exclude in file picker and global search results. Defaults to true.
    // pub git_exclude: bool,
    /// Maximum Depth to recurse for filewatching
    pub max_depth: Option<usize>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enable: true,
            watch_vcs: true,
            require_workspace: true,
            hidden: true,
            ignore: true,
            git_ignore: true,
            git_global: true,
            max_depth: Some(10),
        }
    }
}

pub struct Watcher {
    watcher: Option<(filesentry::Watcher, ShutdownOnDrop)>,
    filter: Arc<WatchFilter>,
    roots: Vec<(PathBuf, usize)>,
    review_roots: Vec<PathBuf>,
    /// Narrow metadata directories outside the workspace (linked VCS repos).
    extra_roots: Vec<PathBuf>,
    active_roots: Arc<Mutex<Vec<PathBuf>>>,
    config: Config,
    /// Extra paths that need polling (e.g., VCS HEAD files outside workspace)
    /// Stored with their last known mtime for change detection
    extra_watched_paths: Vec<(PathBuf, Option<SystemTime>)>,
}

impl Watcher {
    pub fn new(config: &Config) -> Watcher {
        let mut watcher = Watcher {
            watcher: None,
            filter: Arc::new(WatchFilter {
                filesentry_ignores: Gitignore::empty(),
                ignore_files: Vec::new(),
                global_ignores: Vec::new(),
                hidden: true,
                watch_vcs: true,
                max_depth: config.max_depth,
            }),
            roots: Vec::new(),
            review_roots: Vec::new(),
            extra_roots: Vec::new(),
            active_roots: Arc::default(),
            config: config.clone(),
            extra_watched_paths: Vec::new(),
        };
        watcher.reload(config);
        watcher
    }

    pub fn reload(&mut self, config: &Config) {
        let old_config = replace(&mut self.config, config.clone());
        let (workspace, no_workspace) = helix_loader::find_workspace();

        if !config.enable
            || config.require_workspace
                && no_workspace
                && self.review_roots.is_empty()
                && self.extra_roots.is_empty()
        {
            self.watcher = None;
            self.active_roots = Arc::default();
            return;
        }
        self.filter = Arc::new(WatchFilter::new(
            config,
            &workspace,
            self.roots
                .iter()
                .map(|(it, _)| &**it)
                .chain(self.review_roots.iter().map(|path| &**path))
                .chain(self.extra_roots.iter().map(|path| &**path)),
        ));
        let watcher = match &mut self.watcher {
            Some((watcher, _)) => {
                // TODO: more fine grained detection of when recrawl is nedded
                watcher.set_filter(self.filter.clone(), old_config != self.config);
                watcher
            }
            None => match filesentry::Watcher::new() {
                Ok(watcher) => {
                    self.active_roots = Arc::default();
                    watcher.set_filter(self.filter.clone(), false);
                    let runtime = tokio::runtime::Handle::try_current().ok();
                    watcher.add_handler(move |events| {
                        // Filesentry delivers on a native thread. Integration
                        // event registries are local to the owning Tokio runtime.
                        let _guard = runtime.as_ref().map(|runtime| runtime.enter());
                        dispatch(FileSystemDidChange { fs_events: events });
                        true
                    });
                    let shutdown_guard = watcher.shutdown_guard();
                    &mut self.watcher.insert((watcher, shutdown_guard)).0
                }
                Err(err) => {
                    log::info!("file-watcher not available: {err}");
                    return;
                }
            },
        };
        let active = self.active_roots.clone();
        let root = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.clone());
        if !no_workspace || !config.require_workspace {
            if let Err(err) = watcher.add_root(&workspace, true, move |success| {
                if success {
                    let mut active = active.lock().unwrap();
                    if !active.contains(&root) {
                        active.push(root);
                    }
                }
            }) {
                log::error!("failed to start file-watcher: {err}");
            }
        }
        for root in self
            .roots
            .iter()
            .map(|(root, _)| root)
            .chain(&self.review_roots)
            .chain(self.extra_roots.iter().filter(|extra| {
                !self.roots.iter().any(|(root, _)| extra.starts_with(root))
                    && !self.review_roots.iter().any(|root| extra.starts_with(root))
            }))
        {
            let active = self.active_roots.clone();
            let path = root.clone();
            if let Err(err) = watcher.add_root(root, true, move |success| {
                if success {
                    let mut active = active.lock().unwrap();
                    if !active.contains(&path) {
                        active.push(path);
                    }
                }
            }) {
                log::error!("failed to start file-watcher: {err}");
            }
        }
        watcher.start();
    }

    /// Explicit comparisons can watch their selected directories even outside a
    /// workspace. Never broaden coverage to the caller's home/current directory.
    pub fn set_review_roots(&mut self, roots: Vec<PathBuf>) {
        let mut roots = roots
            .into_iter()
            .filter_map(|path| path.canonicalize().ok())
            .collect::<Vec<_>>();
        roots.sort();
        roots.dedup();
        if roots == self.review_roots {
            return;
        }
        self.review_roots = roots;
        self.watcher = None;
        self.reload(&self.config.clone());
    }

    pub fn remove_root(&mut self, root: PathBuf) {
        let i = self.roots.partition_point(|(it, _)| it < &root);
        if self.roots.get(i).is_none_or(|(it, _)| it != &root) {
            log::error!("tried to remove root {root:?} from watch list that does not exist!");
            return;
        }
        if self.roots[i].1 <= 1 {
            self.roots.remove(i);
        } else {
            self.roots[i].1 -= 1;
        }
    }

    /// Returns true if the file watcher is active.
    pub fn is_active(&self) -> bool {
        self.watcher.is_some()
    }

    /// Check if a given path is being actively watched.
    /// Returns true if the path is under a watched root and not filtered out.
    pub fn is_watching(&self, path: &Path) -> bool {
        if self.watcher.is_none() {
            return false;
        }
        let path = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => return false,
        };
        // Do not claim coverage until the native backend has installed watches
        // and completed its initial crawl successfully.
        self.active_roots
            .lock()
            .unwrap()
            .iter()
            .any(|root| path.starts_with(root))
            && !self.filter.ignore_path_rec(&path, Some(false))
    }

    /// Poll extra watched paths for changes and return paths that have changed.
    /// Updates internal mtime tracking for the changed paths.
    pub fn poll_extra_paths(&mut self) -> Vec<PathBuf> {
        if !self.config.enable || !self.config.watch_vcs {
            return Vec::new();
        }
        let mut changed = Vec::new();
        for (path, last_mtime) in &mut self.extra_watched_paths {
            let current_mtime = path.metadata().ok().and_then(|m| m.modified().ok());
            if current_mtime != *last_mtime {
                changed.push(path.clone());
                *last_mtime = current_mtime;
            }
        }
        changed
    }

    /// Returns true if there are extra paths that need polling.
    pub fn has_extra_watched_paths(&self) -> bool {
        !self.extra_watched_paths.is_empty()
    }

    /// Watch external VCS metadata natively, with polling as a backstop.
    /// Only metadata outside the workspace needs additional native roots.
    pub fn set_extra_watched_paths(&mut self, paths: Vec<PathBuf>) {
        let (workspace, _) = helix_loader::find_workspace();
        let mut paths = paths
            .into_iter()
            .filter(|path| self.config.watch_vcs && !path.starts_with(&workspace))
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        if paths
            .iter()
            .eq(self.extra_watched_paths.iter().map(|(path, _)| path))
        {
            return;
        }
        // Watching the parent handles atomic file replacement, and watching
        // op_heads/heads catches both head creation and deletion. Never crawl
        // the shared repository's immutable object store.
        let mut roots = paths
            .iter()
            .filter_map(|path| {
                let root = if path.is_dir() {
                    path.as_path()
                } else {
                    path.parent()?
                };
                root.canonicalize().ok()
            })
            .collect::<Vec<_>>();
        roots.sort();
        roots.dedup();
        self.extra_watched_paths = paths
            .into_iter()
            .map(|path| {
                let mtime = path.metadata().ok().and_then(|m| m.modified().ok());
                (path, mtime)
            })
            .collect();
        if roots != self.extra_roots {
            self.extra_roots = roots;
            self.watcher = None;
            self.reload(&self.config.clone());
        }
        if !self.extra_watched_paths.is_empty() {
            log::info!(
                "watching {} extra VCS metadata paths (with polling backstop): {:?}",
                self.extra_watched_paths.len(),
                self.extra_watched_paths
                    .iter()
                    .map(|(p, _)| p)
                    .collect::<Vec<_>>()
            );
        }
    }

    pub fn add_root(&mut self, root: &Path) {
        let root = match root.canonicalize() {
            Ok(root) => root,
            Err(err) => {
                log::error!("failed to watch {root:?}: {err}");
                return;
            }
        };
        let i = self.roots.partition_point(|(it, _)| it < &root);
        if let Some((_, refcnt)) = self.roots.get_mut(i).filter(|(path, _)| path == &root) {
            *refcnt += 1;
            return;
        }
        if self.roots[..i]
            .iter()
            .rev()
            .find(|(it, _)| it.parent().is_none_or(|it| root.starts_with(it)))
            .is_some_and(|(it, _)| root.starts_with(it))
            && !self.filter.ignore_path_rec(&root, Some(true))
        {
            return;
        }
        let (workspace, _) = helix_loader::find_workspace();
        if root.starts_with(&workspace) {
            return;
        }
        self.roots.insert(i, (root.clone(), 1));
        if self
            .extra_roots
            .iter()
            .any(|extra| extra.starts_with(&root))
        {
            // Replace child metadata watches with the new enclosing root.
            // Filesentry cannot recursively add a parent of an existing root.
            self.watcher = None;
            self.reload(&self.config.clone());
            return;
        }
        self.filter = Arc::new(WatchFilter::new(
            &self.config,
            &workspace,
            self.roots
                .iter()
                .map(|(it, _)| &**it)
                .chain(self.review_roots.iter().map(|path| &**path))
                .chain(self.extra_roots.iter().map(|path| &**path)),
        ));
        if let Some((watcher, _)) = &self.watcher {
            watcher.set_filter(self.filter.clone(), false);
            let active = self.active_roots.clone();
            let path = root.clone();
            if let Err(err) = watcher.add_root(&root, true, move |success| {
                if success {
                    let mut active = active.lock().unwrap();
                    if !active.contains(&path) {
                        active.push(path);
                    }
                }
            }) {
                log::error!("failed to watch {root:?}: {err}");
            }
        }
    }
}

fn build_ignore(paths: impl IntoIterator<Item = PathBuf> + Clone, dir: &Path) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(dir);
    for path in paths.clone() {
        if let Some(err) = builder.add(&path) {
            if !err.is_io() {
                log::error!("failed to read ignorefile at {path:?}: {err}");
            }
        }
    }
    match builder.build() {
        Ok(ignore) => (!ignore.is_empty()).then_some(ignore),
        Err(err) => {
            if !err.is_io() {
                log::error!(
                    "failed to read ignorefile at {:?}: {err}",
                    paths.into_iter().collect::<Vec<_>>()
                );
            }
            None
        }
    }
}

struct IgnoreFiles {
    root: PathBuf,
    ignores: Vec<Arc<Gitignore>>,
}

impl IgnoreFiles {
    fn new(
        workspace_ignore: Option<Arc<Gitignore>>,
        config: &Config,
        root: &Path,
        globals: &[Arc<Gitignore>],
    ) -> Self {
        let mut ignores = Vec::with_capacity(8);
        // .helix/ignore
        if let Some(workspace_ignore) = workspace_ignore {
            ignores.push(workspace_ignore);
        }
        for ancestor in root.ancestors() {
            let ignore = if config.ignore {
                if config.git_ignore {
                    // the second path takes priority
                    build_ignore(
                        [ancestor.join(".gitignore"), ancestor.join(".ignore")],
                        ancestor,
                    )
                } else {
                    build_ignore([ancestor.join(".ignore")], ancestor)
                }
            } else if config.git_ignore {
                build_ignore([ancestor.join(".gitignore")], ancestor)
            } else {
                None
            };
            if let Some(ignore) = ignore {
                ignores.push(Arc::new(ignore));
            }
        }
        ignores.extend(globals.iter().cloned());
        Self {
            root: root.into(),
            ignores,
        }
    }

    fn shared_ignores(
        workspace: &Path,
        config: &Config,
    ) -> (Vec<Arc<Gitignore>>, Option<Arc<Gitignore>>) {
        let mut ignores = Vec::new();
        let workspace_ignore = build_ignore(
            [
                helix_loader::config_dir().join("ignore"),
                workspace.join(".helix/ignore"),
            ],
            workspace,
        )
        .map(Arc::new);
        if config.git_global {
            let (gitignore_global, err) = Gitignore::global();
            if let Some(err) = err {
                if !err.is_io() {
                    log::error!("failed to read global global ignorefile: {err}");
                }
            }
            if !gitignore_global.is_empty() {
                ignores.push(Arc::new(gitignore_global));
            }
        }
        // if config.git_exclude {
        // TODO git_exclude implementation, this isn't quite trivial unfortunaetly
        // due to detached workspace etc.
        // }
        // TODO: git exclude
        (ignores, workspace_ignore)
    }

    fn filesentry_ignores(workspace: &Path) -> Gitignore {
        // the second path takes priority
        build_ignore(
            [
                helix_loader::config_dir().join("filesentryignore"),
                workspace.join(".helix/filesentryignore"),
            ],
            workspace,
        )
        .unwrap_or(Gitignore::empty())
    }

    fn is_ignored(
        ignores: &[impl Borrow<Gitignore>],
        path: &Path,
        is_dir: Option<bool>,
    ) -> Option<bool> {
        match is_dir {
            Some(is_dir) => {
                for ignore in ignores {
                    match ignore.borrow().matched(path, is_dir) {
                        ignore::Match::None => continue,
                        ignore::Match::Ignore(_) => return Some(true),
                        ignore::Match::Whitelist(_) => return Some(false),
                    }
                }
            }
            None => {
                // if we don't know wether this is a directory (on windows)
                // then we are conservative and allow the dirs
                for ignore in ignores {
                    match ignore.borrow().matched(path, true) {
                        ignore::Match::None => continue,
                        ignore::Match::Ignore(glob) => {
                            if glob.is_only_dir() {
                                match ignore.borrow().matched(path, false) {
                                    ignore::Match::None => continue,
                                    ignore::Match::Ignore(_) => return Some(true),
                                    ignore::Match::Whitelist(_) => return Some(false),
                                }
                            } else {
                                return Some(true);
                            }
                        }
                        ignore::Match::Whitelist(_) => return Some(false),
                    }
                }
            }
        }
        None
    }
}

/// a filter to ignore hiddeng/ingored files. The point of this
/// is to avoid overwhelming the watcher with watching a ton of
/// files/directories (like the cargo target directory, node_modules or
/// VCS files) so ignoring a file is a performance optimization.
///
/// By default we ignore ignored
struct WatchFilter {
    filesentry_ignores: Gitignore,
    ignore_files: Vec<IgnoreFiles>,
    global_ignores: Vec<Arc<Gitignore>>,
    hidden: bool,
    watch_vcs: bool,
    max_depth: Option<usize>,
}

impl WatchFilter {
    fn new<'a>(
        config: &Config,
        workspace: &'a Path,
        roots: impl Iterator<Item = &'a Path> + Clone,
    ) -> WatchFilter {
        let filesentry_ignores = IgnoreFiles::filesentry_ignores(workspace);
        let (global_ignores, workspace_ignore) = IgnoreFiles::shared_ignores(workspace, config);
        let ignore_files = roots
            .chain([workspace])
            .map(|root| IgnoreFiles::new(workspace_ignore.clone(), config, root, &global_ignores))
            .collect();
        WatchFilter {
            filesentry_ignores,
            ignore_files,
            global_ignores,
            hidden: config.hidden,
            watch_vcs: config.watch_vcs,
            max_depth: config.max_depth,
        }
    }

    fn ignore_path_impl(
        &self,
        path: &Path,
        is_dir: Option<bool>,
        ignore_files: &[Arc<Gitignore>],
    ) -> bool {
        if let Some(ignore) =
            IgnoreFiles::is_ignored(slice::from_ref(&self.filesentry_ignores), path, is_dir)
        {
            return ignore;
        }
        if is_hardcoded_whitelist(path) {
            return false;
        }
        if is_hardcoded_blacklist(path, is_dir.unwrap_or(false)) {
            return true;
        }
        if let Some(ignore) = IgnoreFiles::is_ignored(ignore_files, path, is_dir) {
            return ignore;
        }
        // ignore .git dircectory except .git/HEAD (and .git itself)
        if is_vcs_ignore(path, self.watch_vcs) {
            return true;
        }
        !self.hidden && is_hidden(path)
    }
}

impl filesentry::Filter for WatchFilter {
    fn ignore_path(&self, path: &Path, is_dir: Option<bool>) -> bool {
        let i = self
            .ignore_files
            .iter()
            .filter(|files| path.starts_with(&files.root))
            .max_by_key(|files| files.root.components().count());
        let (root, ignore_files) = i.map_or((Path::new(""), &self.global_ignores), |files| {
            (&files.root, &files.ignores)
        });
        if path == root {
            return false;
        }
        if self.max_depth.is_some_and(|depth| {
            path.strip_prefix(root)
                .is_ok_and(|relative| relative.components().count() > depth)
        }) {
            return true;
        }
        self.ignore_path_impl(path, is_dir, ignore_files)
    }

    fn ignore_path_rec(&self, mut path: &Path, mut is_dir: Option<bool>) -> bool {
        if self.ignore_path(path, is_dir) {
            return true;
        }
        let i = self
            .ignore_files
            .iter()
            .filter(|files| path.starts_with(&files.root))
            .max_by_key(|files| files.root.components().count());
        let (root, ignore_files) = i.map_or((Path::new(""), &self.global_ignores), |files| {
            (&files.root, &files.ignores)
        });
        loop {
            if path == root {
                return false;
            }
            if self.ignore_path_impl(path, is_dir, ignore_files) {
                return true;
            }
            let Some(parent) = path.parent() else {
                break;
            };
            path = parent;
            // Ancestors of the leaf are always directories. Passing the leaf's
            // `is_dir` up the chain makes a dir-only pattern (gitignore `target/`)
            // miss the ancestor directory it names.
            is_dir = Some(true);
        }
        false
    }
}

fn is_hidden(path: &Path) -> bool {
    path.file_name().is_some_and(|it| {
        it.as_encoded_bytes().first() == Some(&b'.')
        // handled by vcs ignore rules
        && it != ".git" && it != ".jj"
    })
}

// hidden directories we want to watch by default
fn is_hardcoded_whitelist(path: &Path) -> bool {
    path.ends_with(".helix")
        | path.ends_with(".github")
        | path.ends_with(".cargo")
        | path.ends_with(".envrc")
}

fn is_hardcoded_blacklist(path: &Path, is_dir: bool) -> bool {
    // don't descend into the cargo regstiry and similar
    path.parent()
        .is_some_and(|parent| parent.ends_with(".cargo"))
        && is_dir
}

fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|it| it.to_str())
}

fn is_vcs_ignore(path: &Path, watch_vcs: bool) -> bool {
    // ignore .git directory contents except .git/HEAD (and .git itself)
    // Note: only checks immediate parent; recursive checking is done by ignore_path_rec
    if watch_vcs
        && path.parent().is_some_and(|it| it.ends_with(".git"))
        && !["HEAD", "index", "refs", "packed-refs", "commondir"]
            .iter()
            .any(|name| path.ends_with(Path::new(".git").join(name)))
    {
        return true;
    }
    match file_name(path) {
        Some(".svn" | ".hg") => true,
        Some(".jj") => !watch_vcs,
        Some(".git") => !watch_vcs,
        _ => {
            // Follow only JJ's operation heads and checkout state, never objects.
            if let Some(root) = path
                .ancestors()
                .find(|path| path.file_name() == Some(std::ffi::OsStr::new(".jj")))
            {
                let relative = path.strip_prefix(root).unwrap();
                return !watch_vcs
                    || ![
                        "repo",
                        "repo/op_heads",
                        "repo/op_heads/heads",
                        "working_copy",
                        "working_copy/checkout",
                        "working_copy/tree_state",
                    ]
                    .iter()
                    .any(|allowed| relative == Path::new(allowed))
                        && relative.parent() != Some(Path::new("repo/op_heads/heads"));
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::file_watcher::{is_hardcoded_whitelist, is_hidden, is_vcs_ignore};

    #[test]
    fn deleted_paths_still_produce_poll_events() {
        let path = std::env::temp_dir().join(format!(
            "helix-deleted-watch-test-{}/source.txt",
            std::process::id()
        ));
        let events = super::events_from_paths([path.clone()]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path.as_std_path(), path);
    }

    #[test]
    fn uses_the_most_specific_root_and_enforces_depth() {
        use filesentry::Filter;
        let config = super::Config {
            max_depth: Some(1),
            ..Default::default()
        };
        let filter = super::WatchFilter::new(
            &config,
            Path::new("/repo"),
            [Path::new("/other"), Path::new("/repo/nested")].into_iter(),
        );
        assert!(!filter.ignore_path_rec(Path::new("/repo/nested/file.txt"), Some(false)));
        assert!(filter.ignore_path_rec(Path::new("/repo/nested/deeper/file.txt"), Some(false)));
        assert!(!filter.ignore_path_rec(Path::new("/other/file.txt"), Some(false)));
    }

    #[test]
    fn test_vcs_ignore() {
        assert!(!is_vcs_ignore(Path::new(".git"), true));
        assert!(!is_vcs_ignore(Path::new(".git/HEAD"), true));
        assert!(is_vcs_ignore(Path::new(".git/foo"), true));
        // Note: .git/foo/bar is NOT caught by is_vcs_ignore (only checks immediate parent)
        // but it IS caught by ignore_path_rec which checks ancestors recursively
        assert!(!is_vcs_ignore(Path::new(".git/foo/bar"), true));
        assert!(!is_vcs_ignore(Path::new(".foo"), true));
        assert!(!is_vcs_ignore(Path::new(".jj"), true));
        assert!(!is_vcs_ignore(Path::new(".git/index"), true));
        assert!(!is_vcs_ignore(Path::new(".git/refs"), true));
        assert!(!is_vcs_ignore(
            Path::new(".jj/repo/op_heads/heads/123"),
            true
        ));
        assert!(is_vcs_ignore(Path::new(".jj/repo/store"), true));
        assert!(is_vcs_ignore(
            Path::new(".jj/repo/op_heads/heads/123"),
            false
        ));
        assert!(is_vcs_ignore(Path::new(".svn"), true));
        assert!(is_vcs_ignore(Path::new(".hg"), true));
    }

    #[test]
    fn test_hidden() {
        assert!(is_hidden(Path::new(".foo")));
        // handled by vcs ignore rules
        assert!(!is_hidden(Path::new(".git")));
    }

    #[test]
    fn test_whitelist() {
        // Note: .git is NOT in whitelist - it has special handling in is_vcs_ignore and is_hidden
        assert!(is_hardcoded_whitelist(Path::new(".helix")));
        assert!(is_hardcoded_whitelist(Path::new(".github")));
        assert!(!is_hardcoded_whitelist(Path::new(".githup")));
    }

    #[test]
    fn ignore_path_rec_treats_ancestors_as_dirs() {
        use std::sync::Arc;

        use filesentry::Filter;
        use ignore::gitignore::{Gitignore, GitignoreBuilder};

        use crate::file_watcher::{IgnoreFiles, WatchFilter};

        let mut builder = GitignoreBuilder::new("/repo");
        // dir-only pattern: matches the `target` directory, not a file named target
        builder.add_line(None, "target/").unwrap();
        let ignores = builder.build().unwrap();
        let filter = WatchFilter {
            filesentry_ignores: Gitignore::empty(),
            ignore_files: vec![IgnoreFiles {
                root: "/repo".into(),
                ignores: vec![Arc::new(ignores)],
            }],
            global_ignores: Vec::new(),
            hidden: true,
            watch_vcs: true,
            max_depth: None,
        };

        // A file under `target/` is ignored even though the leaf is passed as a file:
        // the walk must re-check the `target` ancestor as a directory.
        assert!(filter.ignore_path_rec(Path::new("/repo/target/foo.rs"), Some(false)));
        assert!(filter.ignore_path_rec(Path::new("/repo/target/deep/foo.rs"), Some(false)));
        assert!(!filter.ignore_path_rec(Path::new("/repo/src/main.rs"), Some(false)));
    }
}
