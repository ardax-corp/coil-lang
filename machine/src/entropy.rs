//! `entropy::bytes(n)` for HostInvoke (`entropy_bytes`): `n` bytes from the
//! operating system's CSPRNG (`getrandom`: `getrandom(2)` on Linux,
//! `arc4random_buf` / `getentropy` on the BSDs and macOS, `ProcessPrng` on
//! Windows, `random_get` on WASI).
//!
//! A negative count is a program bug and panics. So does an OS source that
//! fails: there is nothing a caller could do instead, and a fallback to a
//! weaker source would hand out guessable keys silently.

use common::Value;

use crate::memory::{Heap, ObjArray, Object};

/// Fill `buf` from the OS CSPRNG.
pub fn fill(buf: &mut [u8]) -> Result<(), String> {
    getrandom::fill(buf).map_err(|e| format!("entropy::bytes: the OS random source failed: {e}"))
}

/// `entropy::bytes(n) -> Vec<byte>`; `Err` is the panic message.
pub fn host_bytes(heap: &mut Heap, args: &[Value]) -> Result<Value, String> {
    let n = args.first().map(|v| v.as_int()).unwrap_or(0);
    let len = usize::try_from(n).map_err(|_| format!("entropy::bytes: negative count {n}"))?;
    let mut buf = vec![0u8; len];
    fill(&mut buf)?;
    let (obj, _) = heap.alloc(ObjArray::from_bytes(buf), Object::Array);
    Ok(Value::from(obj.addr()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_writes_fresh_bytes() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        fill(&mut a).unwrap();
        fill(&mut b).unwrap();
        assert_ne!(a, b);
    }
}
