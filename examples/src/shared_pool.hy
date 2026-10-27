// Provider for tests/memory/early_free_imported_static.hy: statics whose
// short names are also host natives (`with_capacity`, `format`), returning
// an object the caller does not own.

class Pool {
    pub items: Vec<int>,
}

class Keeper {
    pub kept: Vec<Pool>,
}

impl Pool {
    pub static fn with_capacity(Keeper k, int n) -> Pool {
        if k.kept.len() == 0 {
            k.kept.push(new Pool(Vec::with_capacity(n)));
        }
        return k.kept[0];
    }

    pub static fn format(Keeper k) -> Pool {
        return k.kept[0];
    }
}
