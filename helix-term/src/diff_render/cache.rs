//! Native, private rendering configuration snapshots. No VM, credentials, trees,
//! source text or rendered output are persisted by this layer.
use super::*;
mod highlights;
use anyhow::ensure;
use helix_view::{
    editor,
    icons::{Icon, IconStyle},
    theme::ThemeSnapshot,
};
pub(super) use highlights::Highlights;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
};

const VERSION: u32 = 2;
const LIMIT: u64 = 20 * 1024 * 1024;

pub(super) fn trace(message: impl std::fmt::Display) {
    if std::env::var_os("HELIX_DIFF_CACHE_TRACE").is_some() {
        eprintln!("diff cache: {message}");
    }
}

#[derive(Serialize, Deserialize)]
struct Profile {
    version: u32,
    fingerprint: String,
    editor: serde_json::Value,
    languages: Vec<serde_json::Value>,
    theme: ThemeSnapshot,
    dynamic_themes: Vec<ThemeSnapshot>,
    icons: [(char, Option<IconStyle>); 3],
}

pub(super) struct Context {
    root: PathBuf,
    path: PathBuf,
    fingerprint: String,
    namespace: String,
}

fn uid() -> u32 {
    // SAFETY: geteuid has no preconditions or pointer arguments.
    unsafe { libc::geteuid() }
}

fn private_file(meta: &fs::Metadata) -> bool {
    meta.is_file() && meta.uid() == uid() && meta.mode() & 0o077 == 0 && meta.nlink() == 1
}

impl Context {
    pub(super) fn capture(args: &Args) -> Result<Self> {
        let started = Instant::now();
        ensure!(
            !std::env::var_os("HELIX_DIFF_CACHE").is_some_and(|value| value == "0"),
            "cache disabled"
        );
        let width = args.diff_width.unwrap_or(100);
        let scope = crate::diff_worker::Scope::capture(width)?;
        let root = helix_loader::cache_dir().join("diff-profile-v1");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)?;
        let meta = fs::symlink_metadata(&root)?;
        ensure!(
            meta.is_dir() && meta.uid() == uid() && meta.mode() & 0o077 == 0,
            "unsafe profile directory"
        );
        let query_root = helix_loader::cache_dir().join("diff-queries-v1");
        let namespace = format!(
            "{}:{}:{}.{}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            scope.base,
            scope.fingerprint
        );
        tree_house_bindings::query_cache::configure(Some((&query_root, &namespace)))?;
        trace(format!(
            "dependencies_ms={:.2}",
            started.elapsed().as_secs_f64() * 1000.0
        ));
        Ok(Self {
            path: root.join(format!("{}.{width}.profile", scope.base)),
            root,
            fingerprint: scope.fingerprint,
            namespace,
        })
    }

    pub(super) fn load(&self) -> Result<Option<RenderSession>> {
        let started = Instant::now();
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                trace("profile miss");
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let meta = file.metadata()?;
        ensure!(
            private_file(&meta) && meta.len() <= LIMIT,
            "unsafe/oversized profile file"
        );
        let mut bytes = Vec::new();
        (&mut file).take(LIMIT + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 <= LIMIT, "oversized profile");
        ensure!(bytes.len() >= 32, "truncated native profile");
        let (checksum, payload) = bytes.split_at(32);
        ensure!(
            Sha256::digest(payload).as_slice() == checksum,
            "native profile checksum mismatch"
        );
        let profile: Profile = serde_json::from_slice(payload)?;
        trace(format!(
            "profile_decode_ms={:.2}",
            started.elapsed().as_secs_f64() * 1000.0
        ));
        if profile.version != VERSION || profile.fingerprint != self.fingerprint {
            trace("profile invalidated");
            return Ok(None);
        }
        ensure!(
            profile.languages.len() < 4096 && profile.dynamic_themes.len() < 1024,
            "oversized profile resources"
        );
        let editor: editor::Config = serde_json::from_value(profile.editor)?;
        let theme = profile.theme.restore()?;
        let mut dirs = vec![helix_loader::config_dir()];
        dirs.extend(helix_loader::runtime_dirs().iter().cloned());
        let mut themes = theme::Loader::new(&dirs);
        for saved in profile.dynamic_themes {
            let theme = saved.restore()?;
            themes.add_dynamic_theme(theme.name().to_owned(), theme);
        }
        let mut icons = helix_view::icons::Loader::new(&dirs).default(&theme);
        let [added, deleted, modified] = profile
            .icons
            .map(|(icon_char, style)| Icon { icon_char, style });
        icons.diff.added = added;
        icons.diff.deleted = deleted;
        icons.diff.modified = modified;
        let started = Instant::now();
        let loader = syntax::Loader::new(syntax::config::Configuration {
            language: profile
                .languages
                .into_iter()
                .map(serde_json::from_value)
                .collect::<std::result::Result<Vec<_>, _>>()?,
            language_server: HashMap::new(),
        })?;
        loader.set_scopes(theme.scopes().to_vec());
        trace(format!(
            "language_loader_ms={:.2}",
            started.elapsed().as_secs_f64() * 1000.0
        ));
        // The request's dependency snapshot was validated before opening this
        // profile. Hits consume only rendering data, never cached executable
        // configuration. A concurrent edit is observed on the next request,
        // just as for an editor already using its evaluated configuration.
        trace("native profile hit; Steel not initialized");
        Ok(Some(RenderSession {
            themes: Arc::new(themes),
            theme,
            icons,
            loader: Arc::new(ArcSwap::from_pointee(loader)),
            config: Arc::new(ArcSwap::from_pointee(editor)),
            documents: Rc::default(),
            syntax_cache: Arc::default(),
            highlights: Some(self.highlights()),
        }))
    }

    pub(super) fn highlights(&self) -> Highlights {
        Highlights::new(self.namespace.clone())
    }

    pub(super) fn store(&self, session: &RenderSession) -> Result<()> {
        ensure!(
            crate::diff_worker::fingerprint()? == self.fingerprint,
            "configuration changed during evaluation"
        );
        let languages = session
            .loader
            .load()
            .language_configs()
            .cloned()
            .map(|mut config| {
                // Rendering needs syntax/indent/encoding settings, not commands or credentials.
                config.language_servers.clear();
                config.formatter = None;
                config.debugger = None;
                config.roots = Default::default();
                config.workspace_lsp_roots = None;
                // LanguageConfiguration deliberately omits this from its public
                // serialization. Rendering still needs aliases for injected code.
                let regex = config
                    .injection_regex
                    .as_ref()
                    .map(|regex| regex.pattern().to_owned());
                let mut value = serde_json::to_value(config)?;
                value["injection-regex"] = serde_json::to_value(regex)?;
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()?;
        let dynamic_themes = session
            .themes
            .dynamic_themes()
            .map(|name| session.themes.load(name).map(|theme| theme.snapshot()))
            .collect::<Result<Vec<_>>>()?;
        let diff = &session.icons.diff;
        // Only persist rendering data. Clipboard, shell, LSP and other command
        // configurations can contain credentials and do not belong in this cache.
        let mut editor = serde_json::to_value(session.config.load_full().as_ref())?;
        editor
            .as_object_mut()
            .expect("editor config is an object")
            .retain(|key, _| {
                [
                    "soft-wrap",
                    "whitespace",
                    "indent-guides",
                    "text-width",
                    "default-line-ending",
                    "rulers",
                    "true-color",
                ]
                .contains(&key.as_str())
            });
        let profile = Profile {
            version: VERSION,
            fingerprint: self.fingerprint.clone(),
            editor,
            languages,
            theme: session.theme.snapshot(),
            dynamic_themes,
            icons: [&diff.added, &diff.deleted, &diff.modified]
                .map(|icon| (icon.icon_char, icon.style)),
        };
        let bytes = serde_json::to_vec(&profile)?;
        ensure!(bytes.len() as u64 <= LIMIT, "profile exceeds size limit");
        if let Ok(meta) = fs::symlink_metadata(&self.path) {
            ensure!(private_file(&meta), "unsafe existing profile");
        }
        let mut file = tempfile::NamedTempFile::new_in(&self.root)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(&Sha256::digest(&bytes))?;
        file.write_all(&bytes)?;
        file.persist(&self.path)?;
        // This private cache is disposable and bounded. Never traverse user data.
        let mut entries = fs::read_dir(&self.root)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let meta = fs::symlink_metadata(&path).ok()?;
                (path.extension().is_some_and(|ext| ext == "profile") && private_file(&meta))
                    .then(|| (meta.modified().ok(), path))
            })
            .collect::<Vec<_>>();
        entries.sort();
        let remove = entries.len().saturating_sub(32);
        for (_, path) in entries.into_iter().take(remove) {
            if path != self.path {
                fs::remove_file(path)?;
            }
        }
        trace("native profile stored");
        Ok(())
    }
}
