//! Byte-offset `string` natives (`byte_at`, `slice_bytes`, `find_from`,
//! `rfind`, `match_at`) and the whole-string literal searches (`find_all`,
//! `split`, `replace_all`).
//!
//! They read the string in place. `text::*` helpers used to go through
//! `to_bytes`, which copies every byte into an 8-byte `Value` on each call.
//! Offsets are byte offsets, as in `bytes::*`; `s.len()` is the byte length.

use common::Value;

use crate::io::{IoErrorTag, as_result_value, peel_one_boxed};
use crate::memory::{Heap, ObjArray, Object};

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
    memchr::memmem::find(&hay[start..], needle).map_or(-1, |i| (start + i) as i64)
}

fn rfind(hay: &[u8], needle: &[u8]) -> i64 {
    if needle.is_empty() {
        return hay.len() as i64;
    }
    memchr::memmem::rfind(hay, needle).map_or(-1, |i| i as i64)
}

fn match_at(hay: &[u8], needle: &[u8], at: i64) -> bool {
    usize::try_from(at)
        .ok()
        .and_then(|at| hay.get(at..at.checked_add(needle.len())?))
        .is_some_and(|w| w == needle)
}

/// Start offsets of the non-overlapping matches of `needle`, left to right.
/// An empty needle matches at every character boundary, `0` through
/// `hay.len()`, as `text::Searchable` steps over one character after an
/// empty match.
fn match_starts(hay: &str, needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() {
        return hay
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(hay.len()))
            .collect();
    }
    memchr::memmem::find_iter(hay.as_bytes(), needle).fold(Vec::new(), |mut out, at| {
        // `find_iter` reports overlapping matches; keep the leftmost run.
        if out.last().is_none_or(|&last| at >= last + needle.len()) {
            out.push(at);
        }
        out
    })
}

fn replace_all(hay: &str, needle: &str, with: &str) -> String {
    let starts = match_starts(hay, needle.as_bytes());
    let mut out = String::with_capacity(hay.len() + starts.len() * with.len());
    let mut piece = 0;
    for at in starts {
        out.push_str(&hay[piece..at]);
        out.push_str(with);
        piece = at + needle.len();
    }
    out.push_str(&hay[piece..]);
    out
}

fn str_value(heap: &mut Heap, s: String) -> Value {
    let gc = heap.alloc_string(s);
    Value::from(gc.as_ptr() as *mut u8 as u64)
}

fn array_value(heap: &mut Heap, elements: Vec<Value>) -> Value {
    let (obj, _) = heap.alloc(ObjArray::new(elements), Object::Array);
    Value::from(obj.addr())
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
/// `[0, s.len()]` (and `end` to at least `start`); `Err(InvalidInput)` when an
/// offset falls inside a UTF-8 sequence.
pub fn string_slice_bytes(heap: &mut Heap, args: &[Value]) -> Value {
    let (start, end) = (args[1].as_int(), args[2].as_int());
    let part = match heap.find_object_by_addr(peel_one_boxed(heap, args[0]).raw() as u64) {
        Some(Object::String(gc)) => {
            let n = gc.as_ref().data.len() as i64;
            let start = start.clamp(0, n) as usize;
            let end = end.clamp(start as i64, n) as usize;
            heap.alloc_slice(gc, start, end)
        }
        _ => Some(heap.alloc_string(String::new())),
    };
    let r = match part {
        Some(gc) => Ok(Value::from(gc.as_ptr() as *mut u8 as u64)),
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
/// needle matches at `hay.len()`.
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

/// `find_all(hay, needle) -> Vec<int>`: start offsets of the
/// non-overlapping matches of `needle`, left to right.
pub fn string_find_all(heap: &mut Heap, args: &[Value]) -> Value {
    let starts = with_str(heap, args[0], |hay| {
        with_str(heap, args[1], |needle| match_starts(hay, needle.as_bytes()))
    });
    let elements = starts.into_iter().map(|at| Value::from(at as i64)).collect();
    array_value(heap, elements)
}

/// `split(hay, sep) -> Vec<string>`: the pieces between the matches of
/// `sep`; a match at either end gives an empty first or last piece.
pub fn string_split(heap: &mut Heap, args: &[Value]) -> Value {
    let Some(Object::String(gc)) = heap.find_object_by_addr(peel_one_boxed(heap, args[0]).raw() as u64)
    else {
        let empty = str_value(heap, String::new());
        return array_value(heap, vec![empty]);
    };
    let (starts, sep_len) = with_str(heap, args[1], |sep| {
        (match_starts(gc.as_ref().data.as_str(), sep.as_bytes()), sep.len())
    });
    let mut elements = Vec::with_capacity(starts.len() + 1);
    let mut piece = 0;
    let ends = starts.iter().copied().chain(std::iter::once(gc.as_ref().data.len()));
    for at in ends {
        // Match offsets sit on character boundaries, so the slice exists.
        let part = heap
            .alloc_slice(gc, piece, at)
            .expect("split pieces end on character boundaries");
        elements.push(Value::from(part.as_ptr() as *mut u8 as u64));
        piece = at + sep_len;
    }
    array_value(heap, elements)
}

/// `replace_all(hay, needle, with) -> string`: `hay` with every
/// non-overlapping match of `needle` replaced by `with`.
pub fn string_replace_all(heap: &mut Heap, args: &[Value]) -> Value {
    let out = with_str(heap, args[0], |hay| {
        with_str(heap, args[1], |needle| {
            with_str(heap, args[2], |with| replace_all(hay, needle, with))
        })
    });
    str_value(heap, out)
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

    #[test]
    fn match_starts_skip_overlaps_and_step_empty_needles() {
        assert_eq!(match_starts("a,b,,c", b","), vec![1, 3, 4]);
        assert_eq!(match_starts("aaaa", b"aa"), vec![0, 2]);
        assert_eq!(match_starts("aaa", b"aa"), vec![0]);
        assert_eq!(match_starts("abc", b"x"), Vec::<usize>::new());
        assert_eq!(match_starts("aé", b""), vec![0, 1, 3]);
        assert_eq!(match_starts("", b""), vec![0]);
    }

    #[test]
    fn replace_all_matches_searchable() {
        assert_eq!(replace_all("a-b-c", "-", "+"), "a+b+c");
        assert_eq!(replace_all("aaaa", "aa", "b"), "bb");
        assert_eq!(replace_all("ab", "", "-"), "-a-b-");
        assert_eq!(replace_all("abc", "x", "-"), "abc");
    }

    fn strings(heap: &Heap, v: Value) -> Vec<String> {
        let Some(Object::Array(arr)) = heap.find_object_by_addr(v.raw() as u64) else {
            panic!("expected an array");
        };
        let arr = arr.as_ref();
        (0..arr.len())
            .map(|i| match heap.find_object_by_addr(arr.get(i).unwrap().raw() as u64) {
                Some(Object::String(gc)) => gc.as_ref().data.as_str().to_string(),
                _ => panic!("expected a string"),
            })
            .collect()
    }

    #[test]
    fn split_matches_searchable() {
        let mut heap = Heap::default();
        let mut split = |hay: &str, sep: &str| {
            let (h, s) = (str_val(&mut heap, hay), str_val(&mut heap, sep));
            let v = string_split(&mut heap, &[h, s]);
            strings(&heap, v)
        };
        assert_eq!(split("a,b,,c", ","), ["a", "b", "", "c"]);
        assert_eq!(split(",a,", ","), ["", "a", ""]);
        assert_eq!(split("abc", "x"), ["abc"]);
        assert_eq!(split("ab", ""), ["", "a", "b", ""]);
        assert_eq!(split("", ","), [""]);
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
