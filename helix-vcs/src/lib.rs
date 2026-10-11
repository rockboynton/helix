//! `helix_vcs` provides types for working with diffs from a Version Control System (VCS).
//! Currently `git` is the only supported provider for diffs, but this architecture allows
//! for other providers to be added in the future.

use anyhow::Result;
use arc_swap::ArcSwap;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(feature = "git")]
mod git;
#[cfg(feature = "jj")]
mod jj;
#[cfg(feature = "git")]
pub use git::blame::FileBlame;

/// Unavailable blame provider in builds without Git support.
#[cfg(not(feature = "git"))]
#[derive(Debug)]
pub struct FileBlame;

#[cfg(not(feature = "git"))]
impl FileBlame {
    pub fn try_new(_file: std::path::PathBuf) -> Result<Self> {
        anyhow::bail!("Git blame support is not compiled in")
    }

    pub fn blame_for_line(&self, _line: u32, _inserted: u32, _removed: u32) -> LineBlame {
        LineBlame
    }
}

#[cfg(not(feature = "git"))]
pub struct LineBlame;

#[cfg(not(feature = "git"))]
impl LineBlame {
    pub fn parse_format(self, _format: &str) -> String {
        String::new()
    }
}

mod diff;

pub use diff::{DiffHandle, Hunk};

mod status;

pub use status::FileChange;

/// Contains all active diff providers. Diff providers are compiled in via features. Currently
/// Git and Jujutsu are supported.
#[derive(Default, Clone)]
pub struct DiffProviderRegistry {
    /// Repository root path mapped to their provider.
    ///
    /// When a root path cannot be found after having called `add_file`, it means there is no
    /// provider to speak of.
    providers: HashMap<Arc<Path>, DiffProvider>,
    /// Count the number of files added for a specific provider path.
    /// Providers themselves don't care about that, this is handled entirely in `Self::add_file`,
    /// without knowledge from the `Self::add_file_<provider>` methods.
    ///
    /// Note: it *could* happen that a provider for a path is changed without the number of
    /// associated files changing, e.g deleting a .git/ and initializing a .jj/ repo.
    counters: HashMap<Arc<Path>, u32>,
}

/// Diff-related methods
impl DiffProviderRegistry {
    /// Get the given file from the VCS. This provides the unedited document as a "base"
    /// for a diff to be created.
    pub fn get_diff_base(&self, file: &Path) -> Option<Vec<u8>> {
        match self.provider_for(file)?.get_diff_base(file) {
            Ok(diff_base) => Some(diff_base),
            Err(err) => {
                log::debug!("{err:#?}");
                log::debug!("failed to open diff base for {}", file.display());
                None
            }
        }
    }

    /// Get the current name of the current [HEAD](https://stackoverflow.com/questions/2304087/what-is-head-in-git).
    pub fn get_current_head_name(&self, file: &Path) -> Option<Arc<ArcSwap<Box<str>>>> {
        match self.provider_for(file)?.get_current_head_name() {
            Ok(head_name) => Some(head_name),
            Err(err) => {
                log::debug!("{err:#?}");
                log::debug!("failed to obtain current head name for {}", file.display());
                None
            }
        }
    }

    pub fn needs_reload(&self, fs_event: &helix_core::file_watcher::Event) -> bool {
        self.providers
            .values()
            .any(|provider| provider.needs_reload(fs_event))
    }

    /// Get paths that need to be watched for VCS state changes.
    /// These are paths like HEAD files that indicate branch/commit changes.
    /// The workspace path is used to determine if the VCS metadata is external.
    pub fn get_watched_paths(&self, workspace: &Path) -> Vec<PathBuf> {
        self.providers
            .values()
            .filter_map(|provider| provider.get_watched_path(workspace))
            .collect()
    }

    /// Fire-and-forget changed file iteration. Runs everything in a background task. Keeps
    /// iteration until `on_change` returns `false`.
    pub fn for_each_changed_file(
        self,
        cwd: Arc<Path>,
        f: impl Fn(Result<FileChange>) -> bool + Send + 'static,
    ) {
        tokio::task::spawn_blocking(move || {
            let Some(diff_provider) = self.provider_for(&cwd) else {
                return;
            };
            if let Err(err) = diff_provider.for_each_changed_file(&f) {
                f(Err(err));
            }
        });
    }
}

/// Creation and update methods
#[cfg_attr(not(any(feature = "git", feature = "jj")), allow(unused))]
impl DiffProviderRegistry {
    /// Register a provider (if any is found) for the given path.
    pub fn add(&mut self, path: &Path, trust_full: bool) {
        let Some((repo_path, provider)) = get_possible_provider(path) else {
            // Do nothing here: there is no path to use and so the actual methods to get infos
            // like `get_diff_base` just won't do anything since they won't find a source to
            // work with.
            log::debug!("Found no potential diff provider for {}", path.display());
            // Note: if a `.<vcs>/` dir is deleted, we may end up in a situation where we lose track
            // of a now unused provider. This is acceptable because it doesn't happen that often in
            // practice and people can just reload to force an update.
            //
            // If it becomes an issue in the future, we could fix it by recomputing the providers
            // for each stored paths here.
            return;
        };

        let result: Result<(Arc<Path>, PossibleDiffProvider)> = match provider {
            #[cfg(feature = "git")]
            PossibleDiffProvider::Git => self.add_file_git(repo_path, trust_full),
            #[cfg(feature = "jj")]
            PossibleDiffProvider::JJ => self.add_file_jj(repo_path, trust_full),
        };

        match result {
            Ok((key, prov)) => {
                // Increase the count for this path.
                let count = self.counters.entry(key).or_default();
                let created = *count == 0;
                *count += 1;

                // Only log at info level when adding a new provider
                if created {
                    log::info!(
                        "Added {prov:?} (repo: {}) from {}",
                        repo_path.display(),
                        path.display()
                    )
                } else {
                    log::debug!(
                        "Reused {prov:?} (repo: {}) for {}",
                        repo_path.display(),
                        path.display()
                    );
                }
            }
            Err(err) => log::debug!(
                "Failed to open repo at {} for {}: {:?}",
                repo_path.display(),
                path.display(),
                err
            ),
        }
    }

    /// Reload the provider for the given path.
    pub fn reload(&mut self, path: &Path, trust_full: bool) {
        self.remove(path);
        self.add(path, trust_full);
    }

    /// Remove the given path from the provider cache. If it was the last one using it, this will
    /// free up the provider.
    pub fn remove(&mut self, path: &Path) {
        let Some((repo_path, _)) = get_possible_provider(path) else {
            return;
        };

        let Some(count) = self.counters.get_mut(repo_path) else {
            return;
        };

        *count -= 1;
        if *count == 0 {
            // Cleanup the provider when the last user disappears
            self.counters.remove(repo_path);
            self.providers.remove(repo_path);

            // While reallocating is costly, in most sessions of Helix there will be one main
            // workspace and sometimes a jump to some temporary one (for example from a jump-to-def
            // in an LSP) that will be closed after some time. We want to avoid keeping unused
            // RAM for this.
            self.providers.shrink_to_fit();
            self.counters.shrink_to_fit();
        }
    }

    /// Clears the saved providers completely.
    pub fn reset(&mut self) {
        // NOTE: we keep the allocated memory for reuse since this is mostly used through
        // `reload_all`, in which case the underlying repository is most likely still there and the
        // user just wants a clean slate for diffs and other features.
        let Self {
            providers,
            counters,
        } = self;
        providers.clear();
        counters.clear();
    }
}

/// Private methods
impl DiffProviderRegistry {
    fn provider_for(&self, path: &Path) -> Option<&DiffProvider> {
        let path = get_possible_provider(path)?.0;
        self.providers.get(path)
    }

    // Remove a provider, notably used when its trust setting has changed from the original setup.
    #[cfg(feature = "git")]
    fn remove_provider(&mut self, repo_path: &Path) {
        // Ensure we d'ont
        let Self {
            providers,
            counters,
        } = self;
        providers.remove(repo_path);
        counters.remove(repo_path);
    }

    /// Add the git repo to the known providers *if* it isn't already known.
    #[cfg(feature = "git")]
    fn add_file_git(
        &mut self,
        repo_path: &Path,
        trust_full: bool,
    ) -> Result<(Arc<Path>, PossibleDiffProvider)> {
        // Don't build a git repo object if there is already one for that path.
        if let Some((
            key,
            DiffProvider::Git {
                trust_full: originally_trusted,
                ..
            },
        )) = self.providers.get_key_value(repo_path)
        {
            if *originally_trusted == trust_full {
                return Ok((Arc::clone(key), PossibleDiffProvider::Git));
            } else {
                self.remove_provider(repo_path);
            }
        }

        match git::open_repo(repo_path, trust_full) {
            Ok(repo) => {
                let key = Arc::from(repo_path);
                self.providers.insert(
                    Arc::clone(&key),
                    DiffProvider::Git {
                        repo: Box::new(repo),
                        trust_full,
                    },
                );
                Ok((key, PossibleDiffProvider::Git))
            }
            Err(err) => Err(err),
        }
    }

    /// Add the JJ repo to the known providers *if* it isn't already known.
    #[cfg(feature = "jj")]
    fn add_file_jj(
        &mut self,
        repo_path: &Path,
        trust_full: bool,
    ) -> Result<(Arc<Path>, PossibleDiffProvider)> {
        use anyhow::Context;

        if !trust_full {
            log::warn!(concat!(
                "JJ repositories are only supported when the repository is trusted in Helix",
                " because the provider reads repository and workspace configuration.\n",
                "\n",
                "See <https://docs.jj-vcs.dev/latest/design/secure-config/> and ",
                "<https://docs.jj-vcs.dev/latest/design/managed-config/>."
            ));
            anyhow::bail!("Only fully trusted workspaces are supported for JJ repositories");
        }
        let repo_path = std::fs::canonicalize(repo_path)
            .context("failed to canonicalize potential jj repo path")?;

        // Don't build a JJ repo object if there is already one for that path.
        if let Some((key, DiffProvider::JJ(_))) = self.providers.get_key_value(&*repo_path) {
            return Ok((Arc::clone(key), PossibleDiffProvider::JJ));
        }

        match jj::open_repo(&repo_path) {
            Ok(()) => {
                let key = Arc::from(repo_path);
                self.providers
                    .insert(Arc::clone(&key), DiffProvider::JJ(Arc::clone(&key)));
                Ok((key, PossibleDiffProvider::JJ))
            }
            Err(err) => Err(err),
        }
    }
}

/// A union type that includes all types that implement [DiffProvider]. We need this type to allow
/// cloning [DiffProviderRegistry] as `Clone` cannot be used in trait objects.
#[derive(Clone)]
pub enum DiffProvider {
    #[cfg(feature = "git")]
    Git {
        repo: Box<gix::ThreadSafeRepository>,
        trust_full: bool,
    },
    /// A Jujutsu workspace, queried through jj-lib without spawning processes.
    #[cfg(feature = "jj")]
    JJ(Arc<Path>),
}

#[cfg_attr(not(any(feature = "git", feature = "jj")), allow(unused))]
impl DiffProvider {
    #[cfg_attr(not(feature = "git"), allow(unused_variables))]
    pub fn needs_reload(&self, fs_event: &helix_core::file_watcher::Event) -> bool {
        match *self {
            #[cfg(feature = "git")]
            DiffProvider::Git { .. } => {
                let path = fs_event.path.as_std_path();
                // Check for regular .git/HEAD
                if path.ends_with(".git/HEAD") {
                    return true;
                }
                // Check for worktree HEAD at .git/worktrees/<name>/HEAD
                if path.file_name().is_some_and(|f| f == "HEAD") {
                    // Walk up the path to check for .git/worktrees pattern
                    if let Some(parent) = path.parent() {
                        if let Some(grandparent) = parent.parent() {
                            if grandparent.ends_with(".git/worktrees") {
                                return true;
                            }
                        }
                    }
                }
                false
            }
            #[cfg(feature = "jj")]
            DiffProvider::JJ(_) => false,
        }
    }

    fn get_diff_base(&self, file: &Path) -> Result<Vec<u8>> {
        // We need the */ref else we're matching on a reference and Rust considers all references
        // inhabited.
        match *self {
            #[cfg(feature = "git")]
            Self::Git { ref repo, .. } => git::get_diff_base(repo, file),
            #[cfg(feature = "jj")]
            Self::JJ(ref repo) => jj::get_diff_base(repo, file),
        }
    }

    fn get_current_head_name(&self) -> Result<Arc<ArcSwap<Box<str>>>> {
        match *self {
            #[cfg(feature = "git")]
            Self::Git { ref repo, .. } => git::get_current_head_name(repo),
            #[cfg(feature = "jj")]
            Self::JJ(ref repo) => jj::get_current_head_name(repo),
        }
    }

    fn for_each_changed_file(&self, f: impl Fn(Result<FileChange>) -> bool) -> Result<()> {
        match *self {
            #[cfg(feature = "git")]
            Self::Git { ref repo, .. } => git::for_each_changed_file(repo, f),
            #[cfg(feature = "jj")]
            Self::JJ(ref repo) => jj::for_each_changed_file(repo, f),
        }
    }

    /// Get the path to watch for VCS state changes (e.g., HEAD file).
    fn get_watched_path(&self, _workspace: &Path) -> Option<PathBuf> {
        match *self {
            #[cfg(feature = "git")]
            Self::Git { ref repo, .. } => Some(repo.to_thread_local().git_dir().join("HEAD")),
            #[cfg(feature = "jj")]
            Self::JJ(_) => None,
        }
    }
}

#[cfg(all(test, feature = "git"))]
mod tests {
    use super::*;

    #[cfg(feature = "git")]
    #[test]
    fn test_needs_reload_regular_git() {
        use helix_core::file_watcher::{CanonicalPathBuf, Event, EventType};
        use std::path::Path;

        let directory = tempfile::tempdir().unwrap();
        let repo = gix::init(directory.path()).unwrap();
        let provider = DiffProvider::Git {
            repo: Box::new(repo.into_sync()),
            trust_full: false,
        };

        // Regular .git/HEAD should trigger reload
        let event = Event {
            path: CanonicalPathBuf::assert_canonicalized(Path::new("/home/user/repo/.git/HEAD")),
            ty: EventType::Modified,
        };
        assert!(provider.needs_reload(&event));

        // Other .git files should not trigger reload
        let event = Event {
            path: CanonicalPathBuf::assert_canonicalized(Path::new("/home/user/repo/.git/config")),
            ty: EventType::Modified,
        };
        assert!(!provider.needs_reload(&event));
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_needs_reload_worktree_head() {
        use helix_core::file_watcher::{CanonicalPathBuf, Event, EventType};
        use std::path::Path;

        let directory = tempfile::tempdir().unwrap();
        let repo = gix::init(directory.path()).unwrap();
        let provider = DiffProvider::Git {
            repo: Box::new(repo.into_sync()),
            trust_full: false,
        };

        // Worktree HEAD at .git/worktrees/<name>/HEAD should trigger reload
        let event = Event {
            path: CanonicalPathBuf::assert_canonicalized(Path::new(
                "/home/user/main-repo/.git/worktrees/my-worktree/HEAD",
            )),
            ty: EventType::Modified,
        };
        assert!(provider.needs_reload(&event));

        // Nested worktree name should also work
        let event = Event {
            path: CanonicalPathBuf::assert_canonicalized(Path::new(
                "/home/user/main-repo/.git/worktrees/feature-branch/HEAD",
            )),
            ty: EventType::Modified,
        };
        assert!(provider.needs_reload(&event));

        // Non-HEAD files in worktrees should not trigger reload
        let event = Event {
            path: CanonicalPathBuf::assert_canonicalized(Path::new(
                "/home/user/main-repo/.git/worktrees/my-worktree/index",
            )),
            ty: EventType::Modified,
        };
        assert!(!provider.needs_reload(&event));

        // HEAD files not in .git/worktrees should not trigger reload
        let event = Event {
            path: CanonicalPathBuf::assert_canonicalized(Path::new(
                "/home/user/other/worktrees/my-worktree/HEAD",
            )),
            ty: EventType::Modified,
        };
        assert!(!provider.needs_reload(&event));
    }
}

#[derive(Debug, Copy, Clone)]
pub enum PossibleDiffProvider {
    /// Possibly a git repo rooted at the stored path (i.e. `<path>/.git` exists)
    #[cfg(feature = "git")]
    Git,
    /// Possibly a git repo rooted at the stored path (i.e. `<path>/.jj` exists)
    #[cfg(feature = "jj")]
    JJ,
}

/// Does *possible* diff provider auto detection. Returns the 'root' of the workspace
///
/// We say possible because this function doesn't open the actual repository to check if that's
/// actually the case.
fn get_possible_provider(path: &Path) -> Option<(&Path, PossibleDiffProvider)> {
    // TODO(poliorcetics): make checking order configurable
    let checks: &[(&str, PossibleDiffProvider)] = &[
        #[cfg(feature = "jj")]
        (".jj", PossibleDiffProvider::JJ),
        #[cfg(feature = "git")]
        (".git", PossibleDiffProvider::Git),
    ];

    if !checks.is_empty() {
        for parent in path.ancestors() {
            for &(repo_indic, pdp) in checks {
                if let Ok(true) = parent.join(repo_indic).try_exists() {
                    return Some((parent, pdp));
                }
            }
        }
    }

    None
}
