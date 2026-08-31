//! Fail-closed `dload` gate: `[ffi] allow` plus lock hash or `trusted`.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::resolve::{dload_request_stem, is_libc_alias};
use super::signature::FfiError;

/// Default-deny policy for opening shared libraries.
///
/// Every stem (including `crypto`, `tls`, `regex`, `time`) needs consumer
/// `[ffi] allow` **and** a matching `[[package.native]] sha256`, unless that
/// stem's `[dependencies]` row is `trusted = true` (honor-only skip of native
/// sha256). Host [`Self::grant_file`] / [`Self::grant_stem`] remain test-only
/// and do not restore a first-party exemption.
#[derive(Clone, Debug, Default)]
pub struct DloadGate {
    allowed_stems: HashSet<String>,
    hashes_by_stem: HashMap<String, HashSet<[u8; 32]>>,
    /// Host/test stems that skip lock hashing (`set_dload_allowlist`).
    host_unhashed: HashSet<String>,
    /// Allow-listed stems whose dep row is `trusted = true`.
    trusted_unhashed: HashSet<String>,
    /// Per-Machine `Stream.attach` grant (`[ffi] allow_attach`). Default off.
    allow_attach: bool,
}

impl DloadGate {
    /// Deny every stem, including first-party `crypto` / `tls` / `regex` / `time`.
    pub fn deny_all() -> Self {
        Self::default()
    }

    /// `[ffi] allow_attach` for this gate (default false). Not process-global.
    pub fn set_allow_attach(&mut self, allow: bool) {
        self.allow_attach = allow;
    }

    /// Whether `Stream.attach` is allowed on VMs using this gate.
    pub fn allow_attach(&self) -> bool {
        self.allow_attach
    }

    /// Stems from consumer `[ffi] allow` and lock native `(stem, sha256 hex)` pins.
    ///
    /// Libc aliases in `allow` or lock pins are ignored. First-party stems are
    /// addable via `allow`. A lock pin whose package is not on `allow` is ignored.
    pub fn from_consumer(
        allow: impl IntoIterator<Item = impl AsRef<str>>,
        native_pins: &[(String, String)],
    ) -> Self {
        Self::from_consumer_trusted(allow, native_pins, std::iter::empty::<&str>())
    }

    /// Like [`Self::from_consumer`], plus stems whose dep row is `trusted = true`.
    ///
    /// Trusted skips **native sha256** for that stem. The stem must still be on
    /// `[ffi] allow`. Libc aliases are ignored (`trusted` is not an allowlist).
    pub fn from_consumer_trusted(
        allow: impl IntoIterator<Item = impl AsRef<str>>,
        native_pins: &[(String, String)],
        trusted: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        let mut gate = Self::deny_all();
        for stem in allow {
            let stem = stem.as_ref();
            if is_libc_alias(stem) {
                continue;
            }
            if dload_request_stem(stem) != stem {
                continue;
            }
            gate.allowed_stems.insert(stem.to_string());
        }
        for (pkg, hex) in native_pins {
            if is_libc_alias(pkg) {
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
        for stem in trusted {
            let stem = stem.as_ref();
            if is_libc_alias(stem) {
                continue;
            }
            if dload_request_stem(stem) != stem {
                continue;
            }
            if gate.allowed_stems.contains(stem) {
                gate.trusted_unhashed.insert(stem.to_string());
            }
        }
        gate
    }

    /// Host/test stem with no lock hash (libc / fixtures on dyld or DLL search).
    ///
    /// Manifest `[ffi] allow` cannot do this. Does not restore a first-party exemption.
    pub fn grant_stem(&mut self, stem: &str) {
        self.host_unhashed.insert(stem.to_string());
        self.allowed_stems.insert(stem.to_string());
    }

    /// Host/test grant: allow `stem` only for files whose contents match `path`.
    ///
    /// Fixture hook for hashed extras (`sum`). Does not restore a first-party exemption.
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

    fn stem_granted(&self, stem: &str) -> bool {
        self.allowed_stems.contains(stem)
            && (self.trusted_unhashed.contains(stem)
                || self.hashes_by_stem.get(stem).is_some_and(|h| !h.is_empty()))
    }

    /// Stem check before candidate search. Absolute paths use the filename stem.
    pub fn check_request(&self, name: &str) -> Result<String, FfiError> {
        let stem = dload_request_stem(name);
        if self.stem_granted(&stem) || self.host_unhashed.contains(&stem) {
            return Ok(stem);
        }
        Err(FfiError::LibraryDenied {
            name: name.to_string(),
            stem: stem.clone(),
            reason: if is_libc_alias(name) || is_libc_alias(&stem) {
                "libc aliases cannot be dloaded".into()
            } else {
                "stem lacks consumer [ffi] allow + lock hash or trusted".into()
            },
        })
    }

    /// Allow+hash stems must match a pin. Host unhashed and allow+trusted skip.
    pub fn hash_required(&self, stem: &str) -> bool {
        !self.host_unhashed.contains(stem) && !self.trusted_unhashed.contains(stem)
    }

    /// Whether `path`'s contents may be opened for `stem`.
    ///
    /// Host unhashed stems and allow+trusted stems skip hashing.
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

    /// Denied because the stem is granted but no pin matched this file.
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
    fn allow_attach_defaults_off_and_is_per_gate() {
        let mut a = DloadGate::deny_all();
        let b = DloadGate::deny_all();
        assert!(!a.allow_attach());
        assert!(!b.allow_attach());
        a.set_allow_attach(true);
        assert!(a.allow_attach());
        assert!(!b.allow_attach());
    }

    #[test]
    fn deny_all_rejects_c_and_unknown() {
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
    }

    #[test]
    fn extra_allow_without_pin_is_denied() {
        let g = DloadGate::from_consumer(["plugin"], &[]);
        assert!(matches!(
            g.check_request("plugin"),
            Err(FfiError::LibraryDenied { .. })
        ));
        assert!(matches!(
            g.check_request("tls"),
            Err(FfiError::LibraryDenied { .. })
        ));
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
    fn first_party_allow_without_pin_is_denied() {
        let g = DloadGate::from_consumer(["tls", "crypto"], &[]);
        assert!(matches!(
            g.check_request("tls"),
            Err(FfiError::LibraryDenied { .. })
        ));
        assert!(g.hash_required("tls"));
        assert!(!g.file_hash_allowed("tls", Path::new("/coil-dload-missing/libtls.so")));
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

    #[test]
    fn trusted_extra_on_allow_skips_hash_without_pin() {
        let g = DloadGate::from_consumer_trusted(["plugin"], &[], ["plugin"]);
        assert!(g.check_request("plugin").is_ok());
        assert!(!g.hash_required("plugin"));
        let missing = Path::new("/coil-dload-missing/libplugin.so");
        assert!(g.file_hash_allowed("plugin", missing));
    }

    #[test]
    fn trusted_extra_on_allow_ignores_wrong_pin() {
        let dir = std::env::temp_dir().join("coil_dload_gate_trusted_wrong_pin");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("libplugin.so");
        std::fs::write(&path, b"plugin-bytes").unwrap();
        let wrong = "ab".repeat(32);
        let g =
            DloadGate::from_consumer_trusted(["plugin"], &[("plugin".into(), wrong)], ["plugin"]);
        assert!(g.check_request("plugin").is_ok());
        assert!(!g.hash_required("plugin"));
        assert!(g.file_hash_allowed("plugin", &path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trusted_false_extra_without_pin_is_denied() {
        let g = DloadGate::from_consumer_trusted(["plugin"], &[], std::iter::empty::<&str>());
        assert!(matches!(
            g.check_request("plugin"),
            Err(FfiError::LibraryDenied { .. })
        ));
        assert!(g.hash_required("plugin"));
    }

    #[test]
    fn trusted_without_allow_is_denied() {
        let g = DloadGate::from_consumer_trusted(std::iter::empty::<&str>(), &[], ["plugin"]);
        assert!(matches!(
            g.check_request("plugin"),
            Err(FfiError::LibraryDenied { .. })
        ));
        assert!(g.hash_required("plugin"));
    }

    #[test]
    fn trusted_on_libc_is_still_denied() {
        let g = DloadGate::from_consumer_trusted(["c", "libc", "plugin"], &[], ["c", "libc"]);
        assert!(matches!(
            g.check_request("c"),
            Err(FfiError::LibraryDenied { .. })
        ));
        assert!(g.check_request("libc").is_err());
        assert!(g.hash_required("c"));
        assert!(g.check_request("plugin").is_err());
    }

    #[test]
    fn omitted_trusted_extra_on_allow_still_requires_hash() {
        let g = DloadGate::from_consumer(["plugin"], &[]);
        assert!(matches!(
            g.check_request("plugin"),
            Err(FfiError::LibraryDenied { .. })
        ));
        assert!(g.hash_required("plugin"));
    }

    #[test]
    fn trusted_on_one_extra_does_not_skip_hash_on_another() {
        let g = DloadGate::from_consumer_trusted(["plugin", "sum"], &[], ["plugin"]);
        assert!(g.check_request("plugin").is_ok());
        assert!(!g.hash_required("plugin"));
        assert!(matches!(
            g.check_request("sum"),
            Err(FfiError::LibraryDenied { .. })
        ));
        assert!(g.hash_required("sum"));
    }

    #[test]
    fn trusted_plugin_does_not_grant_c() {
        let g = DloadGate::from_consumer_trusted(["plugin", "c"], &[], ["plugin", "c"]);
        assert!(g.check_request("plugin").is_ok());
        assert!(matches!(
            g.check_request("c"),
            Err(FfiError::LibraryDenied { .. })
        ));
    }

    #[test]
    fn deny_all_denies_first_party_stems() {
        let g = DloadGate::deny_all();
        for stem in DLOAD_PRODUCTION_STEMS {
            assert!(
                matches!(g.check_request(stem), Err(FfiError::LibraryDenied { .. })),
                "{stem} must not load without allow and hash|trusted"
            );
            assert!(
                g.hash_required(stem),
                "{stem} must require a lock hash unless trusted"
            );
        }
    }

    #[test]
    fn first_party_allow_without_hash_or_trusted_is_denied() {
        let g = DloadGate::from_consumer(["crypto", "tls", "regex", "time"], &[]);
        for stem in DLOAD_PRODUCTION_STEMS {
            assert!(matches!(
                g.check_request(stem),
                Err(FfiError::LibraryDenied { .. })
            ));
            assert!(g.hash_required(stem));
        }
    }

    #[test]
    fn first_party_trusted_without_allow_is_denied() {
        let g = DloadGate::from_consumer_trusted(
            std::iter::empty::<&str>(),
            &[],
            ["crypto", "tls", "regex", "time"],
        );
        for stem in DLOAD_PRODUCTION_STEMS {
            assert!(matches!(
                g.check_request(stem),
                Err(FfiError::LibraryDenied { .. })
            ));
        }
    }

    #[test]
    fn first_party_allow_plus_trusted_skips_native_hash() {
        let g = DloadGate::from_consumer_trusted(
            ["crypto", "tls", "regex", "time"],
            &[],
            ["crypto", "tls", "regex", "time"],
        );
        let missing = Path::new("/coil-dload-missing/libcrypto.dylib");
        for stem in DLOAD_PRODUCTION_STEMS {
            g.check_request(stem)
                .unwrap_or_else(|e| panic!("{stem} allow+trusted must pass, got {e:?}"));
            assert!(
                !g.hash_required(stem),
                "{stem} trusted must skip native sha256"
            );
        }
        assert!(g.file_hash_allowed("crypto", missing));
    }

    #[test]
    fn first_party_allow_plus_lock_hash_passes() {
        let dir = std::env::temp_dir().join("coil_dload_gate_crypto_pin");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("libcrypto.so");
        std::fs::write(&path, b"crypto-bytes").unwrap();
        let hex = hex_sha256(&path);
        let g = DloadGate::from_consumer(["crypto"], &[("crypto".into(), hex)]);
        assert!(g.check_request("crypto").is_ok());
        assert!(g.hash_required("crypto"));
        assert!(g.file_hash_allowed("crypto", &path));
        let other = dir.join("other.so");
        std::fs::write(&other, b"different").unwrap();
        assert!(!g.file_hash_allowed("crypto", &other));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_party_trusted_false_without_pin_is_denied() {
        let g = DloadGate::from_consumer_trusted(
            ["crypto", "tls", "regex", "time"],
            &[],
            std::iter::empty::<&str>(),
        );
        for stem in DLOAD_PRODUCTION_STEMS {
            assert!(matches!(
                g.check_request(stem),
                Err(FfiError::LibraryDenied { .. })
            ));
            assert!(g.hash_required(stem));
        }
    }

    #[test]
    fn first_party_trusted_crypto_does_not_grant_tls() {
        let g = DloadGate::from_consumer_trusted(["crypto", "tls"], &[], ["crypto"]);
        assert!(g.check_request("crypto").is_ok());
        assert!(!g.hash_required("crypto"));
        assert!(matches!(
            g.check_request("tls"),
            Err(FfiError::LibraryDenied { .. })
        ));
        assert!(g.hash_required("tls"));
    }
}
