//! The build cache (#1881): `coil FILE` and `coil compile FILE` keep the
//! archive of each compile in `~/.cache/coil/build` (see
//! [`compiler::comptime_cache::user_cache_root`]) and reuse it while nothing
//! it was built from changed. See `docs/internals/build-cache.md`.
//!
//! An entry's name is a sha256 of the entry file's path and bytes, the
//! working directory, the compiler build, and every flag and `COIL_*`
//! variable that can change the output, so two versions of a file (or two
//! builds of coil) never share one and can compile at the same time. The
//! entry lists every file the compile read, with its digest, and the
//! directories its modules were found in; a hit needs all of them unchanged.
//! Entries are written to a temporary file and renamed, so a concurrent run
//! sees a whole entry or none.

use std::path::{Path, PathBuf};

use compiler::comptime_cache::{path_digest, user_cache_root};
use sha2::{Digest, Sha256};

/// Bump when the entry encoding or what a key covers changes.
const FORMAT: u64 = 1;

const MAGIC: &[u8; 8] = b"COILBLD1";

/// Entries kept; the least recently used go first.
const MAX_ENTRIES: usize = 256;

/// `build` in the user cache root.
pub fn dir() -> Option<PathBuf> {
    Some(user_cache_root()?.join("build"))
}

/// The key of a compile of `entry`: `None` when the entry cannot be read.
/// `options` is everything on the command line that can change the archive.
pub fn key(entry: &Path, options: &str) -> Option<String> {
    let mut h = Sha256::new();
    let mut part = |bytes: &[u8]| {
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    part(&FORMAT.to_le_bytes());
    part(coil_build_id::BUILD_ID.as_bytes());
    part(&common::ARCHIVE_VERSION.to_le_bytes());
    let cwd = std::env::current_dir().ok()?;
    part(cwd.as_os_str().as_encoded_bytes());
    let entry_abs = std::fs::canonicalize(entry).ok()?;
    part(entry_abs.as_os_str().as_encoded_bytes());
    part(&std::fs::read(&entry_abs).ok()?);
    part(options.as_bytes());
    let mut vars: Vec<(String, String)> =
        std::env::vars().filter(|(k, _)| k.starts_with("COIL_")).collect();
    vars.sort();
    for (k, v) in vars {
        part(k.as_bytes());
        part(v.as_bytes());
    }
    Some(hex(&h.finalize()))
}

/// The cached archive for `key`, if every file it lists is unchanged.
pub fn load(dir: &Path, key: &str) -> Option<Vec<u8>> {
    let path = dir.join(format!("{key}.hyc"));
    let bytes = std::fs::read(&path).ok()?;
    let (deps, archive) = decode(&bytes)?;
    if deps.iter().any(|(p, digest)| path_digest(p) != *digest) {
        return None;
    }
    // Recently used entries survive pruning.
    if let Ok(f) = std::fs::File::options().append(true).open(&path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
    Some(archive.to_vec())
}

/// What a compile read: its source files, the directories they and the
/// module roots are (a new file there can change what `use` finds), and
/// the entry's proof file.
pub fn inputs(entry: &Path, files: &[PathBuf], roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    for f in files {
        let abs = absolute(f);
        if let Some(parent) = abs.parent() {
            add(parent.to_path_buf());
        }
        add(abs);
    }
    for r in roots {
        add(absolute(r));
    }
    add(absolute(&compiler::verify::proof::path_for(entry)));
    out
}

/// Store `archive` under `key` with the digests of `inputs`, now.
pub fn store(dir: &Path, key: &str, inputs: &[PathBuf], archive: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut out = Vec::with_capacity(archive.len() + 64 * inputs.len() + 16);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(inputs.len() as u32).to_le_bytes());
    for p in inputs {
        let raw = p.as_os_str().as_encoded_bytes();
        out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        out.extend_from_slice(raw);
        out.extend_from_slice(&path_digest(p));
    }
    out.extend_from_slice(archive);
    let tmp = dir.join(format!("{key}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, &out)?;
    if let Err(e) = std::fs::rename(&tmp, dir.join(format!("{key}.hyc"))) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    prune(dir);
    Ok(())
}

fn decode(bytes: &[u8]) -> Option<(Vec<(PathBuf, [u8; 32])>, &[u8])> {
    let rest = bytes.strip_prefix(MAGIC.as_slice())?;
    let (n, mut rest) = take_u32(rest)?;
    let mut deps = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let (len, r) = take_u32(rest)?;
        let (raw, r) = r.split_at_checked(len as usize)?;
        let (digest, r) = r.split_at_checked(32)?;
        // SAFETY: the bytes came from `as_encoded_bytes` on this platform.
        let os = unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(raw) };
        deps.push((PathBuf::from(os), digest.try_into().ok()?));
        rest = r;
    }
    Some((deps, rest))
}

fn take_u32(bytes: &[u8]) -> Option<(u32, &[u8])> {
    let (n, rest) = bytes.split_at_checked(4)?;
    Some((u32::from_le_bytes(n.try_into().ok()?), rest))
}

/// Drop the least recently used entries past [`MAX_ENTRIES`].
fn prune(dir: &Path) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = read
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "hyc"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if entries.len() <= MAX_ENTRIES {
        return;
    }
    entries.sort();
    for (_, p) in &entries[..entries.len() - MAX_ENTRIES] {
        let _ = std::fs::remove_file(p);
    }
}

fn absolute(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf())
        }
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("coil_build_cache_{label}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn hit_until_an_input_changes() {
        let d = tmp("hit");
        let src = d.join("a.hy");
        std::fs::write(&src, "fn main() {}").unwrap();
        let cache = d.join("cache");
        let inputs = inputs(&src, &[src.clone()], &[]);
        store(&cache, "k", &inputs, b"archive").unwrap();
        assert_eq!(load(&cache, "k").as_deref(), Some(b"archive".as_slice()));
        // A new file beside a source can change what `use` finds.
        std::fs::write(d.join("b.hy"), "").unwrap();
        assert_eq!(load(&cache, "k"), None);
        store(&cache, "k", &inputs, b"archive").unwrap();
        std::fs::write(&src, "fn main() { }").unwrap();
        assert_eq!(load(&cache, "k"), None);
        assert_eq!(load(&cache, "missing"), None);
    }

    #[test]
    fn key_follows_the_entry_bytes_and_options() {
        let d = tmp("key");
        let src = d.join("a.hy");
        std::fs::write(&src, "fn main() {}").unwrap();
        let k1 = key(&src, "o").unwrap();
        assert_eq!(key(&src, "o").unwrap(), k1);
        assert_ne!(key(&src, "p").unwrap(), k1);
        std::fs::write(&src, "fn main() { }").unwrap();
        assert_ne!(key(&src, "o").unwrap(), k1);
        assert_eq!(key(&d.join("missing.hy"), "o"), None);
    }

    #[test]
    fn prune_keeps_the_newest() {
        let d = tmp("prune");
        for i in 0..MAX_ENTRIES + 3 {
            std::fs::write(d.join(format!("{i:04}.hyc")), "").unwrap();
            let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i as u64);
            std::fs::File::options().append(true).open(d.join(format!("{i:04}.hyc"))).unwrap().set_modified(t).unwrap();
        }
        prune(&d);
        assert!(!d.join("0000.hyc").exists() && !d.join("0002.hyc").exists());
        assert!(d.join("0003.hyc").exists());
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), MAX_ENTRIES);
    }
}
