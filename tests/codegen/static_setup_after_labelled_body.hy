// The prologue's jump to static setup keeps its own label when bodies are
// flattened. A body emitted from MIR mints labels from a private space that
// can reuse that id; the setup jump must still land on the setup code.
use tests::assert;

static let k: int = 7;

fn chk(int j) -> Result<(), string> {
    assert!(j == 7);
    assert!(j == 7);
    assert!(j == 7);
    assert!(j == 7);
    assert!(j == 7);
    assert!(j == 7);
    assert!(j == 7);
    assert!(j == 7);
    return Result::Ok(());
}

test("static setup runs before a many-label body") {
    assert!(k == 7);
    chk(k)?;
}
