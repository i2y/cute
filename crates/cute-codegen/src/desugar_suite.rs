//! Flatten `suite "X" { test "y" { body } ... }` into sibling
//! `Item::Fn` entries carrying `display_name: Some("X / y")`. The
//! `Item::Suite` is dropped after flattening — `cute fmt` never sees
//! the desugared AST (it parses fresh), so there's no fmt consumer
//! that needs the shell preserved.
//!
//! After this pass HIR / type-check / codegen handle the synth test
//! fns the same way they handle the compact `test fn camelCase`
//! form; runner emission picks `display_name` over the synth
//! identifier for TAP output.
//!
//! Runs before `desugar_store` and `desugar_widget_state`.

use cute_syntax::ast::*;

pub fn desugar_suite(mut module: Module) -> Module {
    if !module.items.iter().any(|i| matches!(i, Item::Suite(_))) {
        return module;
    }
    let mut new_items: Vec<Item> = Vec::with_capacity(module.items.len() + 4);
    for item in std::mem::take(&mut module.items) {
        match item {
            Item::Suite(s) => {
                for t in s.tests {
                    new_items.push(Item::Fn(t));
                }
            }
            other => new_items.push(other),
        }
    }
    module.items = new_items;
    module
}

#[cfg(test)]
mod tests {
    use super::*;
    use cute_syntax::parse;
    use cute_syntax::span::FileId;

    fn parse_str(src: &str) -> Module {
        parse(FileId(0), src).expect("parse")
    }

    #[test]
    fn suite_flattens_to_sibling_fns_with_display_names() {
        let src = r#"
suite "compute" {
  test "adds positive numbers" { let a = 1 }
  test "handles zero" { let b = 0 }
}
"#;
        let m = desugar_suite(parse_str(src));
        assert_eq!(
            m.items.len(),
            2,
            "Item::Suite is dropped, tests become siblings"
        );
        let Item::Fn(f1) = &m.items[0] else {
            panic!("expected Fn first");
        };
        assert!(f1.is_test);
        assert_eq!(
            f1.display_name.as_deref(),
            Some("compute / adds positive numbers"),
        );
        let Item::Fn(f2) = &m.items[1] else {
            panic!("expected Fn second");
        };
        assert!(f2.is_test);
        assert_eq!(f2.display_name.as_deref(), Some("compute / handles zero"),);
    }

    #[test]
    fn top_level_string_named_test_passes_through_unchanged() {
        // Top-level `test "y" { body }` parses directly to Item::Fn
        // (with `display_name = Some("y")`) — desugar_suite leaves
        // it alone since no Item::Suite wraps it.
        let src = r#"test "standalone" { let x = 1 }"#;
        let m = desugar_suite(parse_str(src));
        assert_eq!(m.items.len(), 1);
        let Item::Fn(f) = &m.items[0] else {
            panic!("expected Fn");
        };
        assert!(f.is_test);
        assert_eq!(f.display_name.as_deref(), Some("standalone"));
    }

    #[test]
    fn modules_without_suites_are_untouched() {
        let src = r#"fn main { let x = 1 }"#;
        let m = desugar_suite(parse_str(src));
        assert_eq!(m.items.len(), 1);
        assert!(matches!(&m.items[0], Item::Fn(_)));
    }
}
