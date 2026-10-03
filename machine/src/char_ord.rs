//! Prelude `ord` / `char` host helpers.

use common::Value;

use crate::host_enum::pack_result_or_panic;
use crate::io::{alloc_result_ok, value_as_string};
use crate::memory::Heap;

fn string_val(heap: &mut Heap, text: &str) -> Value {
    let gc = heap.alloc_string(text.to_string());
    Value::from(gc.as_ptr() as *mut u8 as u64)
}

fn err_msg(heap: &mut Heap, text: &str) -> Value {
    let msg = string_val(heap, text);
    pack_result_or_panic(heap, Err(msg))
}

/// `ord(string) -> Result<byte, string>` — UTF-8 code unit must fit in `byte`.
pub fn prelude_ord(heap: &mut Heap, args: &[Value]) -> Value {
    let s = match value_as_string(heap, args[0]) {
        Ok(s) => s,
        Err(_) => return err_msg(heap, "expected string"),
    };
    let mut chars = s.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return err_msg(heap, "empty string"),
    };
    if chars.next().is_some() {
        return err_msg(heap, "expected single character");
    }
    let code = first as u32;
    if code > 255 {
        return err_msg(heap, "character code out of byte range");
    }
    alloc_result_ok(heap, Value::from(code as i64))
}

/// `char(byte) -> Result<string, string>` — one code unit in 0..=255.
pub fn prelude_char(heap: &mut Heap, args: &[Value]) -> Value {
    let b = args[0].as_int();
    if !(0..=255).contains(&b) {
        return err_msg(heap, "byte out of range");
    }
    let ch = char::from_u32(b as u32).unwrap_or('\0');
    let mut buf = [0u8; 4];
    let encoded = ch.encode_utf8(&mut buf);
    let s = string_val(heap, encoded);
    pack_result_or_panic(heap, Ok(s))
}

/// Content hash for `Hash` on `string` — the `ObjString` FNV hash as `int`,
/// cached on the string object after the first call.
pub fn prelude_hash_string(heap: &mut Heap, args: &[Value]) -> Value {
    match heap.find_object_by_addr(args[0].raw() as u64) {
        Some(crate::memory::Object::String(gc)) => Value::from(gc.as_ref().hash_code() as i64),
        _ => match value_as_string(heap, args[0]) {
            Ok(s) => Value::from(crate::memory::ObjString::hash(&s) as i64),
            Err(_) => Value::from(0_i64),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Object;

    fn str_arg(heap: &mut Heap, s: &str) -> Value {
        string_val(heap, s)
    }

    fn result_ok_int(heap: &Heap, v: Value) -> i64 {
        let Some(Object::Enum(gc)) = heap.find_object_by_addr(v.raw() as u64) else {
            panic!("expected Result");
        };
        assert_eq!(gc.as_ref().tag, 0, "expected Ok");
        gc.as_ref().payload[0].as_int()
    }

    fn result_err_string(heap: &Heap, v: Value) -> String {
        let Some(Object::Enum(gc)) = heap.find_object_by_addr(v.raw() as u64) else {
            panic!("expected Result");
        };
        assert_eq!(gc.as_ref().tag, 1, "expected Err");
        value_as_string(heap, gc.as_ref().payload[0]).expect("expected string error")
    }

    fn result_ok_string(heap: &Heap, v: Value) -> String {
        let Some(Object::Enum(gc)) = heap.find_object_by_addr(v.raw() as u64) else {
            panic!("expected Result");
        };
        assert_eq!(gc.as_ref().tag, 0, "expected Ok");
        value_as_string(heap, gc.as_ref().payload[0]).expect("expected string payload")
    }

    #[test]
    fn ord_rejects_empty_and_multichar_char_roundtrips() {
        let mut heap = Heap::default();

        let empty_s = str_arg(&mut heap, "");
        let empty = prelude_ord(&mut heap, &[empty_s]);
        assert_eq!(result_err_string(&heap, empty), "empty string");

        let multi_s = str_arg(&mut heap, "ab");
        let multi = prelude_ord(&mut heap, &[multi_s]);
        assert_eq!(result_err_string(&heap, multi), "expected single character");

        let a_s = str_arg(&mut heap, "A");
        let ok = prelude_ord(&mut heap, &[a_s]);
        assert_eq!(result_ok_int(&heap, ok), 65);

        let ch = prelude_char(&mut heap, &[Value::from(65_i64)]);
        assert_eq!(result_ok_string(&heap, ch), "A");

        let euro_s = str_arg(&mut heap, "€");
        let euro = prelude_ord(&mut heap, &[euro_s]);
        assert_eq!(
            result_err_string(&heap, euro),
            "character code out of byte range"
        );

        let oob = prelude_char(&mut heap, &[Value::from(300_i64)]);
        assert_eq!(result_err_string(&heap, oob), "byte out of range");
    }

    #[test]
    fn hash_string_is_stable_and_content_sensitive() {
        let mut heap = Heap::default();
        let a = str_arg(&mut heap, "hi");
        let b = str_arg(&mut heap, "hi");
        let c = str_arg(&mut heap, "bye");
        let ha = prelude_hash_string(&mut heap, &[a]).as_int();
        let hb = prelude_hash_string(&mut heap, &[b]).as_int();
        let hc = prelude_hash_string(&mut heap, &[c]).as_int();
        assert_eq!(ha, hb);
        assert_ne!(ha, hc);
    }
}
