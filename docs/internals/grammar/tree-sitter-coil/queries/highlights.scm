; Coil highlighting for editors that load this tree-sitter grammar.
; `coil-lsp` semantic tokens are the primary highlighter; keep this in
; sync with parser comments (`//`, nestable `/* */`, `///` docs) and
; contextual macro words (`derive` / `macro` / `quote` / `attrs`).

(comment) @comment
(doc_comment) @comment.documentation
(block_comment) @comment

(string) @string
(number) @number
(boolean) @constant.builtin

[
  "fn"
  "let"
  "const"
  "class"
  "enum"
  "type"
  "if"
  "else"
  "for"
  "while"
  "in"
  "match"
  "return"
  "use"
  "mod"
  "pub"
  "static"
  "async"
  "defer"
  "raise"
  "panic"
  "yield"
  "break"
  "continue"
  "where"
  "impl"
  "trait"
  "extern"
  "as"
  "readonly"
  "new"
  "default"
  "typeof"
  "resume"
  "with"
  "done"
  "attr"
  "struct"
  "test"
  "forall"
  "from"
] @keyword

(function_declaration
  name: (identifier) @function)
(attr_declaration
  name: (identifier) @function)
(call_expression
  function: (identifier) @function)
(class_declaration
  name: (identifier) @type)
(enum_declaration
  name: (identifier) @type)
(trait_declaration
  name: (identifier) @type)
(type_alias
  name: (identifier) @type)
