//! Private content-addressed Tree-sitter style events, not source or rendered output.
use super::{private_file, trace, uid};
use helix_view::{graphics::Style, Theme};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};

const MAGIC: &[u8; 8] = b"NHSTYLE1";
const HEADER: usize = 52;
const FILE_LIMIT: usize = 24 * 1024 * 1024;
const TOTAL_LIMIT: u64 = 128 * 1024 * 1024;
const SOURCE_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub(in crate::diff_render) struct Highlights {
    root: PathBuf,
    identity: String,
}

impl Highlights {
    pub(super) fn new(namespace: String) -> Self {
        Self {
            root: helix_loader::cache_dir().join("diff-highlights-v1"),
            identity: namespace,
        }
    }

    pub(in crate::diff_render) fn for_theme(&self, theme: &Theme) -> Option<Self> {
        if std::env::var_os("HELIX_DIFF_HIGHLIGHT_CACHE").is_some_and(|value| value == "0") {
            return None;
        }
        let bytes = serde_json::to_vec(&theme.snapshot()).ok()?;
        let mut result = self.clone();
        result.identity.push('.');
        for byte in Sha256::digest(bytes) {
            result.identity.push_str(&format!("{byte:02x}"));
        }
        Some(result)
    }

    pub(in crate::diff_render) fn identity(&self) -> &str {
        &self.identity
    }

    pub(in crate::diff_render) fn accepts(&self, source: &[u8]) -> bool {
        source.len() <= SOURCE_LIMIT
    }

    fn key(&self, language: &str, source: &[u8]) -> Option<[u8; 32]> {
        if source.len() > SOURCE_LIMIT {
            return None;
        }
        let mut hash = Sha256::new();
        for field in [self.identity.as_bytes(), language.as_bytes(), source] {
            hash.update((field.len() as u64).to_le_bytes());
            hash.update(field);
        }
        Some(hash.finalize().into())
    }

    fn directory(&self) -> std::io::Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.root)?;
        let meta = fs::symlink_metadata(&self.root)?;
        if !meta.is_dir() || meta.uid() != uid() || meta.mode() & 0o077 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "unsafe highlight directory",
            ));
        }
        Ok(())
    }

    fn path(&self, key: &[u8; 32]) -> PathBuf {
        let name = key
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.root.join(format!("{name}.styles"))
    }

    pub(in crate::diff_render) fn load(
        &self,
        language: &str,
        source: &[u8],
        chars: usize,
    ) -> Option<Vec<(usize, Style)>> {
        let key = self.key(language, source)?;
        self.directory().ok()?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.path(&key))
            .ok()?;
        let meta = file.metadata().ok()?;
        if !private_file(&meta) || meta.len() > FILE_LIMIT as u64 {
            return None;
        }
        let mut bytes = Vec::new();
        file.take((FILE_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        let events = decode(&bytes, &key, chars)?;
        trace(format!(
            "highlight hit language={language} events={}",
            events.len()
        ));
        Some(events)
    }

    pub(in crate::diff_render) fn store(
        &self,
        language: &str,
        source: &[u8],
        chars: usize,
        events: &[(usize, Style)],
    ) {
        let Some(key) = self.key(language, source) else {
            return;
        };
        let Some(bytes) = encode(&key, chars, events) else {
            return;
        };
        let result = (|| -> std::io::Result<()> {
            self.directory()?;
            let path = self.path(&key);
            if let Ok(meta) = fs::symlink_metadata(&path) {
                if !private_file(&meta) || meta.file_type().is_symlink() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "unsafe highlight target",
                    ));
                }
            }
            let mut file = tempfile::NamedTempFile::new_in(&self.root)?;
            file.as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(&bytes)?;
            file.persist(path).map_err(|error| error.error)?;
            let mut entries = fs::read_dir(&self.root)?
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let meta = fs::symlink_metadata(entry.path()).ok()?;
                    (entry.path().extension().is_some_and(|ext| ext == "styles")
                        && private_file(&meta))
                    .then_some((meta.modified().ok()?, meta.len(), entry.path()))
                })
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.0);
            let mut total = entries.iter().map(|entry| entry.1).sum::<u64>();
            let mut count = entries.len();
            for (_, size, path) in entries {
                if count <= 256 && total <= TOTAL_LIMIT {
                    break;
                }
                if fs::remove_file(path).is_ok() {
                    total = total.saturating_sub(size);
                    count -= 1;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            trace(format!("highlight not stored: {error}"));
        }
    }
}

fn encode(key: &[u8; 32], chars: usize, events: &[(usize, Style)]) -> Option<Vec<u8>> {
    let mut styles = Vec::new();
    let mut records = Vec::with_capacity(events.len() * 8);
    for (offset, style) in events {
        let index = styles
            .iter()
            .position(|candidate| candidate == style)
            .unwrap_or_else(|| {
                styles.push(*style);
                styles.len() - 1
            });
        if styles.len() > 4096 {
            return None;
        }
        records.extend_from_slice(&u32::try_from(*offset).ok()?.to_le_bytes());
        records.extend_from_slice(&(index as u32).to_le_bytes());
    }
    let styles = serde_json::to_vec(&styles).ok()?;
    let mut payload = MAGIC.to_vec();
    payload.extend_from_slice(key);
    payload.extend_from_slice(&u32::try_from(chars).ok()?.to_le_bytes());
    payload.extend_from_slice(&u32::try_from(styles.len()).ok()?.to_le_bytes());
    payload.extend_from_slice(&u32::try_from(events.len()).ok()?.to_le_bytes());
    payload.extend(styles);
    payload.extend(records);
    if payload.len() + 32 > FILE_LIMIT {
        return None;
    }
    let mut bytes = Sha256::digest(&payload).to_vec();
    bytes.extend(payload);
    Some(bytes)
}

fn decode(bytes: &[u8], key: &[u8; 32], chars: usize) -> Option<Vec<(usize, Style)>> {
    if bytes.len() < 32 + HEADER || bytes.len() > FILE_LIMIT {
        return None;
    }
    let (checksum, payload) = bytes.split_at(32);
    if Sha256::digest(payload).as_slice() != checksum
        || &payload[..8] != MAGIC
        || payload[8..40] != *key
    {
        return None;
    }
    let number = |at: usize| u32::from_le_bytes(payload[at..at + 4].try_into().unwrap()) as usize;
    let style_bytes = number(44);
    let count = number(48);
    if number(40) != chars
        || style_bytes > 1024 * 1024
        || count > chars.saturating_add(1)
        || HEADER
            .checked_add(style_bytes)?
            .checked_add(count.checked_mul(8)?)?
            != payload.len()
    {
        return None;
    }
    let styles: Vec<Style> = serde_json::from_slice(&payload[HEADER..HEADER + style_bytes]).ok()?;
    if styles.len() > 4096 {
        return None;
    }
    let mut events = Vec::with_capacity(count);
    for record in payload[HEADER + style_bytes..].chunks_exact(8) {
        let offset = u32::from_le_bytes(record[..4].try_into().ok()?) as usize;
        let index = u32::from_le_bytes(record[4..].try_into().ok()?) as usize;
        if offset > chars
            || events
                .last()
                .is_some_and(|(previous, _)| *previous >= offset)
        {
            return None;
        }
        events.push((offset, *styles.get(index)?));
    }
    Some(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use helix_view::graphics::Color;

    #[test]
    fn events_round_trip_and_reject_corruption_wrong_source_and_invalid_records() {
        let key = [42; 32];
        let events = vec![(0, Style::default().fg(Color::Blue)), (3, Style::default())];
        let bytes = encode(&key, 8, &events).unwrap();
        assert_eq!(decode(&bytes, &key, 8), Some(events));
        assert!(decode(&bytes, &[43; 32], 8).is_none());
        assert!(decode(&bytes, &key, 9).is_none());
        for end in 0..bytes.len() {
            assert!(decode(&bytes[..end], &key, 8).is_none());
        }
        let mut invalid = bytes.clone();
        let end = invalid.len();
        invalid[end - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
        let checksum = Sha256::digest(&invalid[32..]);
        invalid[..32].copy_from_slice(&checksum);
        assert!(decode(&invalid, &key, 8).is_none());
        let mut invalid = bytes;
        invalid[end - 8..end - 4].copy_from_slice(&0u32.to_le_bytes());
        let checksum = Sha256::digest(&invalid[32..]);
        invalid[..32].copy_from_slice(&checksum);
        assert!(decode(&invalid, &key, 8).is_none());
        assert!(decode(&encode(&key, 8, &[]).unwrap(), &key, 8)
            .unwrap()
            .is_empty());
    }
}
