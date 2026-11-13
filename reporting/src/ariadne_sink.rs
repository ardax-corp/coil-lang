//! Pretty (ariadne) diagnostic sink.

use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::path::Path;

use ariadne::{Cache, Color, Config, IndexType, Label as AriadneLabel, LabelAttach, Report, ReportKind, Source};

use crate::diagnostic::{Diagnostic, Severity};
use crate::sink::DiagnosticSink;
use crate::source::{SourceId, SourceMap};

/// Renders each diagnostic immediately as an ariadne report.
pub struct AriadneSink {
    sources: SourceMap,
    writer: Box<dyn Write + Send>,
    error_count: usize,
    /// Each source a report has read, by its displayed path, line-indexed
    /// once: indexing every registered file for every report made a
    /// compile with many warnings quadratic in its size (#2352).
    rendered: HashMap<String, Source<String>>,
}

/// The [`Cache`] a report reads its files from: each file of `sources`
/// (the last one registered under a displayed path), indexed the first
/// time a report needs it.
struct RenderedSources<'a> {
    rendered: &'a mut HashMap<String, Source<String>>,
    sources: &'a SourceMap,
}

impl Cache<String> for RenderedSources<'_> {
    type Storage = String;

    fn fetch(&mut self, id: &String) -> Result<&Source<String>, impl fmt::Debug> {
        if !self.rendered.contains_key(id) {
            let text = self
                .sources
                .iter()
                .filter(|(_, path, _)| path.display().to_string() == *id)
                .last()
                .map(|(_, _, text)| text.to_string())
                .ok_or_else(|| format!("Failed to fetch source '{id}'"))?;
            self.rendered.insert(id.clone(), Source::from(text));
        }
        Ok::<_, String>(&self.rendered[id])
    }

    fn display<'b>(&self, id: &'b String) -> Option<impl fmt::Display + 'b> {
        Some(Box::new(id))
    }
}

impl AriadneSink {
    pub fn new(sources: SourceMap, writer: Box<dyn Write + Send>) -> Self {
        Self {
            sources,
            writer,
            error_count: 0,
            rendered: HashMap::new(),
        }
    }

    fn write_spanless(&mut self, diag: &Diagnostic) -> std::io::Result<()> {
        let kind = match diag.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Note => "note",
        };
        if let Some(code) = diag.code {
            write!(self.writer, "{kind}[{code}]: {}", diag.message)?;
        } else {
            write!(self.writer, "{kind}: {}", diag.message)?;
        }
        if let Some(help) = &diag.help {
            write!(self.writer, "\nhelp: {help}")?;
        }
        writeln!(self.writer)?;
        Ok(())
    }

    fn write_spanned(&mut self, diag: &Diagnostic) -> std::io::Result<()> {
        let loc = diag
            .location
            .as_ref()
            .expect("write_spanned requires a primary location");

        let (path, _) = self.sources.get(loc.file).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("unknown SourceId {}", loc.file.as_u32()),
            )
        })?;
        let filename = path.display().to_string();

        let kind = match diag.severity {
            Severity::Error => ReportKind::Error,
            Severity::Warning => ReportKind::Warning,
            Severity::Info => ReportKind::Custom("Info", Color::BrightBlue),
            Severity::Note => ReportKind::Custom("Note", Color::BrightBlue),
        };

        let message = if let Some(code) = diag.code {
            format!("[{code}] {}", diag.message)
        } else {
            diag.message.clone()
        };

        let mut report = Report::build(kind, (filename.clone(), loc.range.clone()))
            .with_message(&message)
            .with_config(
                Config::new()
                    .with_index_type(IndexType::Byte)
                    .with_underlines(true)
                    .with_label_attach(LabelAttach::End)
                    .with_multiline_arrows(true)
                    .with_compact(false),
            );

        for label in &diag.labels {
            let label_path = self
                .sources
                .path(label.location.file)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| filename.clone());
            report = report.with_label(
                AriadneLabel::new((label_path, label.location.range.clone()))
                    .with_message(&label.message)
                    .with_color(Color::Primary),
            );
        }

        if let Some(help) = &diag.help {
            report = report.with_help(help);
        }

        let cache = RenderedSources { rendered: &mut self.rendered, sources: &self.sources };

        report
            .finish()
            .write(cache, &mut self.writer)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }
}

fn diagnostic_has_source_span(diag: &Diagnostic) -> bool {
    if diag
        .labels
        .iter()
        .any(|l| l.location.range.start < l.location.range.end)
    {
        return diag.location.is_some();
    }
    diag.location
        .as_ref()
        .is_some_and(|l| l.range.start < l.range.end)
}

impl DiagnosticSink for AriadneSink {
    fn emit(&mut self, diag: Diagnostic) {
        if diag.is_error() {
            self.error_count += 1;
        }
        let result = if diagnostic_has_source_span(&diag) {
            self.write_spanned(&diag)
        } else {
            self.write_spanless(&diag)
        };
        if let Err(err) = result {
            let _ = writeln!(
                std::io::stderr(),
                "reporting: failed to render diagnostic: {err}"
            );
        }
    }

    fn register_source(&mut self, path: &Path, text: &str) -> SourceId {
        self.rendered.remove(&path.display().to_string());
        self.sources.insert(path, text)
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }

    fn had_errors(&self) -> bool {
        self.error_count > 0
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::codes::ErrorCode;
    use crate::diagnostic::{Location, RelatedLabel};

    #[derive(Clone, Default)]
    struct SharedBuf {
        inner: Arc<Mutex<Vec<u8>>>,
    }

    impl SharedBuf {
        fn new() -> Self {
            Self::default()
        }

        fn into_string(self) -> String {
            String::from_utf8_lossy(&self.inner.lock().unwrap()).into_owned()
        }
    }

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A report after a file is registered again shows its new text, not
    /// the copy an earlier report indexed.
    #[test]
    fn pretty_sink_reads_a_file_registered_again() {
        let shared = SharedBuf::new();
        let mut sink = AriadneSink::new(SourceMap::new(), Box::new(shared.clone()));
        let file = sink.register_source(Path::new("a.coil"), "let old_name = 1;\n");
        sink.emit(Diagnostic::warning("first").at(file, 4..12).with_label(RelatedLabel::new(Location::new(file, 4..12), "here")));
        let file = sink.register_source(Path::new("a.coil"), "let new_name = 2;\n");
        sink.emit(Diagnostic::warning("second").at(file, 4..12).with_label(RelatedLabel::new(Location::new(file, 4..12), "here")));
        let out = shared.into_string();
        let second = out.split("second").nth(1).expect("second report");
        assert!(out.contains("old_name") && second.contains("new_name") && !second.contains("old_name"), "{out}");
    }

    #[test]
    fn pretty_sink_renders_spanned_diagnostic_without_panic() {
        let mut sources = SourceMap::new();
        let file = sources.insert("sample.coil", "let x = \"hi\";\n");
        let shared = SharedBuf::new();
        let mut sink = AriadneSink::new(sources, Box::new(shared.clone()));

        sink.emit(
            Diagnostic::error("Type mismatch")
                .with_code(ErrorCode::TypeMismatch)
                .at(file, 8..12)
                .with_label(RelatedLabel::new(
                    Location::new(file, 8..12),
                    "found string",
                ))
                .with_help("expected int"),
        );
        sink.finish().unwrap();
        assert!(sink.had_errors());
        assert!(!shared.into_string().is_empty());
    }

    #[test]
    fn pretty_sink_renders_spanless_diagnostic() {
        let shared = SharedBuf::new();
        let mut sink = AriadneSink::new(SourceMap::new(), Box::new(shared.clone()));
        sink.emit(
            Diagnostic::error("Bytecode archive version mismatch")
                .with_code(ErrorCode::ArchiveVersionMismatch),
        );
        sink.finish().unwrap();

        let out = shared.into_string();
        assert!(out.contains("E0901"));
        assert!(out.contains("Bytecode archive version mismatch"));
        assert!(sink.had_errors());
    }

    #[test]
    fn pretty_sink_renders_snippet_for_message_without_labels() {
        use crate::message::Message;

        let source = "let x: int = \"hi\";\n";
        let mut sources = SourceMap::new();
        let file = sources.insert("sample.coil", source);
        let shared = SharedBuf::new();
        let mut sink = AriadneSink::new(sources, Box::new(shared.clone()));

        // Typechecker-style message: range set, labels empty — convert must
        // synthesize a primary label so ariadne shows the source line.
        let msg = Message::error(
            ErrorCode::TypeMismatch,
            "Type mismatch: expected `int`, found `string`".into(),
            13..17,
        );
        sink.emit(Diagnostic::from_message(&msg, file));
        sink.finish().unwrap();

        let out = shared.into_string();
        assert!(
            out.contains("let x: int") || out.contains("\"hi\""),
            "expected source snippet in pretty output, got:\n{out}"
        );
        assert!(
            out.contains("E0102") || out.contains("Type mismatch"),
            "{out}"
        );
        assert!(sink.had_errors());
    }

    #[test]
    fn register_source_updates_map() {
        let shared = SharedBuf::new();
        let mut sink = AriadneSink::new(SourceMap::new(), Box::new(shared));
        let id = sink.register_source(Path::new("a.coil"), "fn main() {}");
        assert_eq!(id.as_u32(), 0);
    }
}
