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
                    if let Expression::Method(_, f) = m.1.as_ref() {
                        if let Expression::Function { name, .. } = f.1.as_ref() {
                            names.push((*name).to_string());
                        }
                    }
                }
            }
        }
        names
    }

    #[test]
    fn derive_default_enum_emits_default_method() {
        let (_exp, decls) = expand_src("#[derive(Default)] enum E { A, B(int) } fn main() {}");
        assert!(
            impl_method_names(&decls, "Default").contains(&"default".to_string()),
            "expected Default::default impl"
        );
    }

    #[test]
    fn derive_hash_class_emits_hash_method() {
        let (_exp, decls) = expand_src("#[derive(Hash)] class P { pub x: int, pub y: int } fn main() {}");
        assert!(
            impl_method_names(&decls, "Hash").contains(&"hash".to_string()),
            "expected Hash::hash impl"
        );
    }

    #[test]
    fn derive_hash_emits_recursive_field_hash_calls() {
        let (_exp, decls) =
            expand_src("#[derive(Hash)] enum E { A(int), B { s: string } } fn main() {}");
        let hash_impl = decls.iter().find(|n| {
            matches!(
                n.1.as_ref(),
                Expression::TypeClassImpl { class, .. } if *class == "Hash"
            )
        });
        assert!(hash_impl.is_some(), "expected Hash impl");
        let dump = format!("{:?}", hash_impl.unwrap().1);
        assert!(
            dump.contains("\"hash\"") && dump.contains("Call"),
            "expected recursive field.hash() Call in derived Hash body, got: {dump}"
        );
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
    fn derive_send_emits_marker_impl() {
        let (_exp, decls) = expand_src("#[derive(Send)] enum E { A } fn main() {}");
        assert!(
            decls.iter().any(|n| matches!(
                n.1.as_ref(),
                Expression::TypeClassImpl { class, args, methods }
                    if *class == "Send" && args.len() == 1 && methods.is_empty()
            )),
            "expected empty Send instance"
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
    fn derive_string_enum_emits_to_string_method() {
        let (_exp, decls) = expand_src("#[derive(String)] enum E { A, B(int) } fn main() {}");
        assert!(
            impl_method_names(&decls, "String").contains(&"to_string".to_string()),
            "expected String::to_string impl"
        );
    }

    #[test]
    fn derive_sensitive_emits_marker_impl() {
        let (_exp, decls) = expand_src("#[derive(Sensitive)] class P { pub x: int } fn main() {}");
        assert!(
            decls.iter().any(|n| matches!(
                n.1.as_ref(),
                Expression::TypeClassImpl { class, args, methods }
                    if *class == "Sensitive" && args.len() == 1 && methods.is_empty()
            )),
            "expected empty Sensitive instance"
        );
    }

    #[test]
    fn derive_and_repr_compose_on_scalar_enum() {
        let (exp, decls) = expand_src(
            "#[repr(int)] #[derive(Show, Eq, Ord, Hash)] enum Status { Ok = 200, NotFound = 404 } fn main() {}",
        );
        assert!(
            exp.messages.is_empty(),
            "repr+derive should expand, got: {:?}",
            exp.messages
        );
        assert!(
            impl_method_names(&decls, "Show").contains(&"show".to_string()),
            "expected Show::show"
        );
        assert!(
            impl_method_names(&decls, "Eq").contains(&"eq".to_string()),
            "expected Eq::eq"
        );
        assert!(
            impl_method_names(&decls, "Hash").contains(&"hash".to_string()),
            "expected Hash::hash"
        );
        assert!(
            decls.iter().any(|n| matches!(
                n.1.as_ref(),
                Expression::TypeClassImpl { class, .. } if *class == "Lt"
            )),
            "expected Ord/Lt instance"
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
            show_dbg.contains("Status.Ok") || show_dbg.contains("Status.Ok"),
            "scalar Show should print Status.Ok, got: {show_dbg}"
        );
        assert!(
            !show_dbg.contains("Status::Ok"),
            "scalar Show should not use ::, got: {show_dbg}"
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
