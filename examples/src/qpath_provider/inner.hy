// Nested provider for tests/positive/qualified_paths.hy
// (three-segment paths: `qpath_provider::inner::Item`).

fn triple(int x) -> int {
    return x * 3;
}

class Tag {
    pub label: string,
}

impl Tag {
    pub static fn named(string label) -> Tag {
        return new Tag(label);
    }
}

enum Level {
    Low,
    High(int),
}
