//! Module-aware name mangling for cross-module class collisions.
//!
//! When two `.cute` modules both declare `pub class Counter`, the
//! flat-keyed C++ output we'd otherwise emit produces a duplicate
//! `class Counter { ... }` and the link fails. The namespacing
//! solution is to rewrite the AST before emit so each colliding
//! class becomes `<module>__<name>` and every reference to it is
//! updated in lockstep. Modules with unique simple names pass
//! through unchanged - bare names everywhere, identical to the
//! flat-keyed codegen output.
//!
//! Inputs:
//!   - The merged `ast::Module` (entry + bindings + sibling modules).
//!   - `ProjectInfo::module_for_file` so we can attribute each AST
//!     node back to its declaring module via its span's file id.
//!
//! Outputs:
//!   - A rewritten `ast::Module` ready to be fed to the unchanged
//!     emit pipeline.
//!
//! Rewrite rules:
//!   1. **Class declarations** in modules that share a simple name
//!      with another module are renamed to `<mod>__<simple>`.
//!   2. **Element heads with `module_path`** (e.g. `model.Counter
//!      { ... }`) drop the qualifier and use the mangled name as
//!      the head.
//!   3. **TypeExpr `Named { path: [..., simple] }`** - if the
//!      simple name is in the colliding set, resolve to a (module,
//!      name) pair: the path's penultimate segment if any,
//!      otherwise the surrounding declaration's module. Replace
//!      with the mangled name; clear the prefix.
//!   4. **Class super-types** are TypeExprs; same rule.
//!   5. Bindings are NEVER mangled (Qt stdlib classes keep their
//!      real Qt names; users can't redefine them anyway).
//!
//! Conservatism:
//!   - Only `class` items are mangled. `view` / `widget` / `style`
//!     / `fn` collisions across modules are still errored out at
//!     the driver layer.
//!   - Mangling kicks in only when a real collision exists. Demos
//!     with unique names emit identical C++ to the flat-keyed
//!     baseline, preserving snapshot tests.

use std::collections::{HashMap, HashSet};

use cute_hir::ProjectInfo;
use cute_syntax::ast::{
    Block, ClassDecl, ClassMember, Element, ElementMember, EnumDecl, Expr, ExprKind, FnDecl, Ident,
    Item, Module, Param, Pattern, Stmt, StrPart, StructDecl, StyleBody, StyleDecl, TypeExpr,
    TypeKind, ViewDecl, WidgetDecl,
};
use cute_syntax::span::FileId;

/// Build the emit-name table from a module without rewriting.
/// Useful when callers want to apply the same rewrite to multiple
/// modules (e.g. the bindings+user "combined" view that HIR sees,
/// plus the user-only view that codegen consumes).
pub fn build_emit_names(module: &Module, project: &ProjectInfo) -> EmitNames {
    EmitNames::build(module, project)
}

/// Apply a precomputed `EmitNames` rewrite to `module`. Always runs
/// even when there are no name collisions — `rewrite_element`
/// strips qualified element heads (`model.Counter { ... }` → bare
/// `Counter { ... }`) regardless of whether the simple name is
/// colliding. Without this, the QML emit would carry `model.X` into
/// the output, where QML's resolver treats `model` as a missing
/// namespace and fails to load the component.
///
/// (Earlier versions skipped the rewrite as a perf optimisation
/// when no collisions were present; that broke single-instance
/// qualified refs like `model.Counter { id: counter }`.)
pub fn apply_rewrite(module: &Module, names: &EmitNames, project: &ProjectInfo) -> Module {
    let rewriter = Rewriter { names, project };
    let new_items = module
        .items
        .iter()
        .map(|item| rewriter.rewrite_item(item))
        .collect();
    Module {
        items: new_items,
        span: module.span,
    }
}

/// Convenience: build names from a single module and apply them in
/// one shot. Codegen tests use this; the driver builds names
/// separately so it can apply the same rewrite to both the
/// combined-with-bindings view and the user-only view.
pub fn mangle_module(module: &Module, project: &ProjectInfo) -> (Module, EmitNames) {
    let names = build_emit_names(module, project);
    let rewritten = apply_rewrite(module, &names, project);
    (rewritten, names)
}

/// Map from `(module, simple_name)` to the C++ identifier used in
/// emitted code. Bare for unique names, `<module>__<simple>` when a
/// collision exists. The codegen layer also queries this when
/// registering QML types so the QML-side name matches the C++-side
/// name.
#[derive(Debug, Default, Clone)]
pub struct EmitNames {
    pub map: HashMap<(String, String), String>,
    pub colliding_simple_names: HashSet<String>,
}

impl EmitNames {
    fn build(module: &Module, project: &ProjectInfo) -> Self {
        // First pass: count user-class declarations per simple
        // name. A name with count >= 2 is colliding.
        let mut counts: HashMap<String, usize> = HashMap::new();
        for item in &module.items {
            if let Item::Class(c) = item {
                if module_of(c.span.file, project).is_some() {
                    *counts.entry(c.name.name.clone()).or_insert(0) += 1;
                }
            }
        }
        let colliding_simple_names: HashSet<String> = counts
            .iter()
            .filter_map(|(n, &c)| if c >= 2 { Some(n.clone()) } else { None })
            .collect();
        // Second pass: assign emit names. User classes get mangled
        // when their simple name is in the colliding set; other
        // user classes keep their bare name; bindings always keep
        // their bare name (Qt API surface is fixed).
        let mut map: HashMap<(String, String), String> = HashMap::new();
        for item in &module.items {
            if let Item::Class(c) = item {
                let Some(module_name) = module_of(c.span.file, project) else {
                    continue;
                };
                let emit = if colliding_simple_names.contains(&c.name.name) {
                    // QML type names must begin with an uppercase
                    // letter, so capitalise the module prefix
                    // (`model` → `Model`) before joining. The C++
                    // class name and the qmlRegisterType-registered
                    // name end up identical (`Model__Counter`); QML
                    // is happy and the user's view body's
                    // `model.Counter` still resolves through the
                    // qualifier-aware `resolve` path.
                    format!("{}__{}", capitalize_first(&module_name), c.name.name)
                } else {
                    c.name.name.clone()
                };
                map.insert((module_name, c.name.name.clone()), emit);
            }
        }
        EmitNames {
            map,
            colliding_simple_names,
        }
    }

    pub fn has_any_collision(&self) -> bool {
        !self.colliding_simple_names.is_empty()
    }

    /// Resolve a class reference to its emit name. `qualifier` is
    /// the user-typed module prefix (e.g. `model` from
    /// `model.Counter`); `current_module` is the module the
    /// reference site lives in, used as the fallback when the
    /// reference is unqualified.
    fn resolve(
        &self,
        simple: &str,
        qualifier: Option<&str>,
        current_module: Option<&str>,
    ) -> Option<String> {
        let module = qualifier.or(current_module)?;
        self.map
            .get(&(module.to_string(), simple.to_string()))
            .cloned()
    }
}

fn module_of(file: FileId, project: &ProjectInfo) -> Option<String> {
    project.module_for_file.get(&file).cloned()
}

/// Uppercase the first character of `s` (preserving the rest). Used
/// when joining a Cute file-stem module name (lowercase by
/// convention, matching the path) with a class name to form a QML-
/// legal type identifier.
pub(crate) fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

struct Rewriter<'a> {
    names: &'a EmitNames,
    project: &'a ProjectInfo,
}

impl<'a> Rewriter<'a> {
    fn current_module(&self, file: FileId) -> Option<String> {
        module_of(file, self.project)
    }

    fn rewrite_item(&self, item: &Item) -> Item {
        match item {
            Item::Class(c) => Item::Class(self.rewrite_class(c)),
            Item::Struct(s) => Item::Struct(self.rewrite_struct(s)),
            Item::Fn(f) => Item::Fn(self.rewrite_fn(f, None)),
            Item::View(v) => Item::View(self.rewrite_view(v)),
            Item::Widget(w) => Item::Widget(self.rewrite_widget(w)),
            Item::Style(s) => Item::Style(self.rewrite_style(s)),
            Item::Use(u) => Item::Use(u.clone()),
            Item::UseQml(u) => Item::UseQml(u.clone()),
            // Trait + Impl items don't carry mangled names today.
            // Trait names live in the type system only; impl-method
            // names are local to their target class. If cross-module
            // collision-resolution becomes necessary, route through
            // here next.
            Item::Trait(t) => Item::Trait(t.clone()),
            Item::Impl(i) => Item::Impl(i.clone()),
            // Top-level lets are file-local C++ statics — no name
            // mangling required (different translation units never
            // see each other's `static const auto X`).
            Item::Let(l) => Item::Let(l.clone()),
            // Enum / flags decls don't currently participate in
            // cross-module name mangling — declared once at module
            // scope, referenced verbatim from siblings. Cross-
            // module collision handling can route through here if
            // it becomes necessary.
            Item::Enum(e) => Item::Enum(self.rewrite_enum(e)),
            Item::Flags(f) => Item::Flags(f.clone()),
            Item::Store(_) => unreachable!(
                "Item::Store should be lowered by desugar_store before \
                 name mangling runs",
            ),
            Item::Suite(_) => unreachable!(
                "Item::Suite should be flattened by desugar_suite before \
                 name mangling runs",
            ),
        }
    }

    fn rewrite_class(&self, c: &ClassDecl) -> ClassDecl {
        let module = self.current_module(c.span.file);
        let mut new_name = c.name.clone();
        if let Some(m) = module.as_deref() {
            if let Some(emit) = self.names.resolve(&c.name.name, None, Some(m)) {
                new_name.name = emit;
            }
        }
        ClassDecl {
            name: new_name,
            generics: c.generics.clone(),
            super_class: c
                .super_class
                .as_ref()
                .map(|t| self.rewrite_type(t, module.as_deref())),
            members: c
                .members
                .iter()
                .map(|m| self.rewrite_class_member(m, module.as_deref()))
                .collect(),
            is_pub: c.is_pub,
            is_extern_value: c.is_extern_value,
            is_arc: c.is_arc,
            is_copyable: c.is_copyable,
            span: c.span,
        }
    }

    fn rewrite_class_member(&self, m: &ClassMember, module: Option<&str>) -> ClassMember {
        match m {
            ClassMember::Property(p) => ClassMember::Property(cute_syntax::ast::PropertyDecl {
                name: p.name.clone(),
                ty: self.rewrite_type(&p.ty, module),
                notify: p.notify.clone(),
                default: p.default.as_ref().map(|e| self.rewrite_expr(e, module)),
                is_pub: p.is_pub,
                bindable: p.bindable,
                binding: p.binding.as_ref().map(|e| self.rewrite_expr(e, module)),
                fresh: p.fresh.as_ref().map(|e| self.rewrite_expr(e, module)),
                model: p.model,
                constant: p.constant,
                span: p.span,
                block_id: p.block_id,
            }),
            ClassMember::Signal(s) => ClassMember::Signal(cute_syntax::ast::SignalDecl {
                name: s.name.clone(),
                params: s
                    .params
                    .iter()
                    .map(|p| self.rewrite_param(p, module))
                    .collect(),
                is_pub: s.is_pub,
                span: s.span,
            }),
            ClassMember::Fn(f) => ClassMember::Fn(self.rewrite_fn(f, module)),
            ClassMember::Slot(f) => ClassMember::Slot(self.rewrite_fn(f, module)),
            ClassMember::Field(f) => ClassMember::Field(self.rewrite_field(f, module)),
            ClassMember::Init(i) => ClassMember::Init(cute_syntax::ast::InitDecl {
                params: i
                    .params
                    .iter()
                    .map(|p| self.rewrite_param(p, module))
                    .collect(),
                body: self.rewrite_block(&i.body, module),
                span: i.span,
            }),
            ClassMember::Deinit(d) => ClassMember::Deinit(cute_syntax::ast::DeinitDecl {
                body: self.rewrite_block(&d.body, module),
                span: d.span,
            }),
        }
    }

    fn rewrite_struct(&self, s: &StructDecl) -> StructDecl {
        let module = self.current_module(s.span.file);
        StructDecl {
            name: s.name.clone(),
            fields: s
                .fields
                .iter()
                .map(|f| self.rewrite_field(f, module.as_deref()))
                .collect(),
            methods: s
                .methods
                .iter()
                .map(|f| self.rewrite_fn(f, module.as_deref()))
                .collect(),
            is_pub: s.is_pub,
            is_copyable: s.is_copyable,
            span: s.span,
        }
    }

    fn rewrite_field(
        &self,
        f: &cute_syntax::ast::Field,
        module: Option<&str>,
    ) -> cute_syntax::ast::Field {
        cute_syntax::ast::Field {
            name: f.name.clone(),
            ty: self.rewrite_type(&f.ty, module),
            default: f.default.as_ref().map(|e| self.rewrite_expr(e, module)),
            is_pub: f.is_pub,
            is_mut: f.is_mut,
            weak: f.weak,
            unowned: f.unowned,
            span: f.span,
            block_id: f.block_id,
        }
    }

    fn rewrite_enum(&self, e: &EnumDecl) -> EnumDecl {
        let module = self.current_module(e.span.file);
        EnumDecl {
            name: e.name.clone(),
            variants: e
                .variants
                .iter()
                .map(|v| cute_syntax::ast::EnumVariant {
                    name: v.name.clone(),
                    value: v.value.clone(),
                    fields: v
                        .fields
                        .iter()
                        .map(|f| self.rewrite_field(f, module.as_deref()))
                        .collect(),
                    is_pub: v.is_pub,
                    span: v.span,
                })
                .collect(),
            is_pub: e.is_pub,
            is_extern: e.is_extern,
            is_error: e.is_error,
            cpp_namespace: e.cpp_namespace.clone(),
            span: e.span,
        }
    }

    fn rewrite_fn(&self, f: &FnDecl, surrounding_module: Option<&str>) -> FnDecl {
        let module = surrounding_module
            .map(String::from)
            .or_else(|| self.current_module(f.span.file));
        FnDecl {
            is_async: f.is_async,
            name: f.name.clone(),
            generics: f.generics.clone(),
            params: f
                .params
                .iter()
                .map(|p| self.rewrite_param(p, module.as_deref()))
                .collect(),
            return_ty: f
                .return_ty
                .as_ref()
                .map(|t| self.rewrite_type(t, module.as_deref())),
            body: f
                .body
                .as_ref()
                .map(|b| self.rewrite_block(b, module.as_deref())),
            is_pub: f.is_pub,
            is_test: f.is_test,
            display_name: f.display_name.clone(),
            attributes: f.attributes.clone(),
            span: f.span,
        }
    }

    fn rewrite_param(&self, p: &Param, module: Option<&str>) -> Param {
        Param {
            name: p.name.clone(),
            ty: self.rewrite_type(&p.ty, module),
            default: p.default.as_ref().map(|e| self.rewrite_expr(e, module)),
            is_escaping: p.is_escaping,
            is_consuming: p.is_consuming,
            span: p.span,
        }
    }

    fn rewrite_view(&self, v: &ViewDecl) -> ViewDecl {
        let module = self.current_module(v.span.file);
        ViewDecl {
            name: v.name.clone(),
            params: v
                .params
                .iter()
                .map(|p| self.rewrite_param(p, module.as_deref()))
                .collect(),
            state_fields: v
                .state_fields
                .iter()
                .map(|sf| cute_syntax::ast::StateField {
                    name: sf.name.clone(),
                    kind: sf.kind.clone(),
                    init_expr: self.rewrite_expr(&sf.init_expr, module.as_deref()),
                    span: sf.span,
                })
                .collect(),
            root: self.rewrite_element(&v.root, module.as_deref()),
            is_pub: v.is_pub,
            span: v.span,
        }
    }

    fn rewrite_widget(&self, w: &WidgetDecl) -> WidgetDecl {
        let module = self.current_module(w.span.file);
        WidgetDecl {
            name: w.name.clone(),
            params: w
                .params
                .iter()
                .map(|p| self.rewrite_param(p, module.as_deref()))
                .collect(),
            state_fields: w
                .state_fields
                .iter()
                .map(|sf| cute_syntax::ast::StateField {
                    name: sf.name.clone(),
                    kind: sf.kind.clone(),
                    init_expr: self.rewrite_expr(&sf.init_expr, module.as_deref()),
                    span: sf.span,
                })
                .collect(),
            root: self.rewrite_element(&w.root, module.as_deref()),
            is_pub: w.is_pub,
            span: w.span,
        }
    }

    fn rewrite_style(&self, s: &StyleDecl) -> StyleDecl {
        let module = self.current_module(s.span.file);
        let body = match &s.body {
            StyleBody::Lit(entries) => StyleBody::Lit(
                entries
                    .iter()
                    .map(|e| cute_syntax::ast::StyleEntry {
                        key: e.key.clone(),
                        value: self.rewrite_expr(&e.value, module.as_deref()),
                        span: e.span,
                    })
                    .collect(),
            ),
            StyleBody::Alias(rhs) => StyleBody::Alias(self.rewrite_expr(rhs, module.as_deref())),
        };
        StyleDecl {
            name: s.name.clone(),
            body,
            is_pub: s.is_pub,
            span: s.span,
        }
    }

    fn rewrite_element(&self, e: &Element, module: Option<&str>) -> Element {
        // Element head: resolve to (module, simple-name) using the
        // module_path qualifier (last segment) when present, falling
        // back to the surrounding module. Successful resolve drops
        // the path and substitutes the mangled name.
        let qualifier = e.module_path.last().map(|i| i.name.as_str());
        let mut new_name = e.name.clone();
        let mut new_path = e.module_path.clone();
        if let Some(emit) = self.names.resolve(&e.name.name, qualifier, module) {
            new_name.name = emit;
            new_path.clear();
        }
        Element {
            module_path: new_path,
            name: new_name,
            members: e
                .members
                .iter()
                .map(|m| self.rewrite_element_member(m, module))
                .collect(),
            span: e.span,
        }
    }

    fn rewrite_element_member(&self, m: &ElementMember, module: Option<&str>) -> ElementMember {
        match m {
            ElementMember::Property { key, value, span } => ElementMember::Property {
                key: key.clone(),
                value: self.rewrite_expr(value, module),
                span: *span,
            },
            ElementMember::Child(c) => ElementMember::Child(self.rewrite_element(c, module)),
            ElementMember::Stmt(s) => ElementMember::Stmt(self.rewrite_stmt(s, module)),
        }
    }

    fn rewrite_block(&self, b: &Block, module: Option<&str>) -> Block {
        Block {
            stmts: b
                .stmts
                .iter()
                .map(|s| self.rewrite_stmt(s, module))
                .collect(),
            trailing: b
                .trailing
                .as_ref()
                .map(|e| Box::new(self.rewrite_expr(e, module))),
            span: b.span,
        }
    }

    fn rewrite_stmt(&self, s: &Stmt, module: Option<&str>) -> Stmt {
        match s {
            Stmt::Let {
                name,
                ty,
                value,
                span,
                block_id,
            } => Stmt::Let {
                name: name.clone(),
                ty: ty.as_ref().map(|t| self.rewrite_type(t, module)),
                value: self.rewrite_expr(value, module),
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
                ty: ty.as_ref().map(|t| self.rewrite_type(t, module)),
                value: self.rewrite_expr(value, module),
                span: *span,
                block_id: *block_id,
            },
            Stmt::Expr(e) => Stmt::Expr(self.rewrite_expr(e, module)),
            Stmt::Return { value, span } => Stmt::Return {
                value: value.as_ref().map(|e| self.rewrite_expr(e, module)),
                span: *span,
            },
            Stmt::Emit { signal, args, span } => Stmt::Emit {
                signal: signal.clone(),
                args: args.iter().map(|a| self.rewrite_expr(a, module)).collect(),
                span: *span,
            },
            Stmt::Assign {
                target,
                op,
                value,
                span,
            } => Stmt::Assign {
                target: self.rewrite_expr(target, module),
                op: *op,
                value: self.rewrite_expr(value, module),
                span: *span,
            },
            Stmt::For {
                binding,
                iter,
                body,
                span,
            } => Stmt::For {
                binding: binding.clone(),
                iter: self.rewrite_expr(iter, module),
                body: self.rewrite_block(body, module),
                span: *span,
            },
            Stmt::While { cond, body, span } => Stmt::While {
                cond: self.rewrite_expr(cond, module),
                body: self.rewrite_block(body, module),
                span: *span,
            },
            Stmt::Break { span } => Stmt::Break { span: *span },
            Stmt::Continue { span } => Stmt::Continue { span: *span },
            Stmt::Batch { body, span } => Stmt::Batch {
                body: self.rewrite_block(body, module),
                span: *span,
            },
        }
    }

    fn rewrite_expr(&self, e: &Expr, module: Option<&str>) -> Expr {
        let kind = self.rewrite_expr_kind(&e.kind, module);
        Expr { kind, span: e.span }
    }

    fn rewrite_expr_kind(&self, k: &ExprKind, module: Option<&str>) -> ExprKind {
        use ExprKind as K;
        match k {
            K::Ident(name) => {
                // Bare identifier that happens to name a colliding
                // class - rewrite to its mangled emit name (in the
                // surrounding module). All other Ident uses keep
                // their original spelling.
                if self.names.colliding_simple_names.contains(name) {
                    if let Some(emit) = self.names.resolve(name, None, module) {
                        return K::Ident(emit);
                    }
                }
                K::Ident(name.clone())
            }
            K::Path(segments) => {
                if let Some(last) = segments.last() {
                    if self.names.colliding_simple_names.contains(&last.name) {
                        let qualifier = if segments.len() >= 2 {
                            Some(segments[segments.len() - 2].name.as_str())
                        } else {
                            None
                        };
                        if let Some(emit) = self.names.resolve(&last.name, qualifier, module) {
                            return K::Ident(emit);
                        }
                    }
                }
                K::Path(segments.clone())
            }
            K::Call {
                callee,
                args,
                block,
                type_args,
            } => K::Call {
                callee: Box::new(self.rewrite_expr(callee, module)),
                args: args.iter().map(|a| self.rewrite_expr(a, module)).collect(),
                block: block
                    .as_ref()
                    .map(|b| Box::new(self.rewrite_expr(b, module))),
                type_args: type_args.clone(),
            },
            K::MethodCall {
                receiver,
                method,
                args,
                block,
                type_args,
            } => {
                // `model.Counter.new(...)` parses as
                // `MethodCall { receiver: Member { receiver: Ident("model"), name: "Counter" }, method: "new" }`.
                // We don't fully resolve that yet - method-call
                // receivers fall through to the Member rewrite,
                // which catches the qualified ref via Path.
                K::MethodCall {
                    receiver: Box::new(self.rewrite_expr(receiver, module)),
                    method: method.clone(),
                    args: args.iter().map(|a| self.rewrite_expr(a, module)).collect(),
                    block: block
                        .as_ref()
                        .map(|b| Box::new(self.rewrite_expr(b, module))),
                    type_args: type_args.clone(),
                }
            }
            K::Member { receiver, name } => {
                // `model.Counter` (qualified class reference) parses
                // as `Member { receiver: Ident("model"), name: "Counter" }`.
                // If the receiver is an Ident matching a known
                // module name AND `name` is a colliding class, fold
                // the whole Member into a single mangled Ident.
                if let K::Ident(rcv) = &receiver.kind {
                    if self.names.colliding_simple_names.contains(&name.name) {
                        if let Some(emit) = self.names.resolve(&name.name, Some(rcv), module) {
                            return K::Ident(emit);
                        }
                    }
                }
                K::Member {
                    receiver: Box::new(self.rewrite_expr(receiver, module)),
                    name: name.clone(),
                }
            }
            K::SafeMember { receiver, name } => K::SafeMember {
                receiver: Box::new(self.rewrite_expr(receiver, module)),
                name: name.clone(),
            },
            K::SafeMethodCall {
                receiver,
                method,
                args,
                block,
                type_args,
            } => K::SafeMethodCall {
                receiver: Box::new(self.rewrite_expr(receiver, module)),
                method: method.clone(),
                args: args.iter().map(|a| self.rewrite_expr(a, module)).collect(),
                block: block
                    .as_ref()
                    .map(|b| Box::new(self.rewrite_expr(b, module))),
                type_args: type_args.clone(),
            },
            K::Index { receiver, index } => K::Index {
                receiver: Box::new(self.rewrite_expr(receiver, module)),
                index: Box::new(self.rewrite_expr(index, module)),
            },
            K::Block(b) => K::Block(self.rewrite_block(b, module)),
            K::Lambda { params, body } => K::Lambda {
                params: params
                    .iter()
                    .map(|p| self.rewrite_param(p, module))
                    .collect(),
                body: self.rewrite_block(body, module),
            },
            K::Unary { op, expr } => K::Unary {
                op: *op,
                expr: Box::new(self.rewrite_expr(expr, module)),
            },
            K::Binary { op, lhs, rhs } => K::Binary {
                op: *op,
                lhs: Box::new(self.rewrite_expr(lhs, module)),
                rhs: Box::new(self.rewrite_expr(rhs, module)),
            },
            K::Try(inner) => K::Try(Box::new(self.rewrite_expr(inner, module))),
            K::Await(inner) => K::Await(Box::new(self.rewrite_expr(inner, module))),
            K::If {
                cond,
                then_b,
                else_b,
                let_binding,
            } => K::If {
                cond: Box::new(self.rewrite_expr(cond, module)),
                then_b: self.rewrite_block(then_b, module),
                else_b: else_b.as_ref().map(|b| self.rewrite_block(b, module)),
                let_binding: let_binding
                    .as_ref()
                    .map(|(p, e)| (p.clone(), Box::new(self.rewrite_expr(e, module)))),
            },
            K::Case { scrutinee, arms } => K::Case {
                scrutinee: Box::new(self.rewrite_expr(scrutinee, module)),
                arms: arms
                    .iter()
                    .map(|arm| cute_syntax::ast::CaseArm {
                        pattern: rewrite_pattern(&arm.pattern),
                        body: self.rewrite_block(&arm.body, module),
                        span: arm.span,
                    })
                    .collect(),
            },
            K::Kwarg { key, value } => K::Kwarg {
                key: key.clone(),
                value: Box::new(self.rewrite_expr(value, module)),
            },
            K::Element(el) => K::Element(self.rewrite_element(el, module)),
            K::Array(items) => {
                K::Array(items.iter().map(|i| self.rewrite_expr(i, module)).collect())
            }
            K::Map(entries) => K::Map(
                entries
                    .iter()
                    .map(|(k, v)| (self.rewrite_expr(k, module), self.rewrite_expr(v, module)))
                    .collect(),
            ),
            K::Range {
                start,
                end,
                inclusive,
            } => K::Range {
                start: Box::new(self.rewrite_expr(start, module)),
                end: Box::new(self.rewrite_expr(end, module)),
                inclusive: *inclusive,
            },
            K::Str(parts) => K::Str(
                parts
                    .iter()
                    .map(|p| match p {
                        StrPart::Text(s) => StrPart::Text(s.clone()),
                        StrPart::Interp(e) => {
                            StrPart::Interp(Box::new(self.rewrite_expr(e, module)))
                        }
                        StrPart::InterpFmt { expr, format_spec } => StrPart::InterpFmt {
                            expr: Box::new(self.rewrite_expr(expr, module)),
                            format_spec: format_spec.clone(),
                        },
                    })
                    .collect(),
            ),
            K::Int(_)
            | K::Float(_)
            | K::Bool(_)
            | K::Nil
            | K::Sym(_)
            | K::AtIdent(_)
            | K::SelfRef => k.clone(),
        }
    }

    fn rewrite_type(&self, t: &TypeExpr, module: Option<&str>) -> TypeExpr {
        let kind = match &t.kind {
            TypeKind::Named { path, args } => {
                if let Some(last) = path.last() {
                    if self.names.colliding_simple_names.contains(&last.name) {
                        let qualifier = if path.len() >= 2 {
                            Some(path[path.len() - 2].name.as_str())
                        } else {
                            None
                        };
                        if let Some(emit) = self.names.resolve(&last.name, qualifier, module) {
                            // Replace the entire path with a single
                            // segment containing the mangled name.
                            let mut new_seg = last.clone();
                            new_seg.name = emit;
                            return TypeExpr {
                                kind: TypeKind::Named {
                                    path: vec![new_seg],
                                    args: args
                                        .iter()
                                        .map(|a| self.rewrite_type(a, module))
                                        .collect(),
                                },
                                span: t.span,
                            };
                        }
                    }
                }
                TypeKind::Named {
                    path: path.clone(),
                    args: args.iter().map(|a| self.rewrite_type(a, module)).collect(),
                }
            }
            TypeKind::Nullable(inner) => {
                TypeKind::Nullable(Box::new(self.rewrite_type(inner, module)))
            }
            TypeKind::ErrorUnion(inner) => {
                TypeKind::ErrorUnion(Box::new(self.rewrite_type(inner, module)))
            }
            TypeKind::Fn { params, ret } => TypeKind::Fn {
                params: params
                    .iter()
                    .map(|p| self.rewrite_type(p, module))
                    .collect(),
                ret: Box::new(self.rewrite_type(ret, module)),
            },
            TypeKind::SelfType => TypeKind::SelfType,
        };
        TypeExpr { kind, span: t.span }
    }
}

fn rewrite_pattern(p: &Pattern) -> Pattern {
    // Patterns don't reference class types positionally; ctor names
    // for error variants stay as written. Pass through unchanged.
    let _ = Ident {
        name: String::new(),
        span: p_span(p),
    };
    p.clone()
}

fn p_span(p: &Pattern) -> cute_syntax::span::Span {
    match p {
        Pattern::Ctor { span, .. }
        | Pattern::Literal { span, .. }
        | Pattern::Wild { span }
        | Pattern::Bind { span, .. } => *span,
    }
}
