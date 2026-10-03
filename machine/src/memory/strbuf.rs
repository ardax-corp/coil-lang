//! String storage with amortized in-place append.
//!
//! `a + b` on immutable strings normally copies `a` every time, so building a
//! string with `s = s + x` in a loop is quadratic. A [`StrData::Shared`] string
//! is a prefix `[0, len)` of a [`SharedBuf`] with spare capacity. Concat checks
//! whether `a` ends exactly where the buffer's claimed bytes end; if so it
//! claims the next `b.len()` bytes with a CAS, copies only `b` there, and the
//! result shares the buffer with a longer `len`. Otherwise (someone already
//! appended past `a`, or the buffer is full) it copies `a + b` into a fresh
//! buffer with doubled capacity.
//!
//! Soundness: bytes below `used` are never written again, and every string
//! that shares a buffer has `len <= used`, so a reader's `&str` never overlaps
//! a region another thread may be writing. The CAS makes a claimed region
//! exclusive to one appender, which matters for steal-epoch workers that
//! share the heap.

use std::alloc::{self, Layout};
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{fence, AtomicUsize, Ordering};
use std::{fmt, mem, ptr, slice, str};

/// Smallest capacity of a buffer created by concat.
const MIN_SHARED_CAP: usize = 32;

#[repr(C)]
struct Header {
    refs: AtomicUsize,
    /// Bytes claimed so far; `[0, used)` is immutable.
    used: AtomicUsize,
    cap: usize,
}

/// Reference-counted byte buffer that only grows at its tail.
pub struct SharedBuf(NonNull<Header>);

// SAFETY: the claimed prefix is immutable, tail writes go to regions claimed
// exclusively through `used`, and the refcount is atomic.
unsafe impl Send for SharedBuf {}
unsafe impl Sync for SharedBuf {}

impl SharedBuf {
    fn layout(cap: usize) -> Layout {
        Layout::from_size_align(mem::size_of::<Header>() + cap, mem::align_of::<Header>())
            .expect("string buffer layout")
    }

    /// New buffer holding `parts` back to back, with room for `cap` bytes.
    fn with_parts(cap: usize, parts: &[&[u8]]) -> Self {
        let used: usize = parts.iter().map(|p| p.len()).sum();
        debug_assert!(used <= cap);
        // SAFETY: the layout has non-zero size (the header).
        let raw = unsafe { alloc::alloc(Self::layout(cap)) }.cast::<Header>();
        let Some(hdr) = NonNull::new(raw) else {
            alloc::handle_alloc_error(Self::layout(cap));
        };
        unsafe {
            hdr.as_ptr().write(Header {
                refs: AtomicUsize::new(1),
                used: AtomicUsize::new(used),
                cap,
            });
        }
        let buf = Self(hdr);
        let mut at = buf.bytes_ptr();
        for p in parts {
            // SAFETY: `[0, used)` fits in `cap` and nobody else sees the buffer yet.
            unsafe {
                ptr::copy_nonoverlapping(p.as_ptr(), at, p.len());
                at = at.add(p.len());
            }
        }
        buf
    }

    fn header(&self) -> &Header {
        // SAFETY: the header lives as long as any `SharedBuf` handle.
        unsafe { self.0.as_ref() }
    }

    fn bytes_ptr(&self) -> *mut u8 {
        // SAFETY: the byte area starts right after the header.
        unsafe { self.0.as_ptr().cast::<u8>().add(mem::size_of::<Header>()) }
    }

    /// Claim `[at, at + n)` if `at` is exactly where the claimed bytes end
    /// and the buffer has room, then copy `bytes` there.
    fn try_append_at(&self, at: usize, bytes: &[u8]) -> bool {
        let hdr = self.header();
        let end = at + bytes.len();
        if end > hdr.cap {
            return false;
        }
        if hdr
            .used
            .compare_exchange(at, end, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        // SAFETY: the CAS made `[at, end)` exclusive to this caller, and
        // `bytes` cannot alias it (it was claimed, or outside, before).
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), self.bytes_ptr().add(at), bytes.len()) };
        true
    }
}

impl Clone for SharedBuf {
    fn clone(&self) -> Self {
        self.header().refs.fetch_add(1, Ordering::Relaxed);
        Self(self.0)
    }
}

impl Drop for SharedBuf {
    fn drop(&mut self) {
        if self.header().refs.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        fence(Ordering::Acquire);
        let cap = self.header().cap;
        // SAFETY: last handle; allocated with this layout in `with_parts`.
        unsafe { alloc::dealloc(self.0.as_ptr().cast(), Self::layout(cap)) };
    }
}

/// UTF-8 content of an `ObjString`.
pub enum StrData {
    /// Exact-size string (literals, `format`, decoded bytes).
    Owned(String),
    /// Prefix of a growable buffer (concat results). `accounted` is the part
    /// of the buffer this string is charged for in heap accounting.
    Shared {
        buf: SharedBuf,
        len: usize,
        accounted: usize,
    },
}

impl StrData {
    #[inline]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Owned(s) => s,
            Self::Shared { buf, len, .. } => {
                // SAFETY: `[0, len)` was written before this string existed,
                // is never written again, and was valid UTF-8 (built from
                // `&str` parts).
                unsafe { str::from_utf8_unchecked(slice::from_raw_parts(buf.bytes_ptr(), *len)) }
            }
        }
    }

    /// Bytes charged to this string by heap accounting.
    #[inline]
    pub fn accounted_bytes(&self) -> usize {
        match self {
            Self::Owned(s) => s.len(),
            Self::Shared { accounted, .. } => *accounted,
        }
    }

    /// `self + tail`, appending in place when `self` ends at its buffer's
    /// tail, otherwise copying both into a new buffer with spare capacity.
    pub fn concat(&self, tail: &str) -> Self {
        let head_len = self.as_str().len();
        if let Self::Shared { buf, len, .. } = self
            && buf.try_append_at(*len, tail.as_bytes())
        {
            return Self::Shared {
                buf: buf.clone(),
                len: head_len + tail.len(),
                accounted: tail.len(),
            };
        }
        let total = head_len + tail.len();
        let cap = total.saturating_mul(2).max(MIN_SHARED_CAP);
        let buf = SharedBuf::with_parts(cap, &[self.as_str().as_bytes(), tail.as_bytes()]);
        Self::Shared {
            buf,
            len: total,
            accounted: cap,
        }
    }
}

impl Deref for StrData {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl From<String> for StrData {
    fn from(s: String) -> Self {
        Self::Owned(s)
    }
}

impl PartialEq for StrData {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<&str> for StrData {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<str> for StrData {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl fmt::Display for StrData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for StrData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(s: &str) -> StrData {
        StrData::Owned(String::new()).concat(s)
    }

    #[test]
    fn concat_appends_in_place_at_the_tail() {
        let a = shared("ab");
        let b = a.concat("cd");
        let (StrData::Shared { buf: ba, .. }, StrData::Shared { buf: bb, .. }) = (&a, &b) else {
            panic!("expected shared");
        };
        assert_eq!(ba.0, bb.0, "second concat reuses the buffer");
        assert_eq!(a.as_str(), "ab");
        assert_eq!(b.as_str(), "abcd");
    }

    #[test]
    fn concat_off_the_tail_copies() {
        let a = shared("ab");
        let b = a.concat("cd");
        // `a` no longer ends at the tail: appending to it must not clobber `b`.
        let c = a.concat("XY");
        assert_eq!(b.as_str(), "abcd");
        assert_eq!(c.as_str(), "abXY");
        let (StrData::Shared { buf: ba, .. }, StrData::Shared { buf: bc, .. }) = (&a, &c) else {
            panic!("expected shared");
        };
        assert_ne!(ba.0, bc.0);
    }

    #[test]
    fn concat_grows_past_capacity() {
        let mut s = shared("");
        let mut expect = String::new();
        for i in 0..2000 {
            let part = format!("{i},");
            s = s.concat(&part);
            expect.push_str(&part);
        }
        assert_eq!(s.as_str(), expect);
    }

    #[test]
    fn self_concat_reads_before_it_writes() {
        let a = shared("xyz");
        let b = a.concat(a.as_str());
        assert_eq!(b.as_str(), "xyzxyz");
    }

    #[test]
    fn concurrent_appends_to_one_tail_stay_disjoint() {
        let base = shared("base:");
        let results: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|t| {
                    let base = &base;
                    scope.spawn(move || base.concat(&format!("{t}")).as_str().to_owned())
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (t, r) in results.iter().enumerate() {
            assert_eq!(r, &format!("base:{t}"));
        }
        assert_eq!(base.as_str(), "base:");
    }
}
