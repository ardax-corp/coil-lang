    use super::*;
    use parser::Pratt;

    fn expand_src(src: &str) -> (ExpandResult, Vec<Output<'_>>) {
        let mut ast = Pratt::default().parse(src).expect("parse");
        let expand = expand_program(&mut ast);
        let Expression::Program(children) = ast.1.as_ref() else {
            panic!("expected program");
        };
        (expand, children.clone())
    }

    fn impl_method_names(decls: &[Output<'_>], class: &str) -> Vec<String> {
        let mut names = Vec::new();
        for node in decls {
            if let Expression::TypeClassImpl {
                class: c, methods, ..
            } = node.1.as_ref()
            {
                if *c != class {
                    continue;
                }
                for m in methods {
                    if let Expression::Method(_, f) = m.1.as_ref()
                        && let Expression::Function { name, .. } = f.1.as_ref() {
                            names.push((*name).to_string());
                        }
                }
            }
        }
        names
    }

    #[test]
    fn derive_deserialize_emits_deserialize_method() {
        let (_exp, decls) = expand_src("#[derive(Deserialize)] enum E { A, B(int) } fn main() {}");
        assert!(
            impl_method_names(&decls, "Deserialize").contains(&"deserialize".to_string()),
            "expected Deserialize::deserialize impl"
        );
    }

    #[test]
    fn derive_serialize_class_emits_serialize_method() {
        let (_exp, decls) = expand_src("#[derive(Serialize)] class P { pub x: int } fn main() {}");
        assert!(
            impl_method_names(&decls, "Serialize").contains(&"serialize".to_string()),
            "expected Serialize::serialize on class"
        );
    }

    #[test]
    fn default_show_string_use_type_name_when_no_derive() {
        let (_exp, decls) = expand_src("class Point { pub x: int, pub y: int } fn main() {}");
        assert!(
            impl_method_names(&decls, "Show").contains(&"show".to_string()),
            "expected default Show::show"
        );
        assert!(
            impl_method_names(&decls, "String").contains(&"to_string".to_string()),
            "expected default String::to_string"
        );
        let show_dbg = decls
            .iter()
            .find(|n| {
                matches!(
                    n.1.as_ref(),
                    Expression::TypeClassImpl { class, .. } if *class == "Show"
                )
            })
            .map(|n| format!("{:?}", n.1))
            .unwrap_or_default();
        assert!(
            show_dbg.contains("String(\"Point\")") || show_dbg.contains("Point"),
            "default Show should return type name string, got: {show_dbg}"
        );
    }

    #[test]
    fn max_depth_attr_rejected_on_enum() {
        let (exp, _decls) = expand_src("#[max_depth(8)] enum E { A } fn main() {}");
        assert!(
            exp.messages
                .iter()
                .any(|m| m.message().contains("max_depth") && m.message().contains("not valid")),
            "expected max_depth-on-enum error, got: {:?}",
            exp.messages
        );
    }
