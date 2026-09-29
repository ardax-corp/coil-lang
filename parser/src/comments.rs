//! Comment side table.
//!
//! Comments are trivia to the parser (see `trivia` in `lib.rs`) and never
//! reach the AST. Tooling that must keep them, such as the formatter, scans
//! them here with their byte spans and reattaches them by position.

use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentKind {
    /// `// …` up to (not including) the newline.
    Line,
    /// `/* … */`, possibly nested and multi-line.
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment<'src> {
    pub kind: CommentKind,
    /// Byte span of the whole comment, delimiters included.
    pub span: Range<usize>,
    /// Source text of the comment (trailing whitespace trimmed for line
    /// comments).
    pub text: &'src str,
}

/// Every `//` and `/* */` comment in `source`, in order.
///
/// `///` doc lines are not included: they are part of the AST (`docs` on
/// declarations and parameters). String literals are skipped so `"a//b"` is
/// not a comment. An unterminated block comment runs to the end of input
/// (the parser reports it).
pub fn collect(source: &str) -> Vec<Comment<'_>> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let start = i;
                let end = source[i..].find('\n').map_or(source.len(), |n| i + n);
                let doc = bytes.get(i + 2) == Some(&b'/') && bytes.get(i + 3) != Some(&b'/');
                if !doc {
                    let text = source[start..end].trim_end();
                    out.push(Comment {
                        kind: CommentKind::Line,
                        span: start..start + text.len(),
                        text,
                    });
                }
                i = end;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let start = i;
                let mut depth = 1;
                i += 2;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                let end = i.min(source.len());
                out.push(Comment {
                    kind: CommentKind::Block,
                    span: start..end,
                    text: &source[start..end],
                });
            }
            _ => i += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(source: &str) -> Vec<&str> {
        collect(source).into_iter().map(|c| c.text).collect()
    }

    #[test]
    fn line_and_block_comments() {
        assert_eq!(
            texts("let a = 1; // one  \n/* two */ let b = /* three */ 2;\n"),
            ["// one", "/* two */", "/* three */"]
        );
    }

    #[test]
    fn docs_and_strings_are_not_comments() {
        assert_eq!(texts("/// doc\nlet s = \"http://x /* y */\";\n"), Vec::<&str>::new());
        assert_eq!(texts("//// banner\n"), ["//// banner"]);
        assert_eq!(texts("let s = \"a\\\"//b\"; // real\n"), ["// real"]);
    }

    #[test]
    fn nested_block_comment() {
        assert_eq!(texts("/* a /* b */ c */ x"), ["/* a /* b */ c */"]);
    }
}
