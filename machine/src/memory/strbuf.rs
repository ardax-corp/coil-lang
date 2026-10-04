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
//! A slice (`slice_bytes`) of a `Shared` string is a view `[start, start +
//! len)` into the same buffer instead of a copy, so `rest = slice(rest, i,
//! len(rest))` loops are linear. A view only pins a buffer at most
//! [`VIEW_RETAIN_MAX`] times its own length; smaller slices are copied.
//!
//! Soundness: bytes below `used` are never written again, and every string
//! that shares a buffer has `start + len <= used`, so a reader's `&str` never
//! overlaps a region another thread may be writing. The CAS makes a claimed region
//! exclusive to one appender, which matters for steal-epoch workers that
//! share the heap.

use std::alloc::{self, Layout};
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering, fence};
use std::{fmt, mem, ptr, slice, str};

/// Smallest capacity of a buffer created by concat.
const MIN_SHARED_CAP: usize = 32;

/// Slices shorter than this are copied: the copy is as cheap as a view.
const VIEW_MIN_LEN: usize = 64;

/// A view may pin a buffer at most this many times its own length; a smaller
/// slice is copied so it does not keep a large buffer alive.
const VIEW_RETAIN_MAX: usize = 8;

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
    /// `[start, start + len)` of a growable buffer (concat results and
    /// slices of them). `accounted` is the part of the buffer this string is
    /// charged for in heap accounting.
    Shared {
        buf: SharedBuf,
        start: usize,
        len: usize,
        accounted: usize,
    },
}

impl StrData {
    #[inline]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Owned(s) => s,
            Self::Shared { buf, start, len, .. } => {
                // SAFETY: `[start, start + len)` was written before this
                // string existed, is never written again, and is valid UTF-8
                // (built from `&str` parts, cut on char boundaries).
                unsafe {
                    str::from_utf8_unchecked(slice::from_raw_parts(
                        buf.bytes_ptr().add(*start),
                        *len,
                    ))
                }
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
        if let Self::Shared { buf, start, len, .. } = self
            && buf.try_append_at(start + len, tail.as_bytes())
        {
            return Self::Shared {
                buf: buf.clone(),
                start: *start,
                len: head_len + tail.len(),
                accounted: tail.len(),
            };
        }
        let total = head_len + tail.len();
        let cap = total.saturating_mul(2).max(MIN_SHARED_CAP);
        let buf = SharedBuf::with_parts(cap, &[self.as_str().as_bytes(), tail.as_bytes()]);
        Self::Shared {
            buf,
            start: 0,
            len: total,
            accounted: cap,
        }
    }

    /// Bytes `[from, to)`, or `None` when an offset is out of range or not on
    /// a char boundary. A long enough slice of a `Shared` string is a view
    /// into the same buffer; anything else is copied. A copy of
    /// [`VIEW_MIN_LEN`] bytes or more goes into an exact-size shared buffer,
    /// so slices of the slice are views.
    pub fn slice(&self, from: usize, to: usize) -> Option<Self> {
        let part = self.as_str().get(from..to)?;
        if part.len() < VIEW_MIN_LEN {
            return Some(Self::Owned(part.to_owned()));
        }
        if let Self::Shared { buf, start, .. } = self
            && part.len().saturating_mul(VIEW_RETAIN_MAX) >= buf.header().cap
        {
            return Some(Self::Shared {
                buf: buf.clone(),
                start: start + from,
                len: part.len(),
                // The buffer is charged to the strings that grew it.
                accounted: 0,
            });
        }
        let buf = SharedBuf::with_parts(part.len(), &[part.as_bytes()]);
        Some(Self::Shared {
            buf,
            start: 0,
            len: part.len(),
            accounted: part.len(),
        })
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

    fn buf_of(s: &StrData) -> *mut Header {
        match s {
            StrData::Shared { buf, .. } => buf.0.as_ptr(),
            StrData::Owned(_) => panic!("expected shared"),
        }
    }

    #[test]
    fn long_slice_of_shared_is_a_view() {
        let text = "x".repeat(100) + &"y".repeat(100);
        let a = shared(&text);
        let b = a.slice(50, 200).unwrap();
        assert_eq!(buf_of(&a), buf_of(&b), "slice shares the buffer");
        assert_eq!(b.as_str(), &text[50..200]);
        assert_eq!(b.accounted_bytes(), 0);
        // A view of a view stays in the same buffer at the right offset.
        let c = b.slice(40, 130).unwrap();
        assert_eq!(buf_of(&a), buf_of(&c));
        assert_eq!(c.as_str(), &text[90..180]);
    }

    #[test]
    fn short_or_small_share_slices_copy() {
        let a = shared(&"z".repeat(1000));
        assert!(matches!(a.slice(0, 10).unwrap(), StrData::Owned(_)));
        // 100 bytes of a 2000-byte buffer would pin 20x its size: copy it.
        let b = a.slice(0, 100).unwrap();
        assert_ne!(buf_of(&a), buf_of(&b));
        assert_eq!(b.as_str(), &"z".repeat(100));
        // A long slice of an owned string copies into a shared buffer, so
        // slicing it again is a view.
        let o = StrData::Owned("w".repeat(300));
        let p = o.slice(10, 300).unwrap();
        let q = p.slice(10, 200).unwrap();
        assert_eq!(buf_of(&p), buf_of(&q));
        assert_eq!(q.as_str(), &"w".repeat(190));
    }

    #[test]
    fn slice_rejects_bad_offsets() {
        let a = shared(&"é".repeat(100));
        assert!(a.slice(1, 50).is_none(), "inside a UTF-8 sequence");
        assert!(a.slice(0, 201).is_none(), "past the end");
        assert_eq!(a.slice(2, 200).unwrap().as_str(), &"é".repeat(99));
    }

    #[test]
    fn concat_onto_a_view_keeps_other_strings_intact() {
        let a = shared(&"a".repeat(100));
        // Suffix view ends at the buffer's tail: append in place.
        let tail = a.slice(10, 100).unwrap();
        let grown = tail.concat("!");
        assert_eq!(buf_of(&a), buf_of(&grown));
        assert_eq!(grown.as_str(), "a".repeat(90) + "!");
        // Prefix view does not end at the tail: copy, leave `a` alone.
        let head = a.slice(0, 90).unwrap();
        let other = head.concat("?");
        assert_ne!(buf_of(&a), buf_of(&other));
        assert_eq!(other.as_str(), "a".repeat(90) + "?");
        assert_eq!(a.as_str(), "a".repeat(100));
        assert_eq!(head.as_str(), "a".repeat(90));
    }
}
