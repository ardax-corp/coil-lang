# Project configuration (`coil.toml`)

The **`coil.toml`** file at a project's root tells the compiler where to find module files and optionally which file is the entry point. It may also declare **`[package]`** / **`[dependencies]`** / **`[scripts]`** metadata for the **`spool`** library dependency manager. The compiler parses and stores this schema; spool owns fetch, link, lifecycle scripts, include-hooks, and engine-range checks.

### `spool` vs `coil package`

| Command | Role |
|---------|------|
| **`spool`** | Library dependency management (`install` / `add` / `update`): resolve git/path deps, write `coil.lock`, maintain a shared cache and project `.spool/deps` roots. Coil userland (not a Rust subcommand of `coil`). |
| **`coil package`** | Build an embedded **executable** (`.hyc` + runner such as `coil-embed`). Unrelated to library deps — do not confuse the names. |

---

## File location

Place `coil.toml` in the **project root** — the directory the compiler treats as the working directory when resolving relative paths.

```
my-project/
├── coil.toml
└── src/
    ├── main.hy
    └── foo/
        └── bar.hy
```

If `coil.toml` is absent, the compiler uses built-in defaults (see [Default behavior](#default-behavior-without-coiltoml)).

---

## Format

The parser accepts a minimal TOML-like subset:

- Section headers: `[module]`, `[entry]`, `[env]`, `[ffi]`, `[package]`, `[dependencies]`, `[scripts]`
- Key-value lines: `key = value`
- String values: double-quoted (`"./src"`)
- Array values: `["a", "b"]`
- Inline tables: `{ git = "…" }` (used under `[dependencies]`; optional `version` / `rev`)
- Comments: `#` to end of line
- Blank lines are ignored

Unknown sections or keys are parse errors.

---

## Sections and keys

### `[module]`

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `roots` | array of strings | No (defaults to `["src"]`) | Directories searched for module files, relative to the project root. For coil-stdlib, see [consume](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/consume.md) (`../coil-stdlib/src`, `./.deps/coil-stdlib/src`, or `.spool/deps/stdlib` when a managed root is present). |

Example:

```toml
[module]
roots = ["./src", "./vendor", "./builtins"]
```

Each path in `roots` is a **search root**. When resolving `use foo::bar;`, the compiler looks under each root **in order** for `<root>/foo/bar.hy` (one-item-per-file), then falls back to `<root>/foo.hy` (module file). The first existing path wins; see [Discovery algorithm](#discovery-algorithm).

If the `[module]` section is omitted entirely, roots default to `["src"]`.

If `[module]` is present but `roots` is omitted, roots also default to `["src"]`.

Compiler builtins (`prelude`, `prelude::ops`, `prelude::test`, `prelude::math`, `ffi`, `ffi::types`) are virtual modules owned by the compiler — they are **not** configured via `coil.toml`. Every file always gets the implicit prelude; FFI still requires an explicit `use`. Extra disk search directories belong in `roots`.

### `[entry]`

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `file` | string | No | Explicit entry-point file, relative to the project root |

Example:

```toml
[entry]
file = "./src/main.hy"
```

When set, `coil` and `coil compile` with **no file argument** use this path as the program entry (relative to the project root that owns `coil.toml`).

When omitted, you must pass the entry file on the command line:

```bash
coil examples/modules.hy
# or
coil compile examples/modules.hy
```

```bash
# with [entry] file = "./src/main.hy" in coil.toml:
coil
coil compile
```

### `[env]`

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `allow_exec` | bool | No (defaults to `false`) | When `true`, `env::exec` may spawn subprocesses at runtime (the compiler still warns at compile time) |

Example:

```toml
[env]
allow_exec = true   # opt-in: enable env::exec for trusted scripts
```

### `[package]`

Optional package identity for publishing / consuming libraries via **`spool`**. When the section is present, `name` and `version` are required. Other keys below are optional.

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `name` | string | Yes (if section present) | Short package name. Consumers import this name (`use http::…`), never the git URL. |
| `version` | string | Yes (if section present) | Semver version of this package (e.g. `"0.1.0"`). |
| `coil` | string | No | Optional Coil engine semver range (e.g. `">=0.1.0"`). Same range language as optional git-dep `version`. Stored only — this crate does not resolve or enforce it. Omit means no engine constraint. Compare later against `coil --version`. |
| `include` | string | No | Optional include-hook path, relative to **this package's checkout**. Runs when another project depends on this package (not when this repo is the current project). There is no `[hooks]` table — unknown sections still error. |

Example:

```toml
[package]
name = "http"
version = "0.1.0"
coil = ">=0.1.0"
include = "./hooks/include.sh"
```

The compiler stores these fields but does **not** use them for module discovery. **`spool`** owns dependency semantics, include-hook execution, and engine-range checks.

### `[scripts]`

Optional lifecycle scripts for **this project** when it is the current `spool` consumer. Paths are relative to this project root. Missing keys are `None` (no-op for later runners). Extra keys are parse errors. Allowed keys are only:

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `pre_install` | string | No | Before `spool install` fetch/link |
| `post_install` | string | No | After a successful `spool install` link |
| `pre_update` | string | No | Before `spool update` fetch/link |
| `post_update` | string | No | After a successful `spool update` link |

```toml
[scripts]
pre_install = "./scripts/pre-install.sh"
post_install = "./scripts/post-install.sh"
pre_update = "./scripts/pre-update.sh"
post_update = "./scripts/post-update.sh"
```

**Current-project vs include-hooks:** `[scripts]` fire only for the current project (`spool install` / `update` / `add` that materializes this project's deps). `[package].include` is the dependency's hook and runs in the consumer when that package is linked. They do not double-fire: a dependency's `[scripts]` are ignored during a consumer install; the consumer's `include` is ignored because it is not being consumed. The compiler stores both; spool owns running them.

Keep scripts out of `coil package` (executable embed).

### `[dependencies]`

Declares library dependencies for **`spool`**. Each key is the short package name (must match that dependency’s `[package].name`). Values are inline tables — one of:

| Form | Keys | Description |
|------|------|-------------|
| Git | `git` (string URL). Optional `version`, optional `rev`. | `{ git = "…" }` is valid. `version` is optional schema, not a resolved tag. `rev` is stored only. The pin is `coil.lock` (`rev` + `content_hash`) until COI-219. |
| Path | `path` (string) | Local checkout relative to the project root. |

`git` and `path` must not be combined on the same entry. `version` or `rev` without `git` is a parse error. Unknown inline keys are parse errors. Duplicate dependency names are parse errors.

Example:

```toml
[dependencies]
http = { git = "https://github.com/coil-lang/http.git" }
local_http = { path = "../local-http" }
```

`version` and `rev` may appear on a git entry; both are optional:

```toml
http = { git = "https://github.com/coil-lang/http.git", version = "^0.2" }
http = { git = "https://github.com/coil-lang/http.git", rev = "abc123" }
http = { git = "https://github.com/coil-lang/http.git", version = "^0.2", rev = "abc123" }
```

**Compiler role:** parse and store the schema so manifests with deps still compile. Optional `version` and `rev` are stored as parsed fields only — the compiler does not resolve tags, fetch git, or write a lockfile. Git tag resolution is COI-219; until then `coil.lock` (`rev` + `content_hash`) remains the pin. **`spool`** (`install` / `add` / `update`) resolves deps, writes that lock, and maintains a project-local managed root (e.g. `.spool/deps/<name>`) that should appear in `[module].roots`. The compiler does **not** read `coil.lock` or auto-inject roots.

When a managed root is on disk:

```toml
[module]
roots = ["./src", "./.spool/deps"]
```

Then `use greet::hello;` resolves under `./.spool/deps/greet/hello.hy` via the normal discovery algorithm (see [Modules](modules.md)). For coil-stdlib unprefixed imports, add `.spool/deps/stdlib` as a root ([consume](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/consume.md)).

---

## Complete example

From `coil.toml.example`:

```toml
# coil project manifest

[package]
name = "my_app"
version = "0.1.0"
# coil = ">=0.1.0"
# include = "./hooks/include.sh"

[module]
# Search roots for `use` resolution. Each path is relative to
# the directory containing this coil.toml file. The compiler
# searches the roots in order; the first file that exists wins.
# Include ./.spool/deps when a managed library root is present.
roots = ["./src", "./vendor", "../coil-stdlib/src"]

# Default when no coil.toml exists: roots = ["src"]

[entry]
# Optional entry point. If omitted, use the file from the CLI.
# file = "./src/main.hy"

# [dependencies]
# http = { git = "https://github.com/coil-lang/http.git" }
# http = { git = "https://github.com/coil-lang/http.git", version = "^0.2" }
# http = { git = "https://github.com/coil-lang/http.git", rev = "abc123" }
# local_http = { path = "../local-http" }

# [scripts]
# pre_install = "./scripts/pre-install.sh"
# post_install = "./scripts/post-install.sh"
# pre_update = "./scripts/pre-update.sh"
# post_update = "./scripts/post-update.sh"
```

---

## Discovery algorithm

Given a `use a::b::c;` statement and roots `["./src", "./vendor", "./builtins"]`.
Resolution matches the [modules reference](modules.md#path-resolution-algorithm): **one-item-per-file first**, then a **module-file** fallback. When both `<root>/foo/item.hy` and `<root>/foo.hy` exist, the item file wins (see shadowing note in modules.md). **Migration:** if a project accidentally kept both layouts, `use foo::item` silently binds the item file after this change — delete or rename the unused path.

### Step 1 — Split the import path

- Directory path segments: `["a", "b"]`
- Item name (last segment): `"c"`

### Step 2 — Search each root in order (Convention A)

For root `./src`:

```
./src/a/b/c.hy   → exists? use this file
```

If not found, try `./vendor`:

```
./vendor/a/b/c.hy
```

Then `./builtins`:

```
./builtins/a/b/c.hy
```

### Step 2b — Module-file fallback (Convention B)

If no one-item-per-file candidate exists in any root, try each root again for the parent module file (directory path only):

```
./src/a/b.hy     → exists? use this file (item `c` lives inside)
./vendor/a/b.hy
./builtins/a/b.hy
```

Example: `use math::add;` with only `./src/math.hy` present resolves to that file (namespace `math`, FQN `math::add`).

### Step 3 — First match wins

Stop at the first path that exists on disk. That file is loaded and compiled.

### Step 4 — Compute namespace

Strip the matching root prefix, remove `.hy`, replace `/` with `::`:

```
./src/a/b/c.hy  →  namespace "a::b::c"
./src/a/b.hy    →  namespace "a::b"     (module-file fallback)
```

### Wildcard imports (`E0124`)

`use path::*;` is rejected at typecheck for **every** module — virtual (`io`,
`ffi`, …) and userland disk modules. List names explicitly:
`use io::{stdout, open};`, `use foo::{name, …}`, or concrete `use io::open;`.
Prelude is auto-injected; do not write `use prelude::*` in source.

`mod foo;` loads a file for discovery/link order without binding names.

Discovery may still map a path ending in `*` to `<root>/…/stem.hy` when
scanning dependencies, but no names are imported from a wildcard `use`.

### `mod` declarations

For `mod foo;`:

1. Search each root for `<root>/foo.hy`.
2. First existing file wins.
3. Namespace is `foo`.

### Transitive discovery

1. Enqueue the entry file.
2. Parse it; find all `use` and `mod` declarations.
3. Enqueue referenced files not yet seen.
4. Repeat until no new files are discovered.
5. Compile all discovered files in dependency order.

---

## Default behavior without `coil.toml` {#default-behavior-without-coiltoml}

When no `coil.toml` exists in the project root (or the file cannot be read):

| Setting | Default |
|---------|---------|
| `[module].roots` | `["src"]` |
| `[entry].file` | None — use CLI argument |

This means a minimal project with only `src/main.hy` and `src/foo/bar.hy` works without any manifest, as long as you run the compiler from the project root:

```bash
cargo run -- src/main.hy
```

The namespace test suite confirms that `use foo::greet;` resolves to `src/foo/greet.hy` with no manifest present.

---

## Multiple roots in practice

Use multiple roots for vendored libraries, stdlib, or **`spool`**-managed deps:

```toml
[module]
roots = ["./src", "./.spool/deps", "../coil-stdlib/src"]
```

Resolution order means **your source tree takes precedence**. If both `src/foo/greet.hy` and `.spool/deps/foo/greet.hy` exist, the `src/` copy is used.

Typical layout:

```
project/
├── coil.toml
├── coil.lock          # written by spool (when used)
├── src/               # application code (first priority)
├── .spool/deps/       # managed symlinks into the shared spool cache
└── .deps/coil-stdlib/ # optional local checkout (use .deps/coil-stdlib/src as a root)
```

---

## Related documentation

- [Modules reference](modules.md) — `use` / `mod` syntax, FQN rules, dep roots
- [Tutorial: Modules](../manual/tutorial/06-modules.md) — walkthrough with `examples/modules.hy`
- [Getting started](../manual/getting-started.md) — `coil package` (embed executable), unrelated to `spool`
