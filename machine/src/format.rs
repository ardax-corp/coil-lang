//! `FORMAT` specifier strings, rendered for the interpreter and native code alike.

use std::fmt::Write as _;

use common::{Value, ValueTag};

use crate::memory::{Heap, Object};
use crate::{GcData, ObjString};

/// The text of a `FORMAT` specifier string (after any leading `%s` the
/// caller appends to): `next` yields the arguments in order. Shared by the
/// runtime's `FORMAT` and native code's.
#[cfg(test)]
pub(crate) fn render_format(format_rest: &str, next: impl FnMut() -> Value) -> String {
    let mut message = String::new();
    render_format_into(&mut message, format_rest, next);
    message
}

/// [`render_format`] appended to `message`, which a caller may reuse
/// across calls: literal runs go in as slices and `%i` / `%u` as digits
/// written in place, so a short message needs no allocation of its own.
pub(crate) fn render_format_into(message: &mut String, format_rest: &str, mut next: impl FnMut() -> Value) {
    let mut rest = format_rest;
    // A byte scan: `%` is ASCII, and a format string is short, where
    // `str::find`'s searcher costs more than the loop.
    while let Some(at) = rest.bytes().position(|b| b == b'%') {
        message.push_str(&rest[..at]);
        let spec = rest.as_bytes().get(at + 1).copied();
        rest = &rest[at + 1..];
        match spec {
            Some(b'%') => message.push('%'),
            Some(b'i') => push_int(message, next().as_int()),
            Some(b'f') => {
                let _ = write!(message, "{:.?}", next().as_float());
            }
            Some(b'b') => {
                let _ = write!(message, "{:0b}", next().bits());
            }
            Some(b's') => {
                let string_val = (unsafe { &*next().as_ptr::<GcData<ObjString>>() }).as_ref().data.as_str();
                message.push_str(string_val);
            }
            Some(b'x') => {
                let _ = write!(message, "{:0x}", next().bits());
            }
            Some(b'z') => message.push_str(if next().bits() > 0 { "true" } else { "false" }),
            Some(b'u') => push_uint(message, next().bits(), false),
            Some(b'p') => {
                let _ = write!(message, "{:08x}", next().as_ptr::<bool>().addr());
            }
            _ => {
                message.push('%');
                continue;
            }
        }
        rest = &rest[1..];
    }
    message.push_str(rest);
}

/// `v` in decimal, as `i64::to_string` writes it.
fn push_int(out: &mut String, v: i64) {
    push_uint(out, v.unsigned_abs(), v < 0);
}

/// `v` in decimal after a `-` when `negative`.
fn push_uint(out: &mut String, mut v: u64, negative: bool) {
    let mut digits = [0u8; 21];
    let mut at = digits.len();
    loop {
        at -= 1;
        digits[at] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    if negative {
        at -= 1;
        digits[at] = b'-';
    }
    // SAFETY: ASCII digits and `-`.
    out.push_str(unsafe { std::str::from_utf8_unchecked(&digits[at..]) });
}

/// A value as `STRINGIFY` shows it, when its static type is not known: a
/// string's text, else the word as an `int`.
pub(crate) fn stringify_value(heap: &Heap, v: Value) -> String {
    if v.is_zero() {
        // `Value::default()` / unit / false-ish null pointer.
        return "0".into();
    }
    match heap.find_object_by_addr(v.bits()) {
        Some(Object::String(gc)) => gc.as_ref().data.to_string(),
        Some(_) | None => v.as_int().to_string(),
    }
}

/// The string object at `addr`'s text (empty when it is not one).
pub(crate) fn string_at(heap: &Heap, addr: u64) -> String {
    match heap.str_at(addr) {
        Some(gc) => gc.as_ref().data.to_string(),
        None => String::new(),
    }
}

/// A `Result::Err`'s payload as text (`None` for an `Ok` or a non-result).
pub(crate) fn result_err_text(heap: &Heap, value: Value) -> Option<String> {
    match heap.find_object_by_addr(value.bits()) {
        Some(Object::Enum(gc)) if gc.as_ref().tag == 1 => {
            let payload = gc.as_ref().payload.first().copied()?;
            Some(stringify_value(heap, payload))
        }
        _ => None,
    }
}

/// Whether `value` is a `Result::Ok`.
pub(crate) fn result_is_ok(heap: &Heap, value: Value) -> bool {
    matches!(heap.find_object_by_addr(value.bits()), Some(Object::Enum(gc)) if gc.as_ref().tag == 0)
}

/// `STRINGIFY` of an immediate whose static type the compiler named.
pub(crate) fn stringify_immediate(tag: ValueTag, v: Value) -> String {
    match tag {
        ValueTag::Float => format!("{:?}", v.as_float()),
        ValueTag::Bool => if v.as_int() != 0 { "true" } else { "false" }.into(),
        ValueTag::Unit => "()".into(),
        _ => v.as_int().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(fmt: &str, args: &[Value]) -> String {
        let mut it = args.iter().copied();
        render_format(fmt, || it.next().unwrap_or_default())
    }

    #[test]
    fn ints_and_literals_render_as_to_string() {
        for v in [0i64, 7, -7, 42, 4999, i64::MIN, i64::MAX, -1000000] {
            assert_eq!(render("w%i", &[Value::from(v as u64)]), format!("w{v}"));
        }
        assert_eq!(render("%i-%i!", &[Value::from(3u64), Value::from((-4i64) as u64)]), "3--4!");
        assert_eq!(render("%u", &[Value::from(u64::MAX)]), u64::MAX.to_string());
    }

    #[test]
    fn percent_runs_and_unknown_specifiers() {
        assert_eq!(render("100%%", &[]), "100%");
        assert_eq!(render("a%qb", &[]), "a%qb");
        assert_eq!(render("end%", &[]), "end%");
        assert_eq!(render("é%iü", &[Value::from(5u64)]), "é5ü");
        assert_eq!(render("%z %z", &[Value::from(1u64), Value::from(0u64)]), "true false");
        assert_eq!(render("%x", &[Value::from(255u64)]), "ff");
    }
}
