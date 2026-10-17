//! Local LSP sharing, controlled by editor.lsp.shared. The editor owns a small
//! proxy child, not the shared server. No configuration or credentials are persisted here.
mod broker;
mod discovery;
mod documents;
mod edits;
mod proxy;
mod router;
mod text;
mod wire;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, ffi::OsStr, os::unix::fs::MetadataExt, path::PathBuf};

const SPEC_ENV: &str = "HELIX_INTERNAL_LSP_SPEC";
const SOCKET_ENV: &str = "HELIX_INTERNAL_LSP_SOCKET";
const VERSION: u32 = 2;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Spec {
    command: PathBuf,
    args: Vec<String>,
    root: PathBuf,
    environment: BTreeMap<String, String>,
}

impl Spec {
    pub(crate) fn capture(
        command: PathBuf,
        args: &[String],
        root: &std::path::Path,
        overrides: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> Result<Self> {
        let root = root.canonicalize()?;
        let mut environment = std::env::vars_os()
            .chain(overrides.iter().cloned())
            .map(|(key, value)| {
                Ok((
                    key.into_string()
                        .map_err(|_| anyhow::anyhow!("non-UTF8 environment"))?,
                    value
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("non-UTF8 environment"))?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        for name in [
            SPEC_ENV,
            SOCKET_ENV,
            "_",
            "SHLVL",
            "OLDPWD",
            "COLUMNS",
            "LINES",
            "ZELLIJ_PANE_ID",
        ] {
            environment.remove(name);
        }
        environment.insert(
            "PWD".into(),
            root.to_str().context("non-UTF8 workspace")?.into(),
        );
        Ok(Self {
            command: command.canonicalize()?,
            args: args.to_vec(),
            root,
            environment,
        })
    }

    fn key(&self, initialize: &Value) -> Result<String> {
        let mut init = initialize.clone();
        init.as_object_mut()
            .context("invalid initialization parameters")?
            .remove("processId");
        let meta = self.command.metadata()?;
        let editor = std::env::current_exe()?.metadata()?;
        let mut hash = Sha256::new();
        hash.update(VERSION.to_le_bytes());
        hash.update(serde_json::to_vec(&(self, init))?);
        for meta in [meta, editor] {
            for value in [
                meta.dev(),
                meta.ino(),
                meta.len(),
                meta.ctime() as u64,
                meta.ctime_nsec() as u64,
            ] {
                hash.update(value.to_le_bytes());
            }
        }
        Ok(hash.finalize()[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    fn spawn(&self) -> Result<tokio::process::Child> {
        Ok(tokio::process::Command::new(&self.command)
            .args(&self.args)
            .current_dir(&self.root)
            .env_clear()
            .envs(&self.environment)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?)
    }
}

#[derive(Serialize, Deserialize)]
enum Input {
    Attach {
        version: u32,
        spec: Spec,
        initialize: Value,
    },
    Message(Value),
}

#[derive(Serialize, Deserialize)]
enum Output {
    Attached(Value),
    Message(Value),
    Ack,
    Private(String),
}

/// Construct the proxy command when sharing is enabled in editor configuration.
/// Unsupported environments retain direct spawning.
pub(crate) fn command(
    command: PathBuf,
    args: &[String],
    root: &std::path::Path,
    overrides: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Option<(PathBuf, Vec<String>, (String, String))> {
    let spec = Spec::capture(command, args, root, overrides).ok()?;
    let bootstrap = serde_json::to_string(&spec).ok()?;
    // Linux also limits each individual environment entry (typically 128 KiB).
    // A large effective environment must retain ordinary spawning, not fail
    // because its serialized aggregate became a new oversized env entry.
    if bootstrap.len() > 64 * 1024 {
        return None;
    }
    Some((
        std::env::current_exe().ok()?,
        vec!["--internal-lsp-proxy".into()],
        (SPEC_ENV.into(), bootstrap),
    ))
}

/// Hidden executable modes are intercepted before editor/config initialization.
pub async fn run_internal(mode: &str) -> Result<()> {
    match mode {
        "--internal-lsp-proxy" => {
            proxy::run(serde_json::from_str(&std::env::var(SPEC_ENV)?)?).await
        }
        "--internal-lsp-broker" => broker::serve(PathBuf::from(std::env::var(SOCKET_ENV)?)).await,
        _ => anyhow::bail!("invalid LSP internal mode"),
    }
}

#[cfg(test)]
mod tests;
