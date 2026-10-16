//! Session-local syntax reuse independent of displayed hunks and snapshot paths.
//! No source text or trees are persisted. Callers update a cloned forest with
//! complete source edits and the new coverage before highlighting any bytes.
use super::*;

#[derive(Clone)]
pub(super) struct CachedSyntax {
    pub source: Rope,
    pub syntax: syntax::Syntax,
    pub coverage: Option<Vec<Range<u32>>>,
}

struct Entry {
    path: PathBuf,
    language: String,
    value: CachedSyntax,
}

pub(super) struct SyntaxCache {
    entries: VecDeque<Entry>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl Default for SyntaxCache {
    fn default() -> Self {
        Self::with_limits(64, 8 * 1024 * 1024)
    }
}

impl SyntaxCache {
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            max_entries,
            max_bytes,
        }
    }
    pub fn get(&mut self, path: &Path, language: &str, source: &Rope) -> Option<CachedSyntax> {
        let matches = |entry: &&Entry| entry.path == path && entry.language == language;
        // Prefer an exact source version, otherwise the latest version of this
        // logical path/language. Directory snapshots have request-specific roots.
        let index = self
            .entries
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, entry)| matches(entry))
            .find(|(_, entry)| entry.value.source == *source)
            .map(|(index, _)| index)
            .or_else(|| self.entries.iter().rposition(|entry| matches(&entry)))?;
        let entry = self.entries.remove(index)?;
        let value = entry.value.clone();
        self.entries.push_back(entry);
        Some(value)
    }

    pub fn insert(
        &mut self,
        path: &Path,
        language: &str,
        source: &Rope,
        syntax: &syntax::Syntax,
        coverage: Option<&[Range<u32>]>,
    ) {
        let bytes = source.len_bytes();
        // Do not retain large inputs or more than the budget. Syntax allocations
        // consume additional RAM; the entry count also bounds retained forests.
        if self.max_entries == 0 || bytes > 512 * 1024 || bytes > self.max_bytes {
            return;
        }
        if let Some(index) = self.entries.iter().position(|entry| {
            entry.path == path && entry.language == language && entry.value.source == *source
        }) {
            self.bytes -= self.entries.remove(index).unwrap().value.source.len_bytes();
        }
        while self.entries.len() >= self.max_entries || self.bytes + bytes > self.max_bytes {
            self.bytes -= self.entries.pop_front().unwrap().value.source.len_bytes();
        }
        self.bytes += bytes;
        self.entries.push_back(Entry {
            path: path.to_owned(),
            language: language.to_owned(),
            value: CachedSyntax {
                source: source.clone(),
                syntax: syntax.clone(),
                coverage: coverage.map(<[_]>::to_vec),
            },
        });
    }
}
