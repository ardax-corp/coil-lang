//! `///` doc stubs for the compiler builtins, one file per virtual module
//! (`builtin_docs/io/fs.hy` documents `io::fs`). Stubs are declarations with
//! empty bodies: they are parsed for their docs and signatures (the language
//! server's hover) and never compiled. Their examples follow the doc-test
//! rules in `builtin_docs/README.md`.

/// `(module path, stub source)` for every virtual module.
pub const MODULE_SOURCES: &[(&str, &str)] = &[
    ("prelude", include_str!("builtin_docs/prelude.hy")),
    ("prelude::ops", include_str!("builtin_docs/prelude/ops.hy")),
    ("prelude::math", include_str!("builtin_docs/prelude/math.hy")),
    ("prelude::task", include_str!("builtin_docs/prelude/task.hy")),
    ("prelude::process", include_str!("builtin_docs/prelude/process.hy")),
    ("io", include_str!("builtin_docs/io.hy")),
    ("io::fs", include_str!("builtin_docs/io/fs.hy")),
    ("io::tty", include_str!("builtin_docs/io/tty.hy")),
    ("io::net::tcp", include_str!("builtin_docs/io/net/tcp.hy")),
    ("io::net::udp", include_str!("builtin_docs/io/net/udp.hy")),
    ("string", include_str!("builtin_docs/string.hy")),
    ("thread", include_str!("builtin_docs/thread.hy")),
    ("gc", include_str!("builtin_docs/gc.hy")),
    ("env", include_str!("builtin_docs/env.hy")),
    ("clock", include_str!("builtin_docs/clock.hy")),
    ("symbol", include_str!("builtin_docs/symbol.hy")),
    ("ffi", include_str!("builtin_docs/ffi.hy")),
    ("ffi::types", include_str!("builtin_docs/ffi/types.hy")),
];

/// The primitive types (`int`, `string`, …), in scope everywhere.
pub const TYPES_SOURCE: &str = include_str!("builtin_docs/types.hy");

/// Keyword docs: one `## word` section per keyword.
pub const KEYWORDS: &str = include_str!("builtin_docs/keywords.md");
