// Helper for tests/modules/module_resolution.hy: exports a `Client`, as does
// `ns_left`.

class Client {
    pub n: int,
}

impl Client {
    pub fn side() -> string {
        return "right";
    }
}
