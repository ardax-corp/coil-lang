//! Shared FFI type tags and names (compiler + runtime must agree).

/// Tag integers embedded in `CONST` operands and `FFIType` enum variants.
pub mod tag {
    pub const INT: u32 = 0;
    pub const FLOAT: u32 = 1;
    pub const STRING: u32 = 2;
    pub const VOID: u32 = 3;
    pub const BOOL: u32 = 4;
    pub const INT8: u32 = 5;
    pub const INT16: u32 = 6;
    pub const INT32: u32 = 7;
    pub const UINT8: u32 = 8;
    pub const UINT16: u32 = 9;
    pub const UINT32: u32 = 10;
    pub const UINT64: u32 = 11;
    pub const PTR: u32 = 12;
    pub const CALLBACK: u32 = 13;
    /// Struct-by-value; operand carries struct layout id in upper bits at declare time.
    pub const STRUCT: u32 = 14;
    /// Byte buffer (`Vec<byte>`): passed as `uint8_t *`, copied back after the call.
    pub const BYTES: u32 = 15;
    /// Read-only byte view (`string` or `Vec<byte>`): passed as `const uint8_t *`,
    /// never copied back; a string is lent in place.
    pub const CONST_BYTES: u32 = 16;
    /// Highest tag; operands above it carry an aux id (see [`super::encode_tag_operand`]).
    pub const LAST: u32 = CONST_BYTES;
}

pub const BUILTIN_FFI_TYPE_ENUM: &str = "FFIType";

/// Built-in `FFIType` variant names in tag order (must match runtime decoder).
pub const BUILTIN_FFI_TYPE_VARIANTS: &[&str] = &[
    "Int", "Float", "String", "Void", "Bool", "Int8", "Int16", "Int32", "UInt8", "UInt16",
    "UInt32", "UInt64", "Ptr", "Callback", "Struct", "Bytes", "ConstBytes",
];

/// Map a bare type name (extern blocks, aliases) to a tag.
pub fn tag_from_type_name(name: &str) -> Option<u32> {
    match name.to_ascii_lowercase().as_str() {
        "int" | "int64" | "i64" => Some(tag::INT),
        "float" | "f64" | "double" => Some(tag::FLOAT),
        "string" | "str" | "cstring" => Some(tag::STRING),
        "void" => Some(tag::VOID),
        "bool" | "boolean" => Some(tag::BOOL),
        "int8" | "i8" => Some(tag::INT8),
        "int16" | "i16" => Some(tag::INT16),
        "int32" | "i32" => Some(tag::INT32),
        "uint8" | "u8" => Some(tag::UINT8),
        "uint16" | "u16" => Some(tag::UINT16),
        "uint32" | "u32" => Some(tag::UINT32),
        "uint64" | "u64" => Some(tag::UINT64),
        "ptr" | "pointer" => Some(tag::PTR),
        "callback" => Some(tag::CALLBACK),
        "bytes" => Some(tag::BYTES),
        "constbytes" | "const_bytes" => Some(tag::CONST_BYTES),
        _ => None,
    }
}

/// Surface module path for FFI type tags (`use ffi::types::*;`).
pub const FFI_TYPES_MODULE: &str = "ffi::types";

/// True when `name` is the reserved built-in FFI enum (legacy `FFIType`
/// or the namespaced `ffi::types` path).
pub fn is_builtin_ffi_enum(name: &str) -> bool {
    name == BUILTIN_FFI_TYPE_ENUM || name == FFI_TYPES_MODULE
}

/// True when `enum_name::variant` is a built-in FFI type constructor.
pub fn is_builtin_ffi_variant(enum_name: &str, variant_name: &str) -> bool {
    if !is_builtin_ffi_enum(enum_name) {
        return false;
    }
    BUILTIN_FFI_TYPE_VARIANTS.contains(&variant_name)
}

/// Encode a tag (and optional aux id) into a CONST operand for declare/invoke.
pub fn encode_tag_operand(tag: u32, aux: u32) -> u32 {
    if aux == 0 {
        tag
    } else {
        (aux << 16) | (tag & 0xFFFF)
    }
}

/// The parameter tags a C callback can take, by their digit in
/// [`encode_callback_sig`] (`0` ends the list).
const CALLBACK_PARAMS: [u32; 5] = [tag::INT, tag::FLOAT, tag::BOOL, tag::PTR, tag::STRING];
/// The result tags a C callback can return, by their digit.
const CALLBACK_RETS: [u32; 5] = [tag::VOID, tag::INT, tag::FLOAT, tag::BOOL, tag::PTR];

/// The most parameters a callback signature (a [`tag::CALLBACK`] aux) holds.
pub const CALLBACK_MAX_PARAMS: usize = 5;

/// A C callback's signature as a [`tag::CALLBACK`] tag's aux: the result's
/// digit plus five times the parameters' digits in base 6, plus one, so it
/// fits the aux's 16 bits for up to [`CALLBACK_MAX_PARAMS`] parameters. `0`
/// (no aux) is `int (*)(int64_t)`, what `ffi::types::Callback` has always
/// meant. `None`: a type a callback cannot take or return, or too many.
pub fn encode_callback_sig(params: &[u32], ret: u32) -> Option<u32> {
    if params.len() > CALLBACK_MAX_PARAMS {
        return None;
    }
    let r = CALLBACK_RETS.iter().position(|&t| t == ret)? as u32;
    let mut digits = 0u32;
    for &p in params.iter().rev() {
        digits = digits * 6 + 1 + CALLBACK_PARAMS.iter().position(|&t| t == p)? as u32;
    }
    Some(1 + r + 5 * digits)
}

/// Inverse of [`encode_callback_sig`]: the parameters' and the result's
/// tags.
pub fn decode_callback_sig(aux: u32) -> Option<(Vec<u32>, u32)> {
    if aux == 0 {
        return Some((vec![tag::INT], tag::INT));
    }
    let n = aux - 1;
    let ret = CALLBACK_RETS[(n % 5) as usize];
    let mut digits = n / 5;
    let mut params = Vec::new();
    while digits != 0 {
        let d = digits % 6;
        if d == 0 || params.len() == CALLBACK_MAX_PARAMS {
            return None;
        }
        params.push(CALLBACK_PARAMS[d as usize - 1]);
        digits /= 6;
    }
    Some((params, ret))
}

/// Inverse of [`encode_tag_operand`].
pub fn decode_tag_operand(enc: u32) -> (u32, u32) {
    if enc <= tag::LAST {
        (enc, 0)
    } else {
        (enc & 0xFFFF, enc >> 16)
    }
}

/// Tag for a built-in `FFIType::Variant` name.
pub fn tag_from_variant_name(variant_name: &str) -> Option<u32> {
    BUILTIN_FFI_TYPE_VARIANTS
        .iter()
        .position(|v| *v == variant_name)
        .map(|i| i as u32)
}

/// The FFI operations native code asks the runtime for.
pub const FFI_OP_LOAD: u8 = 0;
pub const FFI_OP_DECLARE: u8 = 1;
pub const FFI_OP_INVOKE: u8 = 2;
/// An `extern` call on its words (`ExternCall`): library, function id,
/// the `n` arguments (operand bits 0..16) and, with operand bit 16, the
/// `n` arguments' type tags. The result is the C function's word; an error
/// panics.
pub const FFI_OP_CALL: u8 = 3;
/// The most words an [`FFI_OP_CALL`] passes the runtime: library, function
/// id, the arguments and a variadic call's tags. A call with more keeps
/// the argument tuple.
pub const FFI_CALL_MAX_WORDS: usize = 64;
/// The C address of a declared function: library and function id. Its
/// `Result` holds the address word (module setup stores it for a direct
/// call).
pub const FFI_OP_ADDR: u8 = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_tags_match_declaration_order() {
        assert_eq!(tag_from_variant_name("Int"), Some(tag::INT));
        assert_eq!(tag_from_variant_name("Void"), Some(tag::VOID));
        assert_eq!(tag_from_variant_name("Ptr"), Some(tag::PTR));
        assert_eq!(tag_from_variant_name("Bytes"), Some(tag::BYTES));
        assert_eq!(tag_from_variant_name("ConstBytes"), Some(tag::CONST_BYTES));
    }

    #[test]
    fn type_name_aliases_resolve() {
        assert_eq!(tag_from_type_name("int32"), Some(tag::INT32));
        assert_eq!(tag_from_type_name("pointer"), Some(tag::PTR));
        assert_eq!(tag_from_type_name("const_bytes"), Some(tag::CONST_BYTES));
    }

    #[test]
    fn encode_decode_tag_operand_round_trip() {
        assert_eq!(
            decode_tag_operand(encode_tag_operand(tag::INT, 0)),
            (tag::INT, 0)
        );
        assert_eq!(
            decode_tag_operand(encode_tag_operand(tag::STRUCT, 3)),
            (tag::STRUCT, 3)
        );
        assert_eq!(
            decode_tag_operand(encode_tag_operand(tag::BYTES, 0)),
            (tag::BYTES, 0)
        );
        assert_eq!(
            decode_tag_operand(encode_tag_operand(tag::CONST_BYTES, 0)),
            (tag::CONST_BYTES, 0)
        );
    }
}

#[cfg(test)]
mod callback_sig_tests {
    use super::*;

    #[test]
    fn a_callback_signature_round_trips_through_its_aux() {
        let sigs: &[(&[u32], u32)] = &[
            (&[], tag::VOID),
            (&[tag::INT], tag::INT),
            (&[tag::PTR, tag::PTR], tag::INT),
            (&[tag::FLOAT, tag::BOOL, tag::STRING], tag::FLOAT),
            (&[tag::STRING; 5], tag::PTR),
        ];
        for &(params, ret) in sigs {
            let aux = encode_callback_sig(params, ret).expect("encodes");
            assert!(aux != 0 && aux <= 0xFFFF);
            assert_eq!(decode_callback_sig(aux), Some((params.to_vec(), ret)));
            let (t, a) = decode_tag_operand(encode_tag_operand(tag::CALLBACK, aux));
            assert_eq!((t, a), (tag::CALLBACK, aux));
        }
        assert_eq!(decode_callback_sig(0), Some((vec![tag::INT], tag::INT)));
        assert_eq!(encode_callback_sig(&[tag::INT; 6], tag::INT), None);
        assert_eq!(encode_callback_sig(&[tag::STRUCT], tag::INT), None);
        assert_eq!(encode_callback_sig(&[], tag::STRING), None);
    }
}
