//! Desugar `store Name { ... }` declarative singletons.
//!
//! `store Foo { state x : T = init; fn ... }` rewrites to:
//!
//!   class Foo < QObject { pub prop x : T, notify: :xChanged, default: init
//!                         pub signal xChanged
//!                         pub fn ... }
//!   let Foo : Foo = Foo.new()
//!
//! The synthesized `let` is the same surface the user could have
//! written by hand; the existing `Q_GLOBAL_STATIC` post-pass at
//! `cpp.rs:624` lifts it to a process-lifetime singleton, and the
//! Ident-rewrite that makes `X.member` work for any QObject-typed
//! top-level let resolves bare `Foo.x` references through the
//! generated `Foo()` accessor. No codegen plumbing beyond this
//! desugar is needed.
//!
//! Runs before `desugar_widget_state` and before name mangling.

use cute_syntax::ast::*;
use cute_syntax::span::Span;

pub fn desugar_store(mut module: Module) -> Module {
    if !module.items.iter().any(|i| matches!(i, Item::Store(_))) {
        return module;
    }
    let mut new_items: Vec<Item> = Vec::with_capacity(module.items.len() + 4);
    for item in std::mem::take(&mut module.items) {
        match item {
            Item::Store(s) => {
                let span = s.span;
                let is_pub = s.is_pub;
                let name = s.name.clone();
                new_items.push(Item::Class(synthesize_store_class(s)));
                new_items.push(Item::Let(synthesize_singleton_let(name, span, is_pub)));
            }
            other => new_items.push(other),
        }
    }
    module.items = new_items;
    module
}

fn synthesize_store_class(s: StoreDecl) -> ClassDecl {
    let span = s.span;
    let qobject_super = TypeExpr {
        kind: TypeKind::Named {
            path: vec![Ident {
                name: "QObject".into(),
                span,
            }],
            args: Vec::new(),
        },
        span,
    };
    let mut members: Vec<ClassMember> =
        Vec::with_capacity(s.state_fields.len() * 2 + s.members.len());
    // `state X : T = init` becomes a (prop, signal) pair so QML / widget
    // bindings on `Store.X` re-render when X mutates.
    for sf in s.state_fields {
        let StateFieldKind::Property { ty } = sf.kind else {
            continue;
        };
        let notify = Ident {
            name: synth_notify_name(&sf.name.name),
            span: sf.span,
        };
        members.push(ClassMember::Property(PropertyDecl {
            name: sf.name,
            ty,
            notify: Some(notify.clone()),
            default: Some(sf.init_expr),
            is_pub: true,
            bindable: false,
            binding: None,
            fresh: None,
            model: false,
            constant: false,
            span: sf.span,
            block_id: None,
        }));
        members.push(ClassMember::Signal(SignalDecl {
            name: notify,
            params: Vec::new(),
            is_pub: true,
            span: sf.span,
        }));
    }
    for mut m in s.members {
        m.set_pub(true);
        members.push(m);
    }
    ClassDecl {
        name: s.name,
        generics: Vec::new(),
        super_class: Some(qobject_super),
        members,
        is_pub: s.is_pub,
        is_extern_value: false,
        is_arc: false,
        is_copyable: true,
        span,
    }
}

fn synthesize_singleton_let(name: Ident, span: Span, is_pub: bool) -> LetDecl {
    let ty = TypeExpr {
        kind: TypeKind::Named {
            path: vec![name.clone()],
            args: Vec::new(),
        },
        span,
    };
    // `Foo.new()` — the post-pass at cpp.rs:624 matches this exact
    // MethodCall shape; bare `Foo()` (Ident-as-callable) wouldn't.
    let value = Expr {
        kind: ExprKind::MethodCall {
            receiver: Box::new(Expr {
                kind: ExprKind::Ident(name.name.clone()),
                span,
            }),
            method: Ident {
                name: "new".into(),
                span,
            },
            args: Vec::new(),
            block: None,
            type_args: Vec::new(),
        },
        span,
    };
    LetDecl {
        name,
        ty,
        value,
        is_pub,
        span,
    }
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
    fn empty_store_lowers_to_qobject_class_plus_singleton_let() {
        let m = desugar_store(parse_str("store Empty {}"));
        assert_eq!(m.items.len(), 2);
        match (&m.items[0], &m.items[1]) {
            (Item::Class(c), Item::Let(l)) => {
                assert_eq!(c.name.name, "Empty");
                assert!(c.members.is_empty());
                assert!(matches!(
                    &c.super_class,
                    Some(TypeExpr { kind: TypeKind::Named { path, .. }, .. })
                        if path.last().map(|i| i.name.as_str()) == Some("QObject"),
                ));
                assert_eq!(l.name.name, "Empty");
                assert!(matches!(
                    &l.value.kind,
                    ExprKind::MethodCall { method, args, .. }
                        if method.name == "new" && args.is_empty(),
                ));
            }
            other => panic!("expected (Class, Let), got {other:?}"),
        }
    }

    #[test]
    fn state_field_lowers_to_prop_plus_signal_pair() {
        let m = desugar_store(parse_str("store Counter { state value : Int = 0 }"));
        let Item::Class(c) = &m.items[0] else {
            panic!("expected Class");
        };
        assert_eq!(c.members.len(), 2, "state field → prop + signal pair");
        let ClassMember::Property(p) = &c.members[0] else {
            panic!("expected Property first");
        };
        assert_eq!(p.name.name, "value");
        assert!(p.is_pub);
        assert!(matches!(
            p.notify.as_ref().map(|i| i.name.as_str()),
            Some("valueChanged")
        ));
        assert!(p.default.is_some());
        let ClassMember::Signal(sig) = &c.members[1] else {
            panic!("expected Signal second");
        };
        assert_eq!(sig.name.name, "valueChanged");
        assert!(sig.is_pub);
    }

    #[test]
    fn user_methods_become_pub() {
        let m = desugar_store(parse_str(
            "store Counter { state value : Int = 0\nfn bump { value = value + 1 } }",
        ));
        let Item::Class(c) = &m.items[0] else {
            panic!("expected Class");
        };
        let ClassMember::Fn(f) = &c.members[2] else {
            panic!("expected fn third, got {:?}", &c.members[2]);
        };
        assert_eq!(f.name.name, "bump");
        assert!(f.is_pub, "store members are forced `pub`");
    }

    #[test]
    fn pub_propagates_to_class_and_let() {
        let m = desugar_store(parse_str("pub store Hub {}"));
        let (Item::Class(c), Item::Let(l)) = (&m.items[0], &m.items[1]) else {
            panic!("expected (Class, Let)");
        };
        assert!(c.is_pub);
        assert!(l.is_pub);
    }

    #[test]
    fn module_without_store_passes_through() {
        let m = desugar_store(parse_str("fn main { }"));
        assert_eq!(m.items.len(), 1);
        assert!(matches!(&m.items[0], Item::Fn(_)));
    }
}
