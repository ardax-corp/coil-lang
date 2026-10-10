//! Running an SMT-LIB solver on one query.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// How to start the solver: `z3 -in` by default.
#[derive(Debug, Clone)]
pub struct Solver {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Per query, in seconds.
    pub timeout: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// The check can never fail.
    Unsat,
    /// It can: the model's `(name value)` pairs.
    Sat(Vec<(String, String)>),
    /// Timeout, or a solver that gave up.
    Unknown(String),
}

impl Solver {
    pub fn z3(program: Option<PathBuf>, timeout: u32) -> Self {
        Self { program: program.unwrap_or_else(|| PathBuf::from("z3")), args: vec!["-in".into(), "-smt2".into()], timeout }
    }

    /// The solver's version line, or why it cannot be run.
    pub fn probe(&self) -> Result<String, String> {
        let out = Command::new(&self.program)
            .arg("--version")
            .output()
            .map_err(|e| format!("cannot run `{}`: {e}", self.program.display()))?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn check(&self, smt: &str) -> Answer {
        let script = format!("(set-option :timeout {})\n{smt}", u64::from(self.timeout) * 1000);
        let child = Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => return Answer::Unknown(format!("cannot run `{}`: {e}", self.program.display())),
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(script.as_bytes());
        }
        let out = match child.wait_with_output() {
            Ok(o) => o,
            Err(e) => return Answer::Unknown(e.to_string()),
        };
        parse_answer(&String::from_utf8_lossy(&out.stdout))
    }
}

pub fn parse_answer(out: &str) -> Answer {
    let mut lines = out.lines();
    match lines.next().map(str::trim) {
        Some("unsat") => Answer::Unsat,
        Some("sat") => Answer::Sat(parse_model(&lines.collect::<Vec<_>>().join(" "))),
        Some(other) => Answer::Unknown(other.trim_matches(|c| c == '(' || c == ')').to_string()),
        None => Answer::Unknown("no answer".into()),
    }
}

/// `((p_x #x…) (p_b true))` → `[("p_x", "#x…"), ("p_b", "true")]`.
fn parse_model(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let tokens: Vec<String> = text
        .replace('(', " ( ")
        .replace(')', " ) ")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut i = 0;
    while i + 3 < tokens.len() {
        if tokens[i] == "(" && tokens[i + 1] != "(" && tokens[i + 2] != "(" && tokens[i + 3] == ")" {
            out.push((tokens[i + 1].clone(), tokens[i + 2].clone()));
            i += 4;
        } else {
            i += 1;
        }
    }
    out
}

/// A model value as Coil writes it: `#xffffffffffffffff` is `-1`.
pub fn show_value(v: &str) -> String {
    if let Some(hex) = v.strip_prefix("#x")
        && let Ok(u) = u64::from_str_radix(hex, 16)
    {
        return (u as i64).to_string();
    }
    if let Some(bin) = v.strip_prefix("#b")
        && let Ok(u) = u64::from_str_radix(bin, 2)
    {
        return (u as i64).to_string();
    }
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sat_models_and_unsat() {
        assert_eq!(parse_answer("unsat\n"), Answer::Unsat);
        let a = parse_answer("sat\n((p_x #xffffffffffffffff)\n (p_b true))\n");
        assert_eq!(
            a,
            Answer::Sat(vec![("p_x".into(), "#xffffffffffffffff".into()), ("p_b".into(), "true".into())])
        );
        assert_eq!(show_value("#xffffffffffffffff"), "-1");
        assert_eq!(show_value("#x0000000000000003"), "3");
        assert_eq!(parse_answer("unknown\n"), Answer::Unknown("unknown".into()));
    }
}
