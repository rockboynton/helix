//! Native Jujutsu support. Queries never commit a transaction, reconcile
//! operations, import/export Git refs, or run configured external tools.
//! Status uses a discarded working-copy snapshot: JJ may store immutable
//! file/tree objects, but neither operation heads, refs, nor working-copy state
//! are saved. Rename detection compares trees without writing hidden commits.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use etcetera::BaseStrategy;
use futures_executor::block_on;
use futures_util::{StreamExt, TryStreamExt};
use gix_jj as gix;
use jj_lib::backend::{CopyRecord, FileId, MergedTreeValue, TreeValue};
use jj_lib::commit::Commit;
use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};
use jj_lib::conflicts::{self, ConflictMaterializeOptions, MaterializedTreeValue};
use jj_lib::copies::{CopyOperation, CopyRecords};
use jj_lib::default_backend_factories::{
    default_backend_factories, default_working_copy_factories,
};
use jj_lib::fileset::{self, FilesetAliasesMap};
use jj_lib::gitignore::GitIgnoreFile;
use jj_lib::matchers::{EverythingMatcher, NothingMatcher};
use jj_lib::merged_tree::MergedTree;
use jj_lib::object_id::ObjectId;
use jj_lib::repo::{ReadonlyRepo, Repo};
use jj_lib::repo_path::{RepoPath, RepoPathBuf};
use jj_lib::revset::{self, RevsetAliasesMap, RevsetExtensions, SymbolResolver};
use jj_lib::settings::{HumanByteSize, UserSettings};
use jj_lib::ui_path::RepoPathUiConverter;
use jj_lib::working_copy::SnapshotOptions;
use jj_lib::workspace::Workspace;

use crate::FileChange;

// Match JJ's standard immutable-head aliases, while allowing user/repository
// revset-aliases to override these (including trunk() and immutable_heads()).
const DEFAULT_ALIASES: &str = r#"
[snapshot]
max-new-file-size = "1MiB"
auto-track = "all()"
[revset-aliases]
'trunk()' = '''latest(
  remote_bookmarks(exact:"main", exact:"origin") |
  remote_bookmarks(exact:"master", exact:"origin") |
  remote_bookmarks(exact:"trunk", exact:"origin") |
  remote_bookmarks(exact:"main", exact:"upstream") |
  remote_bookmarks(exact:"master", exact:"upstream") |
  remote_bookmarks(exact:"trunk", exact:"upstream") | root())'''
'builtin_immutable_heads()' = 'trunk() | tags() | untracked_remote_bookmarks() | untracked_remote_tags()'
'immutable_heads()' = 'builtin_immutable_heads()'
'immutable()' = '::(immutable_heads() | root())'
'mutable()' = '~immutable()'
"#;

fn load_config_path(config: &mut StackedConfig, source: ConfigSource, path: &Path) -> Result<()> {
    if path.is_dir() {
        config.load_dir(source, path)?;
    } else if path.is_file() {
        config.load_file(source, path)?;
    }
    Ok(())
}

// SecureConfig::maybe_load_config() migrates/creates config and metadata. Read
// the same ID/metadata layout without those side effects. A copied workspace
// must be repaired by JJ before inheriting another workspace's config.
fn load_local_config(
    config: &mut StackedConfig,
    source: ConfigSource,
    directory: &Path,
    id_name: &str,
    legacy_name: &str,
    secure_directory: Option<&Path>,
) -> Result<()> {
    let id_path = directory.join(id_name);
    match std::fs::read_to_string(&id_path) {
        Ok(id) => {
            anyhow::ensure!(
                id.len() == 20 && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "invalid JJ config ID in {}",
                id_path.display()
            );
            let Some(secure_directory) = secure_directory else {
                return Ok(());
            };
            let path = secure_directory.join(id);
            if !path.join("metadata.binpb").exists() {
                log::debug!("JJ secure configuration missing; leave config recovery to JJ");
                return Ok(());
            }
            let metadata = jj_lib::secure_config::read_metadata(&path)?;
            if jj_lib::secure_config::metadata_path(&metadata)? != Some(directory) {
                log::debug!("JJ secure configuration belongs to a different workspace; leave recovery to JJ");
                return Ok(());
            }
            load_config_path(config, source, &path.join("config.toml"))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            load_config_path(config, source, &directory.join(legacy_name))
        }
        Err(err) => Err(err).with_context(|| format!("failed to read {}", id_path.display())),
    }
}

fn settings(root: &Path) -> Result<UserSettings> {
    let home = etcetera::home_dir().ok();
    let mut config = StackedConfig::with_defaults();
    config.add_layer(ConfigLayer::parse(ConfigSource::Default, DEFAULT_ALIASES)?);
    if let Some(paths) = std::env::var_os("JJ_CONFIG") {
        for path in std::env::split_paths(&paths).filter(|path| !path.as_os_str().is_empty()) {
            load_config_path(&mut config, ConfigSource::User, &path)?;
        }
    } else if let Some(home) = &home {
        load_config_path(
            &mut config,
            ConfigSource::User,
            &home.join(".jjconfig.toml"),
        )?;
        if let Ok(strategy) = etcetera::choose_base_strategy() {
            let directory = strategy.config_dir().join("jj");
            load_config_path(
                &mut config,
                ConfigSource::User,
                &directory.join("config.toml"),
            )?;
            load_config_path(&mut config, ConfigSource::User, &directory.join("conf.d"))?;
        }
    }
    // Find the shared repo's config, including linked workspaces.
    let workspace = Workspace::load(
        &UserSettings::from_config_and_home_dir(StackedConfig::with_defaults(), home.clone())?,
        root,
        &default_backend_factories(),
        &default_working_copy_factories(),
    )?;
    let secure_directory = etcetera::choose_base_strategy()
        .ok()
        .map(|strategy| strategy.config_dir().join("jj"));
    load_local_config(
        &mut config,
        ConfigSource::Repo,
        workspace.repo_path(),
        "config-id",
        "config.toml",
        secure_directory
            .as_ref()
            .map(|path| path.join("repos"))
            .as_deref(),
    )?;
    load_local_config(
        &mut config,
        ConfigSource::Workspace,
        &root.join(".jj"),
        "workspace-config-id",
        "workspace-config.toml",
        secure_directory
            .as_ref()
            .map(|path| path.join("workspaces"))
            .as_deref(),
    )?;
    // Library queries must not invoke signing or filesystem-monitor programs.
    config.add_layer(ConfigLayer::parse(
        ConfigSource::CommandArg,
        "signing.backend = 'none'\nsigning.behavior = 'drop'\nfsmonitor.backend = 'none'",
    )?);
    let environment = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    let resolved = jj_lib::config::resolve(
        &config,
        &jj_lib::config::ConfigResolutionContext {
            home_dir: home.as_deref(),
            repo_path: Some(workspace.repo_path()),
            workspace_path: Some(workspace.workspace_root()),
            command: None,
            hostname: &std::env::var("HOSTNAME").unwrap_or_default(),
            environment: &environment,
        },
    )?;
    Ok(UserSettings::from_config_and_home_dir(resolved, home)?)
}

struct ContextData {
    workspace: Workspace,
    repo: Arc<ReadonlyRepo>,
    commit: Commit,
}

impl ContextData {
    async fn load(root: &Path) -> Result<Self> {
        let settings = settings(root)?;
        let workspace = Workspace::load(
            &settings,
            root,
            &default_backend_factories(),
            &default_working_copy_factories(),
        )
        .context("failed to load JJ workspace (unsupported backend or repository format)")?;
        let loader = workspace.repo_loader();
        // load_at_head() reconciles divergent operations as a side effect.
        // An editor must leave that recovery to JJ, not silently mutate history.
        let heads = loader.op_heads_store().get_op_heads().await?;
        anyhow::ensure!(
            heads.len() == 1,
            "JJ has concurrent operations; run jj to reconcile them"
        );
        let operation = loader.load_operation(&heads[0]).await?;
        let repo = loader.load_at(&operation).await?;
        let id = repo
            .view()
            .get_wc_commit_id(workspace.workspace_name())
            .context("JJ workspace has no working-copy commit")?;
        let commit = repo.store().get_commit_async(id).await?;
        Ok(Self {
            workspace,
            repo,
            commit,
        })
    }

    async fn snapshot(&mut self) -> Result<MergedTree> {
        let base_ignores = base_ignores(&self.repo, self.workspace.workspace_root())?;
        let settings = self.workspace.settings();
        let aliases = fileset_aliases(settings)?;
        let converter = path_converter(&self.workspace);
        let expression: String = settings.get("snapshot.auto-track")?;
        let matcher = fileset::parse(
            &mut fileset::FilesetDiagnostics::new(),
            &expression,
            &fileset::FilesetParseContext {
                aliases_map: &aliases,
                path_converter: &converter,
            },
        )?
        .to_matcher();
        let HumanByteSize(maximum) =
            settings.get_value_with("snapshot.max-new-file-size", HumanByteSize::try_from)?;
        let mut locked = self.workspace.working_copy().start_mutation().await?;
        anyhow::ensure!(
            locked.old_tree().tree_ids_and_labels() == self.commit.tree().tree_ids_and_labels(),
            "JJ working copy is stale; run jj workspace update-stale"
        );
        let options = SnapshotOptions {
            base_ignores,
            progress: None,
            start_tracking_matcher: matcher.as_ref(),
            force_tracking_matcher: &NothingMatcher,
            max_new_file_size: if maximum == 0 { u64::MAX } else { maximum },
        };
        let (tree, _) = locked.snapshot(&options).await?;
        // Deliberately drop the lock WITHOUT finish(): only immutable tree/file
        // objects are written. No WC state, commits, operations, or refs change.
        Ok(tree)
    }
}

fn base_ignores(repo: &ReadonlyRepo, root: &Path) -> Result<Arc<GitIgnoreFile>> {
    let mut ignores = GitIgnoreFile::empty();
    if let Ok(backend) = jj_lib::git::get_git_backend(repo.store()) {
        let git = backend.git_repo();
        let config = git.config_snapshot();
        let excludes = config
            .string("core.excludesFile")
            .and_then(|value| std::str::from_utf8(&value).ok().map(str::to_owned))
            .map(|path| {
                jj_lib::file_util::expand_home_path(&path, etcetera::home_dir().ok().as_deref())
            })
            .or_else(|| {
                std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .or_else(|| etcetera::home_dir().ok().map(|home| home.join(".config")))
                    .map(|path| path.join("git/ignore"))
            });
        if let Some(excludes) = excludes {
            ignores = ignores.chain_with_file(RepoPath::root(), root.join(excludes))?;
        }
        ignores = ignores.chain_with_file(
            RepoPath::root(),
            backend.git_repo_path().join("info/exclude"),
        )?;
    }
    Ok(ignores)
}

pub(crate) fn open_repo(root: &Path) -> Result<()> {
    block_on(ContextData::load(root)).map(|_| ())
}

pub(super) fn get_diff_base(root: &Path, file: &Path) -> Result<Vec<u8>> {
    block_on(async {
        let mut context = ContextData::load(root).await?;
        let path = RepoPathBuf::from_relative_path(
            file.strip_prefix(context.workspace.workspace_root())?,
        )?;
        let parent = context.commit.parent_tree(context.repo.as_ref()).await?;
        if !parent.path_value(&path).await?.is_absent() {
            return read_tree_value(&context, &parent, &path).await;
        }
        let mut after = context.commit.tree();
        let mut copies = copy_records(&context, &after).await?;
        if copies.for_target(&path).is_none() && file.exists() {
            after = context.snapshot().await?;
            copies = copy_records(&context, &after).await?;
        }
        if let Some(copy) = copies.for_target(&path) {
            if after.path_value(&copy.source).await?.is_absent() {
                return read_tree_value(&context, &parent, &copy.source).await;
            }
        }
        // Added/copied files have an empty base, unlike renamed files.
        Ok(Vec::new())
    })
}

async fn read_tree_value(
    context: &ContextData,
    tree: &MergedTree,
    path: &RepoPath,
) -> Result<Vec<u8>> {
    let value = tree.path_value(path).await?;
    match conflicts::materialize_tree_value(context.repo.store(), path, value, tree.labels())
        .await?
    {
        MaterializedTreeValue::Absent => Ok(Vec::new()),
        MaterializedTreeValue::File(mut file) => Ok(file.read_all(path).await?),
        MaterializedTreeValue::Symlink { target, .. } => Ok(target.into_bytes()),
        MaterializedTreeValue::FileConflict(conflict) => {
            let options = ConflictMaterializeOptions {
                marker_style: context
                    .workspace
                    .settings()
                    .get("ui.conflict-marker-style")?,
                marker_len: None,
                merge: context.repo.store().merge_options().clone(),
            };
            Ok(conflicts::materialize_merge_result_to_bytes(
                &conflict.contents,
                &conflict.labels,
                &options,
            )
            .to_vec())
        }
        MaterializedTreeValue::AccessDenied(err) => Err(anyhow::anyhow!(err)),
        _ => anyhow::bail!("JJ diff base is not a file or file conflict: {path:?}"),
    }
}

pub(crate) fn get_current_head_name(root: &Path) -> Result<Arc<ArcSwap<Box<str>>>> {
    block_on(async {
        let context = ContextData::load(root).await?;
        let name = head_name(&context).await?;
        Ok(Arc::new(ArcSwap::from_pointee(name.into_boxed_str())))
    })
}

async fn resolve_revisions(
    context: &ContextData,
    expression: &str,
) -> Result<Vec<jj_lib::backend::CommitId>> {
    resolve_revisions_bounded(context, expression, usize::MAX).await
}

async fn resolve_revisions_bounded(
    context: &ContextData,
    expression: &str,
    limit: usize,
) -> Result<Vec<jj_lib::backend::CommitId>> {
    let settings = context.workspace.settings();
    let mut aliases = RevsetAliasesMap::new();
    for key in settings.config().table_keys("revset-aliases") {
        let value: String = settings.get(["revset-aliases", key])?;
        aliases.insert(key, value, None)?;
    }
    let filesets = fileset_aliases(settings)?;
    let extensions = RevsetExtensions::default();
    let converter = path_converter(&context.workspace);
    let parse_context = revset::RevsetParseContext {
        aliases_map: &aliases,
        local_variables: HashMap::new(),
        user_email: settings.user_email(),
        date_pattern_context: chrono::Local::now().into(),
        default_ignored_remote: jj_lib::git::get_git_backend(context.repo.store())
            .ok()
            .map(|_| jj_lib::ref_name::RemoteName::new("git")),
        fileset_aliases_map: &filesets,
        extensions: &extensions,
        workspace: Some(revset::RevsetWorkspaceContext {
            path_converter: &converter,
            workspace_name: context.workspace.workspace_name(),
        }),
    };
    let expression = revset::parse(
        &mut revset::RevsetDiagnostics::new(),
        expression,
        &parse_context,
    )?;
    let resolver = SymbolResolver::new(context.repo.as_ref(), extensions.symbol_resolvers());
    let revisions = expression
        .resolve_user_expression(context.repo.as_ref(), &resolver)?
        .evaluate()?;
    let mut ids = Vec::new();
    let mut stream = revisions.stream();
    while ids.len() < limit {
        let Some(revision) = stream.next().await else {
            break;
        };
        ids.push(revision?);
    }
    Ok(ids)
}

async fn head_name(context: &ContextData) -> Result<String> {
    let revisions = resolve_revisions(context, "@ | (immutable_heads()::@ & bookmarks())").await?;
    let exact = bookmark_names(context, context.commit.id());
    let id = format!("{:.8}", context.commit.change_id());
    if !exact.is_empty() {
        return Ok(format!("{id} ({})", exact.join(" ")));
    }
    let mut ancestors = Vec::new();
    for revision in revisions {
        if &revision != context.commit.id() {
            ancestors.extend(bookmark_names(context, &revision));
        }
    }
    Ok(if ancestors.is_empty() {
        id
    } else {
        format!("{id} [{}]", ancestors.join(" "))
    })
}

pub(crate) fn revisions(
    root: &Path,
    expression: &str,
    limit: usize,
) -> Result<Vec<crate::JjRevision>> {
    block_on(async {
        let context = ContextData::load(root).await?;
        let ids = resolve_revisions_bounded(&context, expression, limit.min(500)).await?;
        let mut result = Vec::new();
        for id in ids {
            let commit = context.repo.store().get_commit_async(&id).await?;
            result.push(crate::JjRevision {
                commit_id: id.hex(),
                change_id: format!("{:.8}", commit.change_id()),
                description: commit
                    .description()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
                author: commit.author().name.clone(),
                bookmarks: bookmark_names(&context, &id),
                working_copy: id == *context.commit.id(),
                parents: commit.parent_ids().iter().map(ObjectId::hex).collect(),
            });
        }
        Ok(result)
    })
}

pub(crate) fn revision_files(root: &Path, commit: &str) -> Result<Vec<crate::RevisionFile>> {
    // Accept only full immutable commit IDs, never expressions which could
    // change meaning between selecting a log row and loading its snapshots.
    anyhow::ensure!(
        commit.len() >= 32 && commit.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "expected a full JJ commit ID"
    );
    block_on(async {
        let mut context = ContextData::load(root).await?;
        let ids = resolve_revisions_bounded(&context, &format!("commit_id({commit})"), 2).await?;
        anyhow::ensure!(
            ids.len() == 1 && ids[0].hex() == commit,
            "revision is unavailable or ambiguous"
        );
        context.commit = context.repo.store().get_commit_async(&ids[0]).await?;
        let before = context.commit.parent_tree(context.repo.as_ref()).await?;
        let after = context.commit.tree();
        let copies = copy_records(&context, &after).await?;
        let mut stream = before.diff_stream_with_copies(&after, &EverythingMatcher, &copies);
        let mut files = Vec::new();
        while let Some(entry) = stream.next().await {
            let values = entry.values?;
            if !file_value(&values.before) || !file_value(&values.after) {
                continue;
            }
            let before_path = entry
                .path
                .source
                .as_ref()
                .map_or(&entry.path.target, |(path, _)| path);
            let status = if matches!(entry.path.source, Some((_, CopyOperation::Rename))) {
                "R"
            } else if values.after.is_absent() {
                "D"
            } else if values.before.is_absent() {
                "A"
            } else if !values.after.is_resolved() {
                "!"
            } else {
                "M"
            };
            files.push(crate::RevisionFile {
                path: entry.path.target.to_fs_path(Path::new(""))?,
                before: read_tree_value(&context, &before, before_path).await?,
                after: read_tree_value(&context, &after, &entry.path.target).await?,
                status,
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
    })
}

fn bookmark_names(context: &ContextData, id: &jj_lib::backend::CommitId) -> Vec<String> {
    let mut names = Vec::new();
    for (name, targets) in context.repo.view().bookmarks() {
        let local = targets.local_target;
        if local.present_adds().any(|target| target == id) {
            let modified = targets
                .remote_refs
                .iter()
                .any(|(_, remote)| remote.is_tracked() && &remote.target != local);
            let suffix = if !local.is_resolved() {
                "??"
            } else if modified {
                "*"
            } else {
                ""
            };
            names.push(format!("{}{suffix}", revset::format_symbol(name.as_str())));
        }
        for (remote_name, remote) in targets.remote_refs {
            // JJ's bookmarks template hides synchronized tracked remote refs.
            if remote.target.present_adds().any(|target| target == id)
                && (!remote.is_tracked() || &remote.target != local)
            {
                let suffix = if remote.target.is_resolved() {
                    ""
                } else {
                    "??"
                };
                names.push(format!(
                    "{}@{}{suffix}",
                    revset::format_symbol(name.as_str()),
                    revset::format_symbol(remote_name.as_str())
                ));
            }
        }
    }
    names
}

fn file_value(value: &MergedTreeValue) -> bool {
    value.iter().all(|term| {
        matches!(
            term,
            None | Some(TreeValue::File { .. } | TreeValue::Symlink(_))
        )
    })
}

fn path_converter(workspace: &Workspace) -> RepoPathUiConverter {
    RepoPathUiConverter::Fs {
        cwd: workspace.workspace_root().into(),
        base: workspace.workspace_root().into(),
    }
}

fn fileset_aliases(settings: &UserSettings) -> Result<FilesetAliasesMap> {
    let mut aliases = FilesetAliasesMap::new();
    for key in settings.config().table_keys("fileset-aliases") {
        let value: String = settings.get(["fileset-aliases", key])?;
        aliases.insert(key, value, None)?;
    }
    Ok(aliases)
}

async fn copy_records(context: &ContextData, tree: &MergedTree) -> Result<CopyRecords> {
    let mut copies = CopyRecords::default();
    if let Ok(backend) = jj_lib::git::get_git_backend(context.repo.store()) {
        fn path_from_bytes(bytes: &[u8]) -> gix::ExnResult<RepoPathBuf> {
            let text = std::str::from_utf8(bytes).map_err(|err| gix::Exn::new(err).erased())?;
            RepoPathBuf::from_internal_string(text).map_err(|err| gix::Exn::new(err).erased())
        }
        let Some(after_id) = tree.tree_ids().as_resolved() else {
            return Ok(copies);
        };
        let git = backend.git_repo();
        let after = git.find_tree(
            gix::hash::ObjectId::try_from(after_id.as_bytes()).map_err(|err| err.into_error())?,
        )?;
        // Like JJ's Git backend, use an empty attribute/filter pipeline. Do not
        // run a repository's configured textconv/filter/external diff commands.
        let attributes = gix::worktree::Stack::new(
            Path::new(""),
            gix::worktree::stack::State::AttributesStack(Default::default()),
            gix::worktree::glob::pattern::Case::Sensitive,
            Vec::new(),
            Vec::new(),
        );
        let filter = gix::diff::blob::Pipeline::new(
            Default::default(),
            gix::filter::plumbing::Pipeline::new(git.command_context()?, Default::default()),
            Vec::new(),
            Default::default(),
        );
        let mut cache = gix::diff::blob::Platform::new(
            Default::default(),
            filter,
            gix::diff::blob::pipeline::Mode::ToGit,
            attributes,
        );
        for parent in context.commit.parents().await? {
            let parent_tree = parent.tree();
            let Some(before_id) = parent_tree.tree_ids().as_resolved() else {
                continue;
            };
            let before = git.find_tree(
                gix::hash::ObjectId::try_from(before_id.as_bytes())
                    .map_err(|err| err.into_error())?,
            )?;
            let mut records = Vec::new();
            before
                .changes()?
                .options(|options| {
                    options
                        .track_path()
                        .track_rewrites(Some(gix::diff::Rewrites {
                            copies: Some(gix::diff::rewrites::Copies {
                                source: gix::diff::rewrites::CopySource::FromSetOfModifiedFiles,
                                percentage: Some(0.5),
                            }),
                            percentage: Some(0.5),
                            limit: 1000,
                            track_empty: false,
                        }));
                })
                .for_each_to_obtain_tree_with_cache(&after, &mut cache, |change| {
                    if let gix::object::tree::diff::Change::Rewrite {
                        source_location,
                        source_entry_mode,
                        source_id,
                        entry_mode,
                        location,
                        ..
                    } = change
                    {
                        if source_entry_mode.is_blob() && entry_mode.is_blob() {
                            records.push(CopyRecord {
                                source: path_from_bytes(source_location)?,
                                target: path_from_bytes(location)?,
                                source_file: FileId::from_bytes(source_id.as_bytes()),
                                source_commit: parent.id().clone(),
                                target_commit: context.commit.id().clone(),
                            });
                        }
                    }
                    Ok(gix::object::tree::diff::Action::Continue(()))
                })?;
            copies.add_records(records);
        }
    } else {
        for parent in context.commit.parent_ids() {
            let records = context
                .repo
                .store()
                .get_copy_records(None, parent, context.commit.id())?
                .try_collect::<Vec<_>>()
                .await?;
            copies.add_records(records);
        }
    }
    Ok(copies)
}

pub(crate) fn for_each_changed_file(
    root: &Path,
    callback: impl Fn(Result<FileChange>) -> bool,
) -> Result<()> {
    block_on(async {
        let mut context = ContextData::load(root).await?;
        let before = context.commit.parent_tree(context.repo.as_ref()).await?;
        let after = context.snapshot().await?;
        let mut conflicted = HashSet::new();
        for (path, value) in after.conflicts() {
            value?;
            conflicted.insert(path.clone());
            if !callback(Ok(FileChange::Conflict {
                path: path.to_fs_path(Path::new(""))?,
            })) {
                return Ok(());
            }
        }
        let copies = copy_records(&context, &after).await?;
        let mut stream = before.diff_stream_with_copies(&after, &EverythingMatcher, &copies);
        while let Some(entry) = stream.next().await {
            if conflicted.contains(&entry.path.target) {
                continue;
            }
            let values = entry.values?;
            if !file_value(&values.before) || !file_value(&values.after) {
                continue;
            }
            let path = entry.path.target.to_fs_path(Path::new(""))?;
            let change = if let Some((ref source, CopyOperation::Rename)) = entry.path.source {
                FileChange::Renamed {
                    from_path: source.to_fs_path(Path::new(""))?,
                    to_path: path,
                }
            } else if values.after.is_absent() {
                FileChange::Deleted { path }
            } else if values.before.is_absent() || entry.path.source.is_some() {
                FileChange::Untracked { path }
            } else {
                FileChange::Modified { path }
            };
            if !callback(Ok(change)) {
                break;
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jj_lib::backend::{CommitId, CopyId};
    use jj_lib::merge::Merge;
    use jj_lib::merged_tree_builder::MergedTreeBuilder;
    use jj_lib::op_store::{RefTarget, RemoteRef, RemoteRefState};
    use jj_lib::ref_name::{RefName, RemoteName, RemoteRefSymbol, WorkspaceNameBuf};
    use std::cell::RefCell;

    #[test]
    fn revision_log_and_historical_snapshots_are_bounded_and_leave_checkout_untouched() {
        for git in [false, true] {
            let mut fixture = Fixture::new(git);
            let baseline = fixture.baseline(&[("nested/file.rs", b"fn old() {}\n")]);
            let tree = fixture.tree(&[(
                "nested/file.rs",
                fixture.file("nested/file.rs", b"fn new() {}\n"),
            )]);
            let current = fixture.checkout(vec![fixture.current().id().clone()], tree);
            // Unsnapshotted checkout contents must not leak into a historical commit view.
            std::fs::write(
                fixture.root.join("nested/file.rs"),
                "unsaved working copy\n",
            )
            .unwrap();
            let heads = block_on(fixture.repo.loader().op_heads_store().get_op_heads()).unwrap();
            let rows = revisions(&fixture.root, "ancestors(@)", 2).unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].commit_id, current.id().hex());
            assert!(rows[0].working_copy);
            assert!(!rows[1].working_copy);
            assert_eq!(rows[0].parents, vec![rows[1].commit_id.clone()]);
            assert!(revisions(&fixture.root, "all()", 0).unwrap().is_empty());
            assert!(revisions(&fixture.root, "invalid_function()", 10).is_err());
            let files = revision_files(&fixture.root, &current.id().hex()).unwrap();
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].path, Path::new("nested/file.rs"));
            assert_eq!(files[0].before, b"fn old() {}\n");
            assert_eq!(files[0].after, b"fn new() {}\n");
            let added = revision_files(&fixture.root, &baseline.id().hex()).unwrap();
            assert_eq!(added[0].status, "A");
            assert!(added[0].before.is_empty());
            assert!(revision_files(&fixture.root, "@ | all()").is_err());
            assert_eq!(
                std::fs::read(fixture.root.join("nested/file.rs")).unwrap(),
                b"unsaved working copy\n"
            );
            assert_eq!(
                block_on(fixture.repo.loader().op_heads_store().get_op_heads()).unwrap(),
                heads
            );
            assert_eq!(
                block_on(ContextData::load(&fixture.root))
                    .unwrap()
                    .commit
                    .id(),
                current.id()
            );
        }
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        root: PathBuf,
        workspace: Workspace,
        repo: Arc<ReadonlyRepo>,
    }

    impl Fixture {
        fn new(git: bool) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().canonicalize().unwrap();
            let settings =
                UserSettings::from_config_and_home_dir(StackedConfig::with_defaults(), None)
                    .unwrap();
            let (workspace, repo) = block_on(async {
                if git {
                    Workspace::init_internal_git(&settings, &root, "sha1".parse().unwrap())
                        .await
                        .unwrap()
                } else {
                    Workspace::init_simple(&settings, &root).await.unwrap()
                }
            });
            Self {
                _directory: directory,
                root,
                workspace,
                repo,
            }
        }

        fn current(&self) -> Commit {
            self.repo
                .store()
                .get_commit(
                    self.repo
                        .view()
                        .get_wc_commit_id(self.workspace.workspace_name())
                        .unwrap(),
                )
                .unwrap()
        }

        fn file(&self, path: &str, bytes: &[u8]) -> MergedTreeValue {
            let path = RepoPathBuf::from_internal_string(path).unwrap();
            let id = block_on(self.repo.store().write_file(&path, &mut &*bytes)).unwrap();
            Merge::normal(TreeValue::File {
                id,
                executable: false,
                copy_id: CopyId::placeholder(),
            })
        }

        fn tree(&self, entries: &[(&str, MergedTreeValue)]) -> MergedTree {
            let mut builder = MergedTreeBuilder::new(self.current().tree());
            for (path, value) in entries {
                builder.set_or_remove(
                    RepoPathBuf::from_internal_string(*path).unwrap(),
                    value.clone(),
                );
            }
            block_on(builder.write_tree()).unwrap()
        }

        fn checkout(&mut self, parents: Vec<CommitId>, tree: MergedTree) -> Commit {
            block_on(async {
                let mut tx = self.repo.start_transaction();
                let commit = tx
                    .repo_mut()
                    .new_commit(parents, tree)
                    .write()
                    .await
                    .unwrap();
                tx.repo_mut()
                    .edit(self.workspace.workspace_name().to_owned(), &commit)
                    .await
                    .unwrap();
                tx.repo_mut().rebase_descendants().await.unwrap();
                self.repo = tx.commit("test checkout").await.unwrap();
                self.workspace
                    .check_out(self.repo.op_id().clone(), None, &commit)
                    .await
                    .unwrap();
                commit
            })
        }

        fn baseline(&mut self, entries: &[(&str, &[u8])]) -> Commit {
            let entries: Vec<_> = entries
                .iter()
                .map(|(path, bytes)| (*path, self.file(path, bytes)))
                .collect();
            let tree = self.tree(&entries);
            let parent = self.checkout(
                vec![self.repo.store().root_commit_id().clone()],
                tree.clone(),
            );
            self.checkout(vec![parent.id().clone()], tree);
            parent
        }

        fn bookmark(&mut self, name: &str, target: RefTarget) {
            let mut tx = self.repo.start_transaction();
            tx.repo_mut()
                .set_local_bookmark_target(RefName::new(name), target);
            self.repo = block_on(tx.commit("test bookmark")).unwrap();
        }

        fn changes(&self) -> Vec<FileChange> {
            let changes = RefCell::new(Vec::new());
            for_each_changed_file(&self.root, |change| {
                changes.borrow_mut().push(change.unwrap());
                true
            })
            .unwrap();
            changes.into_inner()
        }
    }

    #[test]
    fn native_git_diff_base_and_unsnapshotted_status() {
        let mut fixture = Fixture::new(true);
        fixture.baseline(&[("source.txt", b"original\n"), ("removed.txt", b"remove\n")]);
        std::fs::write(fixture.root.join("source.txt"), b"modified\n").unwrap();
        let added_name = if cfg!(windows) {
            "--new'file.txt"
        } else {
            "--new\n'file.txt"
        };
        std::fs::write(fixture.root.join(added_name), b"new\n").unwrap();
        std::fs::remove_file(fixture.root.join("removed.txt")).unwrap();
        assert_eq!(
            get_diff_base(&fixture.root, &fixture.root.join("source.txt")).unwrap(),
            b"original\n"
        );
        assert_eq!(
            get_diff_base(&fixture.root, &fixture.root.join("removed.txt")).unwrap(),
            b"remove\n"
        );
        assert!(get_diff_base(&fixture.root, &fixture.root.join(added_name))
            .unwrap()
            .is_empty());
        let changes = fixture.changes();
        assert_eq!(changes.len(), 3);
        assert!(changes.contains(&FileChange::Modified {
            path: "source.txt".into()
        }));
        assert!(changes.contains(&FileChange::Deleted {
            path: "removed.txt".into()
        }));
        assert!(changes.contains(&FileChange::Untracked {
            path: added_name.into()
        }));
    }

    #[test]
    fn native_simple_backend_and_unchanged_base() {
        let mut fixture = Fixture::new(false);
        fixture.baseline(&[("source.txt", b"original\n")]);
        assert_eq!(
            get_diff_base(&fixture.root, &fixture.root.join("source.txt")).unwrap(),
            b"original\n"
        );
        assert!(fixture.changes().is_empty());
        std::fs::write(fixture.root.join("source.txt"), b"modified\n").unwrap();
        assert_eq!(
            fixture.changes(),
            vec![FileChange::Modified {
                path: "source.txt".into()
            }]
        );
    }

    #[test]
    fn status_queries_preserve_operation_heads_and_working_copy_state() {
        let mut fixture = Fixture::new(true);
        fixture.baseline(&[("source.txt", b"original\n")]);
        std::fs::write(fixture.root.join("source.txt"), b"modified\n").unwrap();
        let metadata = |root: &Path| {
            fn collect(root: &Path, entries: &mut Vec<(PathBuf, Vec<u8>)>) {
                for entry in std::fs::read_dir(root).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        collect(&path, entries);
                    } else {
                        entries.push((path.clone(), std::fs::read(&path).unwrap()));
                    }
                }
            }
            let mut entries = Vec::new();
            collect(root, &mut entries);
            entries.sort();
            entries
        };
        let heads_path = fixture.workspace.repo_path().join("op_heads");
        let state_path = fixture.root.join(".jj/working_copy");
        let heads = metadata(&heads_path);
        let state = metadata(&state_path);
        let refs_path = jj_lib::git::get_git_backend(fixture.repo.store())
            .unwrap()
            .git_repo_path()
            .join("refs");
        let refs = metadata(&refs_path);
        for _ in 0..2 {
            assert_eq!(fixture.changes().len(), 1);
            get_current_head_name(&fixture.root).unwrap();
            get_diff_base(&fixture.root, &fixture.root.join("source.txt")).unwrap();
        }
        assert_eq!(metadata(&heads_path), heads);
        assert_eq!(metadata(&state_path), state);
        assert_eq!(metadata(&refs_path), refs);
        assert_eq!(
            std::fs::read(fixture.root.join("source.txt")).unwrap(),
            b"modified\n"
        );
    }

    #[test]
    fn native_status_detects_unsnapshotted_rename() {
        let mut fixture = Fixture::new(true);
        fixture.baseline(&[("old.txt", b"original text for rename\n")]);
        std::fs::rename(fixture.root.join("old.txt"), fixture.root.join("new.txt")).unwrap();
        assert_eq!(
            fixture.changes(),
            vec![FileChange::Renamed {
                from_path: "old.txt".into(),
                to_path: "new.txt".into()
            }]
        );
        assert_eq!(
            get_diff_base(&fixture.root, &fixture.root.join("new.txt")).unwrap(),
            b"original text for rename\n"
        );
        // Once the rename is recorded in @, the gutter uses the source's base.
        let tree = fixture.tree(&[
            ("old.txt", Merge::absent()),
            (
                "new.txt",
                fixture.file("new.txt", b"original text for rename\n"),
            ),
        ]);
        fixture.checkout(fixture.current().parent_ids().to_vec(), tree);
        assert_eq!(
            get_diff_base(&fixture.root, &fixture.root.join("new.txt")).unwrap(),
            b"original text for rename\n"
        );
    }

    #[test]
    fn native_status_honors_ignores_and_snapshot_settings() {
        let mut fixture = Fixture::new(true);
        fixture.baseline(&[
            (".gitignore", b"ignored.txt\n"),
            ("tracked.txt", b"tracked\n"),
        ]);
        std::fs::write(fixture.root.join("ignored.txt"), b"ignored\n").unwrap();
        std::fs::write(fixture.root.join("new.txt"), b"new\n").unwrap();
        assert_eq!(
            fixture.changes(),
            vec![FileChange::Untracked {
                path: "new.txt".into()
            }]
        );
        std::fs::write(
            fixture.workspace.repo_path().join("config.toml"),
            "snapshot.auto-track = 'none()'\n",
        )
        .unwrap();
        assert!(fixture.changes().is_empty());
        std::fs::write(fixture.root.join("tracked.txt"), b"changed\n").unwrap();
        assert_eq!(
            fixture.changes(),
            vec![FileChange::Modified {
                path: "tracked.txt".into()
            }]
        );
    }

    #[test]
    fn head_has_exact_and_ancestor_bookmarks_and_remote_marker() {
        let mut fixture = Fixture::new(true);
        let parent = fixture.baseline(&[("file.txt", b"content\n")]);
        let short = format!("{:.8}", fixture.current().change_id());
        assert_eq!(
            get_current_head_name(&fixture.root)
                .unwrap()
                .load()
                .as_ref()
                .as_ref(),
            short
        );
        fixture.bookmark("parent", RefTarget::normal(parent.id().clone()));
        assert_eq!(
            get_current_head_name(&fixture.root)
                .unwrap()
                .load()
                .as_ref()
                .as_ref(),
            format!("{short} [parent]")
        );
        fixture.bookmark("local", RefTarget::normal(fixture.current().id().clone()));
        let mut tx = fixture.repo.start_transaction();
        tx.repo_mut().set_remote_bookmark(
            RemoteRefSymbol {
                name: RefName::new("local"),
                remote: RemoteName::new("origin"),
            },
            RemoteRef {
                target: RefTarget::normal(parent.id().clone()),
                state: RemoteRefState::Tracked,
            },
        );
        fixture.repo = block_on(tx.commit("test remote")).unwrap();
        assert_eq!(
            get_current_head_name(&fixture.root)
                .unwrap()
                .load()
                .as_ref()
                .as_ref(),
            format!("{short} (local*)")
        );
    }

    #[test]
    fn head_honors_scoped_immutable_head_alias() {
        let mut fixture = Fixture::new(true);
        let parent = fixture.baseline(&[("file.txt", b"content\n")]);
        fixture.bookmark("parent", RefTarget::normal(parent.id().clone()));
        let config = "[[--scope]]\n--when.platforms = ['unix', 'windows']\n[--scope.revset-aliases]\n'immutable_heads()' = '@'\n";
        std::fs::write(fixture.workspace.repo_path().join("config.toml"), config).unwrap();
        let short = format!("{:.8}", fixture.current().change_id());
        assert_eq!(
            get_current_head_name(&fixture.root)
                .unwrap()
                .load()
                .as_ref()
                .as_ref(),
            short
        );
    }

    #[test]
    fn remote_only_head_bookmark_is_not_lost_or_duplicated() {
        let mut fixture = Fixture::new(true);
        let current = fixture.current();
        let short = format!("{:.8}", current.change_id());
        let mut tx = fixture.repo.start_transaction();
        tx.repo_mut().set_remote_bookmark(
            RemoteRefSymbol {
                name: RefName::new("remote-only"),
                remote: RemoteName::new("origin"),
            },
            RemoteRef {
                target: RefTarget::normal(current.id().clone()),
                state: RemoteRefState::New,
            },
        );
        fixture.repo = block_on(tx.commit("test remote bookmark")).unwrap();
        assert_eq!(
            get_current_head_name(&fixture.root)
                .unwrap()
                .load()
                .as_ref()
                .as_ref(),
            format!("{short} (remote-only@origin)")
        );
    }

    #[test]
    fn registry_uses_native_jj_and_preserves_workspace_trust() {
        let mut fixture = Fixture::new(true);
        fixture.baseline(&[("file.txt", b"native base\0\xff\n")]);
        let path = fixture.root.join("file.txt");
        let mut registry = crate::DiffProviderRegistry::default();
        registry.add(&path, false);
        assert!(registry.get_diff_base(&path).is_none());
        registry.add(&path, true);
        assert_eq!(
            registry.get_diff_base(&path).unwrap(),
            b"native base\0\xff\n"
        );
        assert!(registry.get_current_head_name(&path).is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn changed_file_picker_roots_jj_paths_from_a_nested_directory() {
        let fixture = Fixture::new(true);
        let nested = fixture.root.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let file = nested.join("changed.txt");
        std::fs::write(&file, "changed\n").unwrap();
        let mut registry = crate::DiffProviderRegistry::default();
        registry.add(&nested, true);
        assert_eq!(registry.review_root(&nested), Some(fixture.root.clone()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        registry.for_each_changed_file(Arc::from(nested), move |change| tx.send(change).is_ok());
        let change = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(change.path(), file);
        assert!(change.path().is_absolute());
    }

    #[test]
    fn conflict_status_and_merged_parent_base() {
        let mut fixture = Fixture::new(true);
        let base = fixture.baseline(&[("file.txt", b"base\n")]);
        let left_tree = fixture.tree(&[("file.txt", fixture.file("file.txt", b"left\n"))]);
        let left = fixture.checkout(vec![base.id().clone()], left_tree);
        let right_tree = fixture.tree(&[("file.txt", fixture.file("file.txt", b"right\n"))]);
        let right = fixture.checkout(vec![base.id().clone()], right_tree);
        let merged = block_on(jj_lib::rewrite::merge_commit_trees(
            fixture.repo.as_ref(),
            &[left.clone(), right.clone()],
        ))
        .unwrap();
        fixture.checkout(vec![left.id().clone(), right.id().clone()], merged);
        let base_bytes = get_diff_base(&fixture.root, &fixture.root.join("file.txt")).unwrap();
        assert!(String::from_utf8(base_bytes).unwrap().contains("<<<<<<<"));
        assert_eq!(
            fixture.changes(),
            vec![FileChange::Conflict {
                path: "file.txt".into()
            }]
        );
        std::fs::write(fixture.root.join("file.txt"), b"resolved\n").unwrap();
        assert_eq!(
            fixture.changes(),
            vec![FileChange::Modified {
                path: "file.txt".into()
            }]
        );
    }

    #[test]
    fn linked_workspace_reads_its_own_commit() {
        let mut fixture = Fixture::new(true);
        fixture.baseline(&[("main.txt", b"main\n")]);
        let extra = fixture.root.join("linked");
        std::fs::create_dir(&extra).unwrap();
        let (workspace, repo) = block_on(Workspace::init_workspace_with_existing_repo(
            &extra,
            fixture.workspace.repo_path(),
            &fixture.repo,
            &*jj_lib::default_backend_factories::default_working_copy_factory(),
            WorkspaceNameBuf::from("linked"),
        ))
        .unwrap();
        let id = repo
            .view()
            .get_wc_commit_id(workspace.workspace_name())
            .unwrap();
        let commit = repo.store().get_commit(id).unwrap();
        assert!(get_diff_base(&extra, &extra.join("main.txt"))
            .unwrap()
            .is_empty());
        assert_eq!(
            get_current_head_name(&extra)
                .unwrap()
                .load()
                .as_ref()
                .as_ref(),
            format!("{:.8}", commit.change_id())
        );
    }

    #[test]
    fn invalid_repository_and_outside_path_return_errors() {
        let directory = tempfile::tempdir().unwrap();
        assert!(open_repo(directory.path()).is_err());
        std::fs::create_dir(directory.path().join(".jj")).unwrap();
        assert!(open_repo(directory.path()).is_err());
        let fixture = Fixture::new(true);
        assert!(get_diff_base(&fixture.root, &directory.path().join("outside.txt")).is_err());
    }

    #[test]
    fn status_callback_can_stop_early() {
        let fixture = Fixture::new(true);
        std::fs::write(fixture.root.join("one.txt"), b"one\n").unwrap();
        std::fs::write(fixture.root.join("two.txt"), b"two\n").unwrap();
        let calls = std::cell::Cell::new(0);
        for_each_changed_file(&fixture.root, |_| {
            calls.set(calls.get() + 1);
            false
        })
        .unwrap();
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn secure_config_is_read_without_migration_and_copied_config_is_ignored() {
        use prost::Message;
        let directory = tempfile::tempdir().unwrap();
        let repo = directory.path().join("repo");
        let config_root = directory.path().join("configs");
        let id = "0123456789abcdef0123";
        let config_dir = config_root.join(id);
        std::fs::create_dir(&repo).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(repo.join("config-id"), id).unwrap();
        let metadata = jj_lib::protos::secure_config::ConfigMetadata {
            path: Some(jj_lib::file_util::path_to_bytes(&repo).unwrap().to_vec()),
        }
        .encode_to_vec();
        std::fs::write(config_dir.join("metadata.binpb"), &metadata).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "user.name = 'native config'\n",
        )
        .unwrap();
        let mut config = StackedConfig::with_defaults();
        load_local_config(
            &mut config,
            ConfigSource::Repo,
            &repo,
            "config-id",
            "config.toml",
            Some(&config_root),
        )
        .unwrap();
        assert_eq!(config.get::<String>("user.name").unwrap(), "native config");
        assert_eq!(
            std::fs::read(config_dir.join("metadata.binpb")).unwrap(),
            metadata
        );
        assert!(!repo.join("config.toml").exists());
        let copied = directory.path().join("copy");
        std::fs::create_dir(&copied).unwrap();
        std::fs::write(copied.join("config-id"), id).unwrap();
        let mut config = StackedConfig::with_defaults();
        load_local_config(
            &mut config,
            ConfigSource::Repo,
            &copied,
            "config-id",
            "config.toml",
            Some(&config_root),
        )
        .unwrap();
        assert_eq!(config.get::<String>("user.name").unwrap(), "");
        assert_eq!(
            std::fs::read_to_string(copied.join("config-id")).unwrap(),
            id
        );
    }

    #[test]
    fn divergent_operations_are_not_reconciled_by_queries() {
        let fixture = Fixture::new(true);
        let mut first = fixture.repo.start_transaction();
        let mut second = fixture.repo.start_transaction();
        let current = fixture.current();
        first.repo_mut().set_local_bookmark_target(
            RefName::new("one"),
            RefTarget::normal(current.id().clone()),
        );
        second.repo_mut().set_local_bookmark_target(
            RefName::new("two"),
            RefTarget::normal(current.id().clone()),
        );
        block_on(first.commit("first")).unwrap();
        block_on(second.commit("second")).unwrap();
        let heads = block_on(fixture.repo.loader().op_heads_store().get_op_heads()).unwrap();
        assert_eq!(heads.len(), 2);
        assert!(open_repo(&fixture.root)
            .unwrap_err()
            .to_string()
            .contains("concurrent operations"));
        assert_eq!(
            block_on(fixture.repo.loader().op_heads_store().get_op_heads()).unwrap(),
            heads
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_base_is_target_text_not_dereferenced_content() {
        let mut fixture = Fixture::new(true);
        let path = RepoPathBuf::from_internal_string("link").unwrap();
        let id = block_on(fixture.repo.store().write_symlink(&path, "missing-target")).unwrap();
        let tree = fixture.tree(&[("link", Merge::normal(TreeValue::Symlink(id)))]);
        let parent = fixture.checkout(
            vec![fixture.repo.store().root_commit_id().clone()],
            tree.clone(),
        );
        fixture.checkout(vec![parent.id().clone()], tree);
        assert_eq!(
            get_diff_base(&fixture.root, &fixture.root.join("link")).unwrap(),
            b"missing-target"
        );
    }
}
