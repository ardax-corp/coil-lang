//! The comptime result cache (comptime step 7): the result of each
//! compile-time evaluation that did IO (a `const` value, or the text a
//! macro returned), so the next build reuses it instead of reading the
//! environment or running a program again.
//!
//! An entry is keyed by a sha256 of what decides the result: the
//! evaluation's name and code (the sources of its module and every module
//! that module uses, macro output included), its grants, and the cache and
//! archive formats. It lists the files it read with their sha256
//! ([`path_digest`]), so a changed file is a miss. The environment, the
//! programs run and the network are consulted again only on a miss or with
//! `--comptime-refresh`; environment values are never stored, only names.
//! `--comptime-offline` refuses them on a miss.
//!
//! Evaluations that write files are not cached: a hit would skip the write.
//! Pure evaluations are not cached either: they give the same result every
//! time, and a build always recomputes them.
//!
//! Paths in keys and entries are relative to the working directory when
//! under it, so a committed cache works from another checkout.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::macros::{ComptimeAccess, ComptimeIo};

/// Bump when the entry encoding or what a key covers changes.
const FORMAT: u64 = 1;

const MAGIC: &[u8; 8] = b"COILCMT1";

/// `--comptime-cache`, `--comptime-offline`, `--comptime-refresh`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheConfig {
    /// Where entries live; `None` is [`default_dir`].
    pub dir: Option<PathBuf>,
    /// Use cached results only: on a miss, the environment and programs
    /// are refused.
    pub offline: bool,
    /// Ignore cached results, compute them again and report which changed.
    pub refresh: bool,
    /// The language server: like `offline`, and compile-time code that
    /// writes files is refused too; a refusal says to build the project.
    pub replay: bool,
}

impl CacheConfig {
    pub fn resolved_dir(&self) -> Option<PathBuf> {
        self.dir.clone().or_else(default_dir)
    }
}

/// The user's coil cache: `$COIL_CACHE_DIR`, else `$XDG_CACHE_HOME/coil`,
/// else `~/.cache/coil` (spool's cache root).
pub fn user_cache_root() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    var("COIL_CACHE_DIR")
        .or_else(|| var("XDG_CACHE_HOME").map(|d| d.join("coil")))
        .or_else(|| var("HOME").or_else(|| var("USERPROFILE")).map(|h| h.join(".cache").join("coil")))
}

/// The default `--comptime-cache`: `comptime` in [`user_cache_root`].
pub fn default_dir() -> Option<PathBuf> {
    Some(user_cache_root()?.join("comptime"))
}

/// Compiled expansion programs, which only save time and are never
/// committed: `macro-programs` in [`user_cache_root`].
pub fn program_dir() -> Option<PathBuf> {
    Some(user_cache_root()?.join("macro-programs"))
}

/// sha256 of what a read of `path` saw: a file's bytes, a directory's
/// sorted entry names, or that it is missing; a symlink adds its target.
pub fn path_digest(path: &Path) -> [u8; 32] {
    let mut h = Sha256::new();
    if let Ok(target) = std::fs::read_link(path) {
        h.update(b"l");
        h.update(target.as_os_str().as_encoded_bytes());
        h.update([0]);
    }
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => {
            h.update(b"d");
            let mut names: Vec<_> = std::fs::read_dir(path)
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect();
            names.sort();
            for name in names {
                h.update(name.as_encoded_bytes());
                h.update([0]);
            }
        }
        Ok(_) => {
            h.update(b"f");
            match std::fs::read(path) {
                Ok(bytes) => h.update(&bytes),
                Err(_) => h.update(b"?"),
            }
        }
        Err(_) => h.update(b"m"),
    }
    h.finalize().into()
}

/// `path` relative to the working directory when under it, else as is.
pub fn portable(path: &Path) -> String {
    let cwd = crate::macros::absolute_path(Path::new("."));
    path.strip_prefix(&cwd).unwrap_or(path).to_string_lossy().into_owned()
}

fn resolve(text: &str) -> PathBuf {
    crate::macros::absolute_path(Path::new(text))
}

/// Builds an entry's key.
pub struct Key(Sha256);

impl Key {
    /// A key for an evaluation of kind `kind` (`const`, `macro`).
    pub fn new(kind: &str) -> Self {
        let mut k = Key(Sha256::new());
        k.text("coil comptime cache");
        k.u64(FORMAT);
        k.u64(u64::from(common::ARCHIVE_VERSION));
        k.text(std::env::consts::OS);
        k.text(std::env::consts::ARCH);
        k.text(kind);
        k
    }

    pub fn u64(&mut self, n: u64) {
        self.0.update(n.to_le_bytes());
    }

    pub fn text(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }

    pub fn bytes(&mut self, b: &[u8]) {
        self.u64(b.len() as u64);
        self.0.update(b);
    }

    pub fn path(&mut self, p: &Path) {
        self.text(&portable(p));
    }

    /// The grants, in a fixed order (offline mode is not part of a key).
    pub fn grants(&mut self, io: &ComptimeIo) {
        for paths in [&io.read, &io.write] {
            let mut list: Vec<String> = paths.iter().map(|p| portable(p)).collect();
            list.sort();
            self.u64(list.len() as u64);
            list.iter().for_each(|p| self.text(p));
        }
        for names in [&io.env, &io.exec, &io.net] {
            let mut list = names.clone();
            list.sort();
            self.u64(list.len() as u64);
            list.iter().for_each(|n| self.text(n));
        }
    }

    pub fn finish(self) -> String {
        self.0.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// What an entry holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A const's value: an encoded snapshot (one root) and the key of each
    /// type id in it ([`type_table`]).
    Snapshot { types: Vec<(u32, String)>, words: Vec<u64> },
    /// A macro's output.
    Text(String),
}

/// One cached result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// What it is the result of, for messages (`const config::PORT`).
    pub name: String,
    pub accesses: Vec<ComptimeAccess>,
    pub value: Value,
}

/// Whether an evaluation that did `accesses` is worth keeping: it did IO,
/// and none of it was a write.
pub fn cacheable(accesses: &[ComptimeAccess]) -> bool {
    !accesses.is_empty() && !accesses.iter().any(|a| matches!(a, ComptimeAccess::Write(_)))
}

/// Whether every file `entry` read is as it was.
pub fn still_valid(entry: &Entry) -> bool {
    reads_unchanged(&entry.accesses)
}

/// Whether every file read in `accesses` is as it was.
pub fn reads_unchanged(accesses: &[ComptimeAccess]) -> bool {
    accesses.iter().all(|a| match a {
        ComptimeAccess::Read { path, digest } => path_digest(path) == *digest,
        _ => true,
    })
}

/// Whether `entry` used the environment, ran a program or used the
/// network: what `--comptime-refresh` computes again.
pub fn uses_outside(entry: &Entry) -> bool {
    entry
        .accesses
        .iter()
        .any(|a| matches!(a, ComptimeAccess::Env(_) | ComptimeAccess::Exec(_) | ComptimeAccess::Net(_)))
}

fn entry_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.entry"))
}

/// The entry for `key`, if there is a readable one.
pub fn load(dir: &Path, key: &str) -> Option<Entry> {
    decode(&std::fs::read(entry_path(dir, key)).ok()?)
}

/// Keep `entry` for `key`. Best effort: a cache that cannot be written
/// only costs the next build time.
pub fn store(dir: &Path, key: &str, entry: &Entry) {
    write_atomic(dir, &entry_path(dir, key), &encode(entry));
}

/// Write `bytes` to `path` in `dir` through a temporary file, so a reader
/// never sees part of it. Best effort.
pub fn write_atomic(dir: &Path, path: &Path, bytes: &[u8]) {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn encode(entry: &Entry) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    let num = |out: &mut Vec<u8>, n: u64| out.extend_from_slice(&n.to_le_bytes());
    let text = |out: &mut Vec<u8>, s: &str| {
        num(out, s.len() as u64);
        out.extend_from_slice(s.as_bytes());
    };
    text(&mut out, &entry.name);
    num(&mut out, entry.accesses.len() as u64);
    for a in &entry.accesses {
        match a {
            ComptimeAccess::Read { path, digest } => {
                out.push(0);
                text(&mut out, &portable(path));
                out.extend_from_slice(digest);
            }
            ComptimeAccess::Write(path) => {
                out.push(1);
                text(&mut out, &portable(path));
            }
            ComptimeAccess::Env(name) => {
                out.push(2);
                text(&mut out, name);
            }
            ComptimeAccess::Exec(name) => {
                out.push(3);
                text(&mut out, name);
            }
            ComptimeAccess::Net(at) => {
                out.push(4);
                text(&mut out, at);
            }
        }
    }
    match &entry.value {
        Value::Snapshot { types, words } => {
            out.push(0);
            num(&mut out, types.len() as u64);
            for (id, key) in types {
                num(&mut out, u64::from(*id));
                text(&mut out, key);
            }
            num(&mut out, words.len() as u64);
            words.iter().for_each(|w| num(&mut out, *w));
        }
        Value::Text(s) => {
            out.push(1);
            text(&mut out, s);
        }
    }
    out
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }

    fn byte(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn num(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn len(&mut self) -> Option<usize> {
        usize::try_from(self.num()?).ok().filter(|&n| n <= self.0.len())
    }

    fn text(&mut self) -> Option<String> {
        let n = self.len()?;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

fn decode(bytes: &[u8]) -> Option<Entry> {
    let mut r = Reader(bytes.strip_prefix(MAGIC.as_slice())?);
    let name = r.text()?;
    let n = r.len()?;
    let mut accesses = Vec::with_capacity(n);
    for _ in 0..n {
        accesses.push(match r.byte()? {
            0 => {
                let path = resolve(&r.text()?);
                ComptimeAccess::Read { path, digest: r.take(32)?.try_into().ok()? }
            }
            1 => ComptimeAccess::Write(resolve(&r.text()?)),
            2 => ComptimeAccess::Env(r.text()?),
            3 => ComptimeAccess::Exec(r.text()?),
            4 => ComptimeAccess::Net(r.text()?),
            _ => return None,
        });
    }
    let value = match r.byte()? {
        0 => {
            let n = r.len()?;
            let mut types = Vec::with_capacity(n);
            for _ in 0..n {
                types.push((u32::try_from(r.num()?).ok()?, r.text()?));
            }
            let n = r.len()?;
            let words = (0..n).map(|_| r.num()).collect::<Option<Vec<_>>>()?;
            Value::Snapshot { types, words }
        }
        1 => Value::Text(r.text()?),
        _ => return None,
    };
    r.0.is_empty().then_some(Entry { name, accesses, value })
}

fn type_ids(snapshot: &mut common::HeapSnapshot) -> impl Iterator<Item = &mut u32> {
    snapshot.objects.iter_mut().filter_map(|o| match o {
        common::SnapObject::Enum { type_id, .. } | common::SnapObject::Instance { type_id, .. } => Some(type_id),
        _ => None,
    })
}

/// The key of each type id in `snapshot` (`keys`: this program's), or
/// `None` when one has none and the value cannot be kept.
pub fn type_table(snapshot: &common::HeapSnapshot, keys: &HashMap<u32, String>) -> Option<Vec<(u32, String)>> {
    let mut snapshot = snapshot.clone();
    let mut table: Vec<(u32, String)> = Vec::new();
    for &mut id in type_ids(&mut snapshot) {
        if id != 0 && !table.iter().any(|(t, _)| *t == id) {
            table.push((id, keys.get(&id)?.clone()));
        }
    }
    Some(table)
}

/// Move `snapshot`'s type ids from `table`'s program to this one (`ids`:
/// type key to id; `fields`: field count of each class id). False when a
/// type is gone or its fields changed.
pub fn remap(
    snapshot: &mut common::HeapSnapshot,
    table: &[(u32, String)],
    ids: &HashMap<String, u32>,
    fields: &HashMap<u32, usize>,
) -> bool {
    let map: Option<HashMap<u32, u32>> = table.iter().map(|(old, key)| Some((*old, *ids.get(key)?))).collect();
    let Some(map) = map else { return false };
    for object in &mut snapshot.objects {
        match object {
            common::SnapObject::Enum { type_id, .. } if *type_id != 0 => match map.get(type_id) {
                Some(&new) => *type_id = new,
                None => return false,
            },
            common::SnapObject::Instance { type_id, words } => {
                let Some(&new) = map.get(type_id) else { return false };
                if fields.get(&new).is_some_and(|&n| n != words.len()) {
                    return false;
                }
                *type_id = new;
            }
            _ => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("coil-cmt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::macros::absolute_path(&dir)
    }

    #[test]
    fn entries_round_trip() {
        let dir = temp_dir("round");
        let entry = Entry {
            name: "const a::B".to_string(),
            accesses: vec![
                ComptimeAccess::Read { path: dir.join("x.txt"), digest: [7; 32] },
                ComptimeAccess::Env("HOME".to_string()),
                ComptimeAccess::Exec("git".to_string()),
            ],
            value: Value::Snapshot { types: vec![(3, "class a::P".to_string())], words: vec![1, 2, u64::MAX] },
        };
        store(&dir, "k1", &entry);
        assert_eq!(load(&dir, "k1"), Some(entry));
        let text = Entry { name: "macro m".to_string(), accesses: Vec::new(), value: Value::Text("x".to_string()) };
        store(&dir, "k2", &text);
        assert_eq!(load(&dir, "k2"), Some(text));
        assert_eq!(load(&dir, "missing"), None);
        std::fs::write(dir.join("bad.entry"), b"COILCMT1\x01").unwrap();
        assert_eq!(load(&dir, "bad"), None);
    }

    #[test]
    fn a_changed_file_invalidates() {
        let dir = temp_dir("digest");
        let file = dir.join("data.txt");
        std::fs::write(&file, "one").unwrap();
        let entry = Entry {
            name: "const X".to_string(),
            accesses: vec![
                ComptimeAccess::Read { path: file.clone(), digest: path_digest(&file) },
                ComptimeAccess::Read { path: dir.clone(), digest: path_digest(&dir) },
            ],
            value: Value::Text(String::new()),
        };
        assert!(still_valid(&entry));
        std::fs::write(&file, "two").unwrap();
        assert!(!still_valid(&entry));
        std::fs::write(&file, "one").unwrap();
        assert!(still_valid(&entry));
        std::fs::write(dir.join("new.txt"), "").unwrap();
        assert!(!still_valid(&entry), "a new directory entry changes a listing");
        std::fs::remove_file(&file).unwrap();
        assert_ne!(path_digest(&file), path_digest(&dir.join("new.txt")));
    }

    #[test]
    fn writes_are_not_cached() {
        assert!(!cacheable(&[]));
        assert!(cacheable(&[ComptimeAccess::Env("A".to_string())]));
        assert!(!cacheable(&[ComptimeAccess::Env("A".to_string()), ComptimeAccess::Write(PathBuf::from("/o"))]));
    }

    #[test]
    fn keys_ignore_grant_order_and_offline() {
        let io = |env: &[&str], offline| ComptimeIo {
            env: env.iter().map(|s| s.to_string()).collect(),
            offline,
            ..ComptimeIo::default()
        };
        let key = |io: &ComptimeIo| {
            let mut k = Key::new("const");
            k.grants(io);
            k.finish()
        };
        assert_eq!(key(&io(&["A", "B"], false)), key(&io(&["B", "A"], true)));
        assert_ne!(key(&io(&["A"], false)), key(&io(&["B"], false)));
    }

    #[test]
    fn types_move_by_key() {
        let mut snapshot = common::HeapSnapshot {
            objects: vec![common::SnapObject::Instance { type_id: 4, words: vec![common::SnapWord::Raw(1)] }],
            statics: vec![(0, common::SnapWord::Ref { index: 0, err_bit: false })],
        };
        let keys = HashMap::from([(4, "class P".to_string())]);
        let table = type_table(&snapshot, &keys).unwrap();
        assert_eq!(table, vec![(4, "class P".to_string())]);
        assert_eq!(type_table(&snapshot, &HashMap::new()), None);
        let ids = HashMap::from([("class P".to_string(), 9)]);
        assert!(!remap(&mut snapshot.clone(), &table, &ids, &HashMap::from([(9, 2)])), "field count changed");
        assert!(!remap(&mut snapshot.clone(), &table, &HashMap::new(), &HashMap::new()), "type gone");
        assert!(remap(&mut snapshot, &table, &ids, &HashMap::from([(9, 1)])));
        assert!(matches!(snapshot.objects[0], common::SnapObject::Instance { type_id: 9, .. }));
    }
}
