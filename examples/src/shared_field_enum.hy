// Provider for tests/modules/derived_show_shared_field.hy (#2033): record
// variants that share a field name, with derived `Show` and `Eq`.
#[derive(Show, Eq)]
enum Failure {
    Protocol { message: string },
    Server { message: string },
    Closed,
}
