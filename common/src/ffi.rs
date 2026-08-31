//! Shared FFI type tags and names (compiler + VM must agree).

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
}

pub const BUILTIN_FFI_TYPE_ENUM: &str = "FFIType";

/// Built-in `FFIType` variant names in tag order (must match VM decoder).
pub const BUILTIN_FFI_TYPE_VARIANTS: &[&str] = &[
    "Int", "Float", "String", "Void", "Bool", "Int8", "Int16", "Int32", "UInt8", "UInt16",
    "UInt32", "UInt64", "Ptr", "Callback", "Struct",
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

/// Inverse of [`encode_tag_operand`].
pub fn decode_tag_operand(enc: u32) -> (u32, u32) {
    if enc <= tag::STRUCT {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_tags_match_declaration_order() {
        assert_eq!(tag_from_variant_name("Int"), Some(tag::INT));
        assert_eq!(tag_from_variant_name("Void"), Some(tag::VOID));
        assert_eq!(tag_from_variant_name("Ptr"), Some(tag::PTR));
    }

    #[test]
    fn type_name_aliases_resolve() {
        assert_eq!(tag_from_type_name("int32"), Some(tag::INT32));
        assert_eq!(tag_from_type_name("pointer"), Some(tag::PTR));
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
    }
}
