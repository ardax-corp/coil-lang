//! The code cache: compiling a program's MIR to machine code costs far more
//! than anything else at start, and its result only depends on that MIR, so
//! `coil FILE` and `coil run` keep it in `~/.cache/coil/code` (see
//! `docs/internals/native.md`, "Code cache"). The bytes are
//! `coil_native`'s code images.
//!
//! An entry's name is a sha256 of the MIR section, the compiler build, this
//! host's target and CPU features, and every `COIL_*` variable, so a hit is
//! exactly the code a compile would make here. Entries are written to a
//! temporary file and renamed, so a concurrent run sees a whole entry or none.

use std::path::{Path, PathBuf};

use compiler::comptime_cache::user_cache_root;
use sha2::{Digest, Sha256};

use crate::build_cache::{hex, keep_newest};

/// Bump when what a key covers changes.
const FORMAT: u64 = 1;

const EXT: &str = "code";

/// `code` in the user cache root.
pub fn dir() -> Option<PathBuf> {
    Some(user_cache_root()?.join("code"))
}

/// The key of the code for `section`, a program's MIR section.
pub fn key(section: &[u8]) -> Option<String> {
    let mut h = Sha256::new();
    let mut part = |bytes: &[u8]| {
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    part(&FORMAT.to_le_bytes());
    part(coil_build_id::BUILD_ID.as_bytes());
    part(coil_native::image_fingerprint().ok()?.as_bytes());
    part(section);
    let mut vars: Vec<(String, String)> = std::env::vars().filter(|(k, _)| k.starts_with("COIL_")).collect();
    vars.sort();
    for (k, v) in vars {
        part(k.as_bytes());
        part(v.as_bytes());
    }
    Some(hex(&h.finalize()))
}

/// The image stored for `key`.
pub fn load(dir: &Path, key: &str) -> Option<Vec<u8>> {
    let path = dir.join(format!("{key}.{EXT}"));
    let bytes = std::fs::read(&path).ok()?;
    // Recently used entries survive pruning.
    if let Ok(f) = std::fs::File::options().append(true).open(&path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
    Some(bytes)
}

/// Forget the entry for `key`, which did not load.
pub fn remove(dir: &Path, key: &str) {
    let _ = std::fs::remove_file(dir.join(format!("{key}.{EXT}")));
}

/// Keep `image` for `key`. The directory is the user's alone: what is in it
/// runs as machine code.
pub fn store(dir: &Path, key: &str, image: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let tmp = dir.join(format!(".{key}.{}", std::process::id()));
    std::fs::write(&tmp, image)?;
    std::fs::rename(&tmp, dir.join(format!("{key}.{EXT}")))?;
    prune(dir);
    Ok(())
}

fn prune(dir: &Path) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let entries = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == EXT))
        .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
        .collect();
    keep_newest(entries);
}
