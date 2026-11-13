//! `///` doc stubs for the compiler builtins, one file per virtual module
//! (`builtin_docs/io/fs.coil` documents `io::fs`). Stubs are declarations with
//! empty bodies: they are parsed for their docs and signatures (the language
//! server's hover) and never compiled. Their examples follow the doc-test
//! rules in `builtin_docs/README.md`.

/// `(module path, stub source)` for every virtual module.
pub const MODULE_SOURCES: &[(&str, &str)] = &[
    ("prelude", include_str!("builtin_docs/prelude.coil")),
    ("prelude::ops", include_str!("builtin_docs/prelude/ops.coil")),
    ("prelude::math", include_str!("builtin_docs/prelude/math.coil")),
    ("prelude::task", include_str!("builtin_docs/prelude/task.coil")),
    ("prelude::process", include_str!("builtin_docs/prelude/process.coil")),
    ("prelude::thread", include_str!("builtin_docs/prelude/thread.coil")),
    ("io", include_str!("builtin_docs/io.coil")),
    ("io::fs", include_str!("builtin_docs/io/fs.coil")),
    ("io::tty", include_str!("builtin_docs/io/tty.coil")),
    ("io::net::tcp", include_str!("builtin_docs/io/net/tcp.coil")),
    ("io::net::udp", include_str!("builtin_docs/io/net/udp.coil")),
    ("string", include_str!("builtin_docs/string.coil")),
    ("thread", include_str!("builtin_docs/thread.coil")),
    ("gc", include_str!("builtin_docs/gc.coil")),
    ("env", include_str!("builtin_docs/env.coil")),
    ("clock", include_str!("builtin_docs/clock.coil")),
    ("symbol", include_str!("builtin_docs/symbol.coil")),
    ("entropy", include_str!("builtin_docs/entropy.coil")),
    ("key_index", include_str!("builtin_docs/key_index.coil")),
    ("ffi", include_str!("builtin_docs/ffi.coil")),
    ("ffi::types", include_str!("builtin_docs/ffi/types.coil")),
];

/// The primitive types (`int`, `string`, …), in scope everywhere.
pub const TYPES_SOURCE: &str = include_str!("builtin_docs/types.coil");

/// Keyword docs: one `## word` section per keyword.
pub const KEYWORDS: &str = include_str!("builtin_docs/keywords.md");
