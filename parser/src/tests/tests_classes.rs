    use super::*;

    #[test]
    fn parse_classes_example() {
        let src = include_str!("../../../examples/classes.hy");
        let p = Pratt::default();
        p.parse(src).unwrap_or_else(|e| panic!("PARSE FAIL: {e:?}"));
    }

    /// `impl` methods are space/newline-separated (no commas between methods).
    #[test]
    fn parse_impl_methods_without_commas() {
        let src = r#"
class Point { x: int, y: int, }
impl Point {
    fn sum() -> int { return self.x + self.y; }
    fn set_x(int n) { self.x = n; }
}
fn main() { let p = new Point(1, 2); }
"#;
        let p = Pratt::default();
        p.parse(src)
            .unwrap_or_else(|e| panic!("expected space-separated impl methods: {e:?}"));
    }

    /// E3 writes `#[attr] pub fn`; the parser must accept pub after attributes.
    #[test]
    fn parse_attr_then_pub_method() {
        let src = r#"
class Counter { n: int }
impl Counter {
    #[log(message = "bump")]
    pub fn bump() -> int { return self.n; }
    pub #[log(message = "bump2")]
    fn bump2() -> int { return self.n; }
    pub fn bump3() -> int { return self.n; }
}
fn main() {}
"#;
        let p = Pratt::default();
        p.parse(src)
            .unwrap_or_else(|e| panic!("expected #[attr] pub fn and pub #[attr] fn: {e:?}"));
    }
