//! Directives in a test file's leading comment header (`// @output 3`,
//! `// @expected E0209`, `// @flags --allow-read`), which `coil-test` and
//! the examples check. Names are case-insensitive and the colon after a name
//! is optional (`// @Output: 3`).

/// Every header directive `coil-test` reads.
pub const DIRECTIVES: [&str; 3] = ["output", "expected", "flags"];

/// The comment lines before a file's first line of code.
fn header(src: &str) -> impl Iterator<Item = &str> {
    src.lines().take_while(|l| l.starts_with("//"))
}

/// `(name, value)` when `line` is a `// @name value` directive. The value
/// loses the colon after the name and one space.
fn directive(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("//")?.trim_start().strip_prefix('@')?;
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    let (name, value) = rest.split_at(end);
    if name.is_empty() {
        return None;
    }
    let value = value.strip_prefix(':').unwrap_or(value);
    Some((name, value.strip_prefix(' ').unwrap_or(value)))
}

/// The values of every `// @name` line in the header, in order.
pub fn values<'a>(src: &'a str, name: &'a str) -> impl Iterator<Item = &'a str> {
    header(src)
        .filter_map(directive)
        .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
}

/// The value of the first `// @name` line in the header.
pub fn value<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    header(src)
        .filter_map(directive)
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
}

/// A header line in the old `// Output:` form, with the directive it is
/// now spelled as: such a line would otherwise silently stop being checked.
pub fn legacy_line(src: &str) -> Option<(&str, String)> {
    header(src).find_map(|line| {
        let rest = line.strip_prefix("//")?.trim_start();
        DIRECTIVES.iter().find_map(|name| {
            let head = rest.get(..name.len() + 1)?;
            (head[..name.len()].eq_ignore_ascii_case(name) && head.ends_with(':'))
                .then(|| (line, format!("// @{name}")))
        })
    })
}

/// Undo the escapes of an `@output` value (`\n`, `\t`, `\e`, `\\`).
pub fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('e') => out.push('\u{1b}'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_case_insensitive_and_the_colon_is_optional() {
        for src in ["// @output 3\n", "// @Output: 3\n", "//@OUTPUT 3\n", "// @output:3\n"] {
            assert_eq!(value(src, "output"), Some("3"), "{src:?}");
        }
        assert_eq!(value("// @output\n", "output"), Some(""));
        assert_eq!(value("// @outputs 3\n", "output"), None);
    }

    #[test]
    fn only_the_leading_header_counts() {
        let src = "// a test\n// @flags --allow-read\nfn main() {}\n// @output 3\n";
        assert_eq!(value(src, "flags"), Some("--allow-read"));
        assert_eq!(value(src, "output"), None);
    }

    #[test]
    fn repeated_directives_keep_their_order() {
        let src = "// @expected E0102\n// note\n// @Expected E0103 — either\n";
        assert_eq!(values(src, "expected").collect::<Vec<_>>(), ["E0102", "E0103 — either"]);
    }

    #[test]
    fn old_spellings_are_found_with_their_new_name() {
        assert_eq!(legacy_line("// Output: 3\n").map(|(_, n)| n).as_deref(), Some("// @output"));
        assert_eq!(legacy_line("// x\n// expected: E0102\n").map(|(_, n)| n).as_deref(), Some("// @expected"));
        assert_eq!(legacy_line("// Expected output: ok\n// Flags are fine\n"), None);
        assert_eq!(legacy_line("// @output 3\n"), None);
    }

    #[test]
    fn unescapes_output() {
        assert_eq!(unescape(r"a\nb\t\\\e"), "a\nb\t\\\u{1b}");
    }
}
