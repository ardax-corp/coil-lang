//! Byte-offset `string` natives (`byte_at`, `slice_bytes`, `find_from`,
//! `rfind`, `match_at`).
//!
//! They read the string in place. `text::*` helpers used to go through
//! `to_bytes`, which copies every byte into an 8-byte `Value` on each call.
//! Offsets are byte offsets, as in `bytes::*`; `len(s)` is the byte length.

use common::Value;

use crate::io::{IoErrorTag, as_result_value, peel_one_boxed};
use crate::memory::{Heap, Object};

/// Run `f` on the UTF-8 content of string `v`; non-strings read as `""`
/// (the typechecker rejects them).
fn with_str<R>(heap: &Heap, v: Value, f: impl FnOnce(&str) -> R) -> R {
    match heap.find_object_by_addr(peel_one_boxed(heap, v).raw() as u64) {
        Some(Object::String(gc)) => f(gc.as_ref().data.as_str()),
        _ => f(""),
    }
}

fn find_from(hay: &[u8], needle: &[u8], start: i64) -> i64 {
    let start = start.clamp(0, hay.len() as i64) as usize;
    if needle.is_empty() {
        return start as i64;
    }
    hay[start..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map_or(-1, |i| (start + i) as i64)
}

fn rfind(hay: &[u8], needle: &[u8]) -> i64 {
    if needle.is_empty() {
        return hay.len() as i64;
    }
    hay.windows(needle.len())
        .rposition(|w| w == needle)
        .map_or(-1, |i| i as i64)
}

fn match_at(hay: &[u8], needle: &[u8], at: i64) -> bool {
    usize::try_from(at)
        .ok()
        .and_then(|at| hay.get(at..at.checked_add(needle.len())?))
        .is_some_and(|w| w == needle)
}

/// `byte_at(s, i) -> int`: the byte at offset `i`, or `-1` out of range.
pub fn string_byte_at(heap: &mut Heap, args: &[Value]) -> Value {
    let i = args[1].as_int();
    let b = with_str(heap, args[0], |s| {
        usize::try_from(i)
            .ok()
            .and_then(|i| s.as_bytes().get(i))
            .map_or(-1, |&b| b as i64)
    });
    Value::from(b)
}

/// `slice_bytes(s, start, end) -> Result<string, IoError>`. Offsets clamp to
/// `[0, len(s)]` (and `end` to at least `start`); `Err(InvalidInput)` when an
/// offset falls inside a UTF-8 sequence.
pub fn string_slice_bytes(heap: &mut Heap, args: &[Value]) -> Value {
    let (start, end) = (args[1].as_int(), args[2].as_int());
    let part = with_str(heap, args[0], |s| {
        let start = start.clamp(0, s.len() as i64) as usize;
        let end = end.clamp(start as i64, s.len() as i64) as usize;
        s.get(start..end).map(str::to_owned)
    });
    let r = match part {
        Some(p) => {
            let gc = heap.alloc_string(p);
            Ok(Value::from(gc.as_ptr() as *mut u8 as u64))
        }
        None => Err(IoErrorTag::InvalidInput),
    };
    as_result_value(heap, r)
}

/// `find_from(hay, needle, start) -> int`: first offset `>= start` of
/// `needle`, or `-1`. An empty needle matches at the clamped `start`.
pub fn string_find_from(heap: &mut Heap, args: &[Value]) -> Value {
    let start = args[2].as_int();
    let at = with_str(heap, args[0], |hay| {
        with_str(heap, args[1], |needle| {
            find_from(hay.as_bytes(), needle.as_bytes(), start)
        })
    });
    Value::from(at)
}

/// `rfind(hay, needle) -> int`: last offset of `needle`, or `-1`. An empty
/// needle matches at `len(hay)`.
pub fn string_rfind(heap: &mut Heap, args: &[Value]) -> Value {
    let at = with_str(heap, args[0], |hay| {
        with_str(heap, args[1], |needle| {
            rfind(hay.as_bytes(), needle.as_bytes())
        })
    });
    Value::from(at)
}

/// `match_at(s, needle, at) -> bool`: `needle` occurs at byte offset `at`.
pub fn string_match_at(heap: &mut Heap, args: &[Value]) -> Value {
    let at = args[2].as_int();
    let hit = with_str(heap, args[0], |s| {
        with_str(heap, args[1], |needle| {
            match_at(s.as_bytes(), needle.as_bytes(), at)
        })
    });
    Value::from(hit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_from_matches_bytes_find_from() {
        assert_eq!(find_from(b"a,b,c", b",", 0), 1);
        assert_eq!(find_from(b"a,b,c", b",", 2), 3);
        assert_eq!(find_from(b"a,b,c", b",", 4), -1);
        assert_eq!(find_from(b"abc", b"", 9), 3);
        assert_eq!(find_from(b"abc", b"", -4), 0);
        assert_eq!(find_from(b"ab", b"abc", 0), -1);
    }

    #[test]
    fn rfind_and_match_at() {
        assert_eq!(rfind(b"a,b,c", b","), 3);
        assert_eq!(rfind(b"abc", b""), 3);
        assert_eq!(rfind(b"abc", b"x"), -1);
        assert!(match_at(b"hello", b"he", 0));
        assert!(match_at(b"hello", b"lo", 3));
        assert!(!match_at(b"hello", b"lo", 4));
        assert!(!match_at(b"hello", b"he", -1));
        assert!(match_at(b"hello", b"", 5));
        assert!(!match_at(b"hello", b"", 6));
    }

    fn str_val(heap: &mut Heap, s: &str) -> Value {
        let gc = heap.alloc_string(s.to_string());
        Value::from(gc.as_ptr() as *mut u8 as u64)
    }

    #[test]
    fn byte_at_and_slice_bytes() {
        let mut heap = Heap::default();
        let s = str_val(&mut heap, "hé!");
        let at = |heap: &mut Heap, i: i64| string_byte_at(heap, &[s, Value::from(i)]).as_int();
        assert_eq!(at(&mut heap, 0), b'h' as i64);
        assert_eq!(at(&mut heap, 3), b'!' as i64);
        assert_eq!(at(&mut heap, 4), -1);
        assert_eq!(at(&mut heap, -1), -1);

        let ok = string_slice_bytes(&mut heap, &[s, Value::from(1_i64), Value::from(3_i64)]);
        let mid = string_slice_bytes(&mut heap, &[s, Value::from(2_i64), Value::from(4_i64)]);
        let clamp = string_slice_bytes(&mut heap, &[s, Value::from(-5_i64), Value::from(99_i64)]);
        let tag = |heap: &Heap, v: Value| match heap.find_object_by_addr(v.raw() as u64) {
            Some(Object::Enum(gc)) => gc.as_ref().tag,
            _ => panic!("expected Result"),
        };
        assert_eq!(tag(&heap, ok), 0);
        assert_eq!(tag(&heap, mid), 1, "offset 2 is inside `é`");
        assert_eq!(tag(&heap, clamp), 0);
    }
}
