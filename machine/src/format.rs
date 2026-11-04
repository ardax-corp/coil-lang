//! `FORMAT` specifier strings, rendered for the VM and native code alike.

use std::fmt::Write as _;

use common::Value;

use crate::{GcData, ObjString};

/// The text of a `FORMAT` specifier string (after any leading `%s` the
/// caller appends to): `next` yields the arguments in order. Shared by the
/// VM's `FORMAT` and native code's.
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
