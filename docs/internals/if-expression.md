# `if` as an expression (#1877)

`let y = if c { 1 } else { 2 };` works the way `match` does: the value of an `if` is the trailing expression of the branch that runs.

## Design

- **Parser.** `Pratt::if_expr` is a keyword atom next to `match_expr` in `expr()`. Its branch bodies are `brace_body` value blocks (statements, then an optional trailing expression), the same as match arms. `else if` chains flatten into one `Expression::If(branches)`, and `if let … else …` builds `Expression::IfLet`, as the statement parser does. The node is wrapped in `Expression::Expr`, and only the value form gets that wrapper. A statement `if` is parsed by `Pratt::if_` first and is unchanged: no `else` needed, statement bodies, no `;`.
- **Precedence.** An `if` binds as a primary, as `match` does. `if c { a } else { b } + 1` is `(if …) + 1`. At the start of a statement the statement `if` wins, so write `let y = if … + 1;` and not a bare expression statement.
- **Types.** `infer_if` / `infer_match` were already expression-typed. Branch values join with `join_ty` (`never` from `return` / `panic` is absorbed). Mixed types give E0102.
- **Missing `else`.** In value position, a missing `else` parses and is then rejected by the checker with **E0137** (`IfWithoutElse`): the false path has no value. The check is `Expression::Expr(e) if if_value_without_else(e)` in `infer_inner`. For `if let`, the parser marks a missing `else` with an empty block spanning the whole `if let`.
- **Lowering.** There are no new HIR nodes or opcodes. `E::Expr` peels to `if_chain` / `match_`, which already build `HirKind::If { els: Some(_) }` / `HirKind::Match` with typed values. HIR lowering, the layout plan and MIR (`from_hir::if_`, `join_arms`) already handle value `if`s, because inlining and `?` produce them.
- **Control flow.** `return if c { a } else { b };` is a `Return`, so E0111 does not fire. E0132 (definite assignment) runs on HIR, where a value `if` is the same `If` node as before.
- **Formatter.** `fmt_value` strips the `Expr` wrapper and prints the `if` through `fmt_if`. A branch's trailing value keeps no `;` (`fmt_block_stmt`'s `is_last`).
