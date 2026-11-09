//! `FORMAT` specifier strings, rendered for the interpreter and native code alike.

use std::fmt::Write as _;

use common::{Value, ValueTag};

use crate::memory::{Heap, Object};
use crate::{GcData, ObjString};

/// The text of a `FORMAT` specifier string (after any leading `%s` the
/// caller appends to): `next` yields the arguments in order. Shared by the
/// runtime's `FORMAT` and native code's.
pub(crate) fn render_format(format_rest: &str, mut next: impl FnMut() -> Value) -> String {
    let mut message = String::default();
    let mut chars = format_rest.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            match chars.peek() {
                Some('%') => {
                    chars.next();
                    message.push('%');
                }
                Some('i') => {
                    chars.next();
                    message.push_str(&next().as_int().to_string());
                }
                Some('f') => {
                    chars.next();
                    // message
                    //     .push_str(&format!("{:.?}", next().as_float()));
                    let _ =
                        write!(&mut message, "{:.?}", next().as_float());
                }
                Some('b') => {
                    chars.next();
                    let _ = write!(
                        &mut message,
                        "{:0b}",
                        next().bits()
                    );
                }
                Some('s') => {
                    chars.next();
                    let string_val = (unsafe {
                        &*next().as_ptr::<GcData<ObjString>>()
                    })
                    .as_ref()
                    .data
                    .as_str();
                    // Allocated::<crate::String>::new(next().as_ptr());
                    message.push_str(string_val);
                }
                Some('x') => {
                    chars.next();
                    let _ = write!(
                        &mut message,
                        "{:0x}",
                        next().bits()
                    );
                }
                Some('z') => {
                    chars.next();
                    message.push_str(if next().bits() > 0 {
                        "true"
                    } else {
                        "false"
                    });
                }
                Some('u') => {
                    chars.next();
                    message.push_str(&next().bits().to_string());
                }
                Some('p') => {
                    chars.next();
                    let _ = write!(
                        &mut message,
                        "{:08x}",
                        next().as_ptr::<bool>().addr()
                    );
                }
                _ => {
                    message.push('%');
                }
            }
        } else {
            message.push(ch);
        }
    }
    message
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
    match heap.find_object_by_addr(addr) {
        Some(Object::String(gc)) => gc.as_ref().data.to_string(),
        _ => String::new(),
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
