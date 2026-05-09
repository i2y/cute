//! Desugar widget-side `state X : T = init` declarations into a
//! synthesized state-holder QObject class plus a hidden Object-kind
//! state field.
//!
//! For QML views, `state` lowers directly to a root-level QML
//! `property` (handled in `cpp.rs::emit_view`) and the desugaring is
//! a no-op. For QtWidgets, however, the surrounding widget class is a
//! plain `QWidget` subclass with no Q_PROPERTY machinery — bare
//! references inside the body have nowhere to land. This pass moves
//! every `state X : T = init` field on a `widget Foo { ... }` into a
//! synthesized class:
//!
//! ```text
//! class __FooState < QObject {
//!   pub prop X : T, notify: :XChanged, default: <init>
//!   pub signal XChanged
//! }
//! widget Foo {
//!   let __cute_state = __FooState()
//!   // bare `X` reads → __cute_state.X
//!   // `X = expr` writes → __cute_state.X = expr
//!   ...
//! }
//! ```
//!
//! Once applied, downstream HIR / type-check / codegen passes see only
//! the existing state-field-on-widget machinery (id-tagged QObject
//! child + the reactive-binding wiring at
//! `WidgetEmitter::collect_reactive_deps`). No new emit paths are
//! needed in widget-side codegen.
//!
//! Views are intentionally skipped: QML's own `property` declaration
//! is the natural target there and adding a holder class would just
//! double-bag the same reactivity.

use cute_syntax::ast::*;
use cute_syntax::span::Span;
use std::collections::HashSet;

/// Run the desugaring once on the user module before any AST→AST
/// rewrite passes (mangler etc.) — synthesized classes need to flow
/// through name mangling and HIR / type-check the same as user
/// classes.
pub fn desugar_widget_state(mut module: Module) -> Module {
    let mut new_items: Vec<Item> = Vec::with_capacity(module.items.len());
    for item in std::mem::take(&mut module.items) {
        match item {
            Item::Widget(w) => {
                let prop_fields: Vec<&StateField> = w
                    .state_fields
                    .iter()
                    .filter(|sf| matches!(sf.kind, StateFieldKind::Property { .. }))
                    .collect();
                if prop_fields.is_empty() {
                    new_items.push(Item::Widget(w));
                    continue;
                }
                let holder_name = format!("__{}State", w.name.name);
                let prop_names: HashSet<String> =
                    prop_fields.iter().map(|p| p.name.name.clone()).collect();
                let synth_class = synthesize_holder_class(&holder_name, &prop_fields, w.span);
                new_items.push(Item::Class(synth_class));

                let new_state_fields = rewrite_widget_state_fields(&w, &holder_name);
                let new_root = rewrite_element(&w.root, &prop_names);
                new_items.push(Item::Widget(WidgetDecl {
                    name: w.name,
                    params: w.params,
                    state_fields: new_state_fields,
                    root: new_root,
                    is_pub: w.is_pub,
                    span: w.span,
                }));
            }
            other => new_items.push(other),
        }
    }
    module.items = new_items;
    module
}

fn synthesize_holder_class(name: &str, props: &[&StateField], span: Span) -> ClassDecl {
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
    let mut members: Vec<ClassMember> = Vec::with_capacity(props.len() * 2);
    for sf in props {
        let StateFieldKind::Property { ty } = &sf.kind else {
            continue;
        };
        let notify_name = synth_notify_name(&sf.name.name);
        let notify_ident = Ident {
            name: notify_name,
            span: sf.span,
        };
        members.push(ClassMember::Property(PropertyDecl {
            name: sf.name.clone(),
            ty: ty.clone(),
            notify: Some(notify_ident.clone()),
            default: Some(sf.init_expr.clone()),
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
            name: notify_ident,
            params: Vec::new(),
            is_pub: true,
            span: sf.span,
        }));
    }
    ClassDecl {
        name: Ident {
            name: name.into(),
            span,
        },
        generics: Vec::new(),
        super_class: Some(qobject_super),
        members,
        is_pub: false,
        is_extern_value: false,
        is_arc: false,
        is_copyable: true,
        span,
    }
}

/// Build the new state_fields list: keep `let`-form Object fields,
/// drop `state`-form Property fields, and prepend a synthesized
/// `let __cute_state = <holder>()` so the holder is initialized
/// before any user-declared sub-objects (which might depend on it).
fn rewrite_widget_state_fields(w: &WidgetDecl, holder_name: &str) -> Vec<StateField> {
    let mut out: Vec<StateField> = Vec::with_capacity(w.state_fields.len());
    let span = w.span;
    let init_expr = Expr {
        kind: ExprKind::Call {
            callee: Box::new(Expr {
                kind: ExprKind::Ident(holder_name.into()),
                span,
            }),
            args: Vec::new(),
            block: None,
            type_args: Vec::new(),
        },
        span,
    };
    out.push(StateField {
        name: Ident {
            name: "__cute_state".into(),
            span,
        },
        kind: StateFieldKind::Object,
        init_expr,
        span,
    });
    for sf in &w.state_fields {
        match &sf.kind {
            StateFieldKind::Property { .. } => continue,
            StateFieldKind::Object => out.push(sf.clone()),
        }
    }
    out
}

fn rewrite_element(e: &Element, props: &HashSet<String>) -> Element {
    Element {
        module_path: e.module_path.clone(),
        name: e.name.clone(),
        members: e
            .members
            .iter()
            .map(|m| rewrite_element_member(m, props))
            .collect(),
        span: e.span,
    }
}

fn rewrite_element_member(m: &ElementMember, props: &HashSet<String>) -> ElementMember {
    match m {
        ElementMember::Property { key, value, span } => ElementMember::Property {
            key: key.clone(),
            value: rewrite_expr(value, props),
            span: *span,
        },
        ElementMember::Child(c) => ElementMember::Child(rewrite_element(c, props)),
        ElementMember::Stmt(s) => ElementMember::Stmt(rewrite_stmt(s, props)),
    }
}

fn rewrite_stmt(s: &Stmt, props: &HashSet<String>) -> Stmt {
    match s {
        Stmt::Let {
            name,
            ty,
            value,
            span,
            block_id,
        } => Stmt::Let {
            name: name.clone(),
            ty: ty.clone(),
            value: rewrite_expr(value, props),
            span: *span,
            block_id: *block_id,
        },
        Stmt::Var {
            name,
            ty,
            value,
            span,
            block_id,
        } => Stmt::Var {
            name: name.clone(),
            ty: ty.clone(),
            value: rewrite_expr(value, props),
            span: *span,
            block_id: *block_id,
        },
        Stmt::Expr(e) => Stmt::Expr(rewrite_expr(e, props)),
        Stmt::Return { value, span } => Stmt::Return {
            value: value.as_ref().map(|v| rewrite_expr(v, props)),
            span: *span,
        },
        Stmt::Emit { signal, args, span } => Stmt::Emit {
            signal: signal.clone(),
            args: args.iter().map(|a| rewrite_expr(a, props)).collect(),
            span: *span,
        },
        Stmt::Assign {
            target,
            op,
            value,
            span,
        } => {
            // Bare `count = expr` becomes `__cute_state.count = expr`.
            // Compound targets (member access, indexing) recurse on
            // both sides so `obj.count = expr` continues to mean what
            // it always meant.
            let new_target = if let ExprKind::Ident(name) = &target.kind {
                if props.contains(name) {
                    Expr {
                        kind: ExprKind::Member {
                            receiver: Box::new(Expr {
                                kind: ExprKind::Ident("__cute_state".into()),
                                span: target.span,
                            }),
                            name: Ident {
                                name: name.clone(),
                                span: target.span,
                            },
                        },
                        span: target.span,
                    }
                } else {
                    target.clone()
                }
            } else {
                rewrite_expr(target, props)
            };
            Stmt::Assign {
                target: new_target,
                op: *op,
                value: rewrite_expr(value, props),
                span: *span,
            }
        }
        Stmt::For {
            binding,
            iter,
            body,
            span,
        } => Stmt::For {
            binding: binding.clone(),
            iter: rewrite_expr(iter, props),
            body: rewrite_block(body, props),
            span: *span,
        },
        Stmt::While { cond, body, span } => Stmt::While {
            cond: rewrite_expr(cond, props),
            body: rewrite_block(body, props),
            span: *span,
        },
        Stmt::Batch { body, span } => Stmt::Batch {
            body: rewrite_block(body, props),
            span: *span,
        },
        Stmt::Break { span } => Stmt::Break { span: *span },
        Stmt::Continue { span } => Stmt::Continue { span: *span },
    }
}

fn rewrite_block(b: &Block, props: &HashSet<String>) -> Block {
    Block {
        stmts: b.stmts.iter().map(|s| rewrite_stmt(s, props)).collect(),
        trailing: b
            .trailing
            .as_ref()
            .map(|e| Box::new(rewrite_expr(e, props))),
        span: b.span,
    }
}

fn rewrite_box_expr(b: &Expr, props: &HashSet<String>) -> Box<Expr> {
    Box::new(rewrite_expr(b, props))
}

fn rewrite_expr(e: &Expr, props: &HashSet<String>) -> Expr {
    use ExprKind as K;
    let new_kind = match &e.kind {
        K::Ident(name) if props.contains(name) => K::Member {
            receiver: Box::new(Expr {
                kind: K::Ident("__cute_state".into()),
                span: e.span,
            }),
            name: Ident {
                name: name.clone(),
                span: e.span,
            },
        },
        K::Str(parts) => {
            let new_parts = parts
                .iter()
                .map(|p| match p {
                    StrPart::Text(t) => StrPart::Text(t.clone()),
                    StrPart::Interp(inner) => StrPart::Interp(rewrite_box_expr(inner, props)),
                    StrPart::InterpFmt { expr, format_spec } => StrPart::InterpFmt {
                        expr: rewrite_box_expr(expr, props),
                        format_spec: format_spec.clone(),
                    },
                })
                .collect();
            K::Str(new_parts)
        }
        K::Call {
            callee,
            args,
            block,
            type_args,
        } => K::Call {
            callee: rewrite_box_expr(callee, props),
            args: args.iter().map(|a| rewrite_expr(a, props)).collect(),
            block: block.as_ref().map(|b| rewrite_box_expr(b, props)),
            type_args: type_args.clone(),
        },
        K::MethodCall {
            receiver,
            method,
            args,
            block,
            type_args,
        } => K::MethodCall {
            receiver: rewrite_box_expr(receiver, props),
            method: method.clone(),
            args: args.iter().map(|a| rewrite_expr(a, props)).collect(),
            block: block.as_ref().map(|b| rewrite_box_expr(b, props)),
            type_args: type_args.clone(),
        },
        K::Member { receiver, name } => K::Member {
            receiver: rewrite_box_expr(receiver, props),
            name: name.clone(),
        },
        K::SafeMember { receiver, name } => K::SafeMember {
            receiver: rewrite_box_expr(receiver, props),
            name: name.clone(),
        },
        K::SafeMethodCall {
            receiver,
            method,
            args,
            block,
            type_args,
        } => K::SafeMethodCall {
            receiver: rewrite_box_expr(receiver, props),
            method: method.clone(),
            args: args.iter().map(|a| rewrite_expr(a, props)).collect(),
            block: block.as_ref().map(|b| rewrite_box_expr(b, props)),
            type_args: type_args.clone(),
        },
        K::Index { receiver, index } => K::Index {
            receiver: rewrite_box_expr(receiver, props),
            index: rewrite_box_expr(index, props),
        },
        K::Binary { op, lhs, rhs } => K::Binary {
            op: *op,
            lhs: rewrite_box_expr(lhs, props),
            rhs: rewrite_box_expr(rhs, props),
        },
        K::Unary { op, expr } => K::Unary {
            op: *op,
            expr: rewrite_box_expr(expr, props),
        },
        K::If {
            cond,
            then_b,
            else_b,
            let_binding,
        } => K::If {
            cond: rewrite_box_expr(cond, props),
            then_b: rewrite_block(then_b, props),
            else_b: else_b.as_ref().map(|b| rewrite_block(b, props)),
            let_binding: let_binding.clone(),
        },
        K::Block(b) => K::Block(rewrite_block(b, props)),
        K::Lambda { params, body } => K::Lambda {
            params: params.clone(),
            body: rewrite_block(body, props),
        },
        K::Array(items) => K::Array(items.iter().map(|i| rewrite_expr(i, props)).collect()),
        K::Map(entries) => K::Map(
            entries
                .iter()
                .map(|(k, v)| (rewrite_expr(k, props), rewrite_expr(v, props)))
                .collect(),
        ),
        K::Try(inner) => K::Try(rewrite_box_expr(inner, props)),
        K::Await(inner) => K::Await(rewrite_box_expr(inner, props)),
        K::Element(elem) => K::Element(rewrite_element(elem, props)),
        K::Kwarg { key, value } => K::Kwarg {
            key: key.clone(),
            value: rewrite_box_expr(value, props),
        },
        K::Case { scrutinee, arms } => K::Case {
            scrutinee: rewrite_box_expr(scrutinee, props),
            arms: arms
                .iter()
                .map(|a| CaseArm {
                    pattern: a.pattern.clone(),
                    body: rewrite_block(&a.body, props),
                    span: a.span,
                })
                .collect(),
        },
        K::Range {
            start,
            end,
            inclusive,
        } => K::Range {
            start: rewrite_box_expr(start, props),
            end: rewrite_box_expr(end, props),
            inclusive: *inclusive,
        },
        // Leaves and forms that don't carry sub-expressions: clone as-is.
        other => other.clone(),
    };
    Expr {
        kind: new_kind,
        span: e.span,
    }
}
