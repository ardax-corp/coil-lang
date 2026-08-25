//! Reserved HostInvoke slots for TLS leftover and crypto natives moving to userland.
//!
//! TLS leftover bodies live in `tls.rs` (dload+attach, not rustls). Crypto live
//! bodies stay in `crypto.rs` while that Cargo feature is on. When `crypto` is
//! off, the stub pushers occupy the same ids with fail-closed panics so later
//! natives do not shift.
//!
//! Do not reorder the name tables. Do not bump `ARCHIVE_VERSION` for this
//! reservation.

use std::sync::Arc;

use common::Value;

use crate::memory::{FfiType, Heap};
use crate::{FfiSignature, HostClosureFn, NativeFn};

/// TLS HostInvoke names in table order (stream upgrades, then ALPN).
///
/// The four stream natives sit at the end of the IO block; `tls_alpn_protocol`
/// is append-only after `write_from`. Ids reserved; do not reorder. Live
/// leftover bodies are in `tls.rs` (not these panic stubs).
pub const RESERVED_TLS_HOSTINVOKE: &[&str] = &[
    "tls_client_enable",
    "tls_client_disable",
    "tls_server_enable",
    "tls_server_disable",
    "tls_alpn_protocol",
];

/// Stream-upgrade TLS natives: `(registry_name, arity)`.
///
/// Registered at the end of the IO block (after `udp_local_port`).
pub const RESERVED_TLS_STREAM_HOSTINVOKE: &[(&str, usize)] = &[
    ("tls_client_enable", 3),
    ("tls_client_disable", 1),
    ("tls_server_enable", 2),
    ("tls_server_disable", 1),
];

/// ALPN native: `(registry_name, arity)`. Registered after `write_from`.
pub const RESERVED_TLS_ALPN_HOSTINVOKE: &[(&str, usize)] = &[("tls_alpn_protocol", 1)];

/// Crypto HostInvoke names + arities, same order as `CRYPTO_WIRING`.
///
/// Ids reserved; do not reorder; userland extract will stub these.
pub const RESERVED_CRYPTO_HOSTINVOKE: &[(&str, usize)] = &[
    ("crypto_sha256", 1),
    ("crypto_sha512", 1),
    ("crypto_blake3", 1),
    ("crypto_hasher_init", 1),
    ("crypto_hasher_update", 2),
    ("crypto_hasher_finalize", 1),
    ("crypto_hmac_sha256", 2),
    ("crypto_hmac_sha512", 2),
    ("crypto_hmac_verify_sha256", 3),
    ("crypto_random_bytes", 1),
    ("crypto_random_u64", 0),
    ("crypto_chacha20_poly1305_encrypt", 4),
    ("crypto_chacha20_poly1305_decrypt", 4),
    ("crypto_aes_256_gcm_encrypt", 4),
    ("crypto_aes_256_gcm_decrypt", 4),
    ("crypto_ed25519_generate", 0),
    ("crypto_ed25519_sign", 2),
    ("crypto_ed25519_verify", 3),
    ("crypto_x25519_generate", 0),
    ("crypto_x25519_shared_secret", 2),
    ("crypto_argon2id_hash", 2),
    ("crypto_argon2id_verify", 2),
    ("crypto_ct_eq", 2),
];

/// Fail-closed body after coil-tls / coil-crypto extract the live native.
pub fn reserved_slot_panic(name: &'static str) -> ! {
    panic!("reserved HostInvoke `{name}`: this native moved to coil-tls / coil-crypto; recompile");
}

/// Register the 23 crypto slots as fail-closed panics (when `crypto` is off).
pub fn push_crypto_stubs(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut impl FnMut(&str, usize),
) {
    push_panic_stubs(out, register_id, RESERVED_CRYPTO_HOSTINVOKE);
}

/// Register the four TLS stream-upgrade slots after `udp_local_port`.
///
/// Unused on the live path: leftover enable/disable bodies occupy these ids.
#[allow(dead_code)]
pub fn push_tls_stream_stubs(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut impl FnMut(&str, usize),
) {
    push_panic_stubs(out, register_id, RESERVED_TLS_STREAM_HOSTINVOKE);
}

/// Register `tls_alpn_protocol` after `write_from` as a fail-closed panic.
///
/// Unused on the live path: leftover `tls_alpn_protocol` occupies this id.
#[allow(dead_code)]
pub fn push_tls_alpn_stub(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut impl FnMut(&str, usize),
) {
    push_panic_stubs(out, register_id, RESERVED_TLS_ALPN_HOSTINVOKE);
}

fn push_panic_stubs(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut impl FnMut(&str, usize),
    table: &'static [(&'static str, usize)],
) {
    for &(name, arity) in table {
        let args = vec![FfiType::Int; arity];
        let sig = FfiSignature::from_parts(name.to_string(), args, FfiType::Int)
            .unwrap_or_else(|_| panic!("reserved HostInvoke signature for {name}"));
        let id = out.len();
        register_id(name, id);
        out.push(Arc::new(HostClosureFn::new(
            sig,
            move |_heap: &mut Heap, _args: &[Value]| {
                reserved_slot_panic(name);
            },
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_standard_host_natives;
    use std::collections::HashMap;

    fn registered_ids() -> HashMap<String, usize> {
        let mut map = HashMap::new();
        build_standard_host_natives(|name, id| {
            map.insert(name.to_string(), id);
        });
        map
    }

    fn registered_names() -> Vec<String> {
        let mut names = Vec::new();
        build_standard_host_natives(|name, _id| names.push(name.to_string()));
        names
    }

    #[test]
    fn reserved_tls_and_crypto_slots_are_registered() {
        let map = registered_ids();
        assert_eq!(RESERVED_TLS_HOSTINVOKE.len(), 5);
        assert_eq!(RESERVED_CRYPTO_HOSTINVOKE.len(), 23);
        for name in RESERVED_TLS_HOSTINVOKE {
            assert!(
                map.contains_key(*name),
                "{name} must occupy a HostInvoke slot"
            );
        }
        for &(name, _) in RESERVED_CRYPTO_HOSTINVOKE {
            assert!(
                map.contains_key(name),
                "{name} must occupy a HostInvoke slot"
            );
        }
    }

    #[test]
    fn tls_stream_slots_follow_udp_local_port() {
        let names = registered_names();
        let udp = names
            .iter()
            .position(|n| n == "udp_local_port")
            .expect("udp_local_port");
        for (offset, &(name, _)) in RESERVED_TLS_STREAM_HOSTINVOKE.iter().enumerate() {
            assert_eq!(
                names[udp + 1 + offset],
                name,
                "TLS stream native {name} must keep its IO-block slot"
            );
        }
    }

    #[test]
    fn crypto_slots_follow_env_exec() {
        let names = registered_names();
        let env = names
            .iter()
            .position(|n| n == "env_exec")
            .expect("env_exec");
        for (offset, &(name, _)) in RESERVED_CRYPTO_HOSTINVOKE.iter().enumerate() {
            assert_eq!(
                names[env + 1 + offset],
                name,
                "crypto native {name} must keep its slot after env"
            );
        }
    }

    #[test]
    fn tls_alpn_follows_write_from() {
        let names = registered_names();
        let write_from = names
            .iter()
            .position(|n| n == "write_from")
            .expect("write_from");
        assert_eq!(names[write_from + 1], RESERVED_TLS_ALPN_HOSTINVOKE[0].0);
    }

    #[cfg(feature = "crypto")]
    #[test]
    fn reserved_crypto_table_matches_crypto_wiring() {
        let wiring: Vec<(&str, usize)> = crate::CRYPTO_WIRING
            .iter()
            .map(|&(name, arity, _)| (name, arity))
            .collect();
        assert_eq!(wiring, RESERVED_CRYPTO_HOSTINVOKE);
    }

    #[cfg(all(feature = "crypto", feature = "time"))]
    /// Default-feature HostInvoke ids. Inserting a native *before* a reserved
    /// slot must update this snapshot.
    const RESERVED_HOSTINVOKE_ID_SNAPSHOT: &[(&str, usize)] = &[
        ("tls_client_enable", 25),
        ("tls_client_disable", 26),
        ("tls_server_enable", 27),
        ("tls_server_disable", 28),
        ("crypto_sha256", 69),
        ("crypto_sha512", 70),
        ("crypto_blake3", 71),
        ("crypto_hasher_init", 72),
        ("crypto_hasher_update", 73),
        ("crypto_hasher_finalize", 74),
        ("crypto_hmac_sha256", 75),
        ("crypto_hmac_sha512", 76),
        ("crypto_hmac_verify_sha256", 77),
        ("crypto_random_bytes", 78),
        ("crypto_random_u64", 79),
        ("crypto_chacha20_poly1305_encrypt", 80),
        ("crypto_chacha20_poly1305_decrypt", 81),
        ("crypto_aes_256_gcm_encrypt", 82),
        ("crypto_aes_256_gcm_decrypt", 83),
        ("crypto_ed25519_generate", 84),
        ("crypto_ed25519_sign", 85),
        ("crypto_ed25519_verify", 86),
        ("crypto_x25519_generate", 87),
        ("crypto_x25519_shared_secret", 88),
        ("crypto_argon2id_hash", 89),
        ("crypto_argon2id_verify", 90),
        ("crypto_ct_eq", 91),
        ("tls_alpn_protocol", 121),
    ];

    #[cfg(all(feature = "crypto", feature = "time"))]
    #[test]
    fn reserved_tls_and_crypto_hostinvoke_ids_match_snapshot() {
        let map = registered_ids();
        assert_eq!(RESERVED_HOSTINVOKE_ID_SNAPSHOT.len(), 5 + 23);
        for &(name, id) in RESERVED_HOSTINVOKE_ID_SNAPSHOT {
            assert_eq!(
                map.get(name).copied(),
                Some(id),
                "native `{name}` must keep HostInvoke id {id}"
            );
        }
    }

    #[cfg(not(feature = "crypto"))]
    #[test]
    #[should_panic(expected = "reserved HostInvoke `crypto_sha256`")]
    fn crypto_stub_panics_when_feature_is_off() {
        let natives = build_standard_host_natives(|_, _| {});
        let native = natives
            .iter()
            .find(|n| n.name() == "crypto_sha256")
            .expect("crypto_sha256 stub");
        let mut heap = crate::Heap::default();
        let _ = native.invoke(&mut heap, &[Value::from(0i64)]);
    }
}
