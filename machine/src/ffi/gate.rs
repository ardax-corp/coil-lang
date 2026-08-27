//! Fail-closed `dload` gate: hardcoded first-party stems + extra allow/hash.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::resolve::{dload_request_stem, is_libc_alias, is_production_dload_stem};
use super::signature::FfiError;

/// Default-deny policy for opening shared libraries.
///
/// First-party stems (`crypto`, `tls`, `regex`, `time`) always pass the gate
/// with no lock hash (until COI-60). Extra stems need consumer `[ffi] allow`
/// **and** a matching `[[package.native]] sha256` (or a host [`Self::grant_file`]).
#[derive(Clone, Debug, Default)]
pub struct DloadGate {
    allowed_stems: HashSet<String>,
    hashes_by_stem: HashMap<String, HashSet<[u8; 32]>>,
    /// Host/test extra stems that skip lock hashing (`set_dload_allowlist`).
    host_unhashed: HashSet<String>,
}

impl DloadGate {
    /// No extra stems — production stems still pass; everything else is denied.
    pub fn deny_all() -> Self {
        Self::default()
    }

    /// Extra stems from consumer `[ffi] allow` and lock native `(stem, sha256 hex)` pins.
    ///
    /// Libc aliases in `allow` or lock pins are ignored. Production stems do not
    /// need to be listed. A lock pin whose package is not on `allow` is ignored.
    pub fn from_consumer(
        allow: impl IntoIterator<Item = impl AsRef<str>>,
        native_pins: &[(String, String)],
    ) -> Self {
        let mut gate = Self::deny_all();
        for stem in allow {
            let stem = stem.as_ref();
            if is_libc_alias(stem) || is_production_dload_stem(stem) {
                continue;
            }
            if dload_request_stem(stem) != stem {
                continue;
            }
            gate.allowed_stems.insert(stem.to_string());
        }
        for (pkg, hex) in native_pins {
            if is_libc_alias(pkg) || is_production_dload_stem(pkg) {
                continue;
            }
            if !gate.allowed_stems.contains(pkg.as_str()) {
                continue;
            }
            if let Some(hash) = parse_sha256_hex(hex) {
                gate.hashes_by_stem
                    .entry(pkg.clone())
                    .or_default()
                    .insert(hash);
            }
        }
        gate
    }

    /// Host/test extra stem with no lock hash (libc / fixtures on dyld or DLL search).
    ///
    /// Does not widen the production list. Manifest `[ffi] allow` cannot do this.
    pub fn grant_stem(&mut self, stem: &str) {
        self.host_unhashed.insert(stem.to_string());
        self.allowed_stems.insert(stem.to_string());
    }

    /// Host/test grant: allow extra `stem` only for files whose contents match `path`.
    ///
    /// Fixture hook for hashed extras (`sum`). It does not widen the production list.
    pub fn grant_file(&mut self, stem: &str, path: &Path) -> Result<(), FfiError> {
        let hash = sha256_file(path).map_err(|e| FfiError::LibraryDenied {
            name: path.display().to_string(),
            stem: stem.to_string(),
            reason: format!("cannot hash `{}`: {e}", path.display()),
        })?;
        self.allowed_stems.insert(stem.to_string());
        self.hashes_by_stem
            .entry(stem.to_string())
            .or_default()
            .insert(hash);
        Ok(())
    }

    fn extra_granted(&self, stem: &str) -> bool {
        self.allowed_stems.contains(stem)
            && self.hashes_by_stem.get(stem).is_some_and(|h| !h.is_empty())
    }

    /// Stem check before candidate search. Absolute paths use the filename stem.
    pub fn check_request(&self, name: &str) -> Result<String, FfiError> {
        let stem = dload_request_stem(name);
        if is_production_dload_stem(&stem) {
            return Ok(stem);
        }
        if self.extra_granted(&stem) || self.host_unhashed.contains(&stem) {
            return Ok(stem);
        }
        Err(FfiError::LibraryDenied {
            name: name.to_string(),
            stem: stem.clone(),
            reason: if is_libc_alias(name) || is_libc_alias(&stem) {
                "libc aliases cannot be dloaded".into()
            } else {
                "stem is not a first-party library and lacks consumer [ffi] allow + lock hash"
                    .into()
            },
        })
    }

    /// Extra stems from allow+hash must match a pin. Production and host unhashed skip.
    pub fn hash_required(&self, stem: &str) -> bool {
        !is_production_dload_stem(stem) && !self.host_unhashed.contains(stem)
    }

    /// Whether `path`'s contents may be opened for `stem`.
    ///
    /// Production stems and host unhashed extras skip hashing.
    pub fn file_hash_allowed(&self, stem: &str, path: &Path) -> bool {
        if !self.hash_required(stem) {
            return true;
        }
        let Some(allowed) = self.hashes_by_stem.get(stem) else {
            return false;
        };
        match sha256_file(path) {
            Ok(hash) => allowed.contains(&hash),
            Err(_) => false,
        }
    }

    /// Denied because the extra stem is granted but no pin matched this file.
    pub fn hash_mismatch(name: &str, stem: &str) -> FfiError {
        FfiError::LibraryDenied {
            name: name.to_string(),
            stem: stem.to_string(),
            reason: "shared library is not a lock-hashed native for this stem".into(),
        }
    }
}

pub fn sha256_file(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

pub fn parse_sha256_hex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = from_hex(chunk[0])?;
        let lo = from_hex(chunk[1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::resolve::DLOAD_PRODUCTION_STEMS;
    use std::io::Write;

    #[test]
    fn deny_all_rejects_c_and_unknown_but_allows_production() {
        let g = DloadGate::deny_all();
        assert!(matches!(
            g.check_request("c"),
            Err(FfiError::LibraryDenied { .. })
        ));
        assert!(matches!(
            g.check_request("notalist"),
            Err(FfiError::LibraryDenied { .. })
        ));
        assert!(matches!(
            g.check_request("/lib/x86_64-linux-gnu/libc.so.6"),
            Err(FfiError::LibraryDenied { .. })
        ));
        for stem in DLOAD_PRODUCTION_STEMS {
            g.check_request(stem)
                .unwrap_or_else(|e| panic!("{stem} must pass without allow/hash, got {e:?}"));
        }
    }

    #[test]
    fn extra_allow_without_pin_is_denied() {
        let g = DloadGate::from_consumer(["plugin"], &[]);
        assert!(matches!(
            g.check_request("plugin"),
            Err(FfiError::LibraryDenied { .. })
        ));
        assert!(g.check_request("tls").is_ok());
    }

    #[test]
    fn lock_pin_without_allow_is_not_a_grant() {
        let hash = "ab".repeat(32);
        let g = DloadGate::from_consumer(std::iter::empty::<&str>(), &[("plugin".into(), hash)]);
        assert!(g.check_request("plugin").is_err());
    }

    #[test]
    fn libc_in_allow_or_lock_is_ignored() {
        let hash = "ab".repeat(32);
        let g = DloadGate::from_consumer(
            ["c", "libc", "plugin"],
            &[("c".into(), hash.clone()), ("plugin".into(), hash)],
        );
        assert!(g.check_request("c").is_err());
        assert!(g.check_request("plugin").is_ok());
    }

    #[test]
    fn production_stems_in_allow_do_not_need_hashes() {
        let g = DloadGate::from_consumer(["tls", "crypto"], &[]);
        assert!(g.check_request("tls").is_ok());
        assert!(g.file_hash_allowed("tls", Path::new("/coil-dload-missing/libtls.so")));
    }

    #[test]
    fn grant_file_allows_matching_bytes_only() {
        let dir = std::env::temp_dir().join("coil_dload_gate_hash");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("libplugin.so");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"native-bytes").unwrap();
        drop(f);

        let mut g = DloadGate::deny_all();
        g.grant_file("plugin", &path).unwrap();
        assert!(g.check_request("plugin").is_ok());
        assert!(g.file_hash_allowed("plugin", &path));

        let other = dir.join("other.so");
        std::fs::write(&other, b"different").unwrap();
        assert!(!g.file_hash_allowed("plugin", &other));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn from_consumer_binds_pin_to_allowed_extra_stem() {
        let dir = std::env::temp_dir().join("coil_dload_gate_pin");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("libplugin.so");
        std::fs::write(&path, b"pinned-plugin").unwrap();
        let hex = hex_sha256(&path);

        let g = DloadGate::from_consumer(["plugin"], &[("plugin".into(), hex)]);
        assert!(g.file_hash_allowed("plugin", &path));
        assert!(!g.file_hash_allowed("sum", &path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn hex_sha256(path: &Path) -> String {
        let h = sha256_file(path).unwrap();
        h.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn grant_stem_allows_libc_without_hash() {
        let mut g = DloadGate::deny_all();
        assert!(g.check_request("c").is_err());
        g.grant_stem("c");
        assert!(g.check_request("c").is_ok());
        assert!(!g.hash_required("c"));
    }
}
