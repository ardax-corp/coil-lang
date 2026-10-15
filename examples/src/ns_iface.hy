// Helper for tests/traits/compile_fail/orphan_impl_imported_trait.hy: a trait
// another module must not implement for a built-in type.

trait Foreign<T> {
    fn id(T x) -> int {}
}
