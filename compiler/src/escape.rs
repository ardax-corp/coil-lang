//! Shared escape answers: the `[T; N]` grow-method check (Q3).

/// Vec length-changing methods that are a type error on `[T; N]` (Q3).
pub fn is_fixed_array_grow_method(method: &str) -> bool {
    matches!(
        method,
        "push" | "insert" | "pop" | "remove" | "clear" | "reserve"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grow_methods_are_length_changing() {
        for name in ["push", "insert", "pop", "remove", "clear", "reserve"] {
            assert!(is_fixed_array_grow_method(name), "{name}");
        }
        assert!(!is_fixed_array_grow_method("len"));
        assert!(!is_fixed_array_grow_method("capacity"));
    }
}
