# HTTP client and server

HTTP is **userland** in [coil-http](https://github.com/ardax-corp/coil-http), not a compiler builtin.

## Install via spool

```toml
[dependencies]
http = { git = "https://github.com/ardax-corp/coil-http.git", version = "^0.1" }

[module]
roots = ["./src", "./.spool/deps/http"]
```

Run `spool install` in the project root, then:

```coil
use http::{Client, Server};
```

**Docs:** [coil-http](https://github.com/ardax-corp/coil-http/blob/main/docs/README.md)

Transport uses virtual [`io::net::tcp`](../references/io.md) and the [coil-tls](https://github.com/ardax-corp/coil-tls) package ([tls](../references/tls.md)).

For a sibling checkout instead of spool, add `../coil-http/src` to `[module].roots` (see [consume](https://github.com/ardax-corp/coil-http/blob/main/docs/consume.md)).
