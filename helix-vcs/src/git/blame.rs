use anyhow::Context as _;
use anyhow::Result;
use std::{collections::HashMap, path::PathBuf, sync::Arc};

use super::{get_repo_dir, open_repo};

/// Immutable metadata, fully decoded by the background blame worker.
/// Rendering only performs a map lookup and copies already-cached strings.
#[derive(Debug)]
pub struct FileBlame {
    blame: HashMap<u32, Arc<LineBlame>>,
    remote_urls: Vec<String>,
}

impl FileBlame {
    #[inline]
    pub fn blame_for_line(&self, line: u32, inserted_lines: u32, removed_lines: u32) -> LineBlame {
        let blame_line = line.saturating_sub(inserted_lines) + removed_lines;
        self.blame
            .get(&blame_line)
            .map(|value| (**value).clone())
            .unwrap_or_default()
    }

    pub fn remote_urls(&self) -> &[String] {
        &self.remote_urls
    }

    /// Expensive history traversal and commit decoding: never call on a render path.
    pub fn try_new(file: PathBuf) -> Result<Self> {
        let thread_safe_repo =
            open_repo(get_repo_dir(&file)?, false).context("Failed to open git repo")?;
        let repo = thread_safe_repo.to_thread_local();
        let head = repo.head()?.peel_to_commit()?.id;
        let mut resource_cache = repo.diff_resource_cache_for_tree_diff()?;
        let entries = gix::blame::file(
            &repo.objects,
            head,
            None,
            &mut resource_cache,
            &gix::path::to_unix_separators_on_windows(gix::path::try_into_bstr(
                file.strip_prefix(
                    repo.workdir()
                        .context("Git repository has no working directory")?,
                )?,
            )?),
            gix::blame::Options::default(),
        )?
        .entries;
        let mut commits = HashMap::new();
        let mut blame = HashMap::new();
        for entry in entries {
            let metadata = commits
                .entry(entry.commit_id)
                .or_insert_with(|| Arc::new(decode_commit(&repo, entry.commit_id)));
            for line in entry.start_in_blamed_file..entry.start_in_blamed_file + entry.len.get() {
                blame.insert(line, Arc::clone(metadata));
            }
        }
        // Prefer upstream in forks, where the original PR usually lives.
        let remote_urls = ["upstream", "origin"]
            .into_iter()
            .filter_map(|name| {
                // Read only the URL string: do not enable trust-sensitive filters,
                // URL rewrites, remote helpers, or transport execution.
                repo.config_snapshot()
                    .string(format!("remote.{name}.url").as_str())
                    .map(|url| url.to_string())
            })
            .collect();
        Ok(Self { blame, remote_urls })
    }
}

fn decode_commit(repo: &gix::Repository, id: gix::ObjectId) -> LineBlame {
    let commit = repo.find_commit(id).ok();
    let message = commit.as_ref().and_then(|c| c.message().ok());
    let author = commit.as_ref().and_then(|c| c.author().ok());
    let time = author.and_then(|a| a.time.parse::<gix::date::Time>().ok());
    LineBlame {
        commit_hash: commit
            .as_ref()
            .and_then(|c| c.short_id().map(|id| id.to_string()).ok()),
        full_commit_hash: commit.as_ref().map(|c| c.id.to_string()),
        author_name: author.map(|a| a.name.to_string()),
        author_email: author.map(|a| a.email.to_string()),
        commit_date: time.and_then(|t| t.format(gix::date::time::format::SHORT).ok()),
        commit_datetime: time.and_then(|t| t.format(gix::date::time::format::ISO8601_STRICT).ok()),
        commit_title: message.as_ref().map(|m| m.title.to_string()),
        commit_body: message
            .as_ref()
            .and_then(|m| m.body.map(|body| body.to_string())),
        time_stamp: time.map(|t| (t.seconds, t.offset)),
        time_ago: None,
    }
}

#[derive(Clone, Default, PartialEq, PartialOrd, Ord, Eq, Debug)]
pub struct LineBlame {
    commit_hash: Option<String>,
    full_commit_hash: Option<String>,
    author_name: Option<String>,
    author_email: Option<String>,
    commit_date: Option<String>,
    commit_datetime: Option<String>,
    commit_title: Option<String>,
    commit_body: Option<String>,
    /// Used to compute `time-ago`
    time_stamp: Option<(i64, i32)>,
    /// This field is the only one that needs to be re-computed every time
    /// we request the `LineBlame`. It exists here for lifetime purposes, so we can return
    /// `&str` from `Self::get_variable`.
    ///
    /// This should only be set from within and never initialized.
    time_ago: Option<String>,
}

impl LineBlame {
    pub fn details(&self) -> crate::BlameDetails {
        crate::BlameDetails {
            commit: self.full_commit_hash.clone(),
            author: self.author_name.clone(),
            email: self.author_email.clone(),
            datetime: self.commit_datetime.clone(),
            title: self.commit_title.clone(),
            body: self.commit_body.clone(),
        }
    }
    /// # Returns
    ///
    /// None => Invalid variable
    /// Some(None) => Valid variable, but is empty
    #[inline]
    fn get_variable(&mut self, var: &str) -> Option<Option<&str>> {
        Some(
            // if adding new variables, update `Self::LONGEST_VARIABLE_LENGTH`
            match var {
                "commit" => &self.commit_hash,
                "author" => &self.author_name,
                "date" => &self.commit_date,
                "datetime" => &self.commit_datetime,
                "title" => &self.commit_title,
                "email" => &self.author_email,
                "body" => &self.commit_body,
                "time-ago" => {
                    let time_ago = self.time_stamp.map(|(utc_seconds, timezone_offset)| {
                        helix_stdx::time::format_relative_time(utc_seconds, timezone_offset)
                    });
                    self.time_ago = time_ago;
                    &self.time_ago
                }
                _ => return None,
            }
            .as_deref(),
        )
    }

    /// Parse the user's blame format
    #[inline]
    pub fn parse_format(&mut self, format: &str) -> String {
        let mut line_blame = String::new();
        let mut content_before_variable = String::with_capacity(format.len());

        let mut chars = format.char_indices().peekable();
        // in all cases, when any of the variables is empty we exclude the content before the variable
        // However, if the variable is the first and it is empty - then exclude the content after the variable
        let mut exclude_content_after_variable = false;
        while let Some((ch_idx, ch)) = chars.next() {
            if ch == '{' {
                let mut variable = String::new();
                // eat all characters until the end
                while let Some((_, ch)) = chars.next_if(|(_, ch)| *ch != '}') {
                    variable.push(ch);
                }
                // eat the '}' if it was found
                let has_closing = chars.next().is_some();

                #[derive(PartialEq, Eq, PartialOrd, Ord)]
                enum Variable<'a> {
                    Valid(&'a str),
                    Invalid(&'a str),
                    Empty,
                }

                let variable_value = self.get_variable(&variable).map_or_else(
                    || {
                        // Invalid variable. So just add whatever we parsed before.
                        // The length of the variable, including opening and optionally
                        // closing curly braces
                        let variable_len = 1 + variable.len() + has_closing as usize;

                        Variable::Invalid(&format[ch_idx..ch_idx + variable_len])
                    },
                    |s| s.map(Variable::Valid).unwrap_or(Variable::Empty),
                );

                match variable_value {
                    Variable::Invalid(value) | Variable::Valid(value) => {
                        if exclude_content_after_variable {
                            // don't push anything.
                            exclude_content_after_variable = false;
                        } else {
                            line_blame.push_str(&content_before_variable);
                        }
                        line_blame.push_str(value);
                    }
                    Variable::Empty => {
                        if line_blame.is_empty() {
                            // exclude content AFTER this variable (at next iteration of the loop,
                            // we'll exclude the content before a valid variable)
                            exclude_content_after_variable = true;
                        } else {
                            // exclude content BEFORE this variable
                            // also just don't add anything.
                        }
                    }
                }

                // we've consumed the content before the variable so just get rid of it and
                // make space for new
                content_before_variable.drain(..);
            } else {
                content_before_variable.push(ch);
            }
        }

        line_blame
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::git::test::create_commit_with_message;
    use crate::git::test::empty_git_repo;
    use std::fs::File;

    /// describes how a line was modified
    #[derive(PartialEq, PartialOrd, Ord, Eq)]
    enum LineDiff {
        /// this line is added
        Insert,
        /// this line is deleted
        Delete,
        /// no changes for this line
        None,
    }

    /// checks if the first argument is `no_commit` or not
    macro_rules! no_commit_flag {
        (no_commit, $commit_msg:literal) => {
            false
        };
        (, $commit_msg:literal) => {
            true
        };
        ($any:tt, $commit_msg:literal) => {
            compile_error!(concat!(
                "expected `no_commit` or nothing for commit ",
                $commit_msg
            ))
        };
    }

    /// checks if the first argument is `insert` or `delete`
    macro_rules! line_diff_flag {
        (insert, $commit_msg:literal, $line:expr) => {
            LineDiff::Insert
        };
        (delete, $commit_msg:literal, $line:expr) => {
            LineDiff::Delete
        };
        (, $commit_msg:literal, $line:expr) => {
            LineDiff::None
        };
        ($any:tt, $commit_msg:literal, $line:expr) => {
            compile_error!(concat!(
                "expected `insert`, `delete` or nothing for commit ",
                $commit_msg,
                " line ",
                $line
            ))
        };
    }

    /// This macro exists because we can't pass a `match` statement into `concat!`
    /// we would like to exclude any lines that are `delete`
    macro_rules! line_diff_flag_str {
        (insert, $commit_msg:literal, $line:expr) => {
            concat!($line, newline_literal!())
        };
        (delete, $commit_msg:literal, $line:expr) => {
            ""
        };
        (, $commit_msg:literal, $line:expr) => {
            concat!($line, newline_literal!())
        };
        ($any:tt, $commit_msg:literal, $line:expr) => {
            compile_error!(concat!(
                "expected `insert`, `delete` or nothing for commit ",
                $commit_msg,
                " line ",
                $line
            ))
        };
    }

    #[cfg(windows)]
    macro_rules! newline_literal {
        () => {
            "\r\n"
        };
    }
    #[cfg(not(windows))]
    macro_rules! newline_literal {
        () => {
            "\n"
        };
    }

    /// Helper macro to create a history of the same file being modified.
    macro_rules! assert_line_blame_progress {
        (
            $(
                // a unique identifier for the commit, other commits must not use this
                // If `no_commit` option is used, use the identifier of the previous commit
                $commit_msg:literal
                // must be `no_commit` if exists.
                // If exists, this block won't be committed
                $($no_commit:ident)? =>
                $(
                    // contents of a line in the file
                    $line:literal
                    // what commit identifier we are expecting for this line
                    $($expected:literal)?
                    // must be `insert` or `delete` if exists
                    // if exists, must be used with `no_commit`
                    // - `insert`: this line is added
                    // - `delete`: this line is deleted
                    $($line_diff:ident)?
                ),+
            );+
            $(;)?
        ) => {{
            use std::fs::OpenOptions;
            use std::io::Write;

            let repo = empty_git_repo();
            let file = repo.path().join("file.txt");
            File::create(&file).expect("could not create file");

            $(
                let file_content = concat!(
                    $(
                        line_diff_flag_str!($($line_diff)?, $commit_msg, $line),
                    )*
                );
                eprintln!("at commit {}:\n\n{file_content}", stringify!($commit_msg));

                let mut f = OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&file)
                    .unwrap();

                f.write_all(file_content.as_bytes()).unwrap();

                let should_commit = no_commit_flag!($($no_commit)?, $commit_msg);
                if should_commit {
                    create_commit_with_message(repo.path(), true, stringify!($commit_msg));
                }

                let mut line_number = 0;
                let mut added_lines = 0;
                let mut removed_lines = 0;

                $(
                    let line_diff_flag = line_diff_flag!($($line_diff)?, $commit_msg, $line);
                    #[allow(unused_assignments)]
                    match line_diff_flag {
                        LineDiff::Insert => added_lines += 1,
                        LineDiff::Delete => removed_lines += 1,
                        LineDiff::None => ()
                    }
                    // completely skip lines that are marked as `delete`
                    if line_diff_flag != LineDiff::Delete {
                        // if there is no $expected, then we don't care what blame_line returns
                        // because we won't show it to the user.
                        $(
                            let blame_result =
                                FileBlame::try_new(file.clone())
                                    .unwrap()
                                    .blame_for_line(line_number, added_lines, removed_lines)
                                    .commit_title;

                            assert_eq!(
                                blame_result,
                                Some(concat!(stringify!($expected), newline_literal!()).to_owned()),
                                "Blame mismatch\nat commit: {}\nat line: {}\nline contents: {}\nexpected commit: {}\nbut got commit: {}",
                                $commit_msg,
                                line_number,
                                file_content
                                    .lines()
                                    .nth(line_number.try_into().unwrap())
                                    .unwrap(),
                                stringify!($expected),
                                blame_result
                                    .as_ref()
                                    .map(|blame| blame.trim_end())
                                    .unwrap_or("<no commit>")
                            );
                        )?
                        #[allow(unused_assignments)]
                        {
                            line_number += 1;
                        }
                    }
                )*
            )*
        }};
    }

    // For some reasons the CI is failing on windows with the message "Commits not found".
    // The created temporary repository has no commits... But this is not an issue on unix.
    // There is nothing platform-specific in this implementation. This is a problem only
    // for tests on Windows.
    // As such it should be fine to disable this test in Windows.
    // As long as these tests pass on other platforms, on Windows it will work too.
    #[cfg(not(windows))]
    #[test]
    pub fn blamed_lines() {
        assert_line_blame_progress! {
            // initialize
            1 =>
                "fn main() {" 1,
                "" 1,
                "}" 1;
            // modifying a line works
            2 =>
                "fn main() {" 1,
                "  one" 2,
                "}" 1;
            // inserting a line works
            3 =>
                "fn main() {" 1,
                "  one" 2,
                "  two" 3,
                "}" 1;
            // deleting a line works
            4 =>
                "fn main() {" 1,
                "  two" 3,
                "}" 1;
            // when a line is inserted in-between the blame order is preserved
            4 no_commit =>
                "fn main() {" 1,
                "  hello world" insert,
                "  two" 3,
                "}" 1;
            // Having a bunch of random lines interspersed should not change which lines
            // have blame for which commits
            4 no_commit =>
                "  six" insert,
                "  three" insert,
                "fn main() {" 1,
                "  five" insert,
                "  four" insert,
                "  two" 3,
                "  five" insert,
                "  four" insert,
                "}" 1,
                "  five" insert,
                "  four" insert;
            // committing all of those insertions should recognize that they are
            // from the current commit, while still keeping the information about
            // previous commits
            5 =>
                "  six" 5,
                "  three" 5,
                "fn main() {" 1,
                "  five" 5,
                "  four" 5,
                "  two" 3,
                "  five" 5,
                "  four" 5,
                "}" 1,
                "  five" 5,
                "  four" 5;
            // several lines deleted
            5 no_commit =>
                "  six" 5,
                "  three" 5,
                "fn main() {" delete,
                "  five" delete,
                "  four" delete,
                "  two" delete,
                "  five" delete,
                "  four" 5,
                "}" 1,
                "  five" 5,
                "  four" 5;
            // committing the deleted changes
            6 =>
                "  six" 5,
                "  three" 5,
                "  four" 5,
                "}" 1,
                "  five" 5,
                "  four" 5;
            // mixing inserts with deletes
            6 no_commit =>
                "  six" delete,
                "  2" insert,
                "  three" delete,
                "  four" 5,
                "  1" insert,
                "}" 1,
                "]" insert,
                "  five" delete,
                "  four" 5;
            // committing inserts and deletes
            7 =>
                "  2" 7,
                "  four" 5,
                "  1" 7,
                "}" 1,
                "]" 7,
                "  four" 5;
        };
    }

    fn bob() -> LineBlame {
        LineBlame {
            commit_hash: Some("f14ab1cf".to_owned()),
            full_commit_hash: Some("f14ab1cf".to_owned()),
            author_name: Some("Bob TheBuilder".to_owned()),
            author_email: Some("bob@bob.com".to_owned()),
            commit_date: Some("2028-01-10".to_owned()),
            commit_datetime: Some("2028-01-10T12:00:00+00:00".to_owned()),
            commit_title: Some("feat!: extend house".to_owned()),
            commit_body: Some("BREAKING CHANGE: Removed door".to_owned()),
            time_stamp: None,
            time_ago: None,
        }
    }

    #[test]
    fn metadata_is_ready_before_rendering_and_survives_repository_removal() {
        use super::super::test::{create_commit_with_message, empty_git_repo};
        let repo = empty_git_repo();
        let path = repo.path().join("file.txt");
        std::fs::write(&path, "first\nsecond\n").unwrap();
        create_commit_with_message(repo.path(), true, "feat: cached metadata\n\nBody text");
        super::super::test::exec_git_cmd(
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/owner/repo.git",
            ],
            repo.path(),
        );
        let blame = FileBlame::try_new(path).unwrap();
        assert_eq!(blame.remote_urls(), &["https://github.com/owner/repo.git"]);
        assert!(std::sync::Arc::ptr_eq(
            blame.blame.get(&0).unwrap(),
            blame.blame.get(&1).unwrap()
        ));
        let before = blame.blame_for_line(0, 0, 0).details();
        assert_eq!(before.commit.as_ref().unwrap().len(), 40);
        assert_eq!(before.author.as_deref(), Some("author"));
        assert_eq!(
            before.datetime.as_deref(),
            Some("2000-01-01T00:00:00+00:00")
        );
        assert_eq!(before.title.as_deref(), Some("feat: cached metadata"));
        // Move only this test fixture's Git data out of the repository path.
        std::fs::rename(repo.path().join(".git"), repo.path().join("git-backup")).unwrap();
        assert_eq!(blame.blame_for_line(1, 0, 0).details(), before);
        assert_eq!(
            blame.blame_for_line(99, 0, 0).details(),
            crate::BlameDetails::default()
        );
        assert!(blame
            .blame_for_line(0, 0, 0)
            .parse_format("{datetime}")
            .contains("00:00:00"));
    }

    #[test]
    pub fn inline_blame_format_parser() {
        let format = "{author}, {date} • {title} • {commit}";

        assert_eq!(
            bob().parse_format(format),
            "Bob TheBuilder, 2028-01-10 • feat!: extend house • f14ab1cf".to_owned()
        );
        assert_eq!(
            LineBlame {
                author_name: None,
                ..bob()
            }
            .parse_format(format),
            "2028-01-10 • feat!: extend house • f14ab1cf".to_owned()
        );
        assert_eq!(
            LineBlame {
                commit_date: None,
                ..bob()
            }
            .parse_format(format),
            "Bob TheBuilder • feat!: extend house • f14ab1cf".to_owned()
        );
        assert_eq!(
            LineBlame {
                commit_title: None,
                author_email: None,
                ..bob()
            }
            .parse_format(format),
            "Bob TheBuilder, 2028-01-10 • f14ab1cf".to_owned()
        );
        assert_eq!(
            LineBlame {
                commit_hash: None,
                ..bob()
            }
            .parse_format(format),
            "Bob TheBuilder, 2028-01-10 • feat!: extend house".to_owned()
        );
        assert_eq!(
            LineBlame {
                commit_date: None,
                author_name: None,
                ..bob()
            }
            .parse_format(format),
            "feat!: extend house • f14ab1cf".to_owned()
        );
        assert_eq!(
            LineBlame {
                author_name: None,
                commit_title: None,
                ..bob()
            }
            .parse_format(format),
            "2028-01-10 • f14ab1cf".to_owned()
        );
    }
}
