//! E0148: a local `Stream` a body opens and never closes or hands on.

use compiler::Pipeline;
use reporting::{ErrorCode, MessageKind};

/// `(message, help)` of each E0148 warning in `src`.
fn notes(src: &str) -> Vec<(String, String)> {
    let mut pipeline = Pipeline::new();
    pipeline.grant_all();
    pipeline.compile_src(src).expect("compile");
    pipeline
        .messages()
        .iter()
        .filter(|m| m.code() == Some(ErrorCode::UnclosedStream))
        .inspect(|m| assert!(matches!(m.kind(), MessageKind::WARNING), "{m:?}"))
        .map(|m| (m.message().to_string(), m.help().clone().unwrap_or_default()))
        .collect()
}

fn program(body: &str) -> String {
    format!(
        "
use io::Stream;
use io::IoError;
use io::open;
use io::close;
use io::read;
use io::stdout;
use io::net::tcp::listen;
use io::net::tcp::local_addr;

fn keep(Stream s) -> int {{
    return 1;
}}

fn f(string p) -> Result<int, IoError> {{
{body}
}}

fn main() {{
    let _ = f(\"/etc/hostname\");
}}
"
    )
}

#[test]
fn a_stream_only_read_warns() {
    let src = program(
        "    let s = open(p, \"r\")?;
    let buf: Vec<byte> = [];
    let _ = read(s, buf)?;
    return Ok(0);",
    );
    let n = notes(&src);
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(n[0].0.contains("`s` is opened here and never closed"), "{n:?}");
    assert!(n[0].1.contains("close(s)"), "{n:?}");
}

#[test]
fn a_listener_only_queried_warns() {
    let src = program(
        "    let srv = listen(\"127.0.0.1\", 0)?;
    let _ = local_addr(srv)?;
    return Ok(0);",
    );
    assert_eq!(notes(&src).len(), 1);
}

#[test]
fn close_quiets_it() {
    let src = program(
        "    let s = open(p, \"r\")?;
    close(s)?;
    return Ok(0);",
    );
    assert!(notes(&src).is_empty());
}

#[test]
fn a_defer_close_quiets_it() {
    let src = program(
        "    let s = open(p, \"r\")?;
    defer use (s) {
        let _ = close(s);
    }
    let buf: Vec<byte> = [];
    let _ = read(s, buf)?;
    return Ok(0);",
    );
    assert!(notes(&src).is_empty());
}

#[test]
fn a_hand_off_quiets_it() {
    for tail in ["return Ok(keep(s));", "let t = s;\n    return Ok(keep(t));", "let v = [s];\n    return Ok(v.len());"] {
        let src = program(&format!("    let s = open(p, \"r\")?;\n    {tail}"));
        assert!(notes(&src).is_empty(), "{tail}");
    }
}

#[test]
fn a_lambda_capture_quiets_it() {
    let src = program(
        "    let s = open(p, \"r\")?;
    let g = fn () use (s) => keep(s);
    return Ok(g());",
    );
    assert!(notes(&src).is_empty());
}

#[test]
fn a_standard_stream_does_not_warn() {
    let src = program(
        "    let o = stdout();
    let _ = o.fd()?;
    return Ok(0);",
    );
    assert!(notes(&src).is_empty());
}

#[test]
fn an_unused_open_in_an_underscore_does_not_warn() {
    let src = program(
        "    let _s = open(p, \"r\")?;
    return Ok(0);",
    );
    assert!(notes(&src).is_empty());
}
