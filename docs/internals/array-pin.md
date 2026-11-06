# Array pins

Array pins are gone: nothing in the compiler or the runtime pins an array, and
no engine keeps a per-frame pin table. An element access resolves its array on
each use, or, in native code, reads through a view of the array's words
(`coil_native_view`).

`compiler/src/typechecking/index_facts.rs` still works out which indexes are in
bounds and which array parameters keep their length; its `pin_params` sidecar
has no reader in code generation.
