//! Host-backed cryptography (pure Rust; no OpenSSL).
//!
//! See [`CRYPTO_WIRING`] for pipeline `HostInvoke` registry names and arities.

use aes_gcm::{
    Aes256Gcm, Nonce as AesNonce,
    aead::{Aead, KeyInit, Payload},
};
use argon2::{
    Argon2, Params, Version,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use blake3::Hasher as Blake3;
use chacha20poly1305::{ChaCha20Poly1305, Nonce as ChaChaNonce};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use getrandom::fill as getrandom_fill;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256, Sha512};
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey as X25519Public, StaticSecret as X25519Secret};
use zeroize::Zeroize;

use common::{BUILTIN_CRYPTO_ERROR_VARIANTS, Value};

use crate::crypto_hasher_state::{HasherAlg, ObjCryptoHasher};
use crate::io::{alloc_result_err, alloc_result_ok, value_as_bytes, value_as_string};
use crate::memory::{Heap, Member, ObjArray, ObjTuple, Object};

/// Tag indices for [`CryptoError`](common::BUILTIN_CRYPTO_ERROR_ENUM).
#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CryptoErrorTag {
    InvalidInput = 0,
    InvalidLength = 1,
    AuthenticationFailed = 2,
    UnsupportedAlgorithm = 3,
    AlreadyFinalized = 4,
    Other = 5,
}

impl CryptoErrorTag {
    fn from_getrandom(err: getrandom::Error) -> Self {
        let _ = err;
        Self::Other
    }
}

/// Allocate a unit-payload `CryptoError` variant.
pub fn alloc_crypto_error(heap: &mut Heap, tag: CryptoErrorTag) -> Value {
    let _ = BUILTIN_CRYPTO_ERROR_VARIANTS;
    alloc_enum(heap, tag as u32, vec![])
}

fn alloc_enum(heap: &mut Heap, tag: u32, payload: Vec<Member>) -> Value {
    heap.alloc_enum_value(tag, payload)
}

pub fn alloc_bytes(heap: &mut Heap, bytes: &[u8]) -> Value {
    let elements: Vec<Value> = bytes.iter().map(|&b| Value::from(b as i64)).collect();
    let (obj, _) = heap.alloc(ObjArray { elements }, Object::Array);
    Value::from(obj.addr())
}

fn as_result_bytes_crypto(heap: &mut Heap, r: Result<Vec<u8>, CryptoErrorTag>) -> Value {
    match r {
        Ok(bytes) => {
            let payload = alloc_bytes(heap, &bytes);
            alloc_result_ok(heap, payload)
        }
        Err(tag) => {
            let err = alloc_crypto_error(heap, tag);
            alloc_result_err(heap, err)
        }
    }
}

fn as_result_bool_crypto(heap: &mut Heap, r: Result<bool, CryptoErrorTag>) -> Value {
    match r {
        Ok(b) => alloc_result_ok(heap, Value::from(if b { 1_i64 } else { 0_i64 })),
        Err(tag) => {
            let err = alloc_crypto_error(heap, tag);
            alloc_result_err(heap, err)
        }
    }
}

fn as_result_unit_crypto(heap: &mut Heap, r: Result<(), CryptoErrorTag>) -> Value {
    match r {
        Ok(()) => alloc_result_ok(heap, Value::default()),
        Err(tag) => {
            let err = alloc_crypto_error(heap, tag);
            alloc_result_err(heap, err)
        }
    }
}

fn as_result_value_crypto(heap: &mut Heap, r: Result<Value, CryptoErrorTag>) -> Value {
    match r {
        Ok(v) => alloc_result_ok(heap, v),
        Err(tag) => {
            let err = alloc_crypto_error(heap, tag);
            alloc_result_err(heap, err)
        }
    }
}

fn parse_hasher_alg(heap: &Heap, v: Value) -> Result<HasherAlg, CryptoErrorTag> {
    if let Ok(name) = value_as_string(heap, v) {
        return HasherAlg::from_name(&name).ok_or(CryptoErrorTag::UnsupportedAlgorithm);
    }
    let tag = v.as_int();
    HasherAlg::from_tag(tag).ok_or(CryptoErrorTag::UnsupportedAlgorithm)
}

fn with_crypto_hasher<R>(
    heap: &mut Heap,
    handle: Value,
    f: impl FnOnce(&mut ObjCryptoHasher) -> Result<R, CryptoErrorTag>,
) -> Result<R, CryptoErrorTag> {
    heap.with_crypto_hasher(handle.raw() as u64, f)
        .ok_or(CryptoErrorTag::InvalidInput)?
}

fn try_sha256(heap: &Heap, data: Value) -> Result<Vec<u8>, CryptoErrorTag> {
    let bytes = value_as_bytes(heap, data).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hasher.finalize().to_vec())
}

fn try_sha512(heap: &Heap, data: Value) -> Result<Vec<u8>, CryptoErrorTag> {
    let bytes = value_as_bytes(heap, data).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let mut hasher = Sha512::new();
    hasher.update(&bytes);
    Ok(hasher.finalize().to_vec())
}

fn try_blake3(heap: &Heap, data: Value) -> Result<Vec<u8>, CryptoErrorTag> {
    let bytes = value_as_bytes(heap, data).map_err(|_| CryptoErrorTag::InvalidInput)?;
    Ok(Blake3::new().update(&bytes).finalize().as_bytes().to_vec())
}

pub fn host_sha256(heap: &mut Heap, args: &[Value]) -> Value {
    as_result_bytes_crypto(heap, try_sha256(heap, args[0]))
}

pub fn host_sha512(heap: &mut Heap, args: &[Value]) -> Value {
    as_result_bytes_crypto(heap, try_sha512(heap, args[0]))
}

pub fn host_blake3(heap: &mut Heap, args: &[Value]) -> Value {
    as_result_bytes_crypto(heap, try_blake3(heap, args[0]))
}

pub fn host_hasher_init(heap: &mut Heap, args: &[Value]) -> Value {
    let r = (|| {
        let alg = parse_hasher_alg(heap, args[0])?;
        let (obj, _) = heap.alloc(ObjCryptoHasher::new(alg), Object::CryptoHasher);
        Ok(Value::from(obj.addr()))
    })();
    as_result_value_crypto(heap, r)
}

pub fn host_hasher_update(heap: &mut Heap, args: &[Value]) -> Value {
    let r = (|| {
        let data = value_as_bytes(heap, args[1]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        with_crypto_hasher(heap, args[0], |hasher| {
            let Some(state) = hasher.state.as_mut() else {
                return Err(CryptoErrorTag::AlreadyFinalized);
            };
            state.update(&data);
            Ok(())
        })
    })();
    as_result_unit_crypto(heap, r)
}

pub fn host_hasher_finalize(heap: &mut Heap, args: &[Value]) -> Value {
    let r = (|| {
        let digest: Vec<u8> = with_crypto_hasher(heap, args[0], |hasher| {
            let state = hasher
                .state
                .take()
                .ok_or(CryptoErrorTag::AlreadyFinalized)?;
            Ok(state.finalize())
        })?;
        Ok(alloc_bytes(heap, &digest))
    })();
    as_result_value_crypto(heap, r)
}

type HmacSha256 = Hmac<Sha256>;
type HmacSha512 = Hmac<Sha512>;

fn try_hmac_sha256(heap: &Heap, key: Value, data: Value) -> Result<Vec<u8>, CryptoErrorTag> {
    let key_bytes = value_as_bytes(heap, key).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let data_bytes = value_as_bytes(heap, data).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let mut mac =
        HmacSha256::new_from_slice(&key_bytes).map_err(|_| CryptoErrorTag::InvalidLength)?;
    mac.update(&data_bytes);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn try_hmac_sha512(heap: &Heap, key: Value, data: Value) -> Result<Vec<u8>, CryptoErrorTag> {
    let key_bytes = value_as_bytes(heap, key).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let data_bytes = value_as_bytes(heap, data).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let mut mac =
        HmacSha512::new_from_slice(&key_bytes).map_err(|_| CryptoErrorTag::InvalidLength)?;
    mac.update(&data_bytes);
    Ok(mac.finalize().into_bytes().to_vec())
}

pub fn host_hmac_sha256(heap: &mut Heap, args: &[Value]) -> Value {
    as_result_bytes_crypto(heap, try_hmac_sha256(heap, args[0], args[1]))
}

pub fn host_hmac_sha512(heap: &mut Heap, args: &[Value]) -> Value {
    as_result_bytes_crypto(heap, try_hmac_sha512(heap, args[0], args[1]))
}

pub fn host_hmac_verify_sha256(heap: &mut Heap, args: &[Value]) -> Value {
    let r = (|| {
        let key_bytes = value_as_bytes(heap, args[0]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        let data_bytes = value_as_bytes(heap, args[1]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        let tag_bytes = value_as_bytes(heap, args[2]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        let mut mac =
            HmacSha256::new_from_slice(&key_bytes).map_err(|_| CryptoErrorTag::InvalidLength)?;
        mac.update(&data_bytes);
        mac.verify_slice(&tag_bytes)
            .map_err(|_| CryptoErrorTag::AuthenticationFailed)?;
        Ok(true)
    })();
    as_result_bool_crypto(heap, r)
}

const MAX_RANDOM_BYTES: usize = 1 << 20;

pub fn host_random_bytes(heap: &mut Heap, args: &[Value]) -> Value {
    let r = (|| {
        let n = args[0].as_int();
        if n < 0 || n as usize > MAX_RANDOM_BYTES {
            return Err(CryptoErrorTag::InvalidInput);
        }
        let n = n as usize;
        let mut buf = vec![0_u8; n];
        getrandom_fill(&mut buf).map_err(CryptoErrorTag::from_getrandom)?;
        Ok(alloc_bytes(heap, &buf))
    })();
    as_result_value_crypto(heap, r)
}

pub fn host_random_u64(heap: &mut Heap, _args: &[Value]) -> Value {
    let r = (|| {
        let mut buf = [0_u8; 8];
        getrandom_fill(&mut buf).map_err(CryptoErrorTag::from_getrandom)?;
        Ok(Value::from(u64::from_le_bytes(buf) as i64))
    })();
    as_result_value_crypto(heap, r)
}

fn aead_chacha_encrypt(
    heap: &Heap,
    key: Value,
    nonce: Value,
    plaintext: Value,
    aad: Value,
) -> Result<Vec<u8>, CryptoErrorTag> {
    let key = value_as_bytes(heap, key).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let nonce = value_as_bytes(heap, nonce).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let pt = value_as_bytes(heap, plaintext).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let aad = value_as_bytes(heap, aad).map_err(|_| CryptoErrorTag::InvalidInput)?;
    if key.len() != 32 || nonce.len() != 12 {
        return Err(CryptoErrorTag::InvalidLength);
    }
    let cipher =
        ChaCha20Poly1305::new_from_slice(&key).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let nonce: ChaChaNonce = nonce
        .as_slice()
        .try_into()
        .map_err(|_| CryptoErrorTag::InvalidLength)?;
    cipher
        .encrypt(
            &nonce,
            Payload {
                msg: &pt,
                aad: &aad,
            },
        )
        .map_err(|_| CryptoErrorTag::Other)
}

fn aead_chacha_decrypt(
    heap: &Heap,
    key: Value,
    nonce: Value,
    ciphertext: Value,
    aad: Value,
) -> Result<Vec<u8>, CryptoErrorTag> {
    let key = value_as_bytes(heap, key).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let nonce = value_as_bytes(heap, nonce).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let ct = value_as_bytes(heap, ciphertext).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let aad = value_as_bytes(heap, aad).map_err(|_| CryptoErrorTag::InvalidInput)?;
    if key.len() != 32 || nonce.len() != 12 {
        return Err(CryptoErrorTag::InvalidLength);
    }
    let cipher =
        ChaCha20Poly1305::new_from_slice(&key).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let nonce: ChaChaNonce = nonce
        .as_slice()
        .try_into()
        .map_err(|_| CryptoErrorTag::InvalidLength)?;
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &ct,
                aad: &aad,
            },
        )
        .map_err(|_| CryptoErrorTag::AuthenticationFailed)
}

pub fn host_chacha20_poly1305_encrypt(heap: &mut Heap, args: &[Value]) -> Value {
    as_result_bytes_crypto(
        heap,
        aead_chacha_encrypt(heap, args[0], args[1], args[2], args[3]),
    )
}

pub fn host_chacha20_poly1305_decrypt(heap: &mut Heap, args: &[Value]) -> Value {
    as_result_bytes_crypto(
        heap,
        aead_chacha_decrypt(heap, args[0], args[1], args[2], args[3]),
    )
}

fn aead_aes_encrypt(
    heap: &Heap,
    key: Value,
    nonce: Value,
    plaintext: Value,
    aad: Value,
) -> Result<Vec<u8>, CryptoErrorTag> {
    let key = value_as_bytes(heap, key).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let nonce = value_as_bytes(heap, nonce).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let pt = value_as_bytes(heap, plaintext).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let aad = value_as_bytes(heap, aad).map_err(|_| CryptoErrorTag::InvalidInput)?;
    if key.len() != 32 || nonce.len() != 12 {
        return Err(CryptoErrorTag::InvalidLength);
    }
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let nonce_bytes: [u8; 12] = nonce
        .as_slice()
        .try_into()
        .map_err(|_| CryptoErrorTag::InvalidLength)?;
    let nonce = AesNonce::from(nonce_bytes);
    cipher
        .encrypt(
            &nonce,
            Payload {
                msg: &pt,
                aad: &aad,
            },
        )
        .map_err(|_| CryptoErrorTag::Other)
}

fn aead_aes_decrypt(
    heap: &Heap,
    key: Value,
    nonce: Value,
    ciphertext: Value,
    aad: Value,
) -> Result<Vec<u8>, CryptoErrorTag> {
    let key = value_as_bytes(heap, key).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let nonce = value_as_bytes(heap, nonce).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let ct = value_as_bytes(heap, ciphertext).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let aad = value_as_bytes(heap, aad).map_err(|_| CryptoErrorTag::InvalidInput)?;
    if key.len() != 32 || nonce.len() != 12 {
        return Err(CryptoErrorTag::InvalidLength);
    }
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| CryptoErrorTag::InvalidInput)?;
    let nonce_bytes: [u8; 12] = nonce
        .as_slice()
        .try_into()
        .map_err(|_| CryptoErrorTag::InvalidLength)?;
    let nonce = AesNonce::from(nonce_bytes);
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &ct,
                aad: &aad,
            },
        )
        .map_err(|_| CryptoErrorTag::AuthenticationFailed)
}

pub fn host_aes_256_gcm_encrypt(heap: &mut Heap, args: &[Value]) -> Value {
    as_result_bytes_crypto(
        heap,
        aead_aes_encrypt(heap, args[0], args[1], args[2], args[3]),
    )
}

pub fn host_aes_256_gcm_decrypt(heap: &mut Heap, args: &[Value]) -> Value {
    as_result_bytes_crypto(
        heap,
        aead_aes_decrypt(heap, args[0], args[1], args[2], args[3]),
    )
}

fn alloc_keypair(heap: &mut Heap, secret: &[u8], public: &[u8]) -> Value {
    let sk = alloc_bytes(heap, secret);
    let pk = alloc_bytes(heap, public);
    let (obj, _) = heap.alloc(
        ObjTuple {
            elements: vec![sk, pk],
        },
        Object::Tuple,
    );
    Value::from(obj.addr())
}

pub fn host_ed25519_generate(heap: &mut Heap, _args: &[Value]) -> Value {
    let r = (|| {
        let mut seed = [0_u8; 32];
        getrandom_fill(&mut seed).map_err(CryptoErrorTag::from_getrandom)?;
        let signing = SigningKey::from_bytes(&seed);
        seed.zeroize();
        let verifying: VerifyingKey = signing.verifying_key();
        Ok(alloc_keypair(
            heap,
            signing.as_bytes(),
            verifying.as_bytes(),
        ))
    })();
    as_result_value_crypto(heap, r)
}

pub fn host_ed25519_sign(heap: &mut Heap, args: &[Value]) -> Value {
    let r = (|| {
        let sk_bytes = value_as_bytes(heap, args[0]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        if sk_bytes.len() != 32 {
            return Err(CryptoErrorTag::InvalidLength);
        }
        let mut arr = [0_u8; 32];
        arr.copy_from_slice(&sk_bytes);
        let signing = SigningKey::from_bytes(&arr);
        arr.zeroize();
        let msg = value_as_bytes(heap, args[1]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        let sig = signing.sign(&msg);
        Ok(sig.to_bytes().to_vec())
    })();
    as_result_bytes_crypto(heap, r)
}

pub fn host_ed25519_verify(heap: &mut Heap, args: &[Value]) -> Value {
    let r = (|| {
        let pk_bytes = value_as_bytes(heap, args[0]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        let msg = value_as_bytes(heap, args[1]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        let sig_bytes = value_as_bytes(heap, args[2]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        if pk_bytes.len() != 32 || sig_bytes.len() != 64 {
            return Err(CryptoErrorTag::InvalidLength);
        }
        let verifying = VerifyingKey::from_bytes(pk_bytes.as_slice().try_into().unwrap())
            .map_err(|_| CryptoErrorTag::InvalidInput)?;
        let sig = ed25519_dalek::Signature::from_bytes(
            sig_bytes
                .as_slice()
                .try_into()
                .map_err(|_| CryptoErrorTag::InvalidLength)?,
        );
        verifying
            .verify_strict(&msg, &sig)
            .map_err(|_| CryptoErrorTag::AuthenticationFailed)?;
        Ok(true)
    })();
    as_result_bool_crypto(heap, r)
}

pub fn host_x25519_generate(heap: &mut Heap, _args: &[Value]) -> Value {
    let r = (|| {
        let mut sk = [0_u8; 32];
        getrandom_fill(&mut sk).map_err(CryptoErrorTag::from_getrandom)?;
        let secret = X25519Secret::from(sk);
        sk.zeroize();
        let public = X25519Public::from(&secret);
        Ok(alloc_keypair(heap, secret.as_bytes(), public.as_bytes()))
    })();
    as_result_value_crypto(heap, r)
}

pub fn host_x25519_shared_secret(heap: &mut Heap, args: &[Value]) -> Value {
    let r = (|| {
        let sk_bytes = value_as_bytes(heap, args[0]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        let pk_bytes = value_as_bytes(heap, args[1]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        if sk_bytes.len() != 32 || pk_bytes.len() != 32 {
            return Err(CryptoErrorTag::InvalidLength);
        }
        let sk_arr: [u8; 32] = sk_bytes
            .as_slice()
            .try_into()
            .map_err(|_| CryptoErrorTag::InvalidLength)?;
        let pk_arr: [u8; 32] = pk_bytes
            .as_slice()
            .try_into()
            .map_err(|_| CryptoErrorTag::InvalidLength)?;
        let secret = X25519Secret::from(sk_arr);
        let public = X25519Public::from(pk_arr);
        let shared = secret.diffie_hellman(&public);
        Ok(shared.as_bytes().to_vec())
    })();
    as_result_bytes_crypto(heap, r)
}

pub fn host_argon2id_hash(heap: &mut Heap, args: &[Value]) -> Value {
    let r = (|| {
        let password = value_as_bytes(heap, args[0]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        let salt = value_as_bytes(heap, args[1]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        if salt.is_empty() || salt.len() > 64 {
            return Err(CryptoErrorTag::InvalidLength);
        }
        let salt_b64 = base64_salt(&salt)?;
        // Fixed MVP params (not caller-tunable): 19 MiB, 2 iters, parallelism 1.
        let params =
            Params::new(19 * 1024, 2, 1, None).map_err(|_| CryptoErrorTag::InvalidInput)?;
        let argon2 = Argon2::new(argon2::Algorithm::Argon2id, Version::V0x13, params);
        let hash = argon2
            .hash_password(&password, &salt_b64)
            .map_err(|_| CryptoErrorTag::Other)?;
        let encoded = hash.to_string();
        let gc = heap.intern(encoded);
        Ok(Value::from(gc.as_ptr() as *mut u8 as u64))
    })();
    as_result_value_crypto(heap, r)
}

fn base64_salt(salt: &[u8]) -> Result<SaltString, CryptoErrorTag> {
    if salt.len() >= 16 {
        SaltString::encode_b64(salt).map_err(|_| CryptoErrorTag::InvalidInput)
    } else {
        let mut padded = salt.to_vec();
        padded.resize(16, 0);
        SaltString::encode_b64(&padded).map_err(|_| CryptoErrorTag::InvalidInput)
    }
}

pub fn host_argon2id_verify(heap: &mut Heap, args: &[Value]) -> Value {
    let r = (|| {
        let password = value_as_bytes(heap, args[0]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        let hash_str = value_as_string(heap, args[1]).map_err(|_| CryptoErrorTag::InvalidInput)?;
        let parsed = PasswordHash::new(&hash_str).map_err(|_| CryptoErrorTag::InvalidInput)?;
        Argon2::default()
            .verify_password(&password, &parsed)
            .map_err(|_| CryptoErrorTag::AuthenticationFailed)?;
        Ok(true)
    })();
    as_result_bool_crypto(heap, r)
}

pub fn host_ct_eq(heap: &mut Heap, args: &[Value]) -> Value {
    let a = value_as_bytes(heap, args[0]);
    let b = value_as_bytes(heap, args[1]);
    let eq = match (a, b) {
        (Ok(x), Ok(y)) => x.ct_eq(&y).into(),
        _ => false,
    };
    Value::from(if eq { 1_i64 } else { 0_i64 })
}

/// Stable host-native names for `Pipeline::register_host_native` / `HostInvoke`.
pub const CRYPTO_SHA256: &str = "crypto_sha256";
pub const CRYPTO_SHA512: &str = "crypto_sha512";
pub const CRYPTO_BLAKE3: &str = "crypto_blake3";
pub const CRYPTO_HASHER_INIT: &str = "crypto_hasher_init";
pub const CRYPTO_HASHER_UPDATE: &str = "crypto_hasher_update";
pub const CRYPTO_HASHER_FINALIZE: &str = "crypto_hasher_finalize";
pub const CRYPTO_HMAC_SHA256: &str = "crypto_hmac_sha256";
pub const CRYPTO_HMAC_SHA512: &str = "crypto_hmac_sha512";
pub const CRYPTO_HMAC_VERIFY_SHA256: &str = "crypto_hmac_verify_sha256";
pub const CRYPTO_RANDOM_BYTES: &str = "crypto_random_bytes";
pub const CRYPTO_RANDOM_U64: &str = "crypto_random_u64";
pub const CRYPTO_CHACHA20_POLY1305_ENCRYPT: &str = "crypto_chacha20_poly1305_encrypt";
pub const CRYPTO_CHACHA20_POLY1305_DECRYPT: &str = "crypto_chacha20_poly1305_decrypt";
pub const CRYPTO_AES_256_GCM_ENCRYPT: &str = "crypto_aes_256_gcm_encrypt";
pub const CRYPTO_AES_256_GCM_DECRYPT: &str = "crypto_aes_256_gcm_decrypt";
pub const CRYPTO_ED25519_GENERATE: &str = "crypto_ed25519_generate";
pub const CRYPTO_ED25519_SIGN: &str = "crypto_ed25519_sign";
pub const CRYPTO_ED25519_VERIFY: &str = "crypto_ed25519_verify";
pub const CRYPTO_X25519_GENERATE: &str = "crypto_x25519_generate";
pub const CRYPTO_X25519_SHARED_SECRET: &str = "crypto_x25519_shared_secret";
pub const CRYPTO_ARGON2ID_HASH: &str = "crypto_argon2id_hash";
pub const CRYPTO_ARGON2ID_VERIFY: &str = "crypto_argon2id_verify";
pub const CRYPTO_CT_EQ: &str = "crypto_ct_eq";

/// Pipeline wiring contract: `(registry_name, arity, host_fn)`.
///
/// Ids reserved; do not reorder; userland extract will stub these
/// (coil-crypto). See [`crate::reserved_hostinvoke`].
///
/// Register each entry with `pipeline.register_host_native` and wire via
/// `HostInvoke` + `MakeTuple` (same stack order as `io` / `thread` natives:
/// outer `CONST native_id` first, then `MakeTuple` args in source order).
pub const CRYPTO_WIRING: &[(&str, usize, fn(&mut Heap, &[Value]) -> Value)] = &[
    (CRYPTO_SHA256, 1, host_sha256),
    (CRYPTO_SHA512, 1, host_sha512),
    (CRYPTO_BLAKE3, 1, host_blake3),
    (CRYPTO_HASHER_INIT, 1, host_hasher_init),
    (CRYPTO_HASHER_UPDATE, 2, host_hasher_update),
    (CRYPTO_HASHER_FINALIZE, 1, host_hasher_finalize),
    (CRYPTO_HMAC_SHA256, 2, host_hmac_sha256),
    (CRYPTO_HMAC_SHA512, 2, host_hmac_sha512),
    (CRYPTO_HMAC_VERIFY_SHA256, 3, host_hmac_verify_sha256),
    (CRYPTO_RANDOM_BYTES, 1, host_random_bytes),
    (CRYPTO_RANDOM_U64, 0, host_random_u64),
    (
        CRYPTO_CHACHA20_POLY1305_ENCRYPT,
        4,
        host_chacha20_poly1305_encrypt,
    ),
    (
        CRYPTO_CHACHA20_POLY1305_DECRYPT,
        4,
        host_chacha20_poly1305_decrypt,
    ),
    (CRYPTO_AES_256_GCM_ENCRYPT, 4, host_aes_256_gcm_encrypt),
    (CRYPTO_AES_256_GCM_DECRYPT, 4, host_aes_256_gcm_decrypt),
    (CRYPTO_ED25519_GENERATE, 0, host_ed25519_generate),
    (CRYPTO_ED25519_SIGN, 2, host_ed25519_sign),
    (CRYPTO_ED25519_VERIFY, 3, host_ed25519_verify),
    (CRYPTO_X25519_GENERATE, 0, host_x25519_generate),
    (CRYPTO_X25519_SHARED_SECRET, 2, host_x25519_shared_secret),
    (CRYPTO_ARGON2ID_HASH, 2, host_argon2id_hash),
    (CRYPTO_ARGON2ID_VERIFY, 2, host_argon2id_verify),
    (CRYPTO_CT_EQ, 2, host_ct_eq),
];

pub use host_aes_256_gcm_decrypt as crypto_aes_256_gcm_decrypt;
pub use host_aes_256_gcm_encrypt as crypto_aes_256_gcm_encrypt;
pub use host_argon2id_hash as crypto_argon2id_hash;
pub use host_argon2id_verify as crypto_argon2id_verify;
pub use host_blake3 as crypto_blake3;
pub use host_chacha20_poly1305_decrypt as crypto_chacha20_poly1305_decrypt;
pub use host_chacha20_poly1305_encrypt as crypto_chacha20_poly1305_encrypt;
pub use host_ct_eq as crypto_ct_eq;
pub use host_ed25519_generate as crypto_ed25519_generate;
pub use host_ed25519_sign as crypto_ed25519_sign;
pub use host_ed25519_verify as crypto_ed25519_verify;
pub use host_hasher_finalize as crypto_hasher_finalize;
pub use host_hasher_init as crypto_hasher_init;
pub use host_hasher_update as crypto_hasher_update;
pub use host_hmac_sha256 as crypto_hmac_sha256;
pub use host_hmac_sha512 as crypto_hmac_sha512;
pub use host_hmac_verify_sha256 as crypto_hmac_verify_sha256;
pub use host_random_bytes as crypto_random_bytes;
pub use host_random_u64 as crypto_random_u64;
pub use host_sha256 as crypto_sha256;
pub use host_sha512 as crypto_sha512;
pub use host_x25519_generate as crypto_x25519_generate;
pub use host_x25519_shared_secret as crypto_x25519_shared_secret;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Heap, Object};

    fn result_ok_bytes(heap: &Heap, result: Value) -> Vec<u8> {
        let Object::Enum(gc) = heap.find_object_by_addr(result.raw() as u64).unwrap() else {
            panic!("expected Result");
        };
        assert_eq!(gc.as_ref().tag, 0, "expected Ok");
        let Member::Object(Object::Array(arr)) = &gc.as_ref().payload[0] else {
            panic!("expected byte array");
        };
        arr.as_ref()
            .elements
            .iter()
            .map(|v| v.as_int() as u8)
            .collect()
    }

    fn result_err_tag(heap: &Heap, result: Value) -> u32 {
        let Object::Enum(gc) = heap.find_object_by_addr(result.raw() as u64).unwrap() else {
            panic!("expected Result");
        };
        assert_eq!(gc.as_ref().tag, 1, "expected Err");
        let Member::Object(Object::Enum(err)) = &gc.as_ref().payload[0] else {
            panic!("expected CryptoError enum payload");
        };
        err.as_ref().tag
    }

    fn result_ok_handle(heap: &Heap, result: Value) -> Value {
        let Object::Enum(gc) = heap.find_object_by_addr(result.raw() as u64).unwrap() else {
            panic!("expected Result");
        };
        assert_eq!(gc.as_ref().tag, 0, "expected Ok");
        match &gc.as_ref().payload[0] {
            Member::Value(v) => *v,
            Member::Object(o) => Value::from(o.addr()),
        }
    }

    /// NIST SHA-256 KAT: empty string.
    #[test]
    fn sha256_empty_string_kat() {
        let mut heap = Heap::default();
        let empty = alloc_bytes(&mut heap, b"");
        let out = host_sha256(&mut heap, &[empty]);
        let digest = result_ok_bytes(&heap, out);
        const EXPECTED: [u8; 32] = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];
        assert_eq!(digest.as_slice(), EXPECTED);
    }

    #[test]
    fn chacha20_encrypt_decrypt_roundtrip_and_tamper_fails() {
        let mut heap = Heap::default();
        let key = alloc_bytes(&mut heap, &[0x11_u8; 32]);
        let nonce = alloc_bytes(&mut heap, &[0x22_u8; 12]);
        let pt = alloc_bytes(&mut heap, b"coil-aead");
        let aad = alloc_bytes(&mut heap, b"");

        let ct_r = host_chacha20_poly1305_encrypt(&mut heap, &[key, nonce, pt, aad]);
        let ct = result_ok_bytes(&heap, ct_r);
        assert!(
            ct.len() > b"coil-aead".len(),
            "ciphertext includes auth tag"
        );

        let ct_v = alloc_bytes(&mut heap, &ct);
        let pt_r = host_chacha20_poly1305_decrypt(&mut heap, &[key, nonce, ct_v, aad]);
        assert_eq!(result_ok_bytes(&heap, pt_r), b"coil-aead");

        let mut tampered = ct;
        *tampered.last_mut().unwrap() ^= 0x01;
        let bad = alloc_bytes(&mut heap, &tampered);
        let fail = host_chacha20_poly1305_decrypt(&mut heap, &[key, nonce, bad, aad]);
        assert_eq!(
            result_err_tag(&heap, fail),
            CryptoErrorTag::AuthenticationFailed as u32
        );
    }

    #[test]
    fn hasher_finalize_twice_returns_already_finalized() {
        let mut heap = Heap::default();
        let init = host_hasher_init(&mut heap, &[Value::from(0_i64)]);
        let handle = result_ok_handle(&heap, init);
        let data = alloc_bytes(&mut heap, b"abc");
        let upd = host_hasher_update(&mut heap, &[handle, data]);
        assert_eq!(
            heap.find_object_by_addr(upd.raw() as u64)
                .and_then(|o| match o {
                    Object::Enum(gc) => Some(gc.as_ref().tag),
                    _ => None,
                }),
            Some(0),
            "update should succeed"
        );

        let dig_r = host_hasher_finalize(&mut heap, &[handle]);
        let digest = result_ok_bytes(&heap, dig_r);
        const EXPECTED: [u8; 32] = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(digest.as_slice(), EXPECTED);

        let again = host_hasher_finalize(&mut heap, &[handle]);
        assert_eq!(
            result_err_tag(&heap, again),
            CryptoErrorTag::AlreadyFinalized as u32
        );
    }
}
