use std::path::{Path, PathBuf};

/// States for a file having been changed.
#[derive(Debug, PartialEq, Eq)]
pub enum FileChange {
    /// Not tracked by the VCS.
    Untracked { path: PathBuf },
    /// File has been modified.
    Modified { path: PathBuf },
    /// File modification is in conflict with a different update.
    Conflict { path: PathBuf },
    /// File has been deleted.
    Deleted { path: PathBuf },
    /// File has been renamed.
    Renamed {
        from_path: PathBuf,
        to_path: PathBuf,
    },
}

impl FileChange {
    pub(crate) fn rooted(self, root: &Path) -> Self {
        match self {
            Self::Untracked { path } => Self::Untracked {
                path: root.join(path),
            },
            Self::Modified { path } => Self::Modified {
                path: root.join(path),
            },
            Self::Conflict { path } => Self::Conflict {
                path: root.join(path),
            },
            Self::Deleted { path } => Self::Deleted {
                path: root.join(path),
            },
            Self::Renamed { from_path, to_path } => Self::Renamed {
                from_path: root.join(from_path),
                to_path: root.join(to_path),
            },
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::Untracked { path } => path,
            Self::Modified { path } => path,
            Self::Conflict { path } => path,
            Self::Deleted { path } => path,
            Self::Renamed { to_path, .. } => to_path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_file_paths_are_rooted_without_rebasing_absolute_git_paths() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let target = root.join("nested/file.txt");
        for change in [
            FileChange::Untracked {
                path: "nested/file.txt".into(),
            },
            FileChange::Modified {
                path: "nested/file.txt".into(),
            },
            FileChange::Conflict {
                path: "nested/file.txt".into(),
            },
            FileChange::Deleted {
                path: "nested/file.txt".into(),
            },
            FileChange::Modified {
                path: target.clone(),
            },
        ] {
            assert_eq!(change.rooted(&root).path(), target);
        }
        assert_eq!(
            FileChange::Renamed {
                from_path: "old.txt".into(),
                to_path: "nested/file.txt".into(),
            }
            .rooted(&root),
            FileChange::Renamed {
                from_path: root.join("old.txt"),
                to_path: target,
            }
        );
    }
}
