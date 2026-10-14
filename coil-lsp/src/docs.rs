//! Hover documentation: Rust-style rendering of `///` comments and the
//! builtin doc stubs (`compiler/src/builtin_docs/`).

use std::collections::HashMap;
use std::sync::OnceLock;

use parser::{
    Pratt,
    ast::{Expression, Output},
};

/// Virtual modules whose exports are in scope without a `use`.
pub const PRELUDE_MODULES: &[&str] = &["prelude", "prelude::ops", "prelude::math"];

/// `///` lines (the parser drops `///` and one space) as hover markdown, the
/// way rustdoc shows them: untagged fences (and ```` ```coil ````)
/// highlighted as coil, and `# ` lines inside a coil example hidden (they
/// are setup a doc-test runs but the reader does not need).
pub fn render_doc_lines(lines: &[&str]) -> String {
    let mut out: Vec<String> = Vec::new();
    // `Some(true)`: inside a coil example; `Some(false)`: another fence.
    let mut fence: Option<bool> = None;
    for line in lines {
        let trimmed = line.trim_start();
        if let Some(info) = trimmed.strip_prefix("```") {
            match fence {
                None => {
                    let lang = info.split(',').next().unwrap_or("").trim();
                    let coil = matches!(lang, "" | "coil" | "ignore" | "no_run" | "should_panic");
                    fence = Some(coil);
                    let indent = &line[..line.len() - trimmed.len()];
                    out.push(if coil {
                        format!("{indent}```coil")
                    } else {
                        (*line).to_owned()
                    });
                }
                Some(_) => {
                    fence = None;
                    out.push((*line).to_owned());
                }
            }
            continue;
        }
        if fence == Some(true) && (trimmed == "#" || trimmed.starts_with("# ")) {
            continue;
        }
        out.push((*line).to_owned());
    }
    out.join("\n").trim().to_owned()
}

/// One documented builtin.
#[derive(Debug, Clone)]
pub struct BuiltinDoc {
    /// Module path (`io::fs`), `""` for the primitive types.
    pub module: String,
    /// Class, enum or trait that holds a method or variant.
    pub owner: Option<String>,
    pub name: String,
    /// Declaration as written in the stub, without its body.
    pub signature: String,
    /// Rendered markdown.
    pub docs: String,
}

impl BuiltinDoc {
    /// `use io::write;` for an item of a non-prelude module.
    pub fn import(&self) -> Option<String> {
        if self.owner.is_some() || self.module.is_empty() || PRELUDE_MODULES.contains(&self.module.as_str()) {
            return None;
        }
        Some(format!("use {}::{};", self.module, self.name))
    }

    /// Hover markdown: signature, import row, docs, reference link.
    pub fn markdown(&self) -> String {
        let header = match &self.owner {
            Some(owner) => format!("{owner}\n{}", self.signature),
            None => self.signature.clone(),
        };
        hover_text(&header, self.import().as_deref(), Some(&self.docs_with_reference()))
    }

    /// Docs plus a link to the module's page in the language reference.
    pub fn docs_with_reference(&self) -> String {
        let module = if self.module.is_empty() { "types" } else { &self.module };
        format!("{}\n\n[Read the `{module}` reference]({})", self.docs, reference_url(&self.module))
    }
}

/// Hover body: a coil code block, an optional import row, then docs.
pub fn hover_text(code: &str, import: Option<&str>, docs: Option<&str>) -> String {
    let mut value = format!("```coil\n{code}\n```");
    if let Some(import) = import {
        value.push_str(&format!("\n\n**Import:** `{import}`"));
    }
    if let Some(docs) = docs.filter(|d| !d.is_empty()) {
        value.push_str("\n\n---\n\n");
        value.push_str(docs);
    }
    value
}

const WEBSITE_DOCS: &str = "https://github.com/ardax-corp/coil-website/blob/main/src/content/docs";

/// The language reference page for a builtin module (`""`: the types page).
pub fn reference_url(module: &str) -> String {
    let page = match module {
        "" | "prelude::ops" => "references/types.md",
        "prelude" => "references/option-result.md",
        "prelude::math" => "references/math.md",
        "prelude::task" => "references/modules.md",
        "io" | "io::net::tcp" | "io::net::udp" => "references/io.md",
        "io::fs" => "references/io-fs.md",
        "string" => "references/string.md",
        "thread" => "manual/tutorial/11-threads.md",
        "env" => "references/env.md",
        "gc" => "references/gc.md",
        "ffi" | "ffi::types" => "references/ffi.md",
        _ => "references/modules.md",
    };
    format!("{WEBSITE_DOCS}/{page}")
}

pub struct BuiltinDocs {
    entries: Vec<BuiltinDoc>,
    keywords: HashMap<String, String>,
}

impl BuiltinDocs {
    pub fn get() -> &'static BuiltinDocs {
        static DOCS: OnceLock<BuiltinDocs> = OnceLock::new();
        DOCS.get_or_init(|| {
            let mut entries = Vec::new();
            for (module, source) in compiler::builtin_docs::MODULE_SOURCES {
                collect_stub(module, source, &mut entries);
            }
            collect_stub("", compiler::builtin_docs::TYPES_SOURCE, &mut entries);
            BuiltinDocs {
                entries,
                keywords: parse_keywords(compiler::builtin_docs::KEYWORDS),
            }
        })
    }

    /// A top-level item of `module`.
    pub fn item(&self, module: &str, name: &str) -> Option<&BuiltinDoc> {
        self.entries
            .iter()
            .find(|e| e.owner.is_none() && e.module == module && e.name == name)
    }

    /// A method or variant of the builtin `owner` (bare name, generics dropped).
    pub fn member(&self, owner: &str, name: &str) -> Option<&BuiltinDoc> {
        let owner = bare_type_name(owner);
        self.entries
            .iter()
            .find(|e| e.owner.as_deref().map(bare_type_name) == Some(owner) && e.name == name)
    }

    /// A primitive type (`int`, `string`, …).
    pub fn primitive(&self, name: &str) -> Option<&BuiltinDoc> {
        self.item("", name)
    }

    pub fn keyword(&self, word: &str) -> Option<&str> {
        self.keywords.get(word).map(String::as_str)
    }
}

/// `Vec<T>` → `Vec`, `io::Stream` → `Stream`, `impl Vec<T>` → `Vec`.
pub fn bare_type_name(ty: &str) -> &str {
    let base = ty.split('<').next().unwrap_or(ty).trim();
    let base = base.rsplit(' ').next().unwrap_or(base);
    base.rsplit("::").next().unwrap_or(base)
}

/// `## word` sections of keywords.md.
fn parse_keywords(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    for line in text.lines() {
        if let Some(word) = line.strip_prefix("## ") {
            if let Some((word, body)) = current.take() {
                out.insert(word, render_doc_lines(&body));
            }
            current = Some((word.trim().trim_matches('`').to_owned(), Vec::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push(line);
        }
    }
    if let Some((word, body)) = current {
        out.insert(word, render_doc_lines(&body));
    }
    out
}

fn collect_stub(module: &str, source: &str, out: &mut Vec<BuiltinDoc>) {
    let Ok(ast) = Pratt::default().parse(source) else {
        // A stub that does not parse is caught by the coverage test.
        return;
    };
    let Expression::Program(items) = ast.1.as_ref() else {
        return;
    };
    for item in items {
        collect_item(module, None, source, item, out);
    }
}

fn collect_item(module: &str, owner: Option<&str>, source: &str, item: &Output<'_>, out: &mut Vec<BuiltinDoc>) {
    let push = |out: &mut Vec<BuiltinDoc>, name: &str, signature: String, docs: &[&str]| {
        out.push(BuiltinDoc {
            module: module.to_owned(),
            owner: owner.map(str::to_owned),
            name: name.to_owned(),
            signature,
            docs: render_doc_lines(docs),
        });
    };
    match item.1.as_ref() {
        Expression::Function { name, docs, body, .. } => {
            let end = body.as_ref().map_or(item.0.end, |body| body.0.start);
            push(out, name, declaration_text(source, item.0.start, end), docs);
        }
        Expression::Method(_, inner) => match inner.1.as_ref() {
            // The outer span keeps `pub` / `static`.
            Expression::Function { name, docs, body, .. } => {
                let end = body.as_ref().map_or(inner.0.end, |body| body.0.start);
                push(out, name, declaration_text(source, item.0.start, end), docs);
            }
            _ => collect_item(module, owner, source, inner, out),
        },
        Expression::Class { name, docs, .. } => {
            push(out, name, header_text(source, item), docs);
        }
        Expression::EnumDecl { name, docs, variants, .. } => {
            let header = header_text(source, item);
            push(out, name, header.clone(), docs);
            let owner_name = header.trim_start_matches("enum ").to_owned();
            for variant in variants {
                if let Expression::EnumVariant { name: variant_name, docs, .. } = variant.1.as_ref() {
                    let text = declaration_text(source, variant.0.start, variant.0.end);
                    out.push(BuiltinDoc {
                        module: module.to_owned(),
                        owner: Some(owner_name.clone()),
                        name: (*variant_name).to_owned(),
                        signature: format!("{}::{}", bare_type_name(&owner_name), text.trim_end_matches(',')),
                        docs: render_doc_lines(docs),
                    });
                }
            }
        }
        Expression::TypeClass { name, docs, methods, .. } => {
            let header = header_text(source, item);
            push(out, name, header.clone(), docs);
            for method in methods {
                collect_item(module, Some(&header), source, method, out);
            }
        }
        Expression::TypeAlias { name, docs, .. } => {
            push(out, name, declaration_text(source, item.0.start, item.0.end), docs);
        }
        Expression::Implementation { owner: impl_owner, methods, type_params, .. } => {
            let owner_text = if type_params.is_empty() {
                format!("impl {impl_owner}")
            } else {
                let params: Vec<&str> = type_params.iter().map(|p| p.name).collect();
                format!("impl {impl_owner}<{}>", params.join(", "))
            };
            for method in methods {
                collect_item(module, Some(&owner_text), source, method, out);
            }
        }
        _ => {}
    }
}

/// Source of `start..end` without doc/attribute lines, whitespace collapsed.
pub fn declaration_text(source: &str, start: usize, end: usize) -> String {
    let text = source.get(start..end).unwrap_or("");
    let kept: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("///") && !line.starts_with("#[") && !line.is_empty())
        .collect();
    // A body span may start inside its braces.
    kept.join(" ").trim().trim_end_matches(['{', ';']).trim().to_owned()
}

/// `class Vec<T>` / `enum Option<T>` / `trait Add<T>`: text before the `{`.
fn header_text(source: &str, item: &Output<'_>) -> String {
    let text = declaration_text(source, item.0.start, item.0.end);
    text.split('{').next().unwrap_or(&text).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_lines_render_like_rustdoc() {
        let lines = [
            "Adds one.",
            "",
            "# Examples",
            "",
            "```",
            "# use tests::assert;",
            "if true {",
            "    let x = 1;",
            "#",
            "    assert!(x + 1 == 2);",
            "}",
            "```",
            "",
            "```text",
            "# kept outside coil examples",
            "```",
        ];
        assert_eq!(
            render_doc_lines(&lines),
            "Adds one.\n\n# Examples\n\n```coil\nif true {\n    let x = 1;\n    assert!(x + 1 == 2);\n}\n```\n\n```text\n# kept outside coil examples\n```"
        );
    }

    #[test]
    fn stub_items_methods_and_variants_are_collected() {
        let source = "/// A list.\nclass Vec<T> {}\n\nimpl Vec<T> {\n    /// Appends.\n    pub fn push(T x) {}\n}\n\n/// Maybe.\nenum Option<T> {\n    /// Nothing.\n    None,\n    /// A value.\n    Some(T),\n}\n\n/// Writes.\nfn write(Stream s, Vec<byte> bytes) -> Result<int, IoError> {}\n";
        let mut entries = Vec::new();
        collect_stub("io", source, &mut entries);
        let find = |owner: Option<&str>, name: &str| {
            entries
                .iter()
                .find(|e| e.owner.as_deref() == owner && e.name == name)
                .unwrap_or_else(|| panic!("{name} missing: {entries:?}"))
        };
        assert_eq!(find(None, "Vec").signature, "class Vec<T>");
        let push = find(Some("impl Vec<T>"), "push");
        assert_eq!(push.signature, "pub fn push(T x)");
        assert_eq!(push.docs, "Appends.");
        assert_eq!(find(Some("Option<T>"), "Some").signature, "Option::Some(T)");
        let write = find(None, "write");
        assert_eq!(write.signature, "fn write(Stream s, Vec<byte> bytes) -> Result<int, IoError>");
        assert_eq!(write.import().as_deref(), Some("use io::write;"));
        assert_eq!(push.import(), None);
    }
}
