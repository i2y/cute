//! Style env + element desugar.
//!
//! `style X { padding: 16, ... }` is a top-level item that produces no
//! runtime artifact - it lives only to be merged into element bodies at
//! codegen time. This module owns the resolution: (1) build a project-
//! scoped table from every `Item::Style` in the module, flattening
//! aliases (`style BigCard = Card + Big`) with right-wins merge
//! semantics; (2) walk every `view` / `widget` element tree replacing
//! `ElementMember::Property { key: "style", value }` with the resolved
//! flat list of `(key, value)` properties.
//!
//! After this pass, the rest of codegen sees only ordinary properties -
//! both the QML emitter and the QtWidgets emitter stay style-unaware.
//! The `+` operator and `Card`-style references vanish from the AST.
//!
//! Cycle detection: `style A = A` (or any `A -> B -> A` chain) is an
//! error reported with a clear message. Unknown style references in a
//! `style:` element member fall through to the original property as-is
//! - codegen will then either accept the value (e.g. it's a literal
//!   map) or emit a noisy line that points the user at the typo.
//!
//! Inline merges at the element site (`Label { style: A + B }`) reduce
//! against the same env without going through the alias table, so they
//! work in the obvious way without any extra plumbing.

use std::collections::{HashMap, HashSet};

use cute_syntax::ast::{
    BinOp, Element, ElementMember, Expr, ExprKind, Item, Module, Stmt, StyleBody, StyleDecl,
    StyleEntry, ViewDecl, WidgetDecl,
};

use crate::cpp::EmitError;

/// Resolved per-project style table. Each entry name maps to its
/// flattened (key, value) list - alias chains and `+` merges are
/// already collapsed.
pub struct StyleEnv {
    pub resolved: HashMap<String, Vec<(String, Expr)>>,
}

impl StyleEnv {
    /// Build the env from every `Item::Style` in a module. Errors on
    /// reference cycles in alias chains. Duplicate top-level style
    /// names are detected at the driver layer (the project-wide
    /// collision check); if two slip through here, the later one wins
    /// silently rather than re-erroring.
    pub fn build(module: &Module) -> Result<Self, EmitError> {
        let mut raw: HashMap<String, &StyleDecl> = HashMap::new();
        for item in &module.items {
            if let Item::Style(s) = item {
                raw.insert(s.name.name.clone(), s);
            }
        }
        let mut resolved: HashMap<String, Vec<(String, Expr)>> = HashMap::new();
        for name in raw.keys().cloned().collect::<Vec<_>>() {
            if !resolved.contains_key(&name) {
                let mut visiting: HashSet<String> = HashSet::new();
                resolve_named_style(&name, &raw, &mut resolved, &mut visiting)?;
            }
        }
        Ok(StyleEnv { resolved })
    }

    /// Reduce a `style: <expr>` value to a flat (key, value) list.
    /// `K::Ident` looks up by name; `K::Binary { Add }` recursively
    /// merges with right-wins. Anything else returns `None`, leaving
    /// the original property intact for downstream emit.
    pub fn resolve_expr(&self, e: &Expr) -> Option<Vec<(String, Expr)>> {
        match &e.kind {
            ExprKind::Ident(name) => self.resolved.get(name).cloned(),
            ExprKind::Binary {
                op: BinOp::Add,
                lhs,
                rhs,
            } => {
                let l = self.resolve_expr(lhs)?;
                let r = self.resolve_expr(rhs)?;
                Some(merge_entries(l, r))
            }
            _ => None,
        }
    }
}

/// Right-wins merge. Keys present in `rhs` override keys in `lhs`; the
/// resulting order keeps lhs entries (minus overridden ones) followed
/// by rhs entries in their original order.
fn merge_entries(lhs: Vec<(String, Expr)>, rhs: Vec<(String, Expr)>) -> Vec<(String, Expr)> {
    let rhs_keys: HashSet<&str> = rhs.iter().map(|(k, _)| k.as_str()).collect();
    let mut out: Vec<(String, Expr)> = lhs
        .into_iter()
        .filter(|(k, _)| !rhs_keys.contains(k.as_str()))
        .collect();
    out.extend(rhs);
    out
}

fn resolve_named_style(
    name: &str,
    raw: &HashMap<String, &StyleDecl>,
    resolved: &mut HashMap<String, Vec<(String, Expr)>>,
    visiting: &mut HashSet<String>,
) -> Result<Vec<(String, Expr)>, EmitError> {
    if let Some(r) = resolved.get(name) {
        return Ok(r.clone());
    }
    if !visiting.insert(name.to_string()) {
        return Err(EmitError::StyleCycle(name.to_string()));
    }
    let entries = match raw.get(name) {
        Some(decl) => match &decl.body {
            StyleBody::Lit(es) => entries_from_literal(es),
            StyleBody::Alias(rhs) => resolve_style_expr(rhs, raw, resolved, visiting)?,
        },
        // Reference to an undeclared style name. Surface as an error so
        // typos in alias chains don't silently produce empty styles.
        None => return Err(EmitError::UnknownStyle(name.to_string())),
    };
    visiting.remove(name);
    resolved.insert(name.to_string(), entries.clone());
    Ok(entries)
}

fn resolve_style_expr(
    e: &Expr,
    raw: &HashMap<String, &StyleDecl>,
    resolved: &mut HashMap<String, Vec<(String, Expr)>>,
    visiting: &mut HashSet<String>,
) -> Result<Vec<(String, Expr)>, EmitError> {
    match &e.kind {
        ExprKind::Ident(name) => resolve_named_style(name, raw, resolved, visiting),
        ExprKind::Binary {
            op: BinOp::Add,
            lhs,
            rhs,
        } => {
            let l = resolve_style_expr(lhs, raw, resolved, visiting)?;
            let r = resolve_style_expr(rhs, raw, resolved, visiting)?;
            Ok(merge_entries(l, r))
        }
        _ => Err(EmitError::UnsupportedStyleExpr),
    }
}

fn entries_from_literal(entries: &[StyleEntry]) -> Vec<(String, Expr)> {
    entries
        .iter()
        .map(|e| (e.key.clone(), e.value.clone()))
        .collect()
}

/// Rewrite a module so every `style: <expr>` element member is
/// replaced by the inline (key, value) properties it resolves to. The
/// rest of codegen never sees `Item::Style` references - by the time
/// emit runs, styles look like ordinary inline property lists.
pub fn desugar_module(module: &Module) -> Result<Module, EmitError> {
    let env = StyleEnv::build(module)?;
    let mut new_items = Vec::with_capacity(module.items.len());
    for item in &module.items {
        match item {
            Item::View(v) => new_items.push(Item::View(ViewDecl {
                root: desugar_element(&v.root, &env),
                ..v.clone()
            })),
            Item::Widget(w) => new_items.push(Item::Widget(WidgetDecl {
                root: desugar_element(&w.root, &env),
                ..w.clone()
            })),
            other => new_items.push(other.clone()),
        }
    }
    Ok(Module {
        items: new_items,
        span: module.span,
    })
}

fn desugar_element(e: &Element, env: &StyleEnv) -> Element {
    let mut new_members: Vec<ElementMember> = Vec::with_capacity(e.members.len());
    for m in &e.members {
        match m {
            ElementMember::Property { key, value, span } if key == "style" => {
                if let Some(entries) = env.resolve_expr(value) {
                    for (k, v) in entries {
                        new_members.push(ElementMember::Property {
                            key: k,
                            value: v,
                            span: *span,
                        });
                    }
                } else {
                    // Unresolved (e.g. literal value, dynamic
                    // expression). Fall through unchanged so codegen
                    // can handle it / produce a meaningful error.
                    new_members.push(m.clone());
                }
            }
            ElementMember::Property { key, value, span } => {
                // Property whose value may itself contain a nested
                // element (e.g. `delegate: RowLayout { Rectangle {
                // style: Bubble } }` on a ListView, or
                // `background: Rectangle { ... }` on a Button).
                // Recurse through the expression so any inner
                // `style:` references get spliced.
                new_members.push(ElementMember::Property {
                    key: key.clone(),
                    value: desugar_expr(value, env),
                    span: *span,
                });
            }
            ElementMember::Child(c) => {
                new_members.push(ElementMember::Child(desugar_element(c, env)));
            }
            ElementMember::Stmt(s) => {
                new_members.push(ElementMember::Stmt(desugar_stmt(s, env)));
            }
        }
    }
    Element {
        module_path: e.module_path.clone(),
        name: e.name.clone(),
        members: new_members,
        span: e.span,
    }
}

fn desugar_stmt(s: &Stmt, env: &StyleEnv) -> Stmt {
    match s {
        Stmt::Expr(e) => Stmt::Expr(desugar_expr(e, env)),
        Stmt::For {
            binding,
            iter,
            body,
            span,
        } => Stmt::For {
            binding: binding.clone(),
            iter: iter.clone(),
            body: cute_syntax::ast::Block {
                stmts: body.stmts.iter().map(|s| desugar_stmt(s, env)).collect(),
                trailing: body
                    .trailing
                    .as_ref()
                    .map(|e| Box::new(desugar_expr(e, env))),
                span: body.span,
            },
            span: *span,
        },
        // Other statement forms cannot legally hold an Element-position
        // body in current Cute, so nothing inside them needs rewriting.
        other => other.clone(),
    }
}

fn desugar_expr(e: &Expr, env: &StyleEnv) -> Expr {
    match &e.kind {
        ExprKind::Element(el) => Expr {
            kind: ExprKind::Element(desugar_element(el, env)),
            span: e.span,
        },
        ExprKind::If {
            cond,
            then_b,
            else_b,
            let_binding,
        } => Expr {
            kind: ExprKind::If {
                cond: cond.clone(),
                then_b: desugar_block(then_b, env),
                else_b: else_b.as_ref().map(|b| desugar_block(b, env)),
                let_binding: let_binding.clone(),
            },
            span: e.span,
        },
        ExprKind::Case { scrutinee, arms } => Expr {
            kind: ExprKind::Case {
                scrutinee: scrutinee.clone(),
                arms: arms
                    .iter()
                    .map(|arm| cute_syntax::ast::CaseArm {
                        pattern: arm.pattern.clone(),
                        body: desugar_block(&arm.body, env),
                        span: arm.span,
                    })
                    .collect(),
            },
            span: e.span,
        },
        _ => e.clone(),
    }
}

fn desugar_block(b: &cute_syntax::ast::Block, env: &StyleEnv) -> cute_syntax::ast::Block {
    cute_syntax::ast::Block {
        stmts: b.stmts.iter().map(|s| desugar_stmt(s, env)).collect(),
        trailing: b.trailing.as_ref().map(|e| Box::new(desugar_expr(e, env))),
        span: b.span,
    }
}
