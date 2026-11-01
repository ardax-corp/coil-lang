use std::path::Path;

use crate::{
    Diagnostic, DiagnosticSink, SourceId, SourceMap,
};

/// In-memory diagnostic sink for language tooling and embedding clients.
#[derive(Default)]
pub struct DiagnosticCollector {
    sources: SourceMap,
    diagnostics: Vec<Diagnostic>,
}

impl DiagnosticCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diagnostics
    }

    pub fn sources(&self) -> &SourceMap {
        &self.sources
    }
}

impl DiagnosticSink for DiagnosticCollector {
    fn emit(&mut self, diag: Diagnostic) {
        self.diagnostics.push(diag);
    }

    fn register_source(&mut self, path: &Path, text: &str) -> SourceId {
        self.sources.insert(path, text)
    }

    fn finish(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn had_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Severity;
    use std::path::Path;

    #[test]
    fn collects_diagnostics_and_tracks_errors() {
        let mut sink = DiagnosticCollector::new();
        let id = sink.register_source(Path::new("a.coil"), "fn main() {}\n");
        assert_eq!(sink.sources().text(id), Some("fn main() {}\n"));

        sink.emit(Diagnostic::warning("soft"));
        assert!(!sink.had_errors());
        assert_eq!(sink.diagnostics().len(), 1);
        assert_eq!(sink.diagnostics()[0].severity, Severity::Warning);

        sink.emit(Diagnostic::error("hard"));
        assert!(sink.had_errors());
        assert_eq!(sink.diagnostics().len(), 2);

        sink.finish().unwrap();
        let out = sink.into_diagnostics();
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(Diagnostic::is_error));
    }
}
