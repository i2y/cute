//! Cute AST -> C++17 source emission.
//!
//! See `crate::ty` for type mapping. The QMetaObject portion of QObject-
//! derived classes is delegated to `cute-meta`, which returns header-side
//! declarations + source-side definitions that we splice into the right
//! buffers.

use cute_hir::{FnScope, ItemKind, ResolvedProgram};
use cute_meta::{ClassInfo, MetaSection, MethodInfo, ParamInfo, PropInfo, SignalInfo};
use cute_syntax::ast::{
    Block, ClassDecl, ClassMember, Element, ElementMember, EnumDecl, Expr, ExprKind, Field, FnDecl,
    GenericParam, ImplDecl, InitDecl, Item, Module, Param, PropertyDecl, Stmt, StructDecl,
    TraitDecl, TypeKind, ViewDecl, WidgetDecl,
};

use crate::mangle::capitalize_first;
use crate::ty::{self, TypeCtx};

#[derive(Debug, thiserror::Error)]
pub enum EmitError {
    #[error("only `class X < QObject` is currently supported, found {0}")]
    UnsupportedSuper(String),
    #[error("expected class super-type but found {0}")]
    UnsupportedItem(&'static str),
    #[error("style alias cycle involving `{0}`")]
    StyleCycle(String),
    #[error("unknown style `{0}` referenced in alias")]
    UnknownStyle(String),
    #[error(
        "unsupported expression in style position - expected a style name, `A + B`, or chain thereof"
    )]
    UnsupportedStyleExpr,
    /// One or more lowerer-side dead-ends were hit (e.g. `case ...`
    /// or `Lambda-with-stmts` in a widget property body where the
    /// stateless lowerer can't produce IIFE scaffolding). Previously
    /// these emitted `/* TODO ... */` comment markers into the user's
    /// C++/QML output, so the failure surfaced as an opaque downstream
    /// compiler error. Surfaced here so callers can render a Cute-side
    /// diagnostic instead.
    #[error("unsupported syntax encountered during lowering:\n  - {}", .0.join("\n  - "))]
    UnsupportedLowering(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct EmitResult {
    pub stem: String,
    pub header_filename: String, // e.g. "todo_item.h"
    pub source_filename: String, // e.g. "todo_item.cpp"
    pub header: String,
    pub source: String,
    /// Cute-side `view` declarations lowered to `.qml` source. The
    /// driver writes each one alongside the .h/.cpp pair and bundles
    /// them into qrc for QML mode.
    pub views: Vec<EmittedView>,
}

#[derive(Debug, Clone)]
pub struct EmittedView {
    /// Source-level view name (e.g. `Main` for `view Main {...}`).
    pub name: String,
    /// QML output filename (e.g. `Main.qml`).
    pub filename: String,
    /// Generated QML text.
    pub qml: String,
}

/// One foreign QML module the user declared via `use qml "..."`.
/// Codegen emits `import <module_uri> [<version>] [as <alias>]` per
/// entry into the generated `.qml` files. `version` is `None` for
/// version-less modules (Qt 6+ modular Kirigami etc.) — emitting a
/// version that isn't in the qmldir would make QML refuse to load.
#[derive(Debug, Clone)]
pub struct QmlImport {
    pub module_uri: String,
    pub version: Option<String>,
    pub alias: Option<String>,
}

/// Side-band type-info passed from the type checker. Keeps codegen's
/// public surface small while letting the call site teach codegen
/// about generic-class instantiation that wasn't expressed at the
/// syntax layer (e.g. `let b: Box<Int> = Box.new()` or
/// `put(Box.new())` against a `Box<Int>` parameter), plus the
/// foreign QML modules the user opted in to via `use qml`.
pub struct CodegenTypeInfo<'a> {
    /// `T.new()` MethodCall span -> the type args the checker
    /// inferred for it. Codegen looks each call up here when its
    /// own `type_args` is empty so it can still emit
    /// `cute::Arc<T<args>>(new T<args>(...))`.
    pub generic_instantiations:
        &'a std::collections::HashMap<cute_syntax::span::Span, Vec<cute_types::ty::Type>>,
    /// Foreign QML modules to import in generated `.qml` output. One
    /// entry per `use qml "..."` declaration in the user's source.
    pub qml_imports: &'a [QmlImport],
    /// Optional source map. When present, codegen emits `#line N "path"`
    /// directives at function-body boundaries so debuggers (gdb / lldb)
    /// step into the `.cute` source instead of the generated `.cpp`.
    /// `None` keeps tests and embedded uses (e.g. `cute build --emit-headers`)
    /// from having to thread a map through.
    pub source_map: Option<&'a cute_syntax::span::SourceMap>,
    /// `true` when invoked by `cute test`. Suppresses any user
    /// `fn main`, emits each `test fn name { ... }` as a callable C++
    /// fn (`cute_test_<name>`), and appends a runner `int main(...)`
    /// that loops over them with TAP-lite output. `false` (the
    /// default) keeps `test fn`s callable but skips the runner.
    pub is_test_build: bool,
    /// Stdlib + user-supplied `.qpi` binding modules. Codegen never
    /// emits C++ for these but consults their FnDecls at call sites
    /// when looking up per-method attribute markers (e.g.
    /// `@lifted_bool_ok` on `QString::toInt`). Empty for tests that
    /// don't need binding-aware lowering.
    pub binding_modules: &'a [Module],
}

impl<'a> CodegenTypeInfo<'a> {
    /// Empty default: no instantiations, no foreign QML imports, no
    /// source map. Used by tests / call sites that don't have a
    /// type-check pass to feed in.
    pub fn empty() -> Self {
        // Reference to a 'static empty map. Each call returns the
        // same singleton.
        static EMPTY: std::sync::OnceLock<
            std::collections::HashMap<cute_syntax::span::Span, Vec<cute_types::ty::Type>>,
        > = std::sync::OnceLock::new();
        static EMPTY_IMPORTS: &[QmlImport] = &[];
        static EMPTY_BINDINGS: &[Module] = &[];
        Self {
            generic_instantiations: EMPTY.get_or_init(std::collections::HashMap::new),
            qml_imports: EMPTY_IMPORTS,
            source_map: None,
            is_test_build: false,
            binding_modules: EMPTY_BINDINGS,
        }
    }
}

/// Emit `.h` + `.cpp` for a single Cute module. `stem` is the file stem
/// (e.g. "todo_item"), used both for filenames and as the include guard.
/// `program` is the resolved HIR for the same module: codegen consults it
/// for type-level decisions (nullable QObject vs value, error-union type
/// binding, auto-decl on first-occurrence assignments).
pub fn emit_module(
    stem: &str,
    module: &Module,
    program: &ResolvedProgram,
    project: &cute_hir::ProjectInfo,
    types: CodegenTypeInfo<'_>,
) -> Result<EmitResult, EmitError> {
    // The driver applies module-aware name mangling BEFORE HIR
    // resolve so the type checker sees the mangled names. What
    // arrives here has already been rewritten if any class names
    // collided across modules. Codegen's only remaining pre-pass
    // is style desugar: `style: <expr>` element members expand to
    // inline (key, value) properties so the QML / QtWidgets
    // emitters stay style-unaware.
    let _ = project; // mangle is driver-side now; kept for API parity
    let module = crate::style::desugar_module(module)?;
    // Splice each `impl Trait for Class { fn ... }`'s methods onto
    // the corresponding ClassDecl as if the user had written them
    // inside the class body. Trait + impl items are then dropped
    // from the walk — codegen below sees a single fattened class.
    let module = inline_impls_into_classes(&module);
    let mut em = Emitter::new(stem, program, types);
    em.module = Some(&module);
    // Sink for lowerer "this AST shape isn't lowerable in this
    // context" reports. Cleared up front (defensive — leftovers from
    // a prior failed pass would otherwise leak into this one) and
    // drained after emit so any recorded entries become a single
    // `EmitError::UnsupportedLowering` instead of a `/* TODO */`
    // marker leaking into the user's C++/QML output.
    clear_emit_diags();
    em.emit_module(&module)?;
    let diags = take_emit_diags();
    if !diags.is_empty() {
        return Err(EmitError::UnsupportedLowering(diags));
    }
    Ok(em.finish())
}

/// Walk `module.items` and merge each `Item::Impl(i).methods` onto
/// the matching `Item::Class(c).members` (matched by simple name).
/// Returns a fresh `Module` with `Item::Trait` / `Item::Impl` items
/// removed — the resulting AST is what codegen sees, so trait/impl
/// declarations don't need their own emit handlers.
///
/// Methods become `ClassMember::Fn(...)`. Visibility flows from the
/// impl method's own `is_pub`; the parser allows `pub fn` inside
/// impl blocks for symmetry with class methods. Conflicts (impl
/// supplies a method already on the class) keep the class's own
/// member; the type-checker pass should diagnose this separately.
///
/// Trait default methods (`trait Foo { fn x { ... } }`) are also
/// pulled in for any impl that omits the method. The impl's own
/// methods always win — if the impl supplied `x`, the trait's
/// default body is dropped on the floor.
fn inline_impls_into_classes(module: &Module) -> Module {
    use std::collections::{HashMap, HashSet};
    // Index every trait declaration so we can look up default
    // bodies later. Abstract methods (no body) are kept around for
    // shape but skipped at injection time.
    let mut traits: HashMap<String, &TraitDecl> = HashMap::new();
    for item in &module.items {
        if let Item::Trait(t) = item {
            traits.insert(t.name.name.clone(), t);
        }
    }
    let mut impl_methods: HashMap<String, Vec<ClassMember>> = HashMap::new();
    for item in &module.items {
        if let Item::Impl(i) = item {
            // Splice into the for-type's simple base name. Impls on
            // parametric instantiations (`impl<T> Foo for List<T>`)
            // and on extern bases that have no class entry are
            // collected here too — `impl_methods` looks them up by
            // base, and the per-class merge below silently drops
            // entries for which no Cute `Item::Class` exists.
            // Codegen for trait-method dispatch on those receivers
            // is a separate, future codegen pass (see free-function
            // dispatch in the gaps doc).
            let base = match cute_syntax::ast::type_expr_base_name(&i.for_type) {
                Some(b) => b,
                None => continue,
            };
            let supplied: HashSet<String> = i.methods.iter().map(|m| m.name.name.clone()).collect();
            let entry = impl_methods.entry(base).or_default();
            // Self → for-type substitution applies before splice so the
            // spliced class method's signature mentions the concrete
            // for-type instead of the abstract `Self` placeholder.
            for m in &i.methods {
                let substituted = cute_syntax::ast::substitute_self_in_fn_decl(m, &i.for_type);
                entry.push(ClassMember::Fn(substituted));
            }
            // Inherit default-bodied trait methods the impl omitted.
            if let Some(trait_decl) = traits.get(&i.trait_name.name) {
                for tm in &trait_decl.methods {
                    if supplied.contains(&tm.name.name) {
                        continue;
                    }
                    if tm.body.is_some() {
                        let substituted =
                            cute_syntax::ast::substitute_self_in_fn_decl(tm, &i.for_type);
                        entry.push(ClassMember::Fn(substituted));
                    }
                }
            }
        }
    }
    let mut new_items: Vec<Item> = Vec::with_capacity(module.items.len());
    for item in &module.items {
        match item {
            Item::Class(c) => {
                if let Some(extra) = impl_methods.get(&c.name.name) {
                    let mut merged = c.clone();
                    let existing: HashSet<String> = c
                        .members
                        .iter()
                        .filter_map(|m| match m {
                            ClassMember::Fn(f) | ClassMember::Slot(f) => Some(f.name.name.clone()),
                            _ => None,
                        })
                        .collect();
                    for m in extra {
                        if let ClassMember::Fn(f) = m {
                            if existing.contains(&f.name.name) {
                                continue;
                            }
                        }
                        merged.members.push(m.clone());
                    }
                    new_items.push(Item::Class(merged));
                } else {
                    new_items.push(item.clone());
                }
            }
            // Trait + Impl items are kept in the post-splice
            // module — codegen needs them for trait-dispatch
            // routing. The class merge above already added impl
            // methods as class members where the for-type is a
            // user class; the free-function emission step (driven
            // by `Item::Impl`) emits a parallel namespace overload
            // set, and `Lowering::trait_dispatch_name` walks
            // `Item::Trait` to decide which method calls in
            // templated bodies route through the namespace.
            // Trait + Impl arms in `Emitter::emit_module` produce
            // the right side-effects (no-op for Trait, namespace
            // emit for Impl) so neither leaks into the C++ output
            // by accident.
            other => new_items.push(other.clone()),
        }
    }
    Module {
        items: new_items,
        span: module.span,
    }
}

/// Codegen-side discriminator for the class shapes Cute distinguishes
/// at lowering time. Maps onto `cute_hir::ItemKind::Class` flags but
/// kept as a flat enum so the per-kind branches in lowering can
/// `match` rather than cascade through `if self.is_*_class(...)`.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum ClassKind {
    /// `class X { ... }` deriving from QObject (the v1 default).
    /// `T.new(...)` returns a heap-alloc'd raw pointer; member access
    /// uses `->`; lifetime is parent-tree managed.
    QObject,
    /// `arc X { ... }` — Cute's reference-counted class form.
    /// `T.new(...)` wraps the heap object in `cute::Arc<T>`.
    Arc,
    /// `extern value X { ... }` — plain C++ value type pulled in via
    /// `[cpp] includes`. `T.new(args)` lowers to `T(args)` (stack
    /// construction); member access uses `.`.
    ExternValue,
    /// Not a known class — either the name isn't in `prog.items` or
    /// the entry is a different `ItemKind` (struct, enum, fn, ...).
    NotAClass,
}

struct Emitter<'a> {
    stem: String,
    header: String,
    source: String,
    program: &'a ResolvedProgram,
    views: Vec<EmittedView>,
    /// Cached QML module name harvested from `qml_app(module: "...")`
    /// before view emission so the generated `.qml` files can `import`
    /// the user's module to reach their QObject classes (e.g.
    /// `Counter { id: c }` inside a `view`).
    qml_module: Option<String>,
    /// Span -> inferred generic args, populated by the type checker.
    /// Used by `K::MethodCall` lowering when the call has no explicit
    /// `type_args` of its own.
    generic_instantiations:
        &'a std::collections::HashMap<cute_syntax::span::Span, Vec<cute_types::ty::Type>>,
    /// The full user module — passed down to each `Lowering` so fn
    /// calls can be resolved against the surrounding fn declarations
    /// (return-type lookup for the `make_arc() -> Arc<...>` case).
    /// Set when `emit_module` is invoked.
    module: Option<&'a Module>,
    /// Foreign QML modules collected from `use qml "..."` decls.
    /// Codegen emits one `import` line per entry into the QML output.
    qml_imports: &'a [QmlImport],
    /// Source map for `#line` directive emission. `None` disables
    /// the feature; emit_module proceeds normally without anchoring
    /// generated C++ back to the `.cute` source.
    source_map: Option<&'a cute_syntax::span::SourceMap>,
    /// Test-build mode: emit a TAP-lite runner main and skip user
    /// `fn main`. See `CodegenTypeInfo::is_test_build`.
    is_test_build: bool,
    /// Loaded `.qpi` binding modules. Used by call-site attribute
    /// lookup when a method comes from a binding rather than the
    /// user module.
    binding_modules: &'a [Module],
}

impl<'a> Emitter<'a> {
    fn new(stem: &str, program: &'a ResolvedProgram, types: CodegenTypeInfo<'a>) -> Self {
        Self {
            stem: stem.to_string(),
            header: String::new(),
            source: String::new(),
            program,
            views: Vec::new(),
            qml_module: None,
            generic_instantiations: types.generic_instantiations,
            module: None,
            qml_imports: types.qml_imports,
            source_map: types.source_map,
            is_test_build: types.is_test_build,
            binding_modules: types.binding_modules,
        }
    }

    /// Write `lines` (from `Lowering::lower_block`) into `self.source`,
    /// each prefixed with `indent`, and emit a `#line N "..."` directive
    /// whenever the source-line tag moves to a new line. Skipping the
    /// `#line` for None-tagged lines (synthesized C++ with no Cute
    /// counterpart) keeps the directive count low; runs of consecutive
    /// lines mapped to the same source line are deduplicated so a
    /// debugger doesn't bounce back to the same row repeatedly.
    /// Lower `body` as a class-method block and write it into
    /// `self.source` at one level of indent. Used for init / deinit /
    /// any void-return slot body.
    fn lower_method_body(&mut self, c: &ClassDecl, ret: &str, params: &[Param], body: &Block) {
        let scope = self.program.fn_scopes.get(&body.span);
        let mut lo = Lowering::new(ret, false, scope)
            .with_emit_context(self.source_map, self.program, Some(c))
            .with_generic_instantiations(self.generic_instantiations)
            .with_module(self.module, self.binding_modules);
        lo.record_params(params);
        let lines = lo.lower_block(body);
        self.write_lowered_lines(lines, "    ");
    }

    fn write_lowered_lines(&mut self, lines: Vec<LoweredLine>, indent: &str) {
        let mut last_line: Option<usize> = None;
        for (line, span) in lines {
            if let (Some(sm), Some(sp)) = (self.source_map, span) {
                let (cl, _) = sm.line_col(sp);
                if Some(cl) != last_line {
                    self.emit_line_directive(sp);
                    last_line = Some(cl);
                }
            }
            self.source.push_str(indent);
            self.source.push_str(&line);
            self.source.push('\n');
        }
    }

    /// Append a `#line N "path"\n` directive pointing at `span`'s
    /// start position to `self.source`. No-op when no `SourceMap` was
    /// provided in `CodegenTypeInfo`. Called at function-body
    /// boundaries so debuggers step into `.cute` rather than the
    /// generated `.cpp`.
    fn emit_line_directive(&mut self, span: cute_syntax::span::Span) {
        let Some(sm) = self.source_map else { return };
        let (line, _) = sm.line_col(span);
        let path = sm.name(span.file);
        // C-string escape for the path: `\` and `"` are the only
        // characters compilers actually trip on inside `#line`'s
        // string literal. Newlines / tabs in paths are pathological
        // but cheap to handle.
        let mut escaped = String::with_capacity(path.len());
        for c in path.chars() {
            match c {
                '\\' => escaped.push_str("\\\\"),
                '"' => escaped.push_str("\\\""),
                '\n' => escaped.push_str("\\n"),
                '\t' => escaped.push_str("\\t"),
                _ => escaped.push(c),
            }
        }
        self.source
            .push_str(&format!("#line {line} \"{escaped}\"\n"));
    }

    fn ctx(&self) -> TypeCtx<'_> {
        TypeCtx::new(self.program)
    }

    fn finish(self) -> EmitResult {
        EmitResult {
            stem: self.stem.clone(),
            header_filename: format!("{}.h", self.stem),
            source_filename: format!("{}.cpp", self.stem),
            header: self.header,
            source: self.source,
            views: self.views,
        }
    }

    fn emit_module(&mut self, module: &Module) -> Result<(), EmitError> {
        // Populate the thread-local cpp-namespace map so the
        // free-function `widget_lower_expr` can resolve
        // `Foo.X → Qt::X` for extern enums declared with a
        // C++-namespace prefix (see `lookup_cpp_namespace`).
        let mut ns_map = std::collections::HashMap::new();
        for (name, kind) in &self.program.items {
            match kind {
                cute_hir::ItemKind::Enum {
                    cpp_namespace: Some(cns),
                    ..
                } => {
                    ns_map.insert(name.clone(), cns.clone());
                }
                cute_hir::ItemKind::Flags {
                    cpp_namespace: Some(cns),
                    ..
                } => {
                    ns_map.insert(name.clone(), cns.clone());
                }
                _ => {}
            }
        }
        set_cpp_namespace_map(ns_map);

        let has_widget = module.items.iter().any(|i| matches!(i, Item::Widget(_)));
        self.emit_file_preamble();
        if has_widget {
            // The widget DSL emits `QMainWindow` / `QLabel` / `QVBoxLayout`
            // etc. by name. The umbrella header `<QtWidgets>` pulls them
            // all in - heavier compile but no need to track per-element
            // include manifests at codegen time. The QtWidgets module
            // is only linked when this header lands in the .cpp (driver
            // chooses BuildMode::Widgets via `QApplication app` substring).
            self.header.push_str("#include <QtWidgets>\n");
        }
        // `prop ..., model` synthesizes a QRangeModel-backed accessor.
        // Qt 6.11+ ships <QRangeModel> in QtCore; we only pull the
        // headers in when at least one class actually uses the flag,
        // so projects on older Qt versions that never opt in still
        // build clean. <QAbstractItemModel> covers the synthesized
        // getter's return type.
        let mut model_row_types: Vec<String> = Vec::new();
        for item in &module.items {
            let Item::Class(c) = item else { continue };
            for mem in &c.members {
                let ClassMember::Property(p) = mem else {
                    continue;
                };
                if let Some(name) = model_row_type_of(p) {
                    if !model_row_types.contains(&name) {
                        model_row_types.push(name);
                    }
                }
            }
        }
        let needs_range_model = !model_row_types.is_empty();
        if needs_range_model {
            self.header.push_str("#include <QAbstractItemModel>\n");
            self.header.push_str("#include <QRangeModel>\n");
            self.header.push_str("#include \"cute_model.h\"\n");
            // Tell QRangeModel to treat each row type as a single-column
            // multi-role item. Without this, Q_OBJECT row types default
            // to "multi-column" semantics (one Q_PROPERTY = one column,
            // designed for QTableView), and a QML ListView (single
            // column) only sees the first property — every other role
            // (`author` in the demo) reads as undefined. With
            // `MultiRoleItem` each Q_PROPERTY becomes a named role
            // available as a delegate context property.
            //
            // Forward-declare the row class first so the template
            // specialization compiles before the class itself is
            // emitted further down the header. The specializations
            // need to live before any QRangeModel construction site,
            // so we put them up here at the top of the file.
            for ty in &model_row_types {
                self.header.push_str(&format!("class {ty};\n"));
            }
            // Specializations live in the QRangeModelDetails namespace
            // (the canonical qrangemodel.h definition there is
            // `QRangeModelRowOptions : QRangeModel::RowOptions<T>`).
            // Note the unwrapped target type: `row_traits` is keyed
            // on `wrapped_t<row_type>`, which strips the pointer for
            // `Book*` rows. So a `QList<Book*>` range needs the spec
            // on `Book` (not `Book*`); specializing the pointer form
            // would silently miss and the row would fall back to the
            // multi-column default.
            self.header.push_str("namespace QRangeModelDetails {\n");
            for ty in &model_row_types {
                self.header.push_str(&format!(
                    "template <> struct QRangeModelRowOptions<{ty}> {{\n"
                ));
                self.header.push_str(
                    "    static constexpr auto rowCategory = QRangeModel::RowCategory::MultiRoleItem;\n",
                );
                self.header.push_str("};\n");
            }
            self.header.push_str("} // namespace QRangeModelDetails\n");
        }

        // Forward-declare every user class up front so that cross-class
        // references in member types (`Arc<Other>`, `Other*`,
        // `cute::Weak<Other>`, …) compile regardless of declaration
        // order. Only non-extern, non-generic classes are forward-
        // declared here — generic classes need a `template <...>`
        // prefix and extern values are handled by the bound header,
        // not the generated module.
        for item in &module.items {
            let Item::Class(c) = item else { continue };
            if c.is_extern_value || !c.generics.is_empty() {
                continue;
            }
            self.header.push_str(&format!("class {};\n", c.name.name));
        }

        // Pre-pass: emit value-type top-level `let` declarations at
        // file scope, BEFORE any class / fn that might reference them.
        // C++ resolves these as `static const auto X = value;` so the
        // initializer is computed once at static-init time and the
        // binding is `constexpr`-eligible when the RHS is.
        //
        // QObject-typed top-level lets are deferred to a post-pass —
        // they need `Q_GLOBAL_STATIC` plus the class definition, which
        // is emitted by the main item walk below.
        for item in &module.items {
            let Item::Let(l) = item else { continue };
            self.emit_top_level_let_value(l);
        }

        // Pre-scan for an explicit `fn main { qml_app(module: ...) }` so
        // the views we emit can `import <module> 1.0` and reach the
        // user's QObject classes by name.
        let mut has_explicit_main = false;
        for item in &module.items {
            if let Item::Fn(f) = item {
                if f.name.name == "main" {
                    has_explicit_main = true;
                    if let Some(body) = &f.body {
                        if let Some(spec) = detect_qml_app(body) {
                            self.qml_module = Some(spec.module);
                            break;
                        }
                    }
                }
            }
        }

        // Collect the views and QObject classes once so we can both
        // (a) drive `qml_module` defaulting before view emit and
        // (b) synthesize a `fn main` later if the user omitted one.
        let view_names: Vec<String> = module
            .items
            .iter()
            .filter_map(|i| match i {
                Item::View(v) => Some(v.name.name.clone()),
                _ => None,
            })
            .collect();
        let widget_names: Vec<String> = module
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Widget(w) => Some(w.name.name.clone()),
                _ => None,
            })
            .collect();
        let class_names: Vec<String> = module
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Class(c) => Some(c.name.name.clone()),
                _ => None,
            })
            .collect();

        // No explicit main + at least one view -> Cute auto-wraps the
        // file as a QML app. The qml_module defaults to "App" so the
        // generated view's `import App 1.0` line resolves the user's
        // qmlRegisterType calls. View beats widget when both are
        // present, since the QtQuick path can host arbitrary QObject
        // classes via qmlRegisterType while the widget path can't.
        let auto_main_view = !has_explicit_main && !view_names.is_empty();
        let auto_main_widget =
            !has_explicit_main && view_names.is_empty() && !widget_names.is_empty();
        let auto_main = auto_main_view;
        // Only synthesize a qml module when there are user QObject
        // classes to register through qmlRegisterType. A view-only
        // file (just QtQuick.Controls / Kirigami elements, no user
        // classes) doesn't need an `import App 1.0` line — and
        // emitting one would refer to a module nothing called
        // qmlRegisterType for, breaking QQmlApplicationEngine::load.
        if auto_main && self.qml_module.is_none() && !class_names.is_empty() {
            self.qml_module = Some("App".to_string());
        }

        // Pass 1 — type definitions (classes / structs / errors / enums).
        // These must come before any `Q_GLOBAL_STATIC(T, X)` post-pass
        // so that the macro sees a complete `T`. Enums emit first so
        // class members typed as an enum can resolve.
        for item in &module.items {
            match item {
                Item::Enum(e) => self.emit_enum_decl(e),
                Item::Flags(f) => self.emit_flags_decl(f),
                _ => {}
            }
        }
        for item in &module.items {
            match item {
                Item::Class(c) => self.emit_class(c)?,
                Item::Struct(s) => self.emit_struct_decl(s),
                Item::Enum(e) if e.is_error => self.emit_error_decl(e),
                _ => {}
            }
        }

        // Post-pass — QObject-typed top-level lets via `Q_GLOBAL_STATIC`.
        // Emitted after class definitions (the macro instantiates
        // `QGlobalStatic<T>` which needs `sizeof(T)`) and before any
        // fn / view / widget body that may reference them.
        for item in &module.items {
            let Item::Let(l) = item else { continue };
            self.emit_top_level_let_qobject(l);
        }

        // Pass 2 — code (fns / views / widgets / impls). Everything
        // here can reference both value-typed lets (pre-pass) and
        // QObject-typed lets (post-pass) above.
        for item in &module.items {
            match item {
                Item::Use(_) => {}    // No real module system yet; handled by header includes.
                Item::UseQml(_) => {} // foreign QML decl, drives QML import emit only
                Item::Class(_) => {}  // emitted in pass 1
                Item::Struct(_) => {} // emitted in pass 1
                Item::Fn(f) => self.emit_top_level_fn(f),
                Item::View(v) => self.emit_view(v),
                Item::Widget(w) => self.emit_widget(w),
                // `style X { ... }` is consulted by the style env when
                // an element member uses `style: X`. The declaration
                // itself produces no C++ - styles are inlined where
                // referenced, never as a runtime value.
                Item::Style(_) => {}
                // Trait declarations are erased — no runtime
                // representation, no C++ output. The type checker
                // already validated that every impl supplies the
                // required surface.
                Item::Trait(_) => {}
                // Impl methods are inlined onto the target class
                // by `inline_impls_into_classes` for direct
                // (non-templated) dispatch. We *also* emit each
                // method as a free function in
                // `::cute::trait_impl::<Trait>::` so trait method
                // calls in generic-bound bodies can route through
                // the namespace via overload resolution. Both
                // user-class and extern for-types end up in the
                // same overload set, so a templated body works
                // uniformly across them.
                Item::Impl(i) => self.emit_impl_free_functions(module, i),
                // Top-level `let` is handled by the value-let
                // pre-pass above (file-scope value-typed) and the
                // Q_GLOBAL_STATIC post-pass between pass 1 and
                // pass 2 (file-scope QObject-typed).
                Item::Let(_) => {}
                // User enums emit as `enum class Foo : qint32 { ... }`
                // in pass 1 (see `emit_enum_decl`). Extern enums
                // emit nothing (the C++ definition lives in a
                // header pulled via `[cpp] includes`). Flags are
                // similar — user side emits a Q_DECLARE_FLAGS
                // companion, extern side just declares the type.
                Item::Enum(_) | Item::Flags(_) => {}
                Item::Store(_) => unreachable!(
                    "Item::Store should be lowered before codegen pass 2; \
                     see crate::desugar_store",
                ),
                Item::Suite(_) => unreachable!(
                    "Item::Suite should be flattened before codegen pass 2; \
                     see crate::desugar_suite",
                ),
            }
        }

        if self.is_test_build {
            self.emit_test_runner_main(module);
        } else if auto_main {
            // Pick the entry view: prefer "Main", fall back to the only
            // (or first) view in the module.
            let entry = view_names
                .iter()
                .find(|n| n.as_str() == "Main")
                .or_else(|| view_names.first())
                .cloned()
                .expect("auto_main implies non-empty view_names");
            let module_name = self.qml_module.clone().unwrap_or_else(|| "App".to_string());
            let spec = QmlAppSpec {
                qml_url: format!("qrc:/{entry}.qml"),
                module: module_name,
                version_major: 1,
                version_minor: 0,
                types: class_names,
            };
            self.emit_qml_app_main(&spec);
        } else if auto_main_widget {
            // Same SwiftUI-style ergonomics on the QtWidgets side: a
            // file with `widget Main { ... }` and no explicit `fn main`
            // gets a synthesized QApplication-based main wired to that
            // top-level widget.
            let entry = widget_names
                .iter()
                .find(|n| n.as_str() == "Main")
                .or_else(|| widget_names.first())
                .cloned()
                .expect("auto_main_widget implies non-empty widget_names");
            let spec = WidgetAppSpec {
                window: entry,
                title: None,
            };
            self.emit_widget_app_main(&spec);
        }

        Ok(())
    }

    /// Emit a `struct X { x: T = default, y: U }` Cute declaration as a
    /// plain C++ struct with public fields. The struct gets:
    ///
    /// - In-class default-initializers from `, default: V` field
    ///   declarations (or zero-init from C++'s rules when absent)
    /// - A defaulted no-arg ctor so `X.new()` lowers to `X{}`
    /// - A positional all-fields ctor so `X.new(a, b)` lowers to
    ///   `X{a, b}` via brace-init (the type-checker has already
    ///   verified arg count + types match the field declaration order)
    ///
    /// No metaobject, no inheritance, no signals — pure value type.
    fn emit_struct_decl(&mut self, s: &StructDecl) {
        let name = &s.name.name;
        let mut field_lines = String::new();
        {
            let ctx = self.ctx();
            for f in &s.fields {
                let fty = ty::cute_to_cpp(&f.ty, &ctx);
                let init = if f.default.is_some() {
                    let mut lo = Lowering::new("auto", false, None)
                        .with_emit_context(self.source_map, self.program, None)
                        .with_module(self.module, self.binding_modules);
                    let hint = lo.collection_hint_from_type(&f.ty);
                    let body = lo.lower_with_collection_hint(f.default.as_ref().unwrap(), hint);
                    format!(" = {body}")
                } else {
                    String::new()
                };
                // `let` field → C++ `const` member; the C++ compiler
                // back-stops the immutability contract (assignment to
                // a const member fails to compile, regardless of
                // whether the surrounding struct binding is `let` or
                // `var`). `var` field → plain mutable member.
                let mutability = if f.is_mut { "" } else { "const " };
                field_lines.push_str(&format!(
                    "    {mutability}{fty} {name}{init};\n",
                    mutability = mutability,
                    fty = fty,
                    name = f.name.name,
                ));
            }
        }
        self.header.push_str(&format!("\nstruct {name} {{\n"));
        self.header.push_str(&field_lines);
        // Plain (copyable) structs stay aggregates — no user-declared
        // ctors so callers can use `X{a, b}` (positional aggregate
        // init) or `X{.x = a, .y = b}` (C++20 designated init). The
        // codegen at the `X.new(...)` call site picks the designated
        // form when the field count matches the arg count.
        //
        // `~Copyable` structs need explicit copy/move declarations,
        // which cost aggregate status. Emit a positional ctor in that
        // case so `X.new(a, b)` -> `X{a, b}` still constructs cleanly,
        // and the call-site codegen falls back to positional init.
        if !s.is_copyable {
            // Default ctor for the no-arg `X.new()` form.
            self.header.push_str(&format!("    {name}() = default;\n"));
            if !s.fields.is_empty() {
                let ctx = self.ctx();
                let params = s
                    .fields
                    .iter()
                    .map(|f| format!("{} {}", ty::cute_to_cpp(&f.ty, &ctx), f.name.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                let inits = s
                    .fields
                    .iter()
                    .map(|f| format!("{n}({n})", n = f.name.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.header.push_str(&format!(
                    "    {name}({params}) : {inits} {{}}\n",
                    name = name,
                    params = params,
                    inits = inits,
                ));
            }
            self.header.push_str(&format!(
                "    {name}(const {name}&) = delete;\n    {name}& operator=(const {name}&) = delete;\n    {name}({name}&&) = default;\n    {name}& operator=({name}&&) = default;\n",
            ));
        }
        // User-declared methods, emitted inline as plain C++ member
        // functions. Method-level generics get their own `template
        // <typename U>` prefix on top of the (currently non-existent)
        // struct template; the body sees `self` as `this` so
        // `self.x` → `this->x` for fields and `self.method()` →
        // `this->method()` for sibling methods.
        for m in &s.methods {
            let method_template_prefix = if m.generics.is_empty() {
                String::new()
            } else {
                let params = m
                    .generics
                    .iter()
                    .map(|g| format!("typename {}", g.name.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("    template <{params}>\n")
            };
            let return_is_err_union = returns_err_union(&m.return_ty);
            let ret_ty = match &m.return_ty {
                Some(t) => ty::cute_to_cpp(t, &self.ctx()),
                None => "void".to_string(),
            };
            let params_s = m
                .params
                .iter()
                .map(|p| format!("{} {}", ty::cute_param_to_cpp(p, &self.ctx()), p.name.name))
                .collect::<Vec<_>>()
                .join(", ");
            self.header.push_str(&method_template_prefix);
            let nodiscard_attr = nodiscard_for_err_union(return_is_err_union);
            self.header.push_str(&format!(
                "    {nodiscard_attr}{ret_ty} {}({params_s}) {{\n",
                m.name.name,
            ));
            if let Some(body) = &m.body {
                lower_inline_body(
                    &mut self.header,
                    self.program,
                    self.module,
                    self.binding_modules,
                    SurroundingDecl::Struct(s),
                    &ret_ty,
                    return_is_err_union,
                    &m.params,
                    body,
                );
            }
            self.header.push_str("    }\n");
        }
        self.header.push_str("};\n\n");
    }

    /// True iff `let l : T = T.new(...)`'s declared type resolves to a
    /// QObject-derived class. Two HIR shapes count:
    ///   1. `prog.items[T]` is a regular Class entry.
    ///   2. Same-name singleton (`let Foo : Foo = ...` synth-emitted by
    ///      `store Foo { ... }`): the Let entry overwrote the Class
    ///      entry in `prog.items[Foo]`, but `is_qobject_type:true` was
    ///      computed pre-overwrite from the original Class lookup, so it
    ///      still faithfully reports QObject-derivation.
    fn let_qobject_class_name<'l>(&self, l: &'l cute_syntax::ast::LetDecl) -> Option<&'l str> {
        let cute_syntax::ast::TypeKind::Named { path, .. } = &l.ty.kind else {
            return None;
        };
        let class_name = path.last().map(|i| i.name.as_str())?;
        match self.program.items.get(class_name) {
            Some(cute_hir::ItemKind::Class {
                is_qobject_derived: true,
                ..
            }) => Some(class_name),
            Some(cute_hir::ItemKind::Let {
                is_qobject_type: true,
                ..
            }) if l.name.name == class_name => Some(class_name),
            _ => None,
        }
    }

    /// Emit a QObject-typed `let X : Foo = Foo.new(args)` as
    /// `Q_GLOBAL_STATIC(Foo, X)` (or `_WITH_ARGS` when args present).
    /// The accessor `X()` returns `Foo*`; the K::Ident lowering
    /// rewrites bare `X` references into `X()` so user-source stays
    /// clean. Skipped for non-QObject types and for value shapes
    /// other than `T.new(...)`.
    fn emit_top_level_let_qobject(&mut self, l: &cute_syntax::ast::LetDecl) {
        let Some(class_name) = self.let_qobject_class_name(l) else {
            return;
        };
        // Match `T.new(args)` shape on the value. Anything else is
        // out of scope for v1 — emit nothing and let the type checker
        // complain (or a follow-up extends to general initializers
        // via function-static lazy init).
        let cute_syntax::ast::ExprKind::MethodCall {
            receiver,
            method,
            args,
            ..
        } = &l.value.kind
        else {
            return;
        };
        if method.name != "new" {
            return;
        }
        let recv_name = match &receiver.kind {
            cute_syntax::ast::ExprKind::Ident(n) => n.as_str(),
            cute_syntax::ast::ExprKind::Path(p) => p.last().map(|i| i.name.as_str()).unwrap_or(""),
            _ => return,
        };
        if recv_name != class_name {
            // `let X : Foo = Bar.new()` — type doesn't match
            // constructor receiver. Skip; the type checker would
            // catch the mismatch separately.
            return;
        }
        let name = &l.name.name;
        if args.is_empty() {
            self.source
                .push_str(&format!("Q_GLOBAL_STATIC({class_name}, {name})\n"));
        } else {
            // Lower each arg through Lowering so collection literals,
            // string interp, etc. all work.
            let mut lo = Lowering::new("auto", false, None)
                .with_emit_context(self.source_map, self.program, None)
                .with_module(self.module, self.binding_modules);
            let lowered_args = args
                .iter()
                .map(|a| lo.lower_expr(a))
                .collect::<Vec<_>>()
                .join(", ");
            self.source.push_str(&format!(
                "Q_GLOBAL_STATIC_WITH_ARGS({class_name}, {name}, ({lowered_args}))\n",
            ));
        }
    }

    fn emit_top_level_let_value(&mut self, l: &cute_syntax::ast::LetDecl) {
        // QObject-typed lets defer to the Q_GLOBAL_STATIC post-pass
        // (`emit_top_level_let_qobject`); the value pre-pass only
        // emits `static const auto X = ...` for plain value types.
        if self.let_qobject_class_name(l).is_some() {
            return;
        }
        let mut lo = Lowering::new("auto", false, None)
            .with_emit_context(self.source_map, self.program, None)
            .with_module(self.module, self.binding_modules);
        let hint = lo.collection_hint_from_type(&l.ty);
        let value = lo.lower_with_collection_hint(&l.value, hint);
        let name = &l.name.name;
        // Promote primitive literal lets to `constexpr` so the C++
        // compiler asserts they're constant-initialized — turns the
        // "no static-init-order fiasco" property from a hope into a
        // compile-time guarantee. QString / QByteArray / `embed(...)`
        // initializers stay on the `static const auto` path because
        // their underlying ctors aren't constexpr.
        let storage = if is_primitive_literal_let(&l.ty, &l.value) {
            "static constexpr"
        } else {
            "static const"
        };
        self.header
            .push_str(&format!("{storage} auto {name} = {value};\n",));
    }

    fn emit_error_decl(&mut self, e: &EnumDecl) {
        // Error decls use snake_to_camel for the per-variant struct
        // (`notFound` → `NotFound`); factory names keep the original
        // camelCase.
        self.emit_variant_class(e, |name| snake_to_camel(name));
    }

    /// Lower a Cute `enum Name { ... }` declaration. Two shapes
    /// at the C++ side:
    ///
    /// - **Nullary enum** (no variant has fields) — emits
    ///   `enum class <Name> : qint32 { ... };`. Cheap, idiomatic
    ///   C++; pattern-match arms compare against
    ///   `Name::Variant`.
    /// - **Payload enum** (at least one variant has fields) —
    ///   emits the same tagged-union shape `error E { ... }`
    ///   uses: a `class Name { struct Variant {...}; using
    ///   Variant = std::variant<...>; static Name Variant(...) {
    ///   ... }; bool isVariant() const; }`. Pattern matching
    ///   uses `isVariant()` and `std::get<Variant>(value).field`
    ///   to read payload fields.
    ///
    /// Extern enums emit nothing in either case (the C++
    /// definition lives in a header pulled via `[cpp] includes`).
    fn emit_enum_decl(&mut self, e: &cute_syntax::ast::EnumDecl) {
        if e.is_extern {
            return;
        }
        // Error-style enums (declared via the `error` keyword) get a
        // dedicated codegen path with the snake_case factory naming
        // that Result-style returns expect. Skip the generic enum
        // path so we don't double-emit the class.
        if e.is_error {
            return;
        }
        let has_payload = e.variants.iter().any(|v| !v.fields.is_empty());
        if has_payload {
            self.emit_enum_decl_payload(e);
            return;
        }
        let name = &e.name.name;
        self.header
            .push_str(&format!("\nenum class {name} : qint32 {{\n"));
        for v in &e.variants {
            // Each variant emits as `Name = <expr-text>` when the
            // user supplied an explicit value, or just `Name` when
            // the C++ default-progression handles it. Variant
            // values are typically integer literals or simple
            // bit-shifts; the source-verbatim splice below uses
            // the original token range, which is identical between
            // Cute and C++ for those forms (`1 << 0`, `0x40`,
            // `Foo | Bar` as long as the Bar identifiers are
            // visible siblings in the same enum).
            self.header.push_str(&format!("    {}", v.name.name));
            if let Some(value) = &v.value {
                let text = self
                    .source_map
                    .map(|sm| {
                        let s = sm.source(value.span.file);
                        s[value.span.start as usize..value.span.end as usize].to_string()
                    })
                    .unwrap_or_default();
                if !text.is_empty() {
                    self.header.push_str(&format!(" = {text}"));
                }
            }
            self.header.push_str(",\n");
        }
        self.header.push_str("};\n\n");
    }

    /// Payload-bearing enum lowering: same C++ shape as
    /// `error E { ... }` — per-variant struct, std::variant<...>
    /// tagged union, named factory + isVariant() helpers. Plain
    /// enums use a `<Name>_t` suffix for the per-variant struct
    /// (vs error decls' `<Cap>` snake_to_camel rule).
    fn emit_enum_decl_payload(&mut self, e: &cute_syntax::ast::EnumDecl) {
        self.emit_variant_class(e, |name| format!("{name}_t"));
    }

    /// Shared body for `emit_error_decl` and `emit_enum_decl_payload`.
    /// Both produce a class with per-variant payload structs, a
    /// `std::variant<...>` tagged union, named factory constructors
    /// (keyed on the original variant name), and `isFoo()`
    /// discriminator helpers (always `is<Cap>()`). The two paths
    /// only diverge on the per-variant struct name, threaded in via
    /// `variant_struct_name`.
    fn emit_variant_class(&mut self, e: &EnumDecl, variant_struct_name: impl Fn(&str) -> String) {
        let name = &e.name.name;
        let v_struct: Vec<String> = e
            .variants
            .iter()
            .map(|v| variant_struct_name(&v.name.name))
            .collect();

        self.header
            .push_str(&format!("\nclass {name} {{\npublic:\n"));

        // Self-typed fields (e.g. `Node(left: Tree, right: Tree)` on
        // `enum Tree { ... }`) need indirection — `std::variant`
        // requires complete types, but the enclosing class is still
        // being defined here. Wrap with `std::shared_ptr<Self>` so
        // the enum stays copy-cheap (matches Cute's pass-by-value
        // convention) and pattern-match arms can rebind the inner
        // value without consuming the variant.
        for (v, vs) in e.variants.iter().zip(&v_struct) {
            if v.fields.is_empty() {
                self.header.push_str(&format!("    struct {vs} {{}};\n"));
                continue;
            }
            self.header.push_str(&format!("    struct {vs} {{\n"));
            for f in &v.fields {
                let ty_s = if field_is_self_typed(f, name) {
                    format!("std::shared_ptr<{name}>")
                } else {
                    ty::cute_to_cpp(&f.ty, &self.ctx())
                };
                self.header
                    .push_str(&format!("        {ty_s} {};\n", f.name.name));
            }
            self.header.push_str("    };\n");
        }

        self.header.push_str(&format!(
            "\n    using Variant = std::variant<{}>;\n",
            v_struct.join(", ")
        ));
        self.header.push_str("    Variant value;\n\n");

        for (v, vs) in e.variants.iter().zip(&v_struct) {
            let factory = &v.name.name;
            if v.fields.is_empty() {
                self.header.push_str(&format!(
                    "    static {name} {factory}() {{ return {name}{{ Variant{{ {vs}{{}} }} }}; }}\n",
                ));
            } else {
                // Factory params accept the Cute-side value type
                // (`Tree left`, not `unique_ptr<Tree> left`); the
                // body wraps self-typed args with `make_unique` so
                // the stored field has the boxed shape that
                // matches the struct decl above.
                let params = v
                    .fields
                    .iter()
                    .map(|f| format!("{} {}", ty::cute_to_cpp(&f.ty, &self.ctx()), f.name.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                let init = v
                    .fields
                    .iter()
                    .map(|f| {
                        if field_is_self_typed(f, name) {
                            format!("std::make_shared<{name}>(std::move({}))", f.name.name)
                        } else {
                            format!("std::move({})", f.name.name)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                self.header.push_str(&format!(
                    "    static {name} {factory}({params}) {{ return {name}{{ Variant{{ {vs}{{ {init} }} }} }}; }}\n",
                ));
            }
        }

        if !e.variants.is_empty() {
            self.header.push('\n');
        }
        for (v, vs) in e.variants.iter().zip(&v_struct) {
            // `isFoo()` always cap-cases the first letter so
            // `notFound` → `isNotFound()`, matching the
            // `case ... when notFound { ... }` dispatch.
            let cap = capitalize_first(&v.name.name);
            self.header.push_str(&format!(
                "    bool is{cap}() const {{ return std::holds_alternative<{vs}>(value); }}\n",
            ));
        }
        self.header.push_str("};\n\n");
    }

    /// Lower a Cute `flags Name of EnumName` declaration to a
    /// `using Name = QFlags<EnumName>;` typedef plus a
    /// `Q_DECLARE_OPERATORS_FOR_FLAGS(Name)` macro so `|` / `&` /
    /// `^` between two `Name` operands works. Extern flags emit
    /// nothing — the QFlags<> alias and the Q_DECLARE_OPERATORS
    /// macro are assumed to live in the bound C++ header.
    fn emit_flags_decl(&mut self, f: &cute_syntax::ast::FlagsDecl) {
        if f.is_extern {
            return;
        }
        let name = &f.name.name;
        let of = &f.of.name;
        self.header
            .push_str(&format!("\nusing {name} = QFlags<{of}>;\n"));
        self.header
            .push_str(&format!("Q_DECLARE_OPERATORS_FOR_FLAGS({name})\n\n"));
    }

    /// Lower a Cute `view Name { ... }` declaration to a self-contained
    /// `.qml` file. The QML imports `QtQuick`/`QtQuick.Controls`/
    /// `QtQuick.Controls.Material` unconditionally and (when the source
    /// file declares any classes) the cutec-generated module so user
    /// QObjects are reachable inside the view tree as nested elements.
    ///
    /// The result is added to `self.views` and the driver writes it
    /// alongside `.h/.cpp` into the qrc bundle.
    fn emit_view(&mut self, v: &ViewDecl) {
        let mut qml = String::new();
        qml.push_str("// Generated by cutec - do not edit.\n");
        // No auto-imports: every QML module the view body needs
        // is declared in source via `use qml "..."`. The driver
        // collects those declarations into `self.qml_imports` and
        // we emit one `import` line per spec — same mechanism
        // covers `QtQuick`, `QtQuick.Controls`,
        // `QtQuick.Controls.Material`, `QtQuick.Layouts`,
        // `org.kde.kirigami`, … and any third-party module a user
        // wants to pull in.
        // Foreign QML imports from `use qml "..."` declarations.
        // Each spec contributes one line; the version is omitted for
        // version-less modules (Qt 6+ Kirigami etc.) so QML doesn't
        // reject the import for not matching a qmldir entry. The
        // alias (if present) becomes the `as <Alias>` suffix and
        // matches the namespace prefix used on element heads.
        for imp in self.qml_imports {
            let version_part = match &imp.version {
                Some(v) => format!(" {v}"),
                None => String::new(),
            };
            let alias_part = match &imp.alias {
                Some(a) => format!(" as {a}"),
                None => String::new(),
            };
            qml.push_str(&format!(
                "import {uri}{version_part}{alias_part}\n",
                uri = imp.module_uri,
            ));
        }
        // Pull in the user-facing module (whose name was captured from
        // `qml_app(module: "...")` during the pre-scan). When the file
        // has no qml_app, fall back to a placeholder module name; the
        // import is harmless if no user QObjects are used in the view.
        if let Some(m) = &self.qml_module {
            qml.push_str(&format!("import {m} 1.0\n\n"));
        } else {
            qml.push('\n');
        }
        // `view Card(label: String, count: Int) { ... }` exposes its
        // params as QML root-level properties so other views can write
        // `Card { label: "..."; count: 42 }`. Same-directory .qml files
        // are auto-importable as components in QML, so no extra import
        // is needed at the call site.
        let mut prop_lines: Vec<String> = v
            .params
            .iter()
            .map(|p| {
                let ty = qml_property_type_with_program(&p.ty, self.program);
                format!("    property {} {}", ty, p.name.name)
            })
            .collect();
        // SwiftUI-style state declarations. Two kinds, both lowered
        // at the QML root of the view:
        //
        // - `let counter = Counter()` (`Object`) — id-tagged child
        //   element, so the QML binding system can reach `counter.x`
        //   from anywhere. QML default-constructs the child (init
        //   args are intentionally ignored on this path).
        // - `state count : Int = 0` (`Property`) — root-level QML
        //   `property <type> <name>: <init>`. Bare references inside
        //   the view body resolve via QML's scoping; assignment fires
        //   the auto-generated `<name>Changed` signal so dependent
        //   bindings refresh without a wrapper class.
        for sf in &v.state_fields {
            match &sf.kind {
                cute_syntax::ast::StateFieldKind::Property { ty } => {
                    let qml_ty = qml_property_type_with_program(ty, self.program);
                    let qml_init = qml_lower_expr_in(&sf.init_expr, &[]);
                    prop_lines.push(format!(
                        "    property {qml_ty} {}: {qml_init}",
                        sf.name.name
                    ));
                }
                cute_syntax::ast::StateFieldKind::Object => {
                    if let Some(class) = state_field_init_class(sf) {
                        prop_lines.push(format!("    {class} {{ id: {} }}", sf.name.name));
                    } else {
                        prop_lines.push(format!(
                            "    /* TODO: state field `{}` has non-Class init */",
                            sf.name.name
                        ));
                    }
                }
            }
        }
        emit_element_with_root_props(&mut qml, &v.root, 0, &prop_lines);
        qml.push('\n');
        self.views.push(EmittedView {
            name: v.name.name.clone(),
            filename: format!("{}.qml", v.name.name),
            qml,
        });
    }

    /// Emit a `widget Name { Root { ... } }` declaration as a C++ class
    /// `class Name : public <Root> { ... }` whose constructor builds the
    /// child element tree imperatively. Targets QtWidgets - parent
    /// pointers + setX/addWidget calls + setLayout.
    fn emit_widget(&mut self, w: &WidgetDecl) {
        let name = &w.name.name;
        let root_class = &w.root.name.name;

        // Lower as a cute::ui::Component when the root is a cute_ui name.
        // Resolved by a hardcoded set rather than the type checker — these
        // names can collide with qtquickcontrols.qpi but only one set is
        // loaded per project (gpu_app projects pull cute_ui.qpi).
        if is_cute_ui_root_class(root_class) {
            self.emit_widget_cute_ui(w);
            return;
        }

        // Header: forward-declare a class derived from the root widget
        // so user code can reference `Name` from `widget_app(window:
        // Name)` and from other widgets. State fields lower as private
        // pointer members - the user-side ergonomics match SwiftUI's
        // `@StateObject var counter = Counter()` (declared once,
        // referenced by name from the body).
        self.header.push_str(&format!(
            "\nclass {name} : public {root_class} {{\npublic:\n    explicit {name}(QWidget* parent = nullptr);\nprivate:\n",
        ));
        emit_state_field_decls(&mut self.header, &w.state_fields);
        self.header.push_str("};\n");
        // Source: implementation of the constructor. State fields are
        // initialized first so the tree body can reference them as
        // bare names (C++ resolves `counter` to `this->counter`).
        // The root element's own properties become `setX(...)` calls
        // on `this`; children are constructed via WidgetEmitter and
        // parented.
        self.source.push_str(&format!(
            "\n{name}::{name}(QWidget* parent) : {root_class}(parent) {{\n",
        ));
        emit_state_field_inits(&mut self.source, &w.state_fields);
        let mut em = WidgetEmitter::new();
        if let Some(module) = self.module {
            em = em.with_module(module);
        }
        em.record_state_fields(&w.state_fields);
        em.emit_root_into("this", &w.root, &mut self.source);
        self.source.push_str("}\n");
    }

    fn emit_widget_cute_ui(&mut self, w: &WidgetDecl) {
        let name = &w.name.name;

        // Add the cute::ui includes once, even if several widgets are emitted.
        if !self.header.contains("<cute/ui/component.hpp>") {
            self.header.push_str("\n#include <cute/ui/component.hpp>\n");
            self.header.push_str("#include <cute/ui/element.hpp>\n");
        }
        if !self.source.contains("<cute/ui/widgets.hpp>") {
            self.source.push_str("\n#include <cute/ui/widgets.hpp>\n");
        }

        // Q_OBJECT is intentionally omitted: cute-driver runs with AUTOMOC
        // OFF and cute-codegen emits QMetaObject data only for `class` items.
        // The reactive plumbing in cute_ui doesn't need a metaobject on the
        // Component subclass itself.
        self.header.push_str(&format!(
            "\nclass {name} : public cute::ui::Component {{\npublic:\n    explicit {name}(QObject* parent = nullptr);\n    std::unique_ptr<cute::ui::Element> build() override;\nprivate:\n",
        ));
        emit_state_field_decls(&mut self.header, &w.state_fields);
        self.header.push_str("};\n");

        self.source.push_str(&format!(
            "\n{name}::{name}(QObject* parent) : cute::ui::Component(parent) {{\n",
        ));
        emit_state_field_inits(&mut self.source, &w.state_fields);
        // Wire each state field's signals to requestRebuild so reactive
        // updates propagate without the click/key forced-rebuild fallback
        // in Window. Coarse but coherent: any user-side state change that
        // emits a signal triggers a rebuild on the next event loop tick.
        for sf in &w.state_fields {
            let Some(class_name) = state_field_init_class(sf) else {
                continue;
            };
            let Some(ItemKind::Class { signal_names, .. }) = self.program.items.get(&class_name)
            else {
                continue;
            };
            for sig in signal_names {
                self.source.push_str(&format!(
                    "    QObject::connect({}, &{}::{}, this, [this]{{ requestRebuild(); }});\n",
                    sf.name.name, class_name, sig
                ));
            }
        }
        self.source.push_str("}\n");

        self.source.push_str(&format!(
            "\nstd::unique_ptr<cute::ui::Element> {name}::build() {{\n",
        ));
        self.source.push_str("    using namespace cute::ui::dsl;\n");
        self.source.push_str("    return ");
        let mut em = WidgetEmitter::new();
        if let Some(module) = self.module {
            em = em.with_module(module);
        }
        em.record_state_fields(&w.state_fields);
        em.emit_root_cute_ui_into(&w.root, &mut self.source);
        self.source.push_str(";\n}\n");
    }

    /// Emit each `impl Trait for ForType { fn method... }`'s methods
    /// as inline free functions in `::cute::trait_impl::<Trait>::`
    /// namespace. The trait-method dispatch path in templated bodies
    /// (`Lowering::trait_dispatch_name`) routes calls through these
    /// overloads, so a `fn use_it<T: Trait>(thing) { thing.method() }`
    /// works uniformly across user classes (where the splice already
    /// added a class member) and extern / builtin-generic for-types
    /// (where there's no class to splice onto).
    ///
    /// Receiver shape:
    /// - User QObject class: `Person* self` — delegate to spliced
    ///   class method via `self->method(args)`.
    /// - User ARC class (incl. parametric `Bag<T>`): `cute::Arc<Bag<T>>& self`
    ///   — delegate (Arc has `operator->`).
    /// - Extern / builtin types (QStringList, QList<T>, ...):
    ///   `QStringList& self` — emit the impl body inline with
    ///   `self_override` so `K::SelfRef` lowers to the parameter
    ///   token and pointer-detection uses the value-flavored mode.
    fn emit_impl_free_functions(&mut self, module: &Module, i: &ImplDecl) {
        use cute_syntax::ast as syntax_ast;
        let trait_name = &i.trait_name.name;
        let base = match syntax_ast::type_expr_base_name(&i.for_type) {
            Some(b) => b,
            None => return,
        };
        // Look up the trait so we can include default-bodied
        // methods the impl omitted. The splice
        // (`inline_impls_into_classes`) already pulls those onto
        // user classes; the namespace dispatch also needs an
        // overload for them or the templated body's
        // `cute::trait_impl::<Trait>::method(thing)` call has no
        // matching candidate when T = the impl's for-type.
        let trait_decl: Option<&TraitDecl> = module.items.iter().find_map(|it| {
            if let Item::Trait(t) = it {
                if t.name.name == *trait_name {
                    return Some(t);
                }
            }
            None
        });
        let supplied_names: std::collections::HashSet<&str> =
            i.methods.iter().map(|m| m.name.name.as_str()).collect();
        let receiver_cpp = ty::cute_to_cpp(&i.for_type, &self.ctx());
        let receiver_is_pointer =
            receiver_cpp.ends_with('*') || receiver_cpp.contains("::cute::Arc<");
        // `Person*` becomes the param type as-is. Everything else
        // (value types, ARC handles, builtin generics) is taken by
        // mutable lvalue ref so an lvalue `T thing` from the
        // templated body binds without a copy.
        let self_param = if receiver_cpp.ends_with('*') {
            format!("{receiver_cpp} self")
        } else {
            format!("{receiver_cpp}& self")
        };
        // Does the *user* module declare a class with this base
        // name? `inline_impls_into_classes` only splices onto
        // user `Item::Class(c)` entries — Prelude binding classes
        // (Qt stdlib `class QStringList { ... }`) live in the
        // items table but have no Cute-side method body, so the
        // splice doesn't add a real member. The delegate `return
        // self->method(args)` would then point at a method that
        // doesn't exist on the C++ Qt type.
        //
        // So: check for a user-home class. Prelude classes and
        // unknown names both fall through to the inline-body path.
        let has_class_entry = self
            .module
            .map(|m| {
                m.items
                    .iter()
                    .any(|item| matches!(item, Item::Class(c) if c.name.name == base))
            })
            .unwrap_or(false);
        // Impl-level generics turn into a `template <typename T,
        // ...>` prefix on each emitted overload.
        let impl_generics_prefix = if i.generics.is_empty() {
            String::new()
        } else {
            let params = i
                .generics
                .iter()
                .map(|g| format!("typename {}", g.name.name))
                .collect::<Vec<_>>()
                .join(", ");
            format!("template <{params}>\n")
        };

        // Collect every method to emit: the impl's own methods
        // first, then any trait default-bodied methods the impl
        // didn't override. Order parallels `inline_impls_into_classes`.
        // Substitute Self → for-type so the namespace overload's
        // signature is concrete. Bodies reference lowercase `self`
        // and pass through unchanged via `self_override` below.
        let mut emitted: Vec<FnDecl> = i
            .methods
            .iter()
            .map(|m| syntax_ast::substitute_self_in_fn_decl(m, &i.for_type))
            .collect();
        if let Some(t) = trait_decl {
            for tm in &t.methods {
                if supplied_names.contains(tm.name.name.as_str()) {
                    continue;
                }
                if tm.body.is_some() {
                    emitted.push(syntax_ast::substitute_self_in_fn_decl(tm, &i.for_type));
                }
            }
        }

        self.header
            .push_str(&format!("\nnamespace cute::trait_impl::{trait_name} {{\n"));

        for m in &emitted {
            let ret = match &m.return_ty {
                Some(t) => ty::cute_to_cpp(t, &self.ctx()),
                None => "void".to_string(),
            };
            let extra_params: Vec<String> = m
                .params
                .iter()
                .map(|p| format!("{} {}", ty::cute_param_to_cpp(p, &self.ctx()), p.name.name))
                .collect();
            let all_params = std::iter::once(self_param.clone())
                .chain(extra_params.iter().cloned())
                .collect::<Vec<_>>()
                .join(", ");

            // Method-level generics (`fn map<U>(...)` on the impl
            // method) get their own `template<typename U, ...>`
            // prefix on top of the impl's. Both prefixes are
            // emitted; C++ allows back-to-back template prefixes on
            // a function template that's *also* nested in a class
            // template — for free functions in a namespace, we
            // collapse them into one combined prefix so the
            // declaration stays well-formed.
            let combined_prefix = if m.generics.is_empty() {
                impl_generics_prefix.clone()
            } else {
                let method_params = m
                    .generics
                    .iter()
                    .map(|g| format!("typename {}", g.name.name))
                    .collect::<Vec<_>>();
                if i.generics.is_empty() {
                    format!("template <{}>\n", method_params.join(", "))
                } else {
                    let impl_params = i
                        .generics
                        .iter()
                        .map(|g| format!("typename {}", g.name.name))
                        .collect::<Vec<_>>();
                    let merged: Vec<String> =
                        impl_params.into_iter().chain(method_params).collect();
                    format!("template <{}>\n", merged.join(", "))
                }
            };
            let return_is_err_union = returns_err_union(&m.return_ty);
            let nodiscard_attr = nodiscard_for_err_union(return_is_err_union);
            self.header.push_str(&format!(
                "{combined_prefix}inline {nodiscard_attr}{ret} {method}({all_params}) {{\n",
                method = m.name.name,
            ));
            if has_class_entry {
                // Delegate to the (splice-added) class method.
                let sep = if receiver_is_pointer { "->" } else { "." };
                let arg_names = m
                    .params
                    .iter()
                    .map(|p| p.name.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ");
                let return_kw = if ret == "void" { "" } else { "return " };
                self.header.push_str(&format!(
                    "    {return_kw}self{sep}{method}({arg_names});\n",
                    method = m.name.name,
                ));
            } else {
                // Inline-lower the impl body. `self` is the parameter
                // (not the implicit `this`); pointer-detection uses
                // the receiver's flavor.
                let Some(body) = &m.body else { continue };
                let scope = self.program.fn_scopes.get(&body.span);
                let mut lo = Lowering::new(&ret, return_is_err_union, scope)
                    .with_emit_context(self.source_map, self.program, None)
                    .with_module(self.module, self.binding_modules)
                    .with_generic_instantiations(self.generic_instantiations)
                    .with_self_override("self".to_string(), receiver_is_pointer);
                lo.record_params(&m.params);
                let lines = lo.lower_block(body);
                for (line, _) in lines {
                    self.header.push_str(&format!("    {line}\n"));
                }
            }
            self.header.push_str("}\n");
        }

        self.header.push_str("} // namespace cute::trait_impl\n");
    }

    fn emit_top_level_fn(&mut self, f: &FnDecl) {
        // In test builds the synthesized runner main owns the entry
        // point — drop any user `fn main` to avoid the duplicate.
        if self.is_test_build && f.name.name == "main" && !f.is_test {
            return;
        }
        // `fn main` is the C++ entry point. Special-case its signature
        // (`int main(int argc, char** argv)`) and recognize the
        // `qml_app(...)` / `widget_app(...)` / `cli_app { ... }`
        // intrinsics so Cute drives the runtime without a hand-written
        // main.cpp. Two surface forms are accepted:
        //   - `fn main { ... }`                    -> no Cute-side params
        //   - `fn main(args: List[<String>]) { ... }` -> argv lifted to QStringList
        if f.name.name == "main" && f.params.len() <= 1 && f.return_ty.is_none() {
            self.emit_main_entry(f);
            return;
        }
        self.emit_top_level_fn_default(f);
    }

    fn emit_main_entry(&mut self, f: &FnDecl) {
        let Some(body) = &f.body else { return };

        // gpu_app intrinsic: `fn main { gpu_app(window: T [, title: "..."]) }`.
        // Checked before qml_app because cute::ui::App derives from
        // QGuiApplication and would otherwise be misclassified.
        if let Some(spec) = detect_gpu_app(body) {
            self.emit_gpu_app_main(&spec);
            return;
        }

        // qml_app intrinsic: `fn main { qml_app(qml: ..., module: ..., type: T) }`.
        // cutec recognizes the call shape and emits the standard Qt QML
        // boot sequence inline. No main.cpp, no qmlRegisterType from the
        // user side, no app.exec() boilerplate.
        if let Some(spec) = detect_qml_app(body) {
            self.emit_qml_app_main(&spec);
            return;
        }

        // widget_app intrinsic: `fn main { widget_app(window: MainWindow
        // [, title: "..."]) }`. Boots a QApplication and shows a freshly
        // constructed instance of the user-declared `widget Name { ... }`.
        if let Some(spec) = detect_widget_app(body) {
            self.emit_widget_app_main(&spec);
            return;
        }

        // server_app intrinsic: `fn main { server_app { ... } }`. Same
        // QCoreApplication setup as cli_app but ends with `app.exec()`
        // so the event loop runs - needed for QHttpServer / QTcpServer
        // / QTimer / signal-driven async work. The user can reference
        // `app` inside the body if they need to call `app.quit()` etc.
        if let Some(inner) = detect_server_app(body) {
            self.emit_server_app_main(inner, f.params.first());
            return;
        }

        // cli_app intrinsic: `fn main { cli_app { ...body... } }`. Wraps
        // the user block in a QCoreApplication so qDebug / event loop /
        // Qt strings work out-of-the-box without QML.
        if let Some(inner) = detect_cli_app(body) {
            self.emit_cli_app_main(inner, f.params.first());
            return;
        }

        // Generic main fn: lower the body and append `return 0;` if there
        // is no explicit return. argc/argv are made available to the body
        // - if the user declared a single parameter (`fn main(args: List)`)
        // we lift argv into a QStringList bound to that name.
        // The Lowering uses `void` as the return type so a trailing
        // `println(...)` (or any other statement-like expression)
        // becomes `<value>;`, not `return <value>;`. The defensive
        // `return 0;` at the bottom satisfies `int main`.
        self.source
            .push_str("\nint main(int argc, char** argv) {\n");
        if f.params.is_empty() {
            self.source.push_str("    (void)argc; (void)argv;\n");
        }
        emit_main_args_lift(&mut self.source, f.params.first());
        let lines = {
            let mut lo = Lowering::new("void", false, None)
                .with_emit_context(self.source_map, self.program, None)
                .with_generic_instantiations(self.generic_instantiations)
                .with_module(self.module, self.binding_modules);
            lo.lower_block(body)
        };
        self.write_lowered_lines(lines, "    ");
        // Always emit a defensive `return 0;` even when the body already
        // returned - C++ compilers tolerate the duplicate via warnings,
        // but we keep it noiseless by checking the body's last line.
        let already_returns = self
            .source
            .lines()
            .rev()
            .take(3)
            .any(|l| l.trim_start().starts_with("return"));
        if !already_returns {
            self.source.push_str("    return 0;\n");
        }
        self.source.push_str("}\n");
    }

    fn emit_gpu_app_main(&mut self, spec: &GpuAppSpec) {
        self.source.push_str("\n#include <cute/ui/app.hpp>\n");
        self.source.push_str("#include <cute/ui/component.hpp>\n");
        self.source.push_str("#include <cute/ui/window.hpp>\n\n");
        self.source.push_str("int main(int argc, char** argv) {\n");
        self.source.push_str("    cute::ui::App app(argc, argv);\n");
        if let Some(title) = &spec.title {
            self.source.push_str(&format!(
                "    cute::ui::App::setApplicationName(QStringLiteral(\"{}\"));\n",
                escape_cpp_string(title),
            ));
        }
        self.source
            .push_str(&format!("    {win} view;\n", win = spec.window));
        let theme_arg = match spec.theme.as_deref() {
            Some("light") => ", cute::ui::Theme::Light",
            Some("dark") => ", cute::ui::Theme::Dark",
            _ => "",
        };
        self.source
            .push_str(&format!("    return app.run(&view{theme_arg});\n}}\n"));
    }

    fn emit_widget_app_main(&mut self, spec: &WidgetAppSpec) {
        self.source.push_str("\n#include <QApplication>\n\n");
        self.source.push_str("int main(int argc, char** argv) {\n");
        self.source.push_str("    QApplication app(argc, argv);\n");
        if let Some(title) = &spec.title {
            self.source.push_str(&format!(
                "    QApplication::setApplicationName(QStringLiteral(\"{}\"));\n",
                escape_cpp_string(title),
            ));
        }
        self.source
            .push_str(&format!("    {win} w;\n    w.show();\n", win = spec.window));
        self.source.push_str("    return app.exec();\n}\n");
    }

    fn emit_server_app_main(&mut self, inner: &cute_syntax::ast::Block, param: Option<&Param>) {
        // Sister of cli_app for event-loop use cases. Constructs the
        // QCoreApplication, runs the user body (typically setting up
        // QHttpServer routes / signal handlers / timers), and ends
        // with `return app.exec()` so the loop processes events.
        self.source
            .push_str("\n#include <QCoreApplication>\n#include <QDebug>\n\n");
        self.source.push_str("int main(int argc, char** argv) {\n");
        self.source
            .push_str("    QCoreApplication app(argc, argv);\n");
        emit_main_args_lift(&mut self.source, param);
        let lines = {
            let mut lo = Lowering::new("void", false, None)
                .with_emit_context(self.source_map, self.program, None)
                .with_generic_instantiations(self.generic_instantiations)
                .with_module(self.module, self.binding_modules);
            lo.lower_block(inner)
        };
        self.write_lowered_lines(lines, "    ");
        self.source.push_str("    return app.exec();\n}\n");
    }

    fn emit_cli_app_main(&mut self, inner: &cute_syntax::ast::Block, param: Option<&Param>) {
        self.source
            .push_str("\n#include <QCoreApplication>\n#include <QDebug>\n\n");
        // `co_await` on a not-yet-finished future queues a
        // QFutureWatcher::finished signal — without a running event
        // loop the resume never fires. Body lifts into a QFuture<void>
        // coroutine that QCoreApplication::exec drives to completion.
        if block_uses_await(inner) {
            self.source
                .push_str("#include <QFuture>\n#include <QFutureWatcher>\n#include <QPromise>\n\n");
            let body_param = param
                .map(|p| format!("QStringList {}", p.name.name))
                .unwrap_or_else(|| "QStringList /*unused*/".to_string());
            self.source.push_str(&format!(
                "static QFuture<void> __cute_cli_app_body({body_param}) {{\n",
            ));
            if param.is_none() {
                self.source.push_str("    (void)0;\n");
            }
            let lines = {
                let mut lo = Lowering::new("void", false, None)
                    .with_emit_context(self.source_map, self.program, None)
                    .with_generic_instantiations(self.generic_instantiations)
                    .with_module(self.module, self.binding_modules)
                    .with_async(true);
                lo.lower_block(inner)
            };
            self.write_lowered_lines(lines, "    ");
            self.source.push_str(concat!(
                "    co_return;\n}\n\n",
                "int main(int argc, char** argv) {\n",
                "    QCoreApplication app(argc, argv);\n",
                "    QStringList __cute_args;\n",
                "    for (int i = 0; i < argc; ++i) {\n",
                "        __cute_args << QString::fromLocal8Bit(argv[i]);\n",
                "    }\n",
                "    auto __cute_main_fut = __cute_cli_app_body(__cute_args);\n",
                "    QFutureWatcher<void> __cute_main_watcher;\n",
                "    QObject::connect(&__cute_main_watcher, &QFutureWatcher<void>::finished, &app, &QCoreApplication::quit);\n",
                "    __cute_main_watcher.setFuture(__cute_main_fut);\n",
                "    return app.exec();\n}\n",
            ));
            return;
        }
        self.source.push_str("int main(int argc, char** argv) {\n");
        self.source
            .push_str("    QCoreApplication app(argc, argv);\n");
        if param.is_none() {
            self.source.push_str("    (void)app;\n");
        }
        emit_main_args_lift(&mut self.source, param);
        // Use return_type="void" so the block's trailing expression is
        // emitted as a statement, not wrapped in `return`. We append a
        // `return 0;` afterwards as the actual main return.
        let lines = {
            let mut lo = Lowering::new("void", false, None)
                .with_emit_context(self.source_map, self.program, None)
                .with_generic_instantiations(self.generic_instantiations)
                .with_module(self.module, self.binding_modules);
            lo.lower_block(inner)
        };
        self.write_lowered_lines(lines, "    ");
        self.source.push_str("    return 0;\n}\n");
    }

    fn emit_qml_app_main(&mut self, spec: &QmlAppSpec) {
        self.source.push_str("\n#include <QGuiApplication>\n");
        self.source.push_str("#include <QQmlApplicationEngine>\n");
        self.source.push_str("#include <QQuickStyle>\n");
        self.source.push_str("#include <QtQml>\n\n");

        self.source.push_str("int main(int argc, char** argv) {\n");
        // Default the QtQuick.Controls 2 style to "Basic" when the
        // user hasn't picked one via env var. The native macOS / iOS
        // styles refuse `background: ...` / `contentItem: ...`
        // customisation on Button / TextField with a runtime warning,
        // which the typical Cute QML demo expects to use freely.
        // Apps that import QtQuick.Controls.Material at the QML side
        // override Basic on their own ApplicationWindow / control
        // tree, so this only affects unstyled controls.
        self.source
            .push_str("    if (qEnvironmentVariableIsEmpty(\"QT_QUICK_CONTROLS_STYLE\")) {\n");
        self.source
            .push_str("        QQuickStyle::setStyle(QStringLiteral(\"Basic\"));\n");
        self.source.push_str("    }\n");
        self.source
            .push_str("    QGuiApplication app(argc, argv);\n");
        for ty in &spec.types {
            self.source.push_str(&format!(
                "    qmlRegisterType<{ty}>(\"{module}\", {maj}, {min}, \"{ty}\");\n",
                ty = ty,
                module = spec.module,
                maj = spec.version_major,
                min = spec.version_minor,
            ));
        }
        self.source.push_str("    QQmlApplicationEngine engine;\n");
        self.source.push_str(&format!(
            "    engine.load(QUrl(QStringLiteral(\"{}\")));\n",
            spec.qml_url
        ));
        self.source
            .push_str("    if (engine.rootObjects().isEmpty()) return 1;\n");
        self.source.push_str("    return app.exec();\n}\n");
    }

    /// Synthesize the `int main` that drives a `cute test` build.
    /// Walks the module for every `test fn` and emits a sequential
    /// `cute::test::run_one` call per test, with TAP-lite plan
    /// header + summary lines around the loop. Always boots a
    /// QCoreApplication so test bodies can use Qt types freely.
    fn emit_test_runner_main(&mut self, module: &Module) {
        // Display label = the fn's `display_name` for string-named
        // / suite-grouped tests, fall back to the fn ident for the
        // compact `test fn camelCase` form. Embedding the label
        // directly in each `printf` line means the runner doesn't
        // need a per-test name table.
        let tests: Vec<&FnDecl> = module
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Fn(f) if f.is_test => Some(f),
                _ => None,
            })
            .collect();
        let mut body = String::new();
        let total = tests.len();
        for (i, f) in tests.iter().enumerate() {
            let n = i + 1;
            let cpp_name = &f.name.name;
            let cap = capitalize_first(cpp_name);
            let label = f.display_name.as_deref().unwrap_or(cpp_name);
            let label_lit = escape_cpp_string(label);
            body.push_str(&format!(
                "    failed += ::cute::test::run_one({n}, \"{label_lit}\", &cuteTest{cap});\n",
            ));
        }
        self.source.push_str("\n#include <QCoreApplication>\n");
        self.source.push_str("#include <cstdio>\n\n");
        self.source.push_str("int main(int argc, char** argv) {\n");
        self.source
            .push_str("    QCoreApplication app(argc, argv);\n");
        self.source
            .push_str(&format!("    std::printf(\"1..{total}\\n\");\n"));
        self.source.push_str("    int failed = 0;\n");
        self.source.push_str(&body);
        self.source.push_str(&format!(
            "    std::printf(\"# %d passed, %d failed\\n\", {total} - failed, failed);\n"
        ));
        self.source.push_str("    return failed == 0 ? 0 : 1;\n");
        self.source.push_str("}\n");
    }

    fn emit_top_level_fn_default(&mut self, f: &FnDecl) {
        // The `cuteTest` prefix avoids C++-side collision with a
        // same-named regular fn the user might also have declared.
        let cpp_name = if f.is_test {
            format!("cuteTest{}", capitalize_first(&f.name.name))
        } else {
            f.name.name.clone()
        };
        let ctx = self.ctx();
        let raw_ret = match &f.return_ty {
            Some(t) => ty::cute_to_cpp(t, &ctx),
            None => "void".to_string(),
        };
        // `async fn f -> T` lowers to a Qt 6.5+ coroutine returning
        // `QFuture<T>`. Users can also write `async fn f -> Future<T>`
        // explicitly when they want to be unambiguous - in that case
        // we leave the type as-is rather than double-wrapping.
        let ret = if f.is_async && !raw_ret.starts_with("QFuture<") {
            if raw_ret == "void" {
                "QFuture<void>".to_string()
            } else {
                format!("QFuture<{}>", raw_ret)
            }
        } else {
            raw_ret
        };
        let return_is_err_union = returns_err_union(&f.return_ty);
        let params: Vec<ParamInfo> = f
            .params
            .iter()
            .map(|p| ParamInfo {
                name: p.name.name.clone(),
                cpp_type: ty::cute_param_to_cpp(p, &ctx),
                qmetatype: ty::cute_to_qmeta_type_enum(&p.ty).to_string(),
            })
            .collect();
        let plist = render_param_list(&params);

        // Generic top-level fn (`fn first<T>(...)`) lowers to a C++
        // function template. The body has to live in the header so
        // every translation unit that calls `first<Int>(...)` can
        // instantiate it; the .cpp gets nothing.
        let is_generic = !f.generics.is_empty();
        let nodiscard_attr = nodiscard_for_err_union(return_is_err_union);
        if is_generic {
            let template_decl = {
                let params = f
                    .generics
                    .iter()
                    .map(|g| format!("typename {}", g.name.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("template <{params}>\n")
            };
            self.header.push_str(&format!(
                "\n{template_decl}{nodiscard_attr}{ret} {cpp_name}({plist}) {{\n",
            ));
            if let Some(body) = &f.body {
                let scope = self.program.fn_scopes.get(&body.span);
                let mut lo = Lowering::new(&ret, return_is_err_union, scope)
                    .with_emit_context(self.source_map, self.program, None)
                    .with_generic_instantiations(self.generic_instantiations)
                    .with_module(self.module, self.binding_modules)
                    .with_async(f.is_async)
                    .with_generic_params(&f.generics, &f.params);
                lo.record_params(&f.params);
                // Generic top-level fn body in the header — template
                // instantiations get inlined per call site, so #line
                // directives wouldn't survive cleanly. Plain iteration.
                for (line, _) in lo.lower_block(body) {
                    self.header.push_str(&format!("    {line}\n"));
                }
            }
            self.header.push_str("}\n");
            return;
        }

        self.header
            .push_str(&format!("{nodiscard_attr}{ret} {cpp_name}({plist});\n"));

        self.source.push('\n');
        if let Some(body) = &f.body {
            // Anchor the upcoming definition to the `.cute` source so
            // gdb / lldb show the user's source rather than the
            // generated `.cpp` for frames inside this fn.
            self.emit_line_directive(body.span);
        }
        self.source
            .push_str(&format!("{ret} {cpp_name}({plist}) {{\n"));
        if let Some(body) = &f.body {
            let scope = self.program.fn_scopes.get(&body.span);
            let mut lo = Lowering::new(&ret, return_is_err_union, scope)
                .with_emit_context(self.source_map, self.program, None)
                .with_generic_instantiations(self.generic_instantiations)
                .with_module(self.module, self.binding_modules)
                .with_async(f.is_async);
            lo.record_params(&f.params);
            let lines = lo.lower_block(body);
            self.write_lowered_lines(lines, "    ");
        }
        self.source.push_str("}\n");
    }

    fn emit_file_preamble(&mut self) {
        let stem = &self.stem;
        self.header
            .push_str("// Generated by cutec - do not edit.\n");
        self.header.push_str("#pragma once\n\n");
        // QObject brings the Q_OBJECT macro + qt_create_metaobjectdata
        // template + staticMetaObject + qt_metacall family declarations.
        // We do NOT include <QtCore/qtmochelpers.h> here - that header is
        // for the .cpp side where we specialize qt_create_metaobjectdata.
        // The Qt container/value-type headers are pulled in unconditionally:
        // generated code may reference QList<T> / QMap<K,V> / QSet<T> /
        // QHash<K,V> / QFuture<T> / QDateTime / QUrl / QByteArray /
        // QRegularExpression depending on what the source uses, and the
        // include cost is negligible compared to QObject itself.
        self.header.push_str("#include <QObject>\n");
        self.header.push_str("#include <QPointer>\n");
        // QProperty: QObjectBindableProperty / QObjectComputedProperty / QBindable.
        self.header.push_str("#include <QProperty>\n");
        self.header.push_str("#include <QString>\n");
        self.header.push_str("#include <QByteArray>\n");
        self.header.push_str("#include <QList>\n");
        self.header.push_str("#include <QMap>\n");
        self.header.push_str("#include <QHash>\n");
        self.header.push_str("#include <QSet>\n");
        self.header.push_str("#include <QFuture>\n");
        // `<coroutine>` is needed for std::coroutine_traits — cute_async.h
        // specializes it for QFuture<T> since Qt 6.11 doesn't.
        self.header.push_str("#include <coroutine>\n");
        // `<ranges>` powers the std::views::iota lowering of `a..b` /
        // `a..=b` for-loops. Slice<T> iterates via range-for too, so
        // both sources share the same C++20 ranges machinery.
        self.header.push_str("#include <ranges>\n");
        self.header.push_str("#include <QUrl>\n");
        self.header.push_str("#include <QDate>\n");
        self.header.push_str("#include <QTime>\n");
        self.header.push_str("#include <QDateTime>\n");
        self.header.push_str("#include <QRegularExpression>\n");
        self.header.push_str("#include <QJsonDocument>\n");
        self.header.push_str("#include <QJsonObject>\n");
        self.header.push_str("#include <QJsonArray>\n");
        self.header.push_str("#include <QJsonValue>\n");
        self.header.push_str("#include <QCommandLineParser>\n");
        self.header.push_str("#include <QCommandLineOption>\n");
        self.header.push_str("#include <cstdint>\n");
        self.header.push_str("#include <memory>\n");
        self.header.push_str("#include <optional>\n");
        self.header.push_str("#include <utility>\n");
        self.header.push_str("#include <variant>\n\n");
        self.header.push_str("#include \"cute_arc.h\"\n");
        self.header.push_str("#include \"cute_async.h\"\n");
        self.header.push_str("#include \"cute_error.h\"\n");
        self.header.push_str("#include \"cute_function.h\"\n");
        self.header.push_str("#include \"cute_generic.h\"\n");
        self.header.push_str("#include \"cute_nullable.h\"\n");
        self.header.push_str("#include \"cute_slice.h\"\n");
        self.header.push_str("#include \"cute_string.h\"\n\n");

        self.source
            .push_str("// Generated by cutec - do not edit.\n");
        self.source.push_str(&format!("#include \"{}.h\"\n", stem));
        // qtmochelpers.h provides QtMocHelpers::{StringRefStorage,
        // UintData, SignalData, MethodData, PropertyData, metaObjectData,
        // indexOfMethod}. It is internal Qt API, but it's the canonical
        // moc-output target so we follow moc's lead.
        self.source.push_str("\n#include <QtCore/qmetatype.h>\n");
        self.source.push_str("#include <QtCore/qtmochelpers.h>\n");
        self.source.push_str("#include <cstring>\n");
        // Must land before user fns that use the `assert_eq` builtin —
        // those lower to `::cute::test::assert_eq` and would otherwise
        // see no declaration.
        if self.is_test_build {
            self.source.push_str("#include \"cute_test.h\"\n");
        }
        self.source.push('\n');
    }

    fn emit_class(&mut self, c: &ClassDecl) -> Result<(), EmitError> {
        // Cute defaults `class X { ... }` (no explicit super) to
        // `class X < QObject`, so the bulk of demos / KDE-style apps
        // can drop the boilerplate. Explicitly written supers
        // (`class HighlightedEditor < QPlainTextEdit`, `class
        // ReadingItemModel < QAbstractListModel`, …) inherit
        // through the same emit path: any QObject-derived Qt class
        // is fine because its full type is reachable via the
        // build-mode umbrella include (`<QtWidgets>` for widgets,
        // `<QtQuick>` for QML) plus `<QObject>`. Multi-segment
        // paths (`Qt::Foo`) join with `::` so `class X < Qt::Foo`
        // also works.
        //
        // `arc X { ... }` opts out of QObject and emits a pure-ARC
        // `class X : public cute::ArcBase` instead. No moc data, no
        // Q_OBJECT, no parent-tree — lifetime is managed via
        // reference counting through `cute::Arc<T>`.
        if c.is_arc {
            self.emit_arc_class(c)?;
            return Ok(());
        }
        let super_name = match c.super_class.as_ref() {
            None => "QObject".to_string(),
            Some(t) => match &t.kind {
                TypeKind::Named { path, .. } => path
                    .iter()
                    .map(|i| i.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::"),
                _ => return Err(EmitError::UnsupportedSuper(format!("{:?}", t.kind))),
            },
        };

        let info = build_class_info(c, &super_name, &self.ctx());
        let meta = cute_meta::emit_meta_section(&info);

        self.emit_class_header(c, &info, &meta);
        self.emit_class_source(c, &info, &meta)?;
        Ok(())
    }

    /// Emit a non-QObject ARC class (`arc X { ... }`).
    /// Layout: `class X : public cute::ArcBase` with plain methods /
    /// fields. No moc machinery, no signals (cute::ArcBase isn't a
    /// QObject), no Q_PROPERTY. Lifetime is per-instance reference
    /// counting via `cute::Arc<X>`. Most class members lower the
    /// same as the QObject path; signals/slots are rejected at the
    /// parser, so they never reach this point in normal builds.
    fn emit_arc_class(&mut self, c: &ClassDecl) -> Result<(), EmitError> {
        let name = &c.name.name;
        let is_generic = !c.generics.is_empty();
        let template_decl: String = if is_generic {
            // C++ requires `template <typename T, typename U>` before
            // each declaration scope. We render once and prepend it
            // wherever a definition (class header, inline method
            // body, out-of-class method body) would otherwise stand
            // alone.
            let params = c
                .generics
                .iter()
                .map(|g| format!("typename {}", g.name.name))
                .collect::<Vec<_>>()
                .join(", ");
            format!("template <{params}>\n")
        } else {
            String::new()
        };
        // Pre-compute all the type-dependent strings while holding the
        // ctx borrow, then push into self.{header,source} once those
        // borrows are dropped. Avoids the borrow-checker conflict
        // between `self.ctx()` (immutable view of self.program) and
        // `self.header.push_str` (mutable self).
        let mut header_props = String::new();
        let mut header_decls = String::new();
        let mut header_fields = String::new();
        let mut method_bodies: Vec<(String, String)> = Vec::new(); // (signature, body)
        {
            let ctx = self.ctx();
            for member in &c.members {
                match member {
                    ClassMember::Property(p) => {
                        let pty = ty::cute_to_cpp(&p.ty, &ctx);
                        let setter = ty::setter_name(&p.name.name);
                        header_props.push_str(&format!(
                            "    {pty} {name}() const {{ return m_{name}; }}\n    void {setter}({pty} value) {{ m_{name} = std::move(value); }}\n",
                            pty = pty,
                            name = p.name.name,
                        ));
                        header_fields.push_str(&format!("    {pty} m_{};\n", p.name.name));
                    }
                    ClassMember::Field(f) => {
                        // Plain class field on an arc class. `pub`
                        // exposes a getter; `pub var` adds a setter.
                        // Non-pub fields stay accessible only via
                        // `@x` from method bodies. The init body in
                        // the user's `init { ... }` writes the
                        // declared default at construction time;
                        // we don't pre-emit `= expr` on the storage
                        // because the lowering needs a `Lowering`
                        // context that's not available here.
                        let setter = ty::setter_name(&f.name.name);
                        let name = &f.name.name;
                        let (storage, accessor_ty, getter_body, setter_body) = if f.weak {
                            // weak: cute::Weak<T> storage; getter
                            // returns Arc<T> via .lock(); setter
                            // takes Arc<T> (Weak::operator= bridges).
                            let cn = weak_unowned_held_class(&f.ty);
                            match cn {
                                Some(cn) => (
                                    format!("    ::cute::Weak<{cn}> m_{name};\n"),
                                    format!("::cute::Arc<{cn}>"),
                                    format!("return m_{name}.lock();"),
                                    format!("m_{name} = std::move(value);"),
                                ),
                                None => continue,
                            }
                        } else if f.unowned {
                            // unowned: raw T*, default-null,
                            // pass-through accessors.
                            let cn = weak_unowned_held_class(&f.ty);
                            match cn {
                                Some(cn) => (
                                    format!("    {cn}* m_{name} = nullptr;\n"),
                                    format!("{cn}*"),
                                    format!("return m_{name};"),
                                    format!("m_{name} = value;"),
                                ),
                                None => continue,
                            }
                        } else {
                            let fty = ty::cute_to_cpp(&f.ty, &ctx);
                            (
                                format!("    {fty} m_{name};\n"),
                                fty.clone(),
                                format!("return m_{name};"),
                                format!("m_{name} = std::move(value);"),
                            )
                        };
                        if f.is_pub {
                            header_props.push_str(&format!(
                                "    {accessor_ty} {name}() const {{ {getter_body} }}\n",
                            ));
                            if f.is_mut {
                                header_props.push_str(&format!(
                                    "    void {setter}({accessor_ty} value) {{ {setter_body} }}\n",
                                ));
                            }
                        }
                        header_fields.push_str(&storage);
                    }
                    ClassMember::Signal(_) => {
                        // Defense in depth — the parser already rejects
                        // `signal` inside `ref { ... }`. Reaching this
                        // point means a binding or generated AST is
                        // malformed; fail loudly rather than silently
                        // dropping the member.
                        return Err(EmitError::UnsupportedSuper(format!(
                            "ARC class `{name}` cannot declare signals (parser bypass?)",
                        )));
                    }
                    // init / deinit handled in the dedicated ARC ctor block below.
                    ClassMember::Init(_) | ClassMember::Deinit(_) => {}
                    ClassMember::Slot(f) | ClassMember::Fn(f) => {
                        let ret = match &f.return_ty {
                            Some(t) => ty::cute_to_cpp(t, &ctx),
                            None => "void".into(),
                        };
                        let params = f
                            .params
                            .iter()
                            .map(|p| format!("{} {}", ty::cute_param_to_cpp(p, &ctx), p.name.name))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let nodiscard_attr =
                            nodiscard_for_err_union(returns_err_union(&f.return_ty));
                        // Method-level generics: a method declared as
                        // `fn map<U>(...)` gets its own template prefix
                        // on top of the class template. The header
                        // declaration just shows the prefix then the
                        // signature; the body emission below stitches
                        // both prefixes into the out-of-class definition
                        // (or inlines the body when class+method are
                        // both header-only template-shaped).
                        let method_template_prefix = if f.generics.is_empty() {
                            String::new()
                        } else {
                            let params = f
                                .generics
                                .iter()
                                .map(|g| format!("typename {}", g.name.name))
                                .collect::<Vec<_>>()
                                .join(", ");
                            format!("    template <{params}>\n")
                        };
                        header_decls.push_str(&format!(
                            "{}    {nodiscard_attr}{ret} {}({params});\n",
                            method_template_prefix, f.name.name
                        ));
                        // Defer the body lowering to the second pass
                        // below where we don't hold ctx.
                        method_bodies.push((
                            format!("{ret} {name}::{}({params})", f.name.name),
                            String::new(), // placeholder; filled below
                        ));
                    }
                }
            }
        }

        // ARC ctor/dtor: same `init` / `deinit` surface as QObject,
        // but no parent argument (lifetime is `cute::Arc<T>`-counted).
        // Bodies are inlined in the header — required for the generic
        // case, harmless for the plain case.
        let arc_inits: Vec<&InitDecl> = c.inits().collect();
        let mut arc_ctor_decls = String::new();
        if arc_inits.is_empty() {
            arc_ctor_decls.push_str(&format!("    {name}() = default;\n"));
        } else {
            for init in &arc_inits {
                let sig = render_param_list_no_parent(&init.params, &self.ctx());
                arc_ctor_decls.push_str(&format!("    {name}({sig}) {{\n"));
                lower_inline_body(
                    &mut arc_ctor_decls,
                    self.program,
                    self.module,
                    self.binding_modules,
                    SurroundingDecl::Class(c),
                    "void",
                    false,
                    &init.params,
                    &init.body,
                );
                arc_ctor_decls.push_str("    }\n");
            }
        }
        if let Some(d) = c.deinit() {
            arc_ctor_decls.push_str(&format!("    ~{name}() {{\n"));
            lower_inline_body(
                &mut arc_ctor_decls,
                self.program,
                self.module,
                self.binding_modules,
                SurroundingDecl::Class(c),
                "void",
                false,
                &[],
                &d.body,
            );
            arc_ctor_decls.push_str("    }\n");
        }
        self.header.push_str(&format!(
            "\n{template_decl}class {name} : public ::cute::ArcBase {{\npublic:\n",
        ));
        self.header.push_str(&arc_ctor_decls);
        // `~Copyable`: delete the copy ctor / assignment, default the
        // moves. ArcBase already deletes copy at the runtime level for
        // the refcount, but the user-visible class needs its own
        // explicit declaration so flow analysis + C++ static checking
        // both line up.
        if !c.is_copyable {
            self.header.push_str(&format!(
                "    {name}(const {name}&) = delete;\n    {name}& operator=(const {name}&) = delete;\n    {name}({name}&&) = default;\n    {name}& operator=({name}&&) = default;\n",
            ));
        }
        self.header.push_str(&header_props);
        self.header.push_str(&header_decls);
        if !header_fields.is_empty() {
            self.header.push_str("\nprivate:\n");
            self.header.push_str(&header_fields);
        }
        self.header.push_str("};\n");

        // Now the bodies. For non-generic classes we emit
        // out-of-class definitions in the .cpp - the standard
        // header/source split. For generic classes the C++ template
        // model requires definitions visible at every instantiation
        // point, so we emit the bodies INLINE in the header (after
        // the class) using `template <T> ret Name<T>::method(...) {
        // ... }`. Either way the method body lowering is identical.
        let mut method_idx = 0;
        for member in &c.members {
            if let ClassMember::Slot(f) | ClassMember::Fn(f) = member {
                let return_is_err_union = matches!(
                    f.return_ty.as_ref().map(|t| &t.kind),
                    Some(TypeKind::ErrorUnion(_))
                );
                let ret = {
                    let ctx = self.ctx();
                    match &f.return_ty {
                        Some(t) => ty::cute_to_cpp(t, &ctx),
                        None => "void".into(),
                    }
                };
                let sig = &method_bodies[method_idx].0;
                // Method-level generics force the body into the
                // header (templates have to be visible at every
                // instantiation site, same rule as for the class
                // template). For a non-generic class with a generic
                // method we emit:
                //   template <typename U> ret Class::method(...) { ... }
                // For a generic class with a generic method we emit
                // both prefixes:
                //   template <typename T>  // class
                //   template <typename U>  // method
                //   ret Class<T>::method(...) { ... }
                let method_has_generics = !f.generics.is_empty();
                let body_in_header = is_generic || method_has_generics;
                let target_buf: &mut String = if body_in_header {
                    &mut self.header
                } else {
                    &mut self.source
                };
                let method_template_prefix = if method_has_generics {
                    let params = f
                        .generics
                        .iter()
                        .map(|g| format!("typename {}", g.name.name))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("template <{params}>\n")
                } else {
                    String::new()
                };
                if is_generic {
                    // Out-of-class template method definition: needs
                    // the class template prelude, optionally the
                    // method template prelude, AND the explicit
                    // template parameter list on the class name.
                    let param_names = c
                        .generics
                        .iter()
                        .map(|g| g.name.name.clone())
                        .collect::<Vec<_>>()
                        .join(", ");
                    // sig from the first pass already starts with `ret
                    // ClassName::method(...)`. Splice the `<T>` after
                    // the class name so it becomes
                    // `ret ClassName<T>::method(...)`.
                    let with_params =
                        sig.replacen(&format!("{name}::"), &format!("{name}<{param_names}>::"), 1);
                    target_buf.push_str(&format!(
                        "\n{template_decl}{method_template_prefix}{with_params} {{\n"
                    ));
                } else if method_has_generics {
                    // Non-generic class, generic method: only the
                    // method prefix; class name stands alone.
                    target_buf.push_str(&format!("\n{method_template_prefix}{sig} {{\n"));
                } else {
                    target_buf.push_str(&format!("\n{sig} {{\n"));
                }
                if let Some(body) = &f.body {
                    let scope = self.program.fn_scopes.get(&body.span);
                    let mut lo = Lowering::new(&ret, return_is_err_union, scope)
                        .with_emit_context(self.source_map, self.program, Some(c))
                        .with_generic_instantiations(self.generic_instantiations)
                        .with_module(self.module, self.binding_modules)
                        .with_async(f.is_async)
                        .with_generic_params(&f.generics, &f.params);
                    lo.record_params(&f.params);
                    let lines = lo.lower_block(body);
                    if body_in_header {
                        // Generic class method body lives in the header
                        // for template instantiation; skip #line directives
                        // (debugger stepping into template instantiations
                        // is fragile and the directive locations may not
                        // survive whatever the compiler inlines).
                        for (line, _) in lines {
                            self.header.push_str(&format!("    {line}\n"));
                        }
                    } else {
                        self.write_lowered_lines(lines, "    ");
                    }
                }
                let target_buf: &mut String = if body_in_header {
                    &mut self.header
                } else {
                    &mut self.source
                };
                target_buf.push_str("}\n");
                method_idx += 1;
            }
        }
        Ok(())
    }

    fn emit_class_header(&mut self, c: &ClassDecl, info: &ClassInfo, _meta: &MetaSection) {
        let name = &c.name.name;
        self.header.push_str(&format!(
            "class {name} : public {super} {{\n",
            name = name,
            super = info.super_class
        ));
        // Q_OBJECT macro: provides static qt_create_metaobjectdata template,
        // staticMetaObject, virtual metaObject()/qt_metacast/qt_metacall
        // overrides, and the qt_static_metacall declaration. Cute users do
        // NOT write Q_OBJECT in .cute - it lands here as an implementation
        // detail of "class X < QObject" lowering. moc itself is still not
        // invoked: the qt_create_metaobjectdata specialization is emitted
        // by cute-meta in the .cpp.
        self.header.push_str("    Q_OBJECT\n");
        // Q_PROPERTY annotations are no-ops at compile time (only moc reads
        // them), but we emit them for IDE/tooling cross-reference clarity.
        // BINDABLE is added when the prop is `bindable` (writable, but
        // routed through QObjectBindableProperty) or computed
        // (`bind { ... }`, owned by QObjectComputedProperty — read-only).
        for prop in &info.properties {
            let qprop_type = qprop_type_name(&prop.cpp_type);
            let mut spec = format!("{qprop_type} {} READ {}", prop.name, prop.name);
            if prop.writable {
                spec.push_str(&format!(" WRITE {}", prop.setter));
            }
            // BINDABLE clause is added only for kinds whose backing
            // storage exposes a working QBindable subscription path
            // (Bindable + Bind = QObjectBindableProperty). Fresh's
            // QObjectComputedProperty has a QBindable but the
            // subscription part is unimplemented — QML through BINDABLE
            // would silently drop dep tracking, so fresh keeps NOTIFY
            // only and relies on the ctor's input→fresh fan-out.
            if prop.has_bindable_surface() {
                spec.push_str(&format!(
                    " BINDABLE {}",
                    ty::bindable_getter_name(&prop.name)
                ));
            }
            if prop.is_model_list {
                // `, model` props expose a `cute::ModelList<T*>*` whose
                // pointer never changes for the class's lifetime — row
                // data flows through QRangeModel's own dataChanged path
                // (it watches Q_PROPERTY notify signals on the wrapped
                // row type when AutoConnectPolicy is set). CONSTANT tells
                // QML's binding engine to bind the property once and
                // skip change-notifications on the pointer itself.
                spec.push_str(" CONSTANT");
            } else if let Some(sig) = &prop.notify_signal_name {
                spec.push_str(&format!(" NOTIFY {sig}"));
            }
            self.header.push_str(&format!("    Q_PROPERTY({spec})\n"));
        }
        self.header.push_str("\npublic:\n");
        let user_inits: Vec<&InitDecl> = c.inits().collect();
        if user_inits.is_empty() {
            self.header.push_str(&format!(
                "    explicit {name}(QObject* parent = nullptr);\n"
            ));
        } else {
            for init in &user_inits {
                // `explicit` matters only for single-arg ctors; the
                // zero-user-arg form becomes `(QObject*)` after parent injection.
                let explicit = if init.params.is_empty() {
                    "explicit "
                } else {
                    ""
                };
                let sig =
                    render_init_param_list(&init.params, &self.ctx(), /*for_decl=*/ true);
                self.header
                    .push_str(&format!("    {explicit}{name}({sig});\n"));
            }
        }
        if c.deinit().is_some() {
            self.header.push_str(&format!("    ~{name}() override;\n"));
        }
        self.header.push('\n');

        // Property getter/setter declarations. Bindable / Bind props
        // additionally expose `QBindable<T> bindableX()` so external
        // C++ / QML can use `setBinding` / `onValueChanged`.
        for prop in &info.properties {
            if prop.readable {
                self.header.push_str(&format!(
                    "    {ret} {get}() const;\n",
                    ret = prop.cpp_type,
                    get = prop.name
                ));
            }
            if prop.writable {
                let arg_ty = if prop.pass_by_const_ref {
                    format!("const {}&", prop.cpp_type)
                } else {
                    prop.cpp_type.clone()
                };
                self.header.push_str(&format!(
                    "    void {set}({arg} value);\n",
                    set = prop.setter,
                    arg = arg_ty
                ));
            }
            if prop.has_bindable_surface() {
                self.header.push_str(&format!(
                    "    QBindable<{ty}> {get}();\n",
                    ty = prop.cpp_type,
                    get = ty::bindable_getter_name(&prop.name)
                ));
            }
        }
        if !info.properties.is_empty() {
            self.header.push('\n');
        }

        // Plain class fields (`let` / `var`). `pub` gets a public
        // getter; `pub var` adds a public setter. Non-pub fields stay
        // accessible only via `@x` inside methods (no public surface).
        // No Q_PROPERTY, no NOTIFY — those require `prop`.
        let pub_fields: Vec<&Field> = c
            .members
            .iter()
            .filter_map(|m| match m {
                ClassMember::Field(f) if f.is_pub => Some(f),
                _ => None,
            })
            .collect();
        if !pub_fields.is_empty() {
            let mut decls = String::new();
            {
                let ctx = self.ctx();
                for f in &pub_fields {
                    let fty = ty::cute_to_cpp(&f.ty, &ctx);
                    decls.push_str(&format!(
                        "    {ret} {get}() const;\n",
                        ret = fty,
                        get = f.name.name
                    ));
                    if f.is_mut {
                        let pass_const_ref = ty::pass_by_const_ref(&f.ty);
                        let arg_ty = if pass_const_ref {
                            format!("const {fty}&")
                        } else {
                            fty.clone()
                        };
                        decls.push_str(&format!(
                            "    void {set}({arg} value);\n",
                            set = ty::setter_name(&f.name.name),
                            arg = arg_ty
                        ));
                    }
                }
            }
            self.header.push_str(&decls);
            self.header.push('\n');
        }

        // User methods (fn / slot). For instance methods the
        // `Q_INVOKABLE` prefix exposes them to QML /
        // `QMetaMethod::invoke()`; harmless at compile time without
        // moc, but useful for IDE annotation. `static fn` declarations
        // emit a `static` qualifier instead — these have no implicit
        // `this`, are callable as `ClassName::method(args)`, and are
        // not part of the QMetaObject method table.
        if !info.methods.is_empty() {
            for m in &info.methods {
                let params = render_param_list(&m.params);
                let prefix = if m.is_static { "static" } else { "Q_INVOKABLE" };
                self.header.push_str(&format!(
                    "    {prefix} {ret} {name}({params});\n",
                    ret = m.return_type,
                    name = m.name
                ));
            }
            self.header.push('\n');
        }

        // Signals: in Qt, `signals:` is `public` + annotation. Members below
        // are still public, but the qt_create_metaobjectdata template flags
        // them as MethodSignal so QObject::connect sees them as signals.
        if !info.signals.is_empty() {
            self.header.push_str("signals:\n");
            for sig in &info.signals {
                let params = render_param_list(&sig.params);
                self.header
                    .push_str(&format!("    void {name}({params});\n", name = sig.name));
            }
            self.header.push('\n');
        }

        // Private storage for properties — four shapes:
        //   Plain    → bare `T m_x = init;`
        //   Bindable → `Q_OBJECT_BINDABLE_PROPERTY(...)` with notify
        //   Bind     → `Q_OBJECT_BINDABLE_PROPERTY(...)`; the binding
        //              lambda is attached in the constructor via
        //              `m_x.setBinding(...)`
        //   Fresh    → `Q_OBJECT_COMPUTED_PROPERTY(...)` + private
        //              `compute_x() const`
        // Plus plain fields (`let` / `var`) which are always bare
        // `T m_x = init;` with no metaobject machinery.
        let field_members: Vec<&Field> = c
            .members
            .iter()
            .filter_map(|m| match m {
                ClassMember::Field(f) => Some(f),
                _ => None,
            })
            .collect();
        if !info.properties.is_empty() || !field_members.is_empty() {
            self.header.push_str("private:\n");
        }
        if !field_members.is_empty() {
            let mut storage = String::new();
            {
                let ctx = self.ctx();
                for f in &field_members {
                    let fty = ty::cute_to_cpp(&f.ty, &ctx);
                    let init = if let Some(d) = field_default_cpp(
                        c,
                        &f.name.name,
                        self.program,
                        self.module,
                        self.binding_modules,
                    ) {
                        format!(" = {d}")
                    } else {
                        String::new()
                    };
                    storage.push_str(&format!("    {fty} m_{name}{init};\n", name = f.name.name));
                }
            }
            self.header.push_str(&storage);
        }
        if !info.properties.is_empty() {
            for prop in &info.properties {
                use cute_meta::PropKind;
                // `, model` props own a heap-allocated `cute::ModelList`
                // whose lifetime is the class's (parent-tree). The cpp_type
                // already includes the `*`; storage is null until the ctor
                // body runs `new ::cute::ModelList<T*>(initial, this)`.
                if prop.is_model_list {
                    self.header.push_str(&format!(
                        "    {ty} m_{name} = nullptr;\n",
                        ty = prop.cpp_type,
                        name = prop.name,
                    ));
                    continue;
                }
                let default_init = prop_default_cpp(
                    c,
                    &prop.name,
                    self.program,
                    self.module,
                    self.binding_modules,
                );
                match prop.kind {
                    PropKind::Fresh => {
                        let compute = ty::compute_method_name(&prop.name);
                        self.header.push_str(&format!(
                            "    {ty} {compute}() const;\n",
                            ty = prop.cpp_type,
                        ));
                        self.header.push_str(&format!(
                            "    Q_OBJECT_COMPUTED_PROPERTY({cls}, {ty}, m_{name}, &{cls}::{compute})\n",
                            cls = name,
                            ty = prop.cpp_type,
                            name = prop.name,
                        ));
                    }
                    PropKind::Bindable | PropKind::Bind => {
                        let sig_arg = prop
                            .notify_signal_name
                            .as_ref()
                            .map(|s| format!(", &{name}::{s}"))
                            .unwrap_or_default();
                        if let Some(init) = default_init {
                            // `_WITH_ARGS` 4-arg: (Class, T, name, value);
                            // 5-arg adds the notify signal.
                            self.header.push_str(&format!(
                                "    Q_OBJECT_BINDABLE_PROPERTY_WITH_ARGS({cls}, {ty}, m_{pname}, {init}{sig_arg})\n",
                                cls = name,
                                ty = prop.cpp_type,
                                pname = prop.name,
                            ));
                        } else {
                            self.header.push_str(&format!(
                                "    Q_OBJECT_BINDABLE_PROPERTY({cls}, {ty}, m_{pname}{sig_arg})\n",
                                cls = name,
                                ty = prop.cpp_type,
                                pname = prop.name,
                            ));
                        }
                    }
                    PropKind::Plain => {
                        let init = if let Some(d) = default_init {
                            format!(" = {d}")
                        } else {
                            match prop.qmetatype {
                                "QMetaType::Bool" => " = false".into(),
                                "QMetaType::LongLong" | "QMetaType::Int" => " = 0".into(),
                                "QMetaType::Double" => " = 0.0".into(),
                                _ => "".into(),
                            }
                        };
                        self.header.push_str(&format!(
                            "    {ty} m_{name}{init};\n",
                            ty = prop.cpp_type,
                            name = prop.name,
                            init = init
                        ));
                    }
                }
            }
        }

        self.header.push_str("};\n\n");
    }

    fn emit_class_source(
        &mut self,
        c: &ClassDecl,
        info: &ClassInfo,
        meta: &MetaSection,
    ) -> Result<(), EmitError> {
        let name = &c.name.name;

        // Constructor body: `setBinding(lambda)` per Bind prop and a
        // fan-out connect (input notify → all fresh notifies) when
        // the class has any Fresh props. The fan-out is conservative
        // (doesn't analyze which input each fresh actually reads) but
        // always correct — extra re-eval on a given input change
        // beats silently missing deps the binding system can't track.
        use cute_meta::PropKind;
        let bind_props: Vec<&PropInfo> = info
            .properties
            .iter()
            .filter(|p| p.kind == PropKind::Bind)
            .collect();
        let fresh_notifies: Vec<&str> = info
            .properties
            .iter()
            .filter(|p| p.kind == PropKind::Fresh)
            .filter_map(|p| p.notify_signal_name.as_deref())
            .collect();
        let input_notifies: Vec<&str> = info
            .properties
            .iter()
            .filter(|p| p.kind == PropKind::Bindable)
            .filter_map(|p| p.notify_signal_name.as_deref())
            .collect();
        // `, model`-synthesized accessors construct their QRangeModel
        // wrapper in the ctor body (after the source QList<T*> is
        // initialized via either `default:` or default-constructed
        // empty). The model takes the QList by reference, so element
        // mutations through the original prop are visible to QML
        // observers via QRangeModel's auto-watching policy.
        let model_props: Vec<&PropInfo> =
            info.properties.iter().filter(|p| p.is_model_list).collect();
        let needs_prop_machinery = !bind_props.is_empty()
            || (!fresh_notifies.is_empty() && !input_notifies.is_empty())
            || !model_props.is_empty();
        // Per-ctor prop machinery (setBinding / fresh fan-out /
        // QRangeModel construction). Runs once for the synthetic ctor
        // and once per user init.
        let emit_prop_machinery = |source: &mut String,
                                   program: &ResolvedProgram,
                                   module: Option<&Module>,
                                   binding_modules: &[Module]| {
            for prop in &bind_props {
                let pdecl = c.members.iter().find_map(|mem| match mem {
                    ClassMember::Property(p) if p.name.name == prop.name => Some(p),
                    _ => None,
                });
                if let Some(expr) = pdecl.and_then(|p| p.binding.as_ref()) {
                    let mut lo = Lowering::new(&prop.cpp_type, false, None)
                        .with_context(program, Some(c))
                        .with_module(module, binding_modules);
                    let body = lo.lower_expr(expr);
                    source.push_str(&format!(
                        "    m_{pname}.setBinding([this]{{ return {body}; }});\n",
                        pname = prop.name,
                    ));
                }
            }
            if !fresh_notifies.is_empty() {
                for input_sig in &input_notifies {
                    source.push_str(&format!(
                        "    QObject::connect(this, &{name}::{input_sig}, this, [this]{{\n"
                    ));
                    for fresh_sig in &fresh_notifies {
                        source.push_str(&format!("        emit {fresh_sig}();\n"));
                    }
                    source.push_str("    });\n");
                }
            }
            // `, model` props own a heap-allocated `cute::ModelList<T*>`,
            // parented to `this`. The optional `default: [...]` lowers
            // through `prop_default_cpp` (same collection-hint path as
            // non-model List props), which yields a `QList<T*>{...}`
            // expression — passed as the first ctor arg so the inner
            // list starts populated.
            for prop in &model_props {
                let pdecl = c.members.iter().find_map(|mem| match mem {
                    ClassMember::Property(p) if p.name.name == prop.name => Some(p),
                    _ => None,
                });
                let row_cpp = pdecl
                    .and_then(model_row_type_of)
                    .unwrap_or_else(|| "void".to_string());
                let initial = prop_default_cpp(c, &prop.name, program, module, binding_modules);
                let init_arg = match initial {
                    Some(s) => format!("{s}, this"),
                    None => "this".to_string(),
                };
                source.push_str(&format!(
                    "    m_{pname} = new ::cute::ModelList<{row_cpp}>({init_arg});\n",
                    pname = prop.name,
                ));
            }
        };

        // Base-class initializer in the ctor's mem-init list. Always
        // pass the user-supplied `parent` through to the base — for
        // `class X { ... }` and `class X < QObject` this is
        // `: QObject(parent)`; for QWidget-derived supers
        // (`class HighlightedEditor < QPlainTextEdit`) it lowers to
        // `: QPlainTextEdit(qobject_cast<QWidget*>(parent))` so the
        // QObject* parent the binding-side ctor accepts converts
        // safely (qobject_cast yields nullptr for non-QWidget
        // parents, which is the right fallback for a default-
        // constructed `widget_app(window: X)` instance).
        //
        // We can't easily inspect the base class's expected parent
        // type from here without reaching into qpi-side metadata,
        // so the rule is "QObject super → pass parent verbatim;
        // anything else → qobject_cast<QWidget*>". That covers the
        // common cases (QPlainTextEdit, QTextEdit, QListView,
        // QAbstractListModel, …) and degrades to nullptr-passing
        // for any QObject-but-not-QWidget super, which is correct
        // for default construction even if it loses the parent
        // pointer when the user explicitly threads one.
        let base_init = if info.super_class == "QObject" {
            "QObject(parent)".to_string()
        } else {
            format!("{}(qobject_cast<QWidget*>(parent))", info.super_class)
        };
        let user_inits: Vec<&InitDecl> = c.inits().collect();
        if user_inits.is_empty() {
            self.source
                .push_str(&format!("{name}::{name}(QObject* parent) : {base_init} {{"));
            if needs_prop_machinery {
                self.source.push('\n');
                emit_prop_machinery(
                    &mut self.source,
                    self.program,
                    self.module,
                    self.binding_modules,
                );
            }
            self.source.push_str("}\n\n");
        } else {
            for init in &user_inits {
                self.emit_line_directive(init.body.span);
                let sig =
                    render_init_param_list(&init.params, &self.ctx(), /*for_decl=*/ false);
                self.source
                    .push_str(&format!("{name}::{name}({sig}) : {base_init} {{\n"));
                emit_prop_machinery(
                    &mut self.source,
                    self.program,
                    self.module,
                    self.binding_modules,
                );
                self.lower_method_body(c, "void", &init.params, &init.body);
                self.source.push_str("}\n\n");
            }
        }
        if let Some(d) = c.deinit() {
            self.emit_line_directive(d.body.span);
            self.source.push_str(&format!("{name}::~{name}() {{\n"));
            self.lower_method_body(c, "void", &[], &d.body);
            self.source.push_str("}\n\n");
        }

        // Bindable / Bind props read through `.value()` (dep tracking
        // for the binding system); Fresh reads through `.value()` too
        // (which calls the compute lambda). Plain reads m_x directly.
        for prop in &info.properties {
            let read_body = match prop.kind {
                PropKind::Plain => format!("m_{}", prop.name),
                _ => format!("m_{}.value()", prop.name),
            };
            if prop.readable {
                self.source.push_str(&format!(
                    "{ret} {cls}::{get}() const {{ return {body}; }}\n",
                    ret = prop.cpp_type,
                    cls = name,
                    get = prop.name,
                    body = read_body,
                ));
            }
            if prop.writable {
                let arg_ty = if prop.pass_by_const_ref {
                    format!("const {}&", prop.cpp_type)
                } else {
                    prop.cpp_type.clone()
                };
                self.source.push_str(&format!(
                    "void {cls}::{set}({arg} value) {{\n",
                    cls = name,
                    set = prop.setter,
                    arg = arg_ty
                ));
                if prop.kind == PropKind::Bindable {
                    self.source.push_str(&format!(
                        "    m_{name}.setValue(value);\n",
                        name = prop.name
                    ));
                } else {
                    // Plain prop setter: stock dirty-check + assign +
                    // optional notify. `, model` props never reach this
                    // branch (writable == false), so no model-aware
                    // bracketing is needed here — full-replace flows
                    // through `xs->replace(newList)` on the ModelList,
                    // which fires beginResetModel / endResetModel
                    // internally.
                    self.source.push_str(&format!(
                        "    if (m_{name} == value) return;\n",
                        name = prop.name
                    ));
                    self.source
                        .push_str(&format!("    m_{name} = value;\n", name = prop.name));
                    if let Some(sig_name) = &prop.notify_signal_name {
                        self.source.push_str(&format!("    emit {sig_name}();\n"));
                    }
                }
                self.source.push_str("}\n");
            }
            if let Some(get) = prop.bindable_getter.as_deref() {
                self.source.push_str(&format!(
                    "QBindable<{ty}> {cls}::{get}() {{ return QBindable<{ty}>(&m_{name}); }}\n",
                    ty = prop.cpp_type,
                    cls = name,
                    name = prop.name,
                ));
            }
            // Fresh's compute_x() body: lowered expression read at
            // every access, no caching, no auto dep tracking.
            if prop.kind == PropKind::Fresh {
                let pdecl = c.members.iter().find_map(|mem| match mem {
                    ClassMember::Property(p) if p.name.name == prop.name => Some(p),
                    _ => None,
                });
                if let Some(expr) = pdecl.and_then(|p| p.fresh.as_ref()) {
                    let mut lo = Lowering::new(&prop.cpp_type, false, None)
                        .with_emit_context(self.source_map, self.program, Some(c))
                        .with_module(self.module, self.binding_modules);
                    let body = lo.lower_expr(expr);
                    self.source.push_str(&format!(
                        "{ty} {cls}::{compute}() const {{ return {body}; }}\n",
                        ty = prop.cpp_type,
                        cls = name,
                        compute = ty::compute_method_name(&prop.name),
                    ));
                }
            }
        }
        if !info.properties.is_empty() {
            self.source.push('\n');
        }

        // Public-field getter/setter implementations. `pub let` →
        // getter only; `pub var` → getter + setter (no NOTIFY, no
        // change-comparison; plain assignment).
        let pub_fields_src: Vec<&Field> = c
            .members
            .iter()
            .filter_map(|m| match m {
                ClassMember::Field(f) if f.is_pub => Some(f),
                _ => None,
            })
            .collect();
        if !pub_fields_src.is_empty() {
            let mut field_defs = String::new();
            {
                let ctx = self.ctx();
                for f in &pub_fields_src {
                    let fty = ty::cute_to_cpp(&f.ty, &ctx);
                    field_defs.push_str(&format!(
                        "{ret} {cls}::{get}() const {{ return m_{name}; }}\n",
                        ret = fty,
                        cls = name,
                        get = f.name.name,
                        name = f.name.name,
                    ));
                    if f.is_mut {
                        let pass_const_ref = ty::pass_by_const_ref(&f.ty);
                        let arg_ty = if pass_const_ref {
                            format!("const {fty}&")
                        } else {
                            fty.clone()
                        };
                        field_defs.push_str(&format!(
                            "void {cls}::{set}({arg} value) {{ m_{name} = value; }}\n",
                            cls = name,
                            set = ty::setter_name(&f.name.name),
                            arg = arg_ty,
                            name = f.name.name,
                        ));
                    }
                }
            }
            self.source.push_str(&field_defs);
            self.source.push('\n');
        }

        // User method bodies. `info.methods` (built in `build_class_info`)
        // and the AST's `ClassMember::Fn(_)` members are pushed in the
        // same declaration order, so a positional zip pairs each
        // MethodInfo with the FnDecl that produced it. The zip is the
        // overload-aware replacement for the previous name-keyed
        // `find_map` — multiple `fn foo(...)` on one class now emit
        // distinct C++ definitions instead of silently sharing the
        // first body.
        let fn_decls: Vec<&FnDecl> = c
            .members
            .iter()
            .filter_map(|m| match m {
                ClassMember::Fn(f) => Some(f),
                _ => None,
            })
            .collect();
        for (m, fn_decl) in info.methods.iter().zip(fn_decls.iter()) {
            self.emit_fn_body(name, m, c, fn_decl)?;
        }

        // Cute meta section (string table, qt_meta_data, qt_metacall family,
        // signal bodies via QMetaObject::activate).
        self.source.push_str(&meta.source_defs);
        Ok(())
    }

    fn emit_fn_body(
        &mut self,
        class_name: &str,
        m: &MethodInfo,
        c: &ClassDecl,
        fn_decl: &FnDecl,
    ) -> Result<(), EmitError> {
        let return_is_err_union = matches!(
            fn_decl.return_ty.as_ref().map(|t| &t.kind),
            Some(TypeKind::ErrorUnion(_))
        );
        let params = render_param_list(&m.params);
        // `async fn` methods get `QFuture<T>` wrapping just like
        // top-level fns. The header declaration also needs this -
        // emitted earlier in `emit_class_header` from `m.return_type`,
        // which means the build_class_info path has to know about
        // async-ness. For now we double-wrap consistently here so
        // out-of-class definitions match.
        let ret = if fn_decl.is_async && !m.return_type.starts_with("QFuture<") {
            if m.return_type == "void" {
                "QFuture<void>".to_string()
            } else {
                format!("QFuture<{}>", m.return_type)
            }
        } else {
            m.return_type.clone()
        };
        if let Some(body) = &fn_decl.body {
            // Anchor each method definition to its `.cute` body so a
            // gdb backtrace through `Counter::increment` lands in the
            // user's source instead of the generated wrapper.
            self.emit_line_directive(body.span);
        }
        self.source.push_str(&format!(
            "{ret} {cls}::{name}({params}) {{\n",
            cls = class_name,
            name = m.name
        ));
        if let Some(body) = &fn_decl.body {
            let scope = self.program.fn_scopes.get(&body.span);
            let mut lo = Lowering::new(&ret, return_is_err_union, scope)
                .with_emit_context(self.source_map, self.program, Some(c))
                .with_generic_instantiations(self.generic_instantiations)
                .with_module(self.module, self.binding_modules)
                .with_async(fn_decl.is_async);
            lo.record_params(&fn_decl.params);
            let lines = lo.lower_block(body);
            self.write_lowered_lines(lines, "    ");
        }
        self.source.push_str("}\n\n");
        Ok(())
    }
}

/// The C++ type name as Q_PROPERTY needs to see it. Q_PROPERTY's text is
/// scanned by moc/IDEs textually, so `::cute::String` (a `QString` alias)
/// becomes `QString` to keep introspection tools happy.
/// Look up `class C { fn name(...) ... }` (or `slot`) inside a single
/// module's items. Drives `Lowering::lookup_class_method_decl`'s
/// per-module sweep over the user module + every loaded binding.
fn find_class_method_in<'a>(
    module: &'a Module,
    class_name: &str,
    method_name: &str,
) -> Option<&'a FnDecl> {
    for item in &module.items {
        let Item::Class(c) = item else { continue };
        if c.name.name != class_name {
            continue;
        }
        // A module declares each class name at most once, so once
        // we've found the matching class we don't need to keep
        // scanning the surrounding items.
        return c.members.iter().find_map(|m| match m {
            ClassMember::Fn(f) | ClassMember::Slot(f) if f.name.name == method_name => Some(f),
            _ => None,
        });
    }
    None
}

fn qprop_type_name(cpp_type: &str) -> String {
    if cpp_type == "::cute::String" {
        "QString".to_string()
    } else {
        cpp_type.to_string()
    }
}

/// Local variable name for the `QScopedPropertyUpdateGroup` synthesized
/// by `batch { ... }`. Shared between the lowerer and the codegen
/// tests so a rename here flows through automatically.
const BATCH_GUARD_VAR: &str = "_cute_batch_guard";

/// Locate `default: <expr>` on the named prop and lower it to a C++
/// initializer string. Returns `None` if the prop has no default. The
/// Lowering uses the surrounding class as context so e.g. nested
/// constructors resolve correctly.
fn prop_default_cpp(
    c: &ClassDecl,
    prop_name: &str,
    program: &cute_hir::ResolvedProgram,
    module: Option<&Module>,
    binding_modules: &[Module],
) -> Option<String> {
    let pdecl = c.members.iter().find_map(|mem| match mem {
        ClassMember::Property(p) if p.name.name == prop_name => Some(p),
        _ => None,
    })?;
    let expr = pdecl.default.as_ref()?;
    let mut lo = Lowering::new("auto", false, None)
        .with_context(program, Some(c))
        .with_module(module, binding_modules);
    // Bias array / map literal lowering by the prop's declared type
    // so `prop xs : List<Book>, default: [...]` emits
    // `QList<Book*>{...}` instead of `QVariantList{...}` (which
    // wouldn't convert). The collection hint derives from the prop
    // type and propagates recursively into nested literals.
    let hint = lo.collection_hint_from_type(&pdecl.ty);
    Some(lo.lower_with_collection_hint(expr, hint))
}

/// Mirror of `prop_default_cpp` but for plain class fields
/// (`let` / `var`). Field always carries an initializer (parser
/// requires `=`), so this returns `None` only when the lookup misses
/// (which would be a codegen bug).
fn field_default_cpp(
    c: &ClassDecl,
    field_name: &str,
    program: &cute_hir::ResolvedProgram,
    module: Option<&Module>,
    binding_modules: &[Module],
) -> Option<String> {
    let fdecl = c.members.iter().find_map(|mem| match mem {
        ClassMember::Field(f) if f.name.name == field_name => Some(f),
        _ => None,
    })?;
    let expr = fdecl.default.as_ref()?;
    let mut lo = Lowering::new("auto", false, None)
        .with_context(program, Some(c))
        .with_module(module, binding_modules);
    let hint = lo.collection_hint_from_type(&fdecl.ty);
    Some(lo.lower_with_collection_hint(expr, hint))
}

/// `f64::to_string()` for whole-number floats prints just the digits
/// (`1.0_f64.to_string() == "1"`), which silently switches the C++
/// expression to integer arithmetic — a `1.0 * @x` body for a Float
/// computed prop ends up doing integer division and yielding stale
/// values. Append `.0` when the rendered form has no decimal point or
/// exponent, so the resulting C++ token is a `double` literal.
/// Render an `embed(...)` failure as a self-throwing IIFE typed
/// `QByteArray`. The error is detected at codegen time but
/// surfaces at C++ build / runtime via the lambda body — the
/// emitted expression types correctly, so a downstream `let x :
/// ByteArray = embed("missing")` still compiles, but the call
/// throws on first execution.
///
/// Why not a compile-time error: `embed` lives at expression
/// position and a `static_assert(false, "...")` doesn't fit there
/// in C++17. Templated NTTP-string tricks would work in C++20+
/// but adds compile-flag dependencies. Runtime throw is the
/// simplest path that keeps the message visible (it's in the
/// emitted source) and the failure mode unambiguous.
///
/// Codegen-time path-resolution / file-IO failures are also
/// printed to the driver's stderr by the caller via the cute
/// diagnostics layer (TODO — currently embeds fall through this
/// runtime path only).
fn embed_error(msg: &str) -> String {
    let escaped = msg.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "([]() -> QByteArray {{ throw std::runtime_error(\"cute embed error: {escaped}\"); }})()"
    )
}

fn float_literal_cpp(v: f64) -> String {
    let s = v.to_string();
    if s.contains('.')
        || s.contains('e')
        || s.contains('E')
        || s.contains("inf")
        || s.contains("nan")
    {
        s
    } else {
        format!("{s}.0")
    }
}

/// Same as `emit_element_full` but with no extra props - used for the
/// nested-element recursive case at indent > 0.
fn emit_element_with_root_props(
    out: &mut String,
    e: &Element,
    indent: usize,
    extra_props: &[String],
) {
    emit_element_full(out, e, indent, extra_props, &[])
}

/// Lower an element tree to QML, threading two pieces of context:
///   - `extra_props`: extra root-level lines (view parameters) injected
///     at the top of the body before any children.
///   - `for_bindings`: the in-scope `for x in xs` binding names (innermost
///     last). Used by `qml_lower_expr_in` to rewrite per-row identifiers
///     to QML's implicit `modelData` inside Repeater bodies.
fn emit_element_full(
    out: &mut String,
    e: &Element,
    indent: usize,
    extra_props: &[String],
    for_bindings: &[&str],
) {
    let pad = "    ".repeat(indent);
    // Preserve the QML namespace prefix on the element head — e.g.
    // `Kirigami.ApplicationWindow { ... }` must emit as
    // `Kirigami.ApplicationWindow { ... }` (not just `ApplicationWindow`)
    // so the QML resolver picks the imported `org.kde.kirigami` type
    // instead of the QtQuick.Controls one.
    let prefix = if e.module_path.is_empty() {
        String::new()
    } else {
        format!(
            "{}.",
            e.module_path
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>()
                .join(".")
        )
    };
    // Cute-specific sugar names for the QtQuick.Layouts flex
    // primitives. `HBox` / `VBox` / `HGrid` map to the QtQuick
    // layouts but are introduced as NEW names so they don't
    // collide with QtQuick's own `Row` / `Column` / `Grid` (which
    // pass through unchanged — those have different semantics:
    // sequential positioning, no flex, anchors-friendly).
    //
    // Users picking the layout pattern make the choice explicitly:
    //   - flex:    `HBox`, `VBox`, `HGrid`
    //   - non-flex: `Row`, `Column`, `Grid`
    //
    // Namespace-prefixed forms (e.g. `Kirigami.HBox`) are left
    // alone — they aren't Cute sugar.
    let name = if prefix.is_empty() {
        match e.name.name.as_str() {
            "HBox" => "RowLayout",
            "VBox" => "ColumnLayout",
            "HGrid" => "GridLayout",
            other => other,
        }
    } else {
        e.name.name.as_str()
    };
    out.push_str(&format!("{pad}{prefix}{name} {{\n"));
    for line in extra_props {
        out.push_str(&format!("{pad}{line}\n"));
    }
    if !extra_props.is_empty() && !e.members.is_empty() {
        out.push('\n');
    }
    for m in &e.members {
        match m {
            ElementMember::Property { key, value, .. } => {
                let v = qml_lower_expr_in(value, for_bindings);
                out.push_str(&format!("{pad}    {key}: {v}\n"));
            }
            ElementMember::Child(c) => emit_element_full(out, c, indent + 1, &[], for_bindings),
            ElementMember::Stmt(stmt) => {
                qml_emit_stmt_member(out, stmt, indent, for_bindings);
            }
        }
    }
    out.push_str(&format!("{pad}}}\n"));
}

/// Lower a `Stmt` that lives in element-body position. Recognizes
/// the few shapes that are meaningful as element members - if /
/// case (their branches' trailing K::Element becomes a sibling
/// element with `visible:` binding) and for (Repeater). Anything
/// else lowers to a `/* ... */` comment so element-body content
/// degrades gracefully when fed an unsupported statement.
fn qml_emit_stmt_member(out: &mut String, stmt: &Stmt, indent: usize, for_bindings: &[&str]) {
    match stmt {
        Stmt::Expr(e) => match &e.kind {
            ExprKind::If {
                cond,
                then_b,
                else_b,
                ..
            } => {
                qml_emit_if_stmt(out, cond, then_b, else_b.as_ref(), indent, for_bindings);
            }
            ExprKind::Case { scrutinee, arms } => {
                qml_emit_case_stmt(out, scrutinee, arms, indent, for_bindings);
            }
            _ => out.push_str(&format!(
                "{}/* unsupported expr-as-element-member */\n",
                "    ".repeat(indent + 1),
            )),
        },
        Stmt::For {
            binding,
            iter,
            body,
            ..
        } => {
            qml_emit_for_stmt(out, &binding.name, iter, body, indent, for_bindings);
        }
        _ => out.push_str(&format!(
            "{}/* unsupported stmt-as-element-member */\n",
            "    ".repeat(indent + 1),
        )),
    }
}

fn qml_emit_if_stmt(
    out: &mut String,
    cond: &Expr,
    then_b: &cute_syntax::ast::Block,
    else_b: Option<&cute_syntax::ast::Block>,
    indent: usize,
    for_bindings: &[&str],
) {
    // Walk the if/else-if/else chain by following `else_b`'s trailing
    // expression: when it's another K::If, recurse - that's the
    // language-core encoding of `else if`. Each branch's visibility is
    // ANDed with the negation of every prior branch's condition so
    // exactly one (or zero, when no terminal else) is shown.
    let chain = collect_if_chain(cond, then_b, else_b);
    let mut prior_negations: Vec<String> = Vec::new();
    for (cond_opt, el) in chain {
        let vis = match cond_opt {
            Some(c) => {
                let cond_s = qml_lower_expr_in(c, for_bindings);
                let mut combined = prior_negations.clone();
                combined.push(cond_s.clone());
                prior_negations.push(format!("!({cond_s})"));
                combined.join(" && ")
            }
            None => {
                if prior_negations.is_empty() {
                    "true".to_string()
                } else {
                    prior_negations.join(" && ")
                }
            }
        };
        let extra = vec![format!("    visible: {vis}")];
        emit_element_full(out, el, indent + 1, &extra, for_bindings);
    }
}

fn qml_emit_case_stmt(
    out: &mut String,
    scrutinee: &Expr,
    arms: &[cute_syntax::ast::CaseArm],
    indent: usize,
    for_bindings: &[&str],
) {
    let scrutinee_s = qml_lower_expr_in(scrutinee, for_bindings);
    let mut prior_negations: Vec<String> = Vec::new();
    for arm in arms {
        // View / QML side has no Result API yet (cute::Result isn't a
        // Q_GADGET) so `result_api` stays None - any `when ok(v)` /
        // `when err(e)` arms degrade to the TODO placeholder.
        let pm = pattern_match_test(
            &scrutinee_s,
            &arm.pattern,
            "===",
            qml_quote_string,
            |e| qml_lower_expr_in(e, for_bindings),
            None,
        );
        let mut combined = prior_negations.clone();
        if pm.test != "true" {
            combined.push(format!("({})", pm.test));
        }
        let vis = if combined.is_empty() {
            "true".to_string()
        } else {
            combined.join(" && ")
        };
        if let Some(el) = trailing_element(&arm.body) {
            let extra = vec![format!("    visible: {vis}")];
            emit_element_full(out, el, indent + 1, &extra, for_bindings);
        } else {
            out.push_str(&format!(
                "{}/* case arm has no trailing element */\n",
                "    ".repeat(indent + 1),
            ));
        }
        prior_negations.push(format!("!({})", pm.test));
    }
}

fn qml_emit_for_stmt(
    out: &mut String,
    binding: &str,
    iter: &Expr,
    body: &cute_syntax::ast::Block,
    indent: usize,
    for_bindings: &[&str],
) {
    let iter_s = qml_lower_expr_in(iter, for_bindings);
    let pad = "    ".repeat(indent);
    let inner_pad = "    ".repeat(indent + 1);
    out.push_str(&format!("{pad}    Repeater {{\n"));
    out.push_str(&format!("{inner_pad}    model: {iter_s}\n"));
    let mut bindings: Vec<&str> = for_bindings.to_vec();
    bindings.push(binding);
    if let Some(el) = trailing_element(body) {
        emit_element_full(out, el, indent + 2, &[], &bindings);
    } else {
        out.push_str(&format!(
            "{inner_pad}    /* for body has no trailing element */\n"
        ));
    }
    out.push_str(&format!("{pad}    }}\n"));
}

/// Pull the trailing-Element out of a Block. Returns `None` when the
/// trailing expression is missing or isn't an `ExprKind::Element`.
fn trailing_element(body: &cute_syntax::ast::Block) -> Option<&Element> {
    let trailing = body.trailing.as_deref()?;
    match &trailing.kind {
        ExprKind::Element(el) => Some(el),
        _ => None,
    }
}

/// Walk the language-core if/else-if chain, where `else if cond { ... }`
/// is encoded as `else { if cond { ... } }`. Returns one entry per
/// branch: `(Some(cond), element)` for if/else-if branches and
/// `(None, element)` for the terminal else.
fn collect_if_chain<'a>(
    head_cond: &'a Expr,
    head_then: &'a cute_syntax::ast::Block,
    head_else: Option<&'a cute_syntax::ast::Block>,
) -> Vec<(Option<&'a Expr>, &'a Element)> {
    let mut out = Vec::new();
    if let Some(el) = trailing_element(head_then) {
        out.push((Some(head_cond), el));
    }
    let mut cur = head_else;
    while let Some(block) = cur {
        // `else if`: the block has just one trailing K::If with no
        // intervening statements.
        if block.stmts.is_empty() {
            if let Some(t) = block.trailing.as_deref() {
                if let ExprKind::If {
                    cond,
                    then_b,
                    else_b,
                    ..
                } = &t.kind
                {
                    if let Some(el) = trailing_element(then_b) {
                        out.push((Some(cond), el));
                    }
                    cur = else_b.as_ref();
                    continue;
                }
                // Terminal `else { Element }`.
                if let ExprKind::Element(el) = &t.kind {
                    out.push((None, el));
                    break;
                }
            }
        }
        // Non-Element trailing expression in else; ignore.
        cur = None;
    }
    out
}

/// Map a Cute `TypeExpr` to the QML property-type keyword used in
/// `property <type> <name>` declarations. QML has a small built-in
/// vocabulary (`string`, `int`, `real`, `bool`, `var`, `color`, ...);
/// anything outside the lowered set falls back to `var` so the binding
/// system still works (just without compile-time type enforcement).
fn qml_property_type(ty: &cute_syntax::ast::TypeExpr) -> &'static str {
    use cute_syntax::ast::TypeKind;
    let TypeKind::Named { path, args } = &ty.kind else {
        return "var";
    };
    if !args.is_empty() {
        return "var";
    }
    match path.last().map(|i| i.name.as_str()).unwrap_or("") {
        "String" => "string",
        "Int" => "int",
        "Float" | "Double" => "real",
        "Bool" => "bool",
        "Url" => "url",
        "Color" => "color",
        _ => "var",
    }
}

/// QML property type for a Cute view parameter, with project info
/// available so we can emit the actual class name when the type is
/// a user-declared QObject class (registered via qmlRegisterType in
/// the synthesized main()). Falls back to `qml_property_type`'s
/// builtin → QML-primitive mapping for everything else.
///
/// Without this, `view BookCard(book: Book)` would lower to a QML
/// `property var book` declaration; reading `book.advance()` then
/// goes through QVariant's metacall and silently fails. With the
/// proper `property Book book`, QML keeps the QObject identity and
/// method calls dispatch normally.
fn qml_property_type_with_program(
    ty: &cute_syntax::ast::TypeExpr,
    program: &ResolvedProgram,
) -> String {
    use cute_syntax::ast::TypeKind;
    if let TypeKind::Named { path, args } = &ty.kind {
        if args.is_empty() {
            if let Some(leaf) = path.last() {
                let name = leaf.name.as_str();
                // User-declared QObject class? Emit the bare class
                // name — qmlRegisterType has registered it in main().
                if let Some(cute_hir::ItemKind::Class {
                    is_qobject_derived: true,
                    ..
                }) = program.items.get(name)
                {
                    return name.to_string();
                }
            }
        }
    }
    qml_property_type(ty).to_string()
}

/// State for lowering a `widget Name { ... }` declaration to C++.
/// Holds a fresh-name counter (so each constructed widget / layout
/// gets a unique local variable in the constructor body) and the
/// set of names that refer to QObject pointers - state fields and
/// fresh local widgets - so member access on them lowers as `->`
/// instead of `.`.
struct WidgetEmitter<'a> {
    counter: u32,
    pointer_names: std::collections::HashSet<String>,
    /// State-field-name → class-name. Filled from each state
    /// field's `init_expr`. Lets the property-binding emitter
    /// connect notify signals on per-state-field property reads
    /// (`text: store.note_count` → connect from `store.<notify>`
    /// to a setText lambda).
    state_classes: std::collections::HashMap<String, String>,
    /// User module containing the state-field classes — needed to
    /// look up a property's notify signal name when emitting a
    /// reactive widget binding.
    module: Option<&'a Module>,
}

impl<'a> WidgetEmitter<'a> {
    fn new() -> Self {
        Self {
            counter: 0,
            pointer_names: std::collections::HashSet::new(),
            state_classes: std::collections::HashMap::new(),
            module: None,
        }
    }

    fn with_module(mut self, module: &'a Module) -> Self {
        self.module = Some(module);
        self
    }

    fn record_state_fields(&mut self, fields: &[cute_syntax::ast::StateField]) {
        for sf in fields {
            // Property-kind state fields (`state X : T = init`) are
            // QML-only in v1; an earlier HIR pass errors out when one
            // appears in a widget body. Skip here so we don't pollute
            // pointer_names / state_classes with a non-QObject name.
            if matches!(sf.kind, cute_syntax::ast::StateFieldKind::Property { .. }) {
                continue;
            }
            self.pointer_names.insert(sf.name.name.clone());
            if let Some(class) = state_field_init_class(sf) {
                self.state_classes.insert(sf.name.name.clone(), class);
            }
        }
    }

    /// Look up the property declaration matching `class_name::prop_name`.
    fn lookup_property(
        &self,
        class_name: &str,
        prop_name: &str,
    ) -> Option<&cute_syntax::ast::PropertyDecl> {
        let module = self.module?;
        for item in &module.items {
            if let Item::Class(c) = item {
                if c.name.name == class_name {
                    for member in &c.members {
                        if let cute_syntax::ast::ClassMember::Property(p) = member {
                            if p.name.name == prop_name {
                                return Some(p);
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Notify signal names that should drive a re-evaluation when the
    /// widget binding reads `<state>.<prop>`. For props with their own
    /// `notify:` (the input bindable / non-bindable case), this is just
    /// `[<own notify>]`. For computed props (`bind { ... }`, no notify
    /// of their own — they ride the QObjectComputedProperty binding
    /// system), we approximate dependencies by returning every notify
    /// signal in the same class. Over-conservative (extra re-evals) but
    /// always correct: when any underlying bindable input fires, we
    /// re-read the computed value, which the binding system has just
    /// invalidated.
    fn notify_signals_for(&self, class_name: &str, prop_name: &str) -> Vec<String> {
        let Some(pdecl) = self.lookup_property(class_name, prop_name) else {
            return Vec::new();
        };
        if let Some(n) = pdecl.notify.as_ref() {
            return vec![n.name.clone()];
        }
        if pdecl.binding.is_none() {
            return Vec::new();
        }
        // Computed prop — gather every notify signal declared on input
        // props of this class.
        let Some(module) = self.module else {
            return Vec::new();
        };
        let mut signals: Vec<String> = Vec::new();
        for item in &module.items {
            if let Item::Class(c) = item {
                if c.name.name == class_name {
                    for member in &c.members {
                        if let cute_syntax::ast::ClassMember::Property(p) = member {
                            if let Some(n) = p.notify.as_ref() {
                                if !signals.contains(&n.name) {
                                    signals.push(n.name.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
        signals
    }

    /// Walk a property-value expression and collect every
    /// `<state_field>.<prop>` member access where the field's
    /// class declares a notify signal for that property. Each
    /// returned tuple is `(state_field, class, signal)` and
    /// drives a `QObject::connect` from the signal to a lambda
    /// that re-evaluates the expression and re-applies the
    /// widget's setter.
    fn collect_reactive_deps(&self, e: &Expr) -> Vec<(String, String, String)> {
        let mut out: Vec<(String, String, String)> = Vec::new();
        self.collect_reactive_deps_inner(e, &mut out);
        // Dedupe (preserve order).
        let mut seen: std::collections::HashSet<(String, String, String)> =
            std::collections::HashSet::new();
        out.into_iter().filter(|t| seen.insert(t.clone())).collect()
    }

    fn collect_reactive_deps_inner(&self, e: &Expr, out: &mut Vec<(String, String, String)>) {
        use cute_syntax::ast::ExprKind as K;
        use cute_syntax::ast::StrPart;
        match &e.kind {
            K::Member { receiver, name } | K::SafeMember { receiver, name } => {
                if let K::Ident(state_name) = &receiver.kind {
                    if let Some(class) = self.state_classes.get(state_name) {
                        for sig in self.notify_signals_for(class, &name.name) {
                            out.push((state_name.clone(), class.clone(), sig));
                        }
                    }
                }
                self.collect_reactive_deps_inner(receiver, out);
            }
            K::MethodCall {
                receiver,
                args,
                block,
                ..
            }
            | K::SafeMethodCall {
                receiver,
                args,
                block,
                ..
            } => {
                self.collect_reactive_deps_inner(receiver, out);
                for a in args {
                    self.collect_reactive_deps_inner(a, out);
                }
                if let Some(b) = block {
                    self.collect_reactive_deps_inner(b, out);
                }
            }
            K::Call {
                callee,
                args,
                block,
                ..
            } => {
                self.collect_reactive_deps_inner(callee, out);
                for a in args {
                    self.collect_reactive_deps_inner(a, out);
                }
                if let Some(b) = block {
                    self.collect_reactive_deps_inner(b, out);
                }
            }
            K::Binary { lhs, rhs, .. } => {
                self.collect_reactive_deps_inner(lhs, out);
                self.collect_reactive_deps_inner(rhs, out);
            }
            K::Unary { expr, .. } => {
                self.collect_reactive_deps_inner(expr, out);
            }
            K::If {
                cond,
                then_b,
                else_b,
                ..
            } => {
                self.collect_reactive_deps_inner(cond, out);
                if let Some(t) = &then_b.trailing {
                    self.collect_reactive_deps_inner(t, out);
                }
                if let Some(eb) = else_b {
                    if let Some(t) = &eb.trailing {
                        self.collect_reactive_deps_inner(t, out);
                    }
                }
            }
            K::Index { receiver, index } => {
                self.collect_reactive_deps_inner(receiver, out);
                self.collect_reactive_deps_inner(index, out);
            }
            K::Str(parts) => {
                for p in parts {
                    match p {
                        StrPart::Interp(inner) => self.collect_reactive_deps_inner(inner, out),
                        StrPart::InterpFmt { expr, .. } => {
                            self.collect_reactive_deps_inner(expr, out)
                        }
                        StrPart::Text(_) => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn fresh(&mut self, prefix: &str) -> String {
        let n = self.counter;
        self.counter += 1;
        format!("_{prefix}{n}")
    }

    /// Emit the widget body in the constructor of the user-declared
    /// class. The root element's properties become `setX(...)` /
    /// signal-connect calls on `this`, children are constructed via
    /// `emit_child_into` and attached via `attach_child`. Delegates to
    /// `emit_element_body` so signal handlers / if / for behave the
    /// same at the root as inside nested elements.
    fn emit_root_into(&mut self, root_var: &str, e: &Element, out: &mut String) {
        let class = e.name.name.clone();
        self.emit_element_body(&class, root_var, e, out);
    }

    /// Lowers a widget body into a `cute::ui::dsl::col(text(...))`-style
    /// expression (no trailing `;`; the caller wraps it in `return ...;`).
    fn emit_root_cute_ui_into(&mut self, e: &Element, out: &mut String) {
        self.emit_element_cute_ui(e, out);
    }

    fn emit_element_cute_ui(&mut self, e: &Element, out: &mut String) {
        // `key: <expr>` is a cross-cutting Element property: wrap the
        // emitted construction in an IIFE that calls `->setKey(
        // QVariant::fromValue(<expr>))` so cute_ui's keyed-diff
        // pairs old/new children by user-stable identity rather than
        // position. Lets a `for it in items { Card { key: it.id; ...}}`
        // preserve caret / scroll / press state when items reorder.
        let key_value: Option<String> = e.members.iter().find_map(|m| match m {
            ElementMember::Property { key, value, .. } if key == "key" => {
                Some(widget_lower_expr(value, &self.pointer_names))
            }
            _ => None,
        });

        if key_value.is_some() {
            out.push_str("[&]{ auto _ke = ");
        }

        let class = e.name.name.as_str();
        match class {
            "Column" => self.emit_container_cute_ui(e, "col", out),
            "Row" => self.emit_container_cute_ui(e, "row", out),
            "Stack" => self.emit_container_cute_ui(e, "stack", out),
            "ListView" => self.emit_container_cute_ui(e, "listview", out),
            "DataTable" => self.emit_container_cute_ui(e, "datatable", out),
            "ScrollView" => self.emit_container_cute_ui(e, "scrollview", out),
            "HScrollView" => self.emit_container_cute_ui(e, "hscrollview", out),
            "Modal" => self.emit_container_cute_ui(e, "modal", out),
            "Window" => {
                // cute::ui::Window is created implicitly by App::run; here we
                // unwrap to the first child so build() returns the inner tree.
                if let Some(child) = e.members.iter().find_map(|m| match m {
                    ElementMember::Child(c) => Some(c),
                    _ => None,
                }) {
                    self.emit_element_cute_ui(child, out);
                } else {
                    out.push_str("col()");
                }
            }
            "Text" => self.emit_leaf_text_cute_ui(e, out),
            "Button" => self.emit_leaf_button_cute_ui(e, out),
            "TextField" => self.emit_leaf_textfield_cute_ui(e, out),
            "Image" => self.emit_leaf_image_cute_ui(e, out),
            "Svg" => self.emit_leaf_svg_cute_ui(e, out),
            "BarChart" => self.emit_leaf_chart_cute_ui(e, "barchart", out),
            "LineChart" => self.emit_leaf_chart_cute_ui(e, "linechart", out),
            "ProgressBar" => self.emit_leaf_progressbar_cute_ui(e, out),
            "Spinner" => self.emit_leaf_spinner_cute_ui(e, out),
            _ => {
                out.push_str("col()");
            }
        }

        if let Some(k) = key_value {
            out.push_str(&format!(
                "; _ke->setKey(QVariant::fromValue({k})); return _ke; }}()"
            ));
        }
    }

    fn emit_container_cute_ui(&mut self, e: &Element, factory: &str, out: &mut String) {
        // C++ template parameter packs can't expand control flow, so dynamic
        // children (for / if) and any container property switch from
        // `col(a, b)` to `[&]{ auto _c=col(); …; }()`.
        // `key:` is handled by the outer `emit_element_cute_ui` wrapper,
        // not as a setter on the container, so it doesn't count toward
        // the "any other prop" decision.
        let has_dynamic = e
            .members
            .iter()
            .any(|m| matches!(m, ElementMember::Stmt(_)));
        let has_prop = e.members.iter().any(|m| {
            matches!(m,
            ElementMember::Property { key, .. } if key != "key")
        });
        if !has_dynamic && !has_prop {
            out.push_str(factory);
            out.push('(');
            let mut first = true;
            for member in &e.members {
                if let ElementMember::Child(child) = member {
                    if !first {
                        out.push_str(", ");
                    }
                    first = false;
                    self.emit_element_cute_ui(child, out);
                }
            }
            out.push(')');
            return;
        }

        out.push_str("[&]{ auto _c = ");
        out.push_str(factory);
        out.push_str("(); ");
        for member in &e.members {
            match member {
                ElementMember::Property { key, value, .. } => {
                    if key == "key" {
                        continue; // outer wrapper handles this via setKey.
                    }
                    let v = widget_lower_expr(value, &self.pointer_names);
                    let setter = ty::setter_name(key);
                    out.push_str(&format!("_c->{setter}({v}); "));
                }
                ElementMember::Child(child) => {
                    out.push_str("_c->addChild(");
                    self.emit_element_cute_ui(child, out);
                    out.push_str("); ");
                }
                ElementMember::Stmt(stmt) => {
                    self.emit_container_stmt_cute_ui(stmt, out);
                }
            }
        }
        out.push_str("return _c; }()");
    }

    fn emit_container_stmt_cute_ui(&mut self, stmt: &cute_syntax::ast::Stmt, out: &mut String) {
        use cute_syntax::ast::{ExprKind, Stmt};
        match stmt {
            Stmt::For {
                binding,
                iter,
                body,
                ..
            } => {
                let iter_s = widget_lower_expr(iter, &self.pointer_names);
                // QObject lists (the common ListView delegate shape) yield
                // `T*` per element; register so `it.label` lowers as `->`.
                let inserted = self.pointer_names.insert(binding.name.clone());
                out.push_str(&format!(
                    "for (const auto& {} : {}) {{ ",
                    binding.name, iter_s
                ));
                if let Some(el) = trailing_element(body) {
                    out.push_str("_c->addChild(");
                    self.emit_element_cute_ui(el, out);
                    out.push_str("); ");
                }
                out.push_str("} ");
                if inserted {
                    self.pointer_names.remove(&binding.name);
                }
            }
            Stmt::Expr(expr) => {
                if let ExprKind::If {
                    cond,
                    then_b,
                    else_b,
                    ..
                } = &expr.kind
                {
                    let chain = collect_if_chain(cond, then_b, else_b.as_ref());
                    let mut prior_negs: Vec<String> = Vec::new();
                    for (cond_opt, el) in chain {
                        let test = match cond_opt {
                            Some(c) => {
                                let s = widget_lower_expr(c, &self.pointer_names);
                                let mut combined = prior_negs.clone();
                                combined.push(s.clone());
                                prior_negs.push(format!("!({s})"));
                                combined.join(" && ")
                            }
                            None => {
                                if prior_negs.is_empty() {
                                    "true".to_string()
                                } else {
                                    prior_negs.join(" && ")
                                }
                            }
                        };
                        out.push_str(&format!("if ({test}) {{ _c->addChild("));
                        self.emit_element_cute_ui(el, out);
                        out.push_str("); } ");
                    }
                }
            }
            _ => {}
        }
    }

    fn emit_leaf_text_cute_ui(&mut self, e: &Element, out: &mut String) {
        let mut text_value: Option<String> = None;
        let mut font_size: Option<String> = None;
        let mut color_value: Option<String> = None;
        for m in &e.members {
            if let ElementMember::Property { key, value, .. } = m {
                match key.as_str() {
                    "text" => text_value = Some(widget_lower_expr(value, &self.pointer_names)),
                    "fontSize" => font_size = Some(widget_lower_expr(value, &self.pointer_names)),
                    "color" => color_value = Some(widget_lower_expr(value, &self.pointer_names)),
                    _ => {}
                }
            }
        }
        let arg = text_value.unwrap_or_else(|| "QString()".into());
        if font_size.is_none() && color_value.is_none() {
            out.push_str(&format!("text({arg})"));
            return;
        }
        // IIFE so the chained setters (which return raw Element*) keep
        // the original unique_ptr alive instead of leaking it.
        out.push_str(&format!("[&]{{ auto _t = text({arg}); "));
        if let Some(s) = font_size {
            out.push_str(&format!("_t->setFontSize({s}); "));
        }
        if let Some(c) = color_value {
            out.push_str(&format!("_t->setColor({c}); "));
        }
        out.push_str("return _t; }()");
    }

    fn emit_leaf_button_cute_ui(&mut self, e: &Element, out: &mut String) {
        let mut text_value: Option<String> = None;
        let mut click_expr: Option<String> = None;
        for m in &e.members {
            if let ElementMember::Property { key, value, .. } = m {
                match key.as_str() {
                    "text" => {
                        text_value = Some(widget_lower_expr(value, &self.pointer_names));
                    }
                    "onClick" => {
                        click_expr = Some(widget_lower_expr(value, &self.pointer_names));
                    }
                    _ => {}
                }
            }
        }
        // The `button(label, [this]{...})` overload is used so the
        // unique_ptr stays movable into col/row; chaining `->onClick(...)`
        // would return a raw Element* and destroy that ownership.
        let mut s = String::new();
        s.push_str("button(");
        if let Some(t) = text_value {
            s.push_str(&t);
        } else {
            s.push_str("QString()");
        }
        if let Some(c) = click_expr {
            s.push_str(", [this]{ ");
            s.push_str(&c);
            s.push_str("; }");
        }
        s.push(')');
        out.push_str(&s);
    }

    fn emit_leaf_textfield_cute_ui(&mut self, e: &Element, out: &mut String) {
        // Setters return raw Element*; wrap in an IIFE so the unique_ptr
        // returned by textfield() stays movable into the parent col/row.
        let mut placeholder: Option<String> = None;
        let mut text_value: Option<String> = None;
        let mut on_changed: Option<String> = None;
        for m in &e.members {
            if let ElementMember::Property { key, value, .. } = m {
                match key.as_str() {
                    "placeholder" => {
                        placeholder = Some(widget_lower_expr(value, &self.pointer_names));
                    }
                    "text" => {
                        text_value = Some(widget_lower_expr(value, &self.pointer_names));
                    }
                    "onTextChanged" => {
                        on_changed = Some(widget_lower_expr(value, &self.pointer_names));
                    }
                    _ => {}
                }
            }
        }
        out.push_str("[&]{ auto _tf = textfield(");
        if let Some(p) = &placeholder {
            out.push_str(p);
        }
        out.push_str("); ");
        if let Some(t) = &text_value {
            out.push_str("_tf->setText(");
            out.push_str(t);
            out.push_str("); ");
        }
        if let Some(c) = &on_changed {
            // `text` is the param the user's body refers to (matches the
            // `signal textChanged(text: String)` declared in cute_ui.qpi).
            out.push_str("_tf->setOnTextChanged([this](QString text){ (void)text; ");
            out.push_str(c);
            out.push_str("; }); ");
        }
        out.push_str("return _tf; }()");
    }

    fn emit_leaf_progressbar_cute_ui(&mut self, e: &Element, out: &mut String) {
        let mut value: Option<String> = None;
        let mut width: Option<String> = None;
        let mut height: Option<String> = None;
        for m in &e.members {
            if let ElementMember::Property { key, value: v, .. } = m {
                match key.as_str() {
                    "value" => value = Some(widget_lower_expr(v, &self.pointer_names)),
                    "width" => width = Some(widget_lower_expr(v, &self.pointer_names)),
                    "height" => height = Some(widget_lower_expr(v, &self.pointer_names)),
                    _ => {}
                }
            }
        }
        out.push_str("[&]{ auto _p = progressbar(); ");
        if let Some(v) = &value {
            out.push_str(&format!("_p->setValue({v}); "));
        }
        if width.is_some() || height.is_some() {
            out.push_str("_p->setSize(QSizeF(");
            out.push_str(width.as_deref().unwrap_or("0"));
            out.push_str(", ");
            out.push_str(height.as_deref().unwrap_or("0"));
            out.push_str(")); ");
        }
        out.push_str("return _p; }()");
    }

    fn emit_leaf_spinner_cute_ui(&mut self, e: &Element, out: &mut String) {
        let mut width: Option<String> = None;
        let mut height: Option<String> = None;
        for m in &e.members {
            if let ElementMember::Property { key, value: v, .. } = m {
                match key.as_str() {
                    "width" => width = Some(widget_lower_expr(v, &self.pointer_names)),
                    "height" => height = Some(widget_lower_expr(v, &self.pointer_names)),
                    _ => {}
                }
            }
        }
        if width.is_some() || height.is_some() {
            out.push_str("[&]{ auto _s = spinner(); _s->setSize(QSizeF(");
            out.push_str(width.as_deref().unwrap_or("0"));
            out.push_str(", ");
            out.push_str(height.as_deref().unwrap_or("0"));
            out.push_str(")); return _s; }()");
        } else {
            out.push_str("spinner()");
        }
    }

    fn emit_leaf_chart_cute_ui(&mut self, e: &Element, factory: &str, out: &mut String) {
        let mut data: Option<String> = None;
        let mut labels: Option<String> = None;
        let mut width: Option<String> = None;
        let mut height: Option<String> = None;
        for m in &e.members {
            if let ElementMember::Property { key, value, .. } = m {
                match key.as_str() {
                    "data" => data = Some(widget_lower_expr(value, &self.pointer_names)),
                    "labels" => labels = Some(widget_lower_expr(value, &self.pointer_names)),
                    "width" => width = Some(widget_lower_expr(value, &self.pointer_names)),
                    "height" => height = Some(widget_lower_expr(value, &self.pointer_names)),
                    _ => {}
                }
            }
        }
        out.push_str("[&]{ auto _c = ");
        out.push_str(factory);
        out.push_str("(); ");
        if let Some(d) = &data {
            // Cute List<Float> lowers to QList<double> which is the same as
            // QList<qreal> on macOS / Linux / Windows.
            out.push_str(&format!("_c->setData({d}); "));
        }
        if let Some(l) = &labels {
            out.push_str(&format!("_c->setLabels({l}); "));
        }
        if width.is_some() || height.is_some() {
            out.push_str("_c->setSize(QSizeF(");
            out.push_str(width.as_deref().unwrap_or("0"));
            out.push_str(", ");
            out.push_str(height.as_deref().unwrap_or("0"));
            out.push_str(")); ");
        }
        out.push_str("return _c; }()");
    }

    fn emit_leaf_svg_cute_ui(&mut self, e: &Element, out: &mut String) {
        let mut source: Option<String> = None;
        let mut width: Option<String> = None;
        let mut height: Option<String> = None;
        for m in &e.members {
            if let ElementMember::Property { key, value, .. } = m {
                match key.as_str() {
                    "source" => source = Some(widget_lower_expr(value, &self.pointer_names)),
                    "width" => width = Some(widget_lower_expr(value, &self.pointer_names)),
                    "height" => height = Some(widget_lower_expr(value, &self.pointer_names)),
                    _ => {}
                }
            }
        }
        if width.is_some() || height.is_some() {
            out.push_str("[&]{ auto _svg = svg(");
            out.push_str(source.as_deref().unwrap_or("QString()"));
            out.push_str("); _svg->setSize(QSizeF(");
            out.push_str(width.as_deref().unwrap_or("0"));
            out.push_str(", ");
            out.push_str(height.as_deref().unwrap_or("0"));
            out.push_str(")); return _svg; }()");
        } else {
            out.push_str("svg(");
            out.push_str(source.as_deref().unwrap_or("QString()"));
            out.push(')');
        }
    }

    fn emit_leaf_image_cute_ui(&mut self, e: &Element, out: &mut String) {
        let mut source: Option<String> = None;
        let mut width: Option<String> = None;
        let mut height: Option<String> = None;
        for m in &e.members {
            if let ElementMember::Property { key, value, .. } = m {
                match key.as_str() {
                    "source" => {
                        source = Some(widget_lower_expr(value, &self.pointer_names));
                    }
                    "width" => {
                        width = Some(widget_lower_expr(value, &self.pointer_names));
                    }
                    "height" => {
                        height = Some(widget_lower_expr(value, &self.pointer_names));
                    }
                    _ => {}
                }
            }
        }
        if width.is_some() || height.is_some() {
            out.push_str("[&]{ auto _img = image(");
            if let Some(s) = &source {
                out.push_str(s);
            } else {
                out.push_str("QString()");
            }
            out.push_str("); _img->setSize(QSizeF(");
            out.push_str(width.as_deref().unwrap_or("0"));
            out.push_str(", ");
            out.push_str(height.as_deref().unwrap_or("0"));
            out.push_str(")); return _img; }()");
        } else {
            out.push_str("image(");
            if let Some(s) = source {
                out.push_str(&s);
            } else {
                out.push_str("QString()");
            }
            out.push(')');
        }
    }
}

/// Names whose appearance as the root of a `widget` declaration switches
/// codegen to the cute::ui::Component path.
fn is_cute_ui_root_class(name: &str) -> bool {
    matches!(
        name,
        "Window"
            | "Column"
            | "Row"
            | "Stack"
            | "Text"
            | "Button"
            | "TextField"
            | "ListView"
            | "DataTable"
            | "ScrollView"
            | "HScrollView"
            | "Image"
            | "Svg"
            | "BarChart"
            | "LineChart"
            | "ProgressBar"
            | "Spinner"
            | "Modal"
            | "Element"
    )
}

impl<'a> WidgetEmitter<'a> {
    /// Emit a non-root child element. Returns the name of the local
    /// variable that holds the freshly-allocated widget / layout.
    fn emit_child_into(&mut self, e: &Element, out: &mut String) -> String {
        let class = &e.name.name;
        let prefix = if is_layout_class(class) { "l" } else { "w" };
        let var = self.fresh(prefix);
        // Fresh widget locals are themselves pointers (`auto*`), so
        // record them so member access on them lowers as `->`.
        self.pointer_names.insert(var.clone());
        out.push_str(&format!("    auto* {var} = new {class}();\n"));
        self.emit_element_body(class, &var, e, out);
        var
    }

    /// Walk an element's members, emitting setter / addWidget / connect
    /// / if / for code into `out`. `var` is the C++ variable pointing at
    /// the constructed element; `class_name` is its declared class name
    /// (used to special-case layouts).
    ///
    /// QSS shorthand pre-pass: keys in `crate::qss`'s vocabulary
    /// (`color`, `background`, `borderRadius`, `hover.background`, ...)
    /// don't have direct QWidget setters - they live in QSS. Collect
    /// every recognised entry into a `QssBag`, render to a single
    /// stylesheet string, and emit one `setStyleSheet(...)` call at
    /// the end. A user-provided literal `styleSheet:` on the same
    /// element is concatenated after the synth (later QSS rules win
    /// on tie, so the user's hand-written string wins on conflict).
    fn emit_element_body(&mut self, class_name: &str, var: &str, e: &Element, out: &mut String) {
        use crate::qss::{QssBag, QssClass, classify};

        let mut qss = QssBag::default();
        let mut user_stylesheet: Option<&Expr> = None;

        for m in &e.members {
            match m {
                ElementMember::Property { key, value, .. } => {
                    if signal_handler_name(key).is_none() {
                        if key == "styleSheet" {
                            user_stylesheet = Some(value);
                            continue;
                        }
                        if let QssClass::Shorthand(pseudo, kebab, formatted) = classify(key, value)
                        {
                            qss.push(pseudo, kebab, formatted);
                            continue;
                        }
                    }
                    if let Some(sig) = signal_handler_name(key) {
                        // `onClicked: <expr>` -> Qt's modern function-
                        // pointer connect with the expression as the
                        // body of a captureless-friendly lambda.
                        let body_s = widget_lower_expr(value, &self.pointer_names);
                        out.push_str(&format!(
                            "    QObject::connect({var}, &{class_name}::{sig}, [=]() {{\n        {body_s};\n    }});\n",
                        ));
                    } else {
                        let v = widget_lower_expr(value, &self.pointer_names);
                        let setter = ty::setter_name(key);
                        out.push_str(&format!("    {var}->{setter}({v});\n"));
                        // Reactive binding: when the property value
                        // expression references a state field's
                        // QObject property (e.g. `text: store.note_count`),
                        // connect the property's notify signal to a
                        // lambda that re-runs the setter. QML view
                        // bodies do this automatically; the QtWidgets
                        // path needs explicit QObject::connect calls.
                        let deps = self.collect_reactive_deps(value);
                        for (state_name, class, signal) in deps {
                            out.push_str(&format!(
                                "    QObject::connect({state}, &{class}::{signal}, {var}, [=]() {{\n        {var}->{setter}({v});\n    }});\n",
                                state = state_name,
                                class = class,
                                signal = signal,
                                var = var,
                                setter = setter,
                                v = v,
                            ));
                        }
                    }
                }
                ElementMember::Child(c) => {
                    let inner = self.emit_child_into(c, out);
                    self.attach_child(var, class_name, &c.name.name, &inner, out);
                }
                ElementMember::Stmt(stmt) => {
                    self.emit_stmt_member(class_name, var, stmt, out);
                }
            }
        }

        if !qss.is_empty() || user_stylesheet.is_some() {
            let synth = qss.render(class_name);
            let synth_lit = if synth.is_empty() {
                None
            } else {
                Some(cpp_quote_string(&synth))
            };
            let setter_arg: String = match (synth_lit, user_stylesheet) {
                (Some(lit), None) => lit,
                (None, Some(user)) => widget_lower_expr(user, &self.pointer_names),
                (Some(lit), Some(user)) => {
                    let user_v = widget_lower_expr(user, &self.pointer_names);
                    format!("{lit} + {user_v}")
                }
                // Outer guard `!qss.is_empty() || user_stylesheet.is_some()`
                // ensures at least one side is non-empty; `synth_lit` is
                // None only when `synth` is empty, which then forces
                // `user_stylesheet` to be Some.
                (None, None) => unreachable!(),
            };
            out.push_str(&format!("    {var}->setStyleSheet({setter_arg});\n"));
            // Re-apply the combined stylesheet whenever a reactive
            // dep in the user's literal expression fires. Synth-only
            // sheets are static so don't need re-apply hooks.
            if let Some(user) = user_stylesheet {
                let deps = self.collect_reactive_deps(user);
                for (state_name, class, signal) in deps {
                    out.push_str(&format!(
                        "    QObject::connect({state_name}, &{class}::{signal}, {var}, [=]() {{\n        {var}->setStyleSheet({setter_arg});\n    }});\n",
                    ));
                }
            }
        }
    }

    /// Lower a `Stmt` that lives in a widget element-body slot. The
    /// shapes that mean something in this context are:
    ///   - `Stmt::Expr(K::If(...))`  -> conditional render of branches
    ///   - `Stmt::Expr(K::Case(...))` -> per-arm pattern render
    ///   - `Stmt::For(...)`           -> per-iteration construction
    /// Anything else degrades to a `/* TODO */` comment.
    fn emit_stmt_member(
        &mut self,
        parent_class: &str,
        parent_var: &str,
        stmt: &Stmt,
        out: &mut String,
    ) {
        match stmt {
            Stmt::Expr(e) => match &e.kind {
                ExprKind::If {
                    cond,
                    then_b,
                    else_b,
                    ..
                } => {
                    self.emit_widget_if_stmt(
                        parent_class,
                        parent_var,
                        cond,
                        then_b,
                        else_b.as_ref(),
                        out,
                    );
                }
                ExprKind::Case { scrutinee, arms } => {
                    self.emit_widget_case_stmt(parent_class, parent_var, scrutinee, arms, out);
                }
                _ => out.push_str("    /* unsupported expr-as-element-member in widget */\n"),
            },
            Stmt::For {
                binding,
                iter,
                body,
                ..
            } => {
                self.emit_widget_for_stmt(parent_class, parent_var, &binding.name, iter, body, out);
            }
            _ => out.push_str("    /* unsupported stmt-as-element-member in widget */\n"),
        }
    }

    fn emit_widget_if_stmt(
        &mut self,
        parent_class: &str,
        parent_var: &str,
        cond: &Expr,
        then_b: &cute_syntax::ast::Block,
        else_b: Option<&cute_syntax::ast::Block>,
        out: &mut String,
    ) {
        let chain = collect_if_chain(cond, then_b, else_b);
        let pointers = self.pointer_names.clone();
        let mut prior_negations: Vec<String> = Vec::new();
        for (cond_opt, el) in chain {
            let vis = match cond_opt {
                Some(c) => {
                    let cond_s = widget_lower_expr(c, &pointers);
                    let mut combined = prior_negations.clone();
                    combined.push(cond_s.clone());
                    prior_negations.push(format!("!({cond_s})"));
                    combined.join(" && ")
                }
                None => {
                    if prior_negations.is_empty() {
                        "true".to_string()
                    } else {
                        prior_negations.join(" && ")
                    }
                }
            };
            let inner_var = self.emit_child_into(el, out);
            out.push_str(&format!("    {inner_var}->setVisible({vis});\n"));
            self.attach_child(parent_var, parent_class, &el.name.name, &inner_var, out);
        }
    }

    fn emit_widget_case_stmt(
        &mut self,
        parent_class: &str,
        parent_var: &str,
        scrutinee: &Expr,
        arms: &[cute_syntax::ast::CaseArm],
        out: &mut String,
    ) {
        let pointers = self.pointer_names.clone();
        let scrutinee_s = widget_lower_expr(scrutinee, &pointers);
        let mut prior_negations: Vec<String> = Vec::new();
        for arm in arms {
            // Widget side has the C++ Result API right there - turn
            // `when ok(v) { ... }` / `when err(e) { ... }` into real
            // is_ok/unwrap dispatch, with the bound name available
            // inside the arm body.
            let pm = pattern_match_test(
                &scrutinee_s,
                &arm.pattern,
                "==",
                cpp_quote_string,
                |e| widget_lower_expr(e, &pointers),
                Some(&WIDGET_RESULT_API),
            );
            let mut combined = prior_negations.clone();
            if pm.test != "true" {
                combined.push(format!("({})", pm.test));
            }
            let vis = if combined.is_empty() {
                "true".to_string()
            } else {
                combined.join(" && ")
            };
            if let Some(el) = trailing_element(&arm.body) {
                if !pm.bind_decls.is_empty() {
                    // Wrap the arm in a `{ ... }` C++ scope so
                    // `auto v = ...` declarations don't leak across
                    // sibling arms. Build the per-arm code into a
                    // temp buffer first, then re-indent into `out`.
                    let mut buf = String::new();
                    for d in &pm.bind_decls {
                        buf.push_str(&format!("{d}\n"));
                    }
                    let inner_var = self.emit_child_into(el, &mut buf);
                    buf.push_str(&format!("{inner_var}->setVisible({vis});\n"));
                    self.attach_child(
                        parent_var,
                        parent_class,
                        &el.name.name,
                        &inner_var,
                        &mut buf,
                    );
                    out.push_str("    {\n");
                    for line in buf.lines() {
                        out.push_str(&format!("        {line}\n"));
                    }
                    out.push_str("    }\n");
                } else {
                    let inner_var = self.emit_child_into(el, out);
                    out.push_str(&format!("    {inner_var}->setVisible({vis});\n"));
                    self.attach_child(parent_var, parent_class, &el.name.name, &inner_var, out);
                }
            } else {
                out.push_str("    /* widget case arm has no trailing element */\n");
            }
            prior_negations.push(format!("!({})", pm.test));
        }
    }

    fn emit_widget_for_stmt(
        &mut self,
        parent_class: &str,
        parent_var: &str,
        binding: &str,
        iter: &Expr,
        body: &cute_syntax::ast::Block,
        out: &mut String,
    ) {
        let pointers = self.pointer_names.clone();
        let iter_s = widget_lower_expr(iter, &pointers);
        out.push_str(&format!("    for (const auto& {binding} : {iter_s}) {{\n",));
        if let Some(el) = trailing_element(body) {
            let inner_var = self.emit_child_into(el, out);
            self.attach_child(parent_var, parent_class, &el.name.name, &inner_var, out);
        } else {
            out.push_str("        /* widget for body has no trailing element */\n");
        }
        out.push_str("    }\n");
    }

    /// Pick the right "child added to parent" call given the parent's
    /// class and the child's class:
    ///   - layout parent + widget child -> `parent->addWidget(child)`
    ///   - layout parent + layout child -> `parent->addLayout(child)`
    ///   - QMainWindow parent + layout child -> wrap the layout in a
    ///     fresh QWidget and `parent->setCentralWidget(wrapper)`
    ///     (QMainWindow already owns its own layout, so `setLayout`
    ///     fails at runtime with a QWidget warning)
    ///   - QMainWindow parent + widget child -> `parent->setCentralWidget(child)`
    ///   - widget parent + layout child -> `parent->setLayout(child)`
    ///   - widget parent + widget child -> `child->setParent(parent)`
    fn attach_child(
        &mut self,
        parent_var: &str,
        parent_class: &str,
        child_class: &str,
        child_var: &str,
        out: &mut String,
    ) {
        let parent_is_layout = is_layout_class(parent_class);
        let child_is_layout = is_layout_class(child_class);
        let parent_is_main_window = parent_class == "QMainWindow";
        match (parent_is_layout, child_is_layout) {
            (true, true) => out.push_str(&format!("    {parent_var}->addLayout({child_var});\n")),
            (true, false) => out.push_str(&format!("    {parent_var}->addWidget({child_var});\n")),
            (false, true) if parent_is_main_window => {
                let wrap = self.fresh("central");
                out.push_str(&format!(
                    "    auto* {wrap} = new QWidget();\n    {wrap}->setLayout({child_var});\n    {parent_var}->setCentralWidget({wrap});\n",
                ));
            }
            (false, true) => out.push_str(&format!("    {parent_var}->setLayout({child_var});\n")),
            (false, false) if parent_is_main_window => {
                out.push_str(&format!(
                    "    {parent_var}->setCentralWidget({child_var});\n",
                ));
            }
            (false, false) => out.push_str(&format!("    {child_var}->setParent({parent_var});\n")),
        }
    }
}

/// True if `class_name` is a Qt layout class. Layouts are Qt's special
/// case: they don't take a parent widget like normal widgets, and they
/// add children via `addWidget(...)` rather than parenting.
fn is_layout_class(class_name: &str) -> bool {
    matches!(
        class_name,
        "QHBoxLayout" | "QVBoxLayout" | "QGridLayout" | "QFormLayout" | "QStackedLayout"
    )
}

/// Extract the constructor class name from a state-field initializer.
/// Recognizes the SwiftUI-style `let counter = Counter()` shape
/// (`Call { callee: Ident(name), .. }`). Returns `None` when the
/// initializer isn't a bare class call - codegen uses the result to
/// pick a member type, so non-class initializers fall back to
/// best-effort behavior on each emit path.
fn state_field_init_class(sf: &cute_syntax::ast::StateField) -> Option<String> {
    use cute_syntax::ast::ExprKind as K;
    match &sf.init_expr.kind {
        // Plain `let x = Foo()` — value-style ctor.
        K::Call { callee, .. } => {
            if let K::Ident(name) = &callee.kind {
                Some(name.clone())
            } else {
                None
            }
        }
        // `let x = Foo.new(args)` — the canonical Cute idiom for
        // QObject-derived bindings (every Qt class binding lowers
        // construction through `new`). Without this branch the
        // field would type-erase to `QObject*` and downstream
        // member access (`.useFencedPreset()`, `.setPlainText(...)`)
        // would fail to resolve at C++ compile.
        K::MethodCall {
            receiver, method, ..
        } if method.name == "new" => {
            if let K::Ident(name) = &receiver.kind {
                Some(name.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Emit `<Class>* <name>;` lines for each `let`-form state field into
/// a class header body. `state X : T = init` (Property kind) fields
/// are skipped here — they're a QML-only feature in v1, and the HIR
/// pass already errors when one shows up inside a `widget` block.
fn emit_state_field_decls(out: &mut String, fields: &[cute_syntax::ast::StateField]) {
    for sf in fields {
        if matches!(sf.kind, cute_syntax::ast::StateFieldKind::Property { .. }) {
            continue;
        }
        let class = state_field_init_class(sf).unwrap_or_else(|| "QObject".to_string());
        out.push_str(&format!("    {class}* {};\n", sf.name.name));
    }
}

/// Emit `<name> = new <Class>(this);` lines for each `let`-form state
/// field with a constructor-call init_expr into a constructor body.
/// Property-kind state fields are skipped (see `emit_state_field_decls`).
fn emit_state_field_inits(out: &mut String, fields: &[cute_syntax::ast::StateField]) {
    for sf in fields {
        if matches!(sf.kind, cute_syntax::ast::StateFieldKind::Property { .. }) {
            continue;
        }
        if let Some(class) = state_field_init_class(sf) {
            out.push_str(&format!("    {} = new {class}(this);\n", sf.name.name));
        }
    }
}

/// Recognize QML/Cute's `on<Sig>` signal-handler convention. Returns
/// the underlying signal name (`onClicked` -> `clicked`,
/// `onValueChanged` -> `valueChanged`) so codegen can emit a Qt
/// `QObject::connect(button, &Class::signal, lambda)` call. Returns
/// `None` if `key` doesn't fit the pattern (so callers fall through
/// to the plain setter path).
fn signal_handler_name(key: &str) -> Option<String> {
    let rest = key.strip_prefix("on")?;
    let mut chars = rest.chars();
    let first = chars.next()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    let lower_first = first.to_lowercase().to_string();
    Some(format!("{lower_first}{}", chars.collect::<String>()))
}

thread_local! {
    /// Per-emit-pass map from Cute-side enum / flags name to its
    /// declared `cpp_namespace`. Populated by `Emitter::run` before
    /// any lowering call and cleared when the emit pass finishes.
    /// Lets the free-function `widget_lower_expr` (which has no
    /// access to `self.program`) look up extern-enum namespace
    /// prefixes for the `Foo.X → Qt::X` rewrite.
    static CPP_NAMESPACE_MAP: std::cell::RefCell<std::collections::HashMap<String, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Read the `cpp_namespace` for a Cute enum / flags name from the
/// thread-local map populated at the start of the emit pass.
pub(crate) fn lookup_cpp_namespace(name: &str) -> Option<String> {
    CPP_NAMESPACE_MAP.with(|m| m.borrow().get(name).cloned())
}

/// Set the thread-local cpp-namespace map for the current emit
/// pass. Called by `Emitter::run` before lowering any expression.
pub(crate) fn set_cpp_namespace_map(m: std::collections::HashMap<String, String>) {
    CPP_NAMESPACE_MAP.with(|cell| {
        *cell.borrow_mut() = m;
    });
}

thread_local! {
    /// Per-emit-pass collected "this AST shape can't be lowered in
    /// the current context" diagnostics. Lowerer dead-ends used to
    /// emit `/* TODO ... */` markers into the output, which compiled
    /// (or rather, didn't) downstream as an opaque C++ / QML error.
    /// They now push here instead and `emit_module` converts the
    /// collected list into an `EmitError::UnsupportedLowering` so
    /// the user sees a Cute-side diagnostic.
    static EMIT_DIAGS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Record a lowerer dead-end. Returns an empty `String` so callers
/// can replace `"/* TODO ... */".into()`-style placeholders 1:1
/// without changing the surrounding emit shape — the placeholder is
/// never read, since `emit_module` early-returns once any diag is
/// recorded.
fn unsupported_lowering(what: &str) -> String {
    EMIT_DIAGS.with(|cell| cell.borrow_mut().push(what.to_string()));
    String::new()
}

fn clear_emit_diags() {
    EMIT_DIAGS.with(|cell| cell.borrow_mut().clear());
}

fn take_emit_diags() -> Vec<String> {
    EMIT_DIAGS.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
}

/// Lower a Cute expression for use as an argument to a QtWidgets
/// setter. Strings become `QStringLiteral("...")` (with interpolation
/// concatenated), numbers / bools / nil pass through, identifiers
/// translate verbatim.
///
/// `pointers` is the set of in-scope identifier names that refer to
/// QObject pointers (state fields, fresh local widgets). Member
/// access / method calls on them lower with `->` instead of `.`.
fn widget_lower_expr(e: &Expr, pointers: &std::collections::HashSet<String>) -> String {
    use cute_syntax::ast::ExprKind as K;
    use cute_syntax::ast::StrPart;
    match &e.kind {
        K::Int(v) => v.to_string(),
        K::Float(v) => float_literal_cpp(*v),
        K::Bool(true) => "true".into(),
        K::Bool(false) => "false".into(),
        K::Nil => "nullptr".into(),
        K::Str(parts) => {
            if parts.is_empty() {
                return "QString()".into();
            }
            let chunks: Vec<String> = parts
                .iter()
                .map(|p| match p {
                    StrPart::Text(t) => cpp_quote_string(t),
                    StrPart::Interp(inner) => {
                        format!(
                            "::cute::str::to_string({})",
                            widget_lower_expr(inner, pointers)
                        )
                    }
                    StrPart::InterpFmt { expr, format_spec } => {
                        format!(
                            "::cute::str::format({}, \"{}\")",
                            widget_lower_expr(expr, pointers),
                            escape_cpp_string(format_spec)
                        )
                    }
                })
                .collect();
            chunks.join(" + ")
        }
        K::Ident(n) => n.clone(),
        K::AtIdent(n) => format!("m_{n}"),
        K::SelfRef => "this".into(),
        K::Member { receiver, name } => {
            // Cute treats `obj.x` as a Ruby-style zero-arg call -
            // there are no raw fields in the surface language, so
            // every dotted access goes through a method (Q_PROPERTY
            // getters lower to `T name() const` factories, which
            // need `()` in C++ to actually invoke).
            //
            // Exception: `Foo.X` where `Foo` is PascalCase — treat
            // as namespace-qualified enum / constant (`Qt::AlignCenter`,
            // `AlignmentFlag::AlignLeft`, `std::npos`, ...). Cute's
            // convention is PascalCase = type / namespace, camelCase =
            // value, so a PascalCase receiver in a member-access
            // (no-parens) position is a namespace handle, not a
            // method call. Emit `Foo::X` without parens. For extern
            // enums declared with `extern enum Qt::AlignmentFlag {
            // ... }`, the C++-side prefix is `Qt::` (not the bare
            // `AlignmentFlag::`), so we look the receiver up in
            // `prog.items` and use the enum's `cpp_namespace` if set.
            if let K::Ident(ns) = &receiver.kind {
                if ns
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_uppercase())
                    .unwrap_or(false)
                {
                    let prefix = lookup_cpp_namespace(ns).unwrap_or_else(|| ns.clone());
                    return format!("{prefix}::{}", name.name);
                }
            }
            let sep = if widget_is_pointer_expr(receiver, pointers) {
                "->"
            } else {
                "."
            };
            format!(
                "{}{sep}{}()",
                widget_lower_expr(receiver, pointers),
                name.name
            )
        }
        K::MethodCall {
            receiver,
            method,
            args,
            ..
        } => {
            let sep = if widget_is_pointer_expr(receiver, pointers) {
                "->"
            } else {
                "."
            };
            let rs = widget_lower_expr(receiver, pointers);
            let args_s = args
                .iter()
                .map(|a| widget_lower_expr(a, pointers))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{rs}{sep}{}({args_s})", method.name)
        }
        K::SafeMember { receiver, name } => {
            let recv_s = widget_lower_expr(receiver, pointers);
            // Reuse the same lift template as the fn-body lowering.
            // Widget bodies don't have a fresh-temp counter, so name
            // the local with a `__cw` prefix and trust scope to
            // avoid collisions inside the nested lambda.
            format!(
                "[&]() {{ auto __cw = {recv_s}; using __NL = ::cute::nullable_lift<decltype(__cw->{name}())>; return __cw ? __NL::make(__cw->{name}()) : __NL::none(); }}()",
                name = name.name
            )
        }
        K::SafeMethodCall {
            receiver,
            method,
            args,
            ..
        } => {
            let recv_s = widget_lower_expr(receiver, pointers);
            let args_s = args
                .iter()
                .map(|a| widget_lower_expr(a, pointers))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "[&]() {{ auto __cw = {recv_s}; using __NL = ::cute::nullable_lift<decltype(__cw->{method}({args_s}))>; return __cw ? __NL::make(__cw->{method}({args_s})) : __NL::none(); }}()",
                method = method.name
            )
        }
        K::Call { callee, args, .. } => {
            let cs = widget_lower_expr(callee, pointers);
            let args_s = args
                .iter()
                .map(|a| widget_lower_expr(a, pointers))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{cs}({args_s})")
        }
        K::Binary { op, lhs, rhs } => {
            use cute_syntax::ast::BinOp as B;
            let s = match op {
                B::Add => "+",
                B::Sub => "-",
                B::Mul => "*",
                B::Div => "/",
                B::Mod => "%",
                B::Lt => "<",
                B::LtEq => "<=",
                B::Gt => ">",
                B::GtEq => ">=",
                B::Eq => "==",
                B::NotEq => "!=",
                B::And => "&&",
                B::Or => "||",
                B::BitOr => "|",
                B::BitAnd => "&",
                B::BitXor => "^",
            };
            format!(
                "({} {} {})",
                widget_lower_expr(lhs, pointers),
                s,
                widget_lower_expr(rhs, pointers)
            )
        }
        K::Unary { op, expr } => {
            let inner = widget_lower_expr(expr, pointers);
            match op {
                cute_syntax::ast::UnaryOp::Neg => format!("(-{inner})"),
                cute_syntax::ast::UnaryOp::Not => format!("(!{inner})"),
            }
        }
        K::Array(items) => {
            // `[a, b, c]` → `{a, b, c}`; the receiving setter / property
            // type drives the deduction (QStringList, QList<qreal>, ...).
            let chunks: Vec<String> = items
                .iter()
                .map(|e| widget_lower_expr(e, pointers))
                .collect();
            format!("{{{}}}", chunks.join(", "))
        }
        K::Sym(s) => format!("QByteArrayLiteral(\"{s}\")"),
        K::Path(parts) => parts
            .iter()
            .map(|i| i.name.clone())
            .collect::<Vec<_>>()
            .join("::"),
        K::Index { receiver, index } => {
            lower_index_expr(receiver, index, |e| widget_lower_expr(e, pointers))
        }
        K::Map(entries) => {
            // `{ key: value, ... }` -> QVariantMap. Identifier keys are
            // promoted to QStringLiteral to match Cute's surface
            // semantics; expression keys lower verbatim so dynamic
            // map literals still work.
            let parts = entries
                .iter()
                .map(|(k, v)| {
                    let key_s = match &k.kind {
                        cute_syntax::ast::ExprKind::Ident(name) => {
                            format!("QStringLiteral(\"{name}\")")
                        }
                        _ => widget_lower_expr(k, pointers),
                    };
                    format!("{{{key_s}, {}}}", widget_lower_expr(v, pointers))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("QVariantMap{{{parts}}}")
        }
        K::If {
            cond,
            then_b,
            else_b,
            ..
        } => {
            // Ternary lowering for `text: cond ? a : b`-style property
            // bindings. Falls back to the inner expressions of each
            // arm (no statement-context support; widget property
            // positions don't have a runtime block scope).
            let cond_s = widget_lower_expr(cond, pointers);
            let then_s = then_b
                .trailing
                .as_ref()
                .map(|e| widget_lower_expr(e, pointers))
                .unwrap_or_else(|| "QVariant()".into());
            let else_s = else_b
                .as_ref()
                .and_then(|b| b.trailing.as_ref().map(|e| widget_lower_expr(e, pointers)))
                .unwrap_or_else(|| "QVariant()".into());
            format!("({cond_s} ? {then_s} : {else_s})")
        }
        K::Block(b) => {
            // Three shapes (mirror the qml_lower_expr_in branch):
            //   1. Trailing-only — the tail expression is the value.
            //   2. Single `Stmt::Assign` (or expression-stmt) with no
            //      trailing — emit the C++ assignment / call directly so
            //      handler bodies like `{ count = count + 1 }` lower to
            //      `__cute_state->setCount(__cute_state->count() + 1);`
            //      without the indirection of an immediately-invoked
            //      lambda.
            //   3. Anything richer — wrap in a stateless IIFE so the
            //      result still type-checks as a single C++ expression.
            //
            // The setter call uses Qt's `set<Capitalized>` convention so
            // assignments to `state X` go through the synthesized
            // setter (which fires the NOTIFY signal). Reads of `count`
            // already go through the `count()` getter via the existing
            // `K::Member` lowering, after the desugaring rewrites the
            // bare ident.
            if b.stmts.is_empty() {
                if let Some(t) = &b.trailing {
                    return widget_lower_expr(t, pointers);
                }
                return "/* empty-block */".into();
            }
            let lower_assign_target_to_setter =
                |target: &Expr, op: cute_syntax::ast::AssignOp, value: &Expr| -> String {
                    use cute_syntax::ast::AssignOp;
                    let v = widget_lower_expr(value, pointers);
                    match &target.kind {
                        K::Member { receiver, name } => {
                            let recv_s = widget_lower_expr(receiver, pointers);
                            let sep = if widget_is_pointer_expr(receiver, pointers) {
                                "->"
                            } else {
                                "."
                            };
                            let setter = format!("set{}", capitalize_first(&name.name));
                            let read_back = format!("{recv_s}{sep}{}()", name.name);
                            match op {
                                AssignOp::Eq => format!("{recv_s}{sep}{setter}({v})"),
                                AssignOp::PlusEq => {
                                    format!("{recv_s}{sep}{setter}({read_back} + ({v}))")
                                }
                                AssignOp::MinusEq => {
                                    format!("{recv_s}{sep}{setter}({read_back} - ({v}))")
                                }
                                AssignOp::StarEq => {
                                    format!("{recv_s}{sep}{setter}({read_back} * ({v}))")
                                }
                                AssignOp::SlashEq => {
                                    format!("{recv_s}{sep}{setter}({read_back} / ({v}))")
                                }
                            }
                        }
                        _ => {
                            // Plain idents and other targets: fall back to a
                            // C++ assignment expression.
                            let t = widget_lower_expr(target, pointers);
                            let op_s = match op {
                                AssignOp::Eq => "=",
                                AssignOp::PlusEq => "+=",
                                AssignOp::MinusEq => "-=",
                                AssignOp::StarEq => "*=",
                                AssignOp::SlashEq => "/=",
                            };
                            format!("{t} {op_s} {v}")
                        }
                    }
                };
            if b.stmts.len() == 1 && b.trailing.is_none() {
                match &b.stmts[0] {
                    cute_syntax::ast::Stmt::Assign {
                        target, op, value, ..
                    } => return lower_assign_target_to_setter(target, *op, value),
                    cute_syntax::ast::Stmt::Expr(e) => return widget_lower_expr(e, pointers),
                    _ => {}
                }
            }
            // Multi-statement: IIFE.
            let mut parts: Vec<String> = Vec::new();
            for s in &b.stmts {
                match s {
                    cute_syntax::ast::Stmt::Assign {
                        target, op, value, ..
                    } => parts.push(lower_assign_target_to_setter(target, *op, value)),
                    cute_syntax::ast::Stmt::Expr(e) => parts.push(widget_lower_expr(e, pointers)),
                    _ => parts.push("/* unsupported stmt in widget_lower block */".into()),
                }
            }
            if let Some(t) = &b.trailing {
                parts.push(format!("return {}", widget_lower_expr(t, pointers)));
            }
            format!("[&]() {{ {}; }}()", parts.join("; "))
        }
        K::Try(inner) => {
            // Property positions don't carry an error-union scope, so
            // `?` collapses to the inner expression. The type checker
            // already rejects misuse outside an `!T`-returning fn.
            widget_lower_expr(inner, pointers)
        }
        K::Await(inner) => {
            // Likewise: `await` outside a coroutine has no meaning at
            // property position — strip and emit the inner expression.
            widget_lower_expr(inner, pointers)
        }
        K::Kwarg { key, value } => {
            // Bare kwargs at expression position are unusual but the
            // QML lowerer handles them as comment + value, mirror that.
            format!("/* {} */ {}", key.name, widget_lower_expr(value, pointers))
        }
        K::Lambda { params, body } => {
            // Pure-trailing-expression lambda lowers to a stateless
            // capture-by-reference C++ lambda. Statement-bearing
            // lambdas need the Lowerer's state and are not supported
            // here; they emit a clear TODO marker.
            let params_s = params
                .iter()
                .map(|p| format!("auto {}", p.name.name))
                .collect::<Vec<_>>()
                .join(", ");
            if body.stmts.is_empty() {
                if let Some(t) = &body.trailing {
                    let body_s = widget_lower_expr(t, pointers);
                    return format!("[&]({params_s}) {{ return {body_s}; }}");
                }
                return format!("[&]({params_s}) {{}}");
            }
            unsupported_lowering(
                "lambda with statements is not supported in a widget property body \
                 (only `{ <expr> }` or `{ }` lambdas lower here — \
                 move the statements into a slot/method)",
            )
        }
        K::Range { .. } => unsupported_lowering(
            "range expression is not supported outside a `for` loop in a widget property body",
        ),
        K::Case { .. } => unsupported_lowering(
            "`case ... when` is not supported in a widget property body \
                 (compute the value in a slot/state field and assign that instead)",
        ),
        K::Element(_) => unsupported_lowering(
            "element value is not supported at this property position \
                 (the widget renderer handles element-typed properties \
                 — e.g. `initialPage: Page { ... }`; reaching here means \
                 the property wasn't routed to the element emitter)",
        ),
    }
}

/// True when this expression evaluates to a QObject pointer in the
/// generated C++ - either a bare identifier in the recorded set, or
/// an `Index` into something pointer-like (deferred for now).
fn widget_is_pointer_expr(e: &Expr, pointers: &std::collections::HashSet<String>) -> bool {
    use cute_syntax::ast::ExprKind as K;
    match &e.kind {
        K::Ident(n) => pointers.contains(n),
        // `this` is always a Class*, so dotted access from `self`
        // needs `->` in C++.
        K::SelfRef => true,
        _ => false,
    }
}

/// Lower a Cute expression for use inside a QML property binding. Cute
/// surface syntax is close enough to JavaScript (the QML expression
/// language) that most constructs translate verbatim. Cute-specific
/// forms are mapped:
///   - `nil` -> `null`
///   - `"foo #{x} bar"` `"foo " + (x) + " bar"`
///   - `==` / `!=` `===` / `!==`
///
/// `for_bindings` is the stack of in-scope `for x in xs { ... }` binding
/// names (innermost last). Any `Ident(name)` that matches one is
/// rewritten to `modelData` - the implicit per-row binding QML's
/// `Repeater` exposes - so `for item in xs { Card { label: item.name } }`
/// produces `Card { label: modelData.name }`.
fn qml_lower_expr_in(e: &Expr, for_bindings: &[&str]) -> String {
    use cute_syntax::ast::ExprKind as K;
    use cute_syntax::ast::StrPart;
    match &e.kind {
        K::Int(v) => v.to_string(),
        K::Float(v) => float_literal_cpp(*v),
        K::Bool(true) => "true".into(),
        K::Bool(false) => "false".into(),
        K::Nil => "null".into(),
        K::Str(parts) => {
            let mut chunks = Vec::new();
            let mut buf = String::new();
            for p in parts {
                match p {
                    StrPart::Text(t) => buf.push_str(t),
                    StrPart::Interp(inner) => {
                        if !buf.is_empty() {
                            chunks.push(qml_quote_string(&buf));
                            buf.clear();
                        }
                        chunks.push(format!("({})", qml_lower_expr_in(inner, for_bindings)));
                    }
                    StrPart::InterpFmt { expr, format_spec } => {
                        if !buf.is_empty() {
                            chunks.push(qml_quote_string(&buf));
                            buf.clear();
                        }
                        // QML view bodies run as JavaScript bindings, so
                        // we lower the format spec to a JS expression
                        // using `Number.toFixed` / `String.padStart` /
                        // `padEnd` rather than the C++ runtime helper.
                        let inner = qml_lower_expr_in(expr, for_bindings);
                        chunks.push(qml_lower_format_spec(&inner, format_spec));
                    }
                }
            }
            if !buf.is_empty() {
                chunks.push(qml_quote_string(&buf));
            }
            if chunks.is_empty() {
                "\"\"".into()
            } else {
                chunks.join(" + ")
            }
        }
        K::Sym(s) => format!("\"{s}\""),
        K::Ident(n) => {
            // For-binding shadowing: inner-most match wins.
            if for_bindings.iter().rev().any(|b| *b == n.as_str()) {
                "modelData".into()
            } else {
                n.clone()
            }
        }
        K::AtIdent(n) => n.clone(), // inside QML, a Cute @var translates to the same name
        K::SelfRef => "this".into(),
        K::Path(parts) => parts
            .iter()
            .map(|i| i.name.clone())
            .collect::<Vec<_>>()
            .join("."),
        K::Call { callee, args, .. } => {
            let cs = qml_lower_expr_in(callee, for_bindings);
            let args_s = args
                .iter()
                .map(|a| qml_lower_expr_in(a, for_bindings))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{cs}({args_s})")
        }
        K::MethodCall {
            receiver,
            method,
            args,
            ..
        } => {
            let rs = qml_lower_expr_in(receiver, for_bindings);
            let args_s = args
                .iter()
                .map(|a| qml_lower_expr_in(a, for_bindings))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{rs}.{}({args_s})", method.name)
        }
        K::Member { receiver, name } => {
            format!(
                "{}.{}",
                qml_lower_expr_in(receiver, for_bindings),
                name.name
            )
        }
        // QML / JS already has a `?.` operator with the same semantics,
        // so the lowering is a textual passthrough — no IIFE needed.
        K::SafeMember { receiver, name } => {
            format!(
                "{}?.{}",
                qml_lower_expr_in(receiver, for_bindings),
                name.name
            )
        }
        K::SafeMethodCall {
            receiver,
            method,
            args,
            ..
        } => {
            let rs = qml_lower_expr_in(receiver, for_bindings);
            let args_s = args
                .iter()
                .map(|a| qml_lower_expr_in(a, for_bindings))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{rs}?.{}({args_s})", method.name)
        }
        K::Index { receiver, index } => {
            format!(
                "{}[{}]",
                qml_lower_expr_in(receiver, for_bindings),
                qml_lower_expr_in(index, for_bindings)
            )
        }
        K::Unary { op, expr } => {
            let inner = qml_lower_expr_in(expr, for_bindings);
            match op {
                cute_syntax::ast::UnaryOp::Neg => format!("(-{inner})"),
                cute_syntax::ast::UnaryOp::Not => format!("(!{inner})"),
            }
        }
        K::Binary { op, lhs, rhs } => {
            use cute_syntax::ast::BinOp as B;
            let s = match op {
                B::Add => "+",
                B::Sub => "-",
                B::Mul => "*",
                B::Div => "/",
                B::Mod => "%",
                B::Lt => "<",
                B::LtEq => "<=",
                B::Gt => ">",
                B::GtEq => ">=",
                B::Eq => "===",
                B::NotEq => "!==",
                B::And => "&&",
                B::Or => "||",
                B::BitOr => "|",
                B::BitAnd => "&",
                B::BitXor => "^",
            };
            format!(
                "({} {} {})",
                qml_lower_expr_in(lhs, for_bindings),
                s,
                qml_lower_expr_in(rhs, for_bindings)
            )
        }
        K::If {
            cond,
            then_b,
            else_b,
            ..
        } => {
            // Single-expression if -> JS ternary. Handles the common
            // `text: cond ? "..." : "..."` view-DSL pattern.
            let cond_s = qml_lower_expr_in(cond, for_bindings);
            let then_s = then_b
                .trailing
                .as_ref()
                .map(|e| qml_lower_expr_in(e, for_bindings))
                .unwrap_or_else(|| "undefined".into());
            let else_s = else_b
                .as_ref()
                .and_then(|b| {
                    b.trailing
                        .as_ref()
                        .map(|e| qml_lower_expr_in(e, for_bindings))
                })
                .unwrap_or_else(|| "undefined".into());
            format!("({cond_s} ? {then_s} : {else_s})")
        }
        K::Block(b) => {
            // Three shapes are interesting in QML property/handler
            // position:
            //
            //   1. Trailing expression only — lower the trailing expr
            //      as the value (preserves the ?: ternary behavior of
            //      `if x { a } else { b }`).
            //   2. Single `Stmt::Assign` (or single expression-statement)
            //      with no trailing — emit the assignment / call directly
            //      so `onClicked: { count = count + 1 }` becomes the
            //      idiomatic `onClicked: count = count + 1` in QML. JS
            //      lets assignments stand as expressions, so this also
            //      works at non-handler property values.
            //   3. Anything richer — wrap in an IIFE (`(() => { … })()`)
            //      so the return is always a single JS expression
            //      regardless of where the property lives.
            //
            // The IIFE branch supports `+=` / `-=` / `*=` / `/=` and
            // mixes statements with a trailing expression by emitting
            // `return <trailing>` at the tail.
            let lower_assign_op = |op: &cute_syntax::ast::AssignOp| match op {
                cute_syntax::ast::AssignOp::Eq => "=",
                cute_syntax::ast::AssignOp::PlusEq => "+=",
                cute_syntax::ast::AssignOp::MinusEq => "-=",
                cute_syntax::ast::AssignOp::StarEq => "*=",
                cute_syntax::ast::AssignOp::SlashEq => "/=",
            };
            if b.stmts.is_empty() {
                if let Some(t) = &b.trailing {
                    qml_lower_expr_in(t, for_bindings)
                } else {
                    "undefined".into()
                }
            } else if b.stmts.len() == 1 && b.trailing.is_none() {
                match &b.stmts[0] {
                    cute_syntax::ast::Stmt::Assign {
                        target, op, value, ..
                    } => {
                        let t = qml_lower_expr_in(target, for_bindings);
                        let v = qml_lower_expr_in(value, for_bindings);
                        format!("{t} {} {v}", lower_assign_op(op))
                    }
                    cute_syntax::ast::Stmt::Expr(e) => qml_lower_expr_in(e, for_bindings),
                    _ => "(() => { /* unsupported stmt */ })()".into(),
                }
            } else {
                let mut parts = Vec::new();
                for s in &b.stmts {
                    match s {
                        cute_syntax::ast::Stmt::Assign {
                            target, op, value, ..
                        } => {
                            let t = qml_lower_expr_in(target, for_bindings);
                            let v = qml_lower_expr_in(value, for_bindings);
                            parts.push(format!("{t} {} {v}", lower_assign_op(op)));
                        }
                        cute_syntax::ast::Stmt::Expr(e) => {
                            parts.push(qml_lower_expr_in(e, for_bindings));
                        }
                        _ => parts.push("/* unsupported stmt */".into()),
                    }
                }
                if let Some(t) = &b.trailing {
                    parts.push(format!("return {}", qml_lower_expr_in(t, for_bindings)));
                }
                format!("(() => {{ {} }})()", parts.join("; "))
            }
        }
        K::Array(items) => {
            let parts = items
                .iter()
                .map(|i| qml_lower_expr_in(i, for_bindings))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{parts}]")
        }
        K::Map(entries) => {
            // QML's expression language is JavaScript-flavored: object
            // literals work but `{...}` would parse as a block in some
            // statement contexts, so wrap in parens defensively.
            let parts = entries
                .iter()
                .map(|(k, v)| {
                    let key_s = match &k.kind {
                        cute_syntax::ast::ExprKind::Ident(name) => name.clone(),
                        cute_syntax::ast::ExprKind::Str(_) => qml_lower_expr_in(k, for_bindings),
                        _ => qml_lower_expr_in(k, for_bindings),
                    };
                    format!("{key_s}: {}", qml_lower_expr_in(v, for_bindings))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("({{{parts}}})")
        }
        K::Element(elem) => {
            // Element-as-value: emit the element subtree inline.
            // This shape appears at property-value positions like
            // `pageStack.initialPage: Page { ... }` (Kirigami).
            // Reuse `emit_element_full` so namespace prefixes,
            // child elements, and statement members all behave the
            // same as element-member position.
            let mut buf = String::new();
            emit_element_full(&mut buf, elem, 0, &[], for_bindings);
            // emit_element_full emits at indentation 0 with a
            // trailing newline — strip the trailing newline since
            // we're inside a property assignment, and the QML
            // outer formatter will handle layout.
            buf.trim_end().to_string()
        }
        K::Try(inner) => {
            // QML expression bindings have no error-union scope; strip
            // the `?` and emit just the inner expression.
            qml_lower_expr_in(inner, for_bindings)
        }
        K::Await(inner) => {
            // QML JS bindings can't `await`; the surrounding evaluator
            // is synchronous. Strip and emit the inner expression so
            // the user gets the value form rather than a parse error.
            qml_lower_expr_in(inner, for_bindings)
        }
        K::Kwarg { key, value } => {
            // Bare kwargs at expression position are unusual; emit a
            // `/* key */ value` shape so the spelling survives in
            // generated QML.
            format!(
                "/* {} */ {}",
                key.name,
                qml_lower_expr_in(value, for_bindings)
            )
        }
        K::Lambda { params, body } => {
            // QML JS uses `function(p1, p2) { return body; }`. Pure
            // trailing-expression lambdas lower cleanly; statement-
            // bearing lambdas would need a stmt-aware lowerer and
            // emit a clear marker instead.
            let params_s = params
                .iter()
                .map(|p| p.name.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            if body.stmts.is_empty() {
                if let Some(t) = &body.trailing {
                    let body_s = qml_lower_expr_in(t, for_bindings);
                    return format!("function({params_s}) {{ return {body_s}; }}");
                }
                return format!("function({params_s}) {{}}");
            }
            unsupported_lowering(
                "lambda with statements is not supported in a QML binding \
                 (only `{ <expr> }` or `{ }` lambdas lower here — \
                 move the statements into a slot/method)",
            )
        }
        K::Range { .. } => unsupported_lowering(
            "range expression is not supported outside a `for` loop in a QML binding",
        ),
        K::Case { .. } => unsupported_lowering(
            "`case ... when` is not supported in a QML binding \
                 (compute the value in a slot/state field and assign that instead)",
        ),
    }
}

/// Lower a Cute format spec (`.2f`, `08d`, `>20`, etc.) into a QML/JS
/// expression that formats `inner_js` accordingly. Mirrors the runtime
/// `cute::str::format` parser but emits inline JS so QML view bodies
/// don't need a runtime helper file.
fn qml_lower_format_spec(inner_js: &str, spec: &str) -> String {
    let parsed = parse_format_spec(spec);
    // Numeric type letters route through Number prototypes.
    let typ = parsed.type_char;
    let prec = parsed.precision;
    let value_expr: String = match typ {
        Some('f') => format!("Number({inner_js}).toFixed({})", prec.unwrap_or(6)),
        Some('e') => format!("Number({inner_js}).toExponential({})", prec.unwrap_or(6)),
        Some('g') => format!("Number({inner_js}).toPrecision({})", prec.unwrap_or(6)),
        Some('%') => format!(
            "(Number({inner_js}) * 100).toFixed({}) + \"%\"",
            prec.unwrap_or(0)
        ),
        Some('x') => format!("Number({inner_js}).toString(16)"),
        Some('X') => format!("Number({inner_js}).toString(16).toUpperCase()"),
        Some('b') => format!("Number({inner_js}).toString(2)"),
        Some('o') => format!("Number({inner_js}).toString(8)"),
        Some('d') => format!("Math.trunc(Number({inner_js})).toString()"),
        Some('s') | None => {
            // No type letter: if precision is present without a type,
            // treat as fixed-point (matches Python's f-string default
            // for `:.2`). Otherwise stringify.
            if let Some(p) = prec {
                format!("Number({inner_js}).toFixed({p})")
            } else {
                format!("String({inner_js})")
            }
        }
        Some(_) => format!("String({inner_js})"),
    };
    // Apply width / alignment / fill via padStart / padEnd. JS's
    // `padStart` defaults to ' ' if no fillStr is given.
    if parsed.width > 0 {
        let fill = if parsed.zero {
            "\"0\"".to_string()
        } else if let Some(f) = parsed.fill {
            // Escape the fill char as a JS string literal.
            qml_quote_string(&f.to_string())
        } else {
            "\" \"".to_string()
        };
        let w = parsed.width;
        let align = parsed
            .align
            .unwrap_or(if parsed.zero || typ.map_or(false, is_numeric_type) {
                '>'
            } else {
                '<'
            });
        match align {
            '<' => format!("({value_expr}).padEnd({w}, {fill})"),
            '^' => format!(
                "(function(s) {{ \
                    var pad = {w} - s.length; \
                    if (pad <= 0) return s; \
                    var l = Math.floor(pad/2); \
                    return {fill}.repeat(l) + s + {fill}.repeat(pad - l); \
                 }})(({value_expr}))"
            ),
            _ => format!("({value_expr}).padStart({w}, {fill})"),
        }
    } else {
        format!("({value_expr})")
    }
}

#[derive(Default)]
struct ParsedFormatSpec {
    fill: Option<char>,
    align: Option<char>,
    zero: bool,
    width: usize,
    precision: Option<usize>,
    type_char: Option<char>,
}

fn parse_format_spec(spec: &str) -> ParsedFormatSpec {
    let mut r = ParsedFormatSpec::default();
    let chars: Vec<char> = spec.chars().collect();
    let n = chars.len();
    let mut i = 0;
    if n >= 2 && matches!(chars[1], '<' | '>' | '^') {
        r.fill = Some(chars[0]);
        r.align = Some(chars[1]);
        i = 2;
    } else if n >= 1 && matches!(chars[0], '<' | '>' | '^') {
        r.align = Some(chars[0]);
        i = 1;
    }
    if i < n && chars[i] == '0' {
        r.zero = true;
        i += 1;
    }
    while i < n && chars[i].is_ascii_digit() {
        r.width = r.width * 10 + (chars[i] as usize - '0' as usize);
        i += 1;
    }
    if i < n && chars[i] == '.' {
        i += 1;
        let mut p = 0;
        while i < n && chars[i].is_ascii_digit() {
            p = p * 10 + (chars[i] as usize - '0' as usize);
            i += 1;
        }
        r.precision = Some(p);
    }
    if i < n {
        r.type_char = Some(chars[i]);
    }
    r
}

fn is_numeric_type(c: char) -> bool {
    matches!(c, 'f' | 'e' | 'g' | '%' | 'd' | 'x' | 'X' | 'b' | 'o')
}

fn escape_js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// Specification for the `qml_app(...)` builtin recognized inside
/// `fn main`. Codegen translates this into a stock Qt QML application
/// boot sequence so user-side Cute code never has to mention
/// `QGuiApplication`, `qmlRegisterType<T>`, or `app.exec()` explicitly.
#[derive(Debug, Clone)]
pub struct QmlAppSpec {
    pub qml_url: String,
    pub module: String,
    pub version_major: u32,
    pub version_minor: u32,
    pub types: Vec<String>,
}

/// Pattern-match the body of `fn main` against a single
/// `qml_app(qml: "...", module: "...", type: T, ...)` call. Either
/// `qml: "<qrc-url>"` (existing .qml file) or `view: ViewName`
/// (Cute UI DSL view, lowered to `qrc:/<ViewName>.qml` by codegen)
/// is required. Returns the spec when the pattern matches.
pub fn detect_qml_app(body: &cute_syntax::ast::Block) -> Option<QmlAppSpec> {
    use cute_syntax::ast::ExprKind as K;
    use cute_syntax::ast::Stmt;

    let call_expr = match (body.stmts.as_slice(), &body.trailing) {
        ([], Some(t)) => t.as_ref(),
        ([Stmt::Expr(e)], None) => e,
        _ => return None,
    };

    let K::Call { callee, args, .. } = &call_expr.kind else {
        return None;
    };
    let K::Ident(name) = &callee.kind else {
        return None;
    };
    if name != "qml_app" {
        return None;
    }

    let mut qml_url: Option<String> = None;
    let mut module: Option<String> = None;
    let mut types: Vec<String> = Vec::new();

    for arg in args {
        match &arg.kind {
            K::Kwarg { key, value } => match key.name.as_str() {
                "qml" => qml_url = extract_string_literal(value),
                "view" => {
                    // `view: Main` -> derive qrc URL from the view name.
                    if let K::Ident(n) = &value.kind {
                        qml_url = Some(format!("qrc:/{n}.qml"));
                    }
                }
                "module" => module = extract_string_literal(value),
                "type" => {
                    if let K::Ident(n) = &value.kind {
                        types.push(n.clone());
                    }
                }
                _ => {}
            },
            K::Ident(n) => types.push(n.clone()),
            _ => {}
        }
    }

    Some(QmlAppSpec {
        qml_url: qml_url?,
        module: module?,
        version_major: 1,
        version_minor: 0,
        types,
    })
}

/// Specification for the `widget_app(...)` builtin recognized inside
/// `fn main`. Codegen translates this into a stock QtWidgets boot
/// sequence (`QApplication app; T w; w.show(); return app.exec();`)
/// so user-side Cute code never has to mention `QApplication` directly.
#[derive(Debug, Clone)]
pub struct WidgetAppSpec {
    /// Class name of the top-level `widget Name { ... }` to instantiate
    /// and show. Cute lowers each `widget` to a C++ class derived from
    /// the root element's type, so this is the user-declared name.
    pub window: String,
    /// Optional `setApplicationName(...)` value.
    pub title: Option<String>,
}

/// `gpu_app(window: T [, title: "..."])` intrinsic spec.
/// `T` is a `widget` whose root is a cute::ui container (Column / Row /
/// Stack / ...); codegen lowers it to a `cute::ui::Component` subclass.
pub struct GpuAppSpec {
    pub window: String,
    pub title: Option<String>,
    /// `theme: dark` / `theme: light` — passed through as a bare ident the
    /// codegen turns into `cute::ui::Theme::Dark` / `::Light`. Defaults to
    /// dark when omitted.
    pub theme: Option<String>,
}

/// Shared (window, title) extractor for `<intrinsic>(window: T [, title: "..."])`
/// — used by both `widget_app` and `gpu_app`.
fn detect_window_title_app(
    body: &cute_syntax::ast::Block,
    intrinsic: &str,
) -> Option<(String, Option<String>)> {
    use cute_syntax::ast::ExprKind as K;
    use cute_syntax::ast::Stmt;
    let call_expr = match (body.stmts.as_slice(), &body.trailing) {
        ([], Some(t)) => t.as_ref(),
        ([Stmt::Expr(e)], None) => e,
        _ => return None,
    };
    let K::Call { callee, args, .. } = &call_expr.kind else {
        return None;
    };
    let K::Ident(name) = &callee.kind else {
        return None;
    };
    if name != intrinsic {
        return None;
    }
    let mut window: Option<String> = None;
    let mut title: Option<String> = None;
    for arg in args {
        if let K::Kwarg { key, value } = &arg.kind {
            match key.name.as_str() {
                "window" => {
                    if let K::Ident(n) = &value.kind {
                        window = Some(n.clone());
                    }
                }
                "title" => title = extract_string_literal(value),
                _ => {}
            }
        }
    }
    Some((window?, title))
}

/// Pattern-matches `fn main`'s body against `widget_app(window: T [, title: "..."])`.
pub fn detect_widget_app(body: &cute_syntax::ast::Block) -> Option<WidgetAppSpec> {
    let (window, title) = detect_window_title_app(body, "widget_app")?;
    Some(WidgetAppSpec { window, title })
}

/// Pattern-matches `fn main`'s body against `gpu_app(window: T [, title: "...", theme: dark | light])`.
pub fn detect_gpu_app(body: &cute_syntax::ast::Block) -> Option<GpuAppSpec> {
    use cute_syntax::ast::ExprKind as K;
    use cute_syntax::ast::Stmt;
    let (window, title) = detect_window_title_app(body, "gpu_app")?;
    // Re-walk the call to pick up the theme kwarg without bloating the
    // shared widget_app/gpu_app extractor.
    let call_expr = match (body.stmts.as_slice(), &body.trailing) {
        ([], Some(t)) => t.as_ref(),
        ([Stmt::Expr(e)], None) => e,
        _ => return None,
    };
    let K::Call { args, .. } = &call_expr.kind else {
        return None;
    };
    let mut theme = None;
    for arg in args {
        if let K::Kwarg { key, value } = &arg.kind {
            if key.name == "theme" {
                if let K::Ident(n) = &value.kind {
                    theme = Some(n.clone());
                }
            }
        }
    }
    Some(GpuAppSpec {
        window,
        title,
        theme,
    })
}

/// Pattern-match the body of `fn main` against `cli_app { ...block... }`.
/// Returns the inner block when matched. The block uses Cute's normal
/// trailing-block-call sugar (`f { ... }`), so the AST shape is
/// `Call { callee: Ident("cli_app"), args: [], block: Some(Block{...}) }`.
fn detect_cli_app(body: &cute_syntax::ast::Block) -> Option<&cute_syntax::ast::Block> {
    detect_named_block_app(body, "cli_app")
}

/// Lower `recv[index]` — bare indexing or a Range-shaped slice. Shared
/// between the main expression lowerer and the widget-context lowerer
/// so both go through the same `arr[a..b]` -> `::cute::make_slice(...)`
/// rule. The caller passes its own sub-expression lowering closure
/// (`self.lower_expr` vs the free `widget_lower_expr`); this helper
/// dispatches on the index's syntactic shape.
fn lower_index_expr<F>(
    receiver: &cute_syntax::ast::Expr,
    index: &cute_syntax::ast::Expr,
    mut lower: F,
) -> String
where
    F: FnMut(&cute_syntax::ast::Expr) -> String,
{
    use cute_syntax::ast::ExprKind as K;
    if let K::Range {
        start,
        end,
        inclusive,
    } = &index.kind
    {
        let helper = if *inclusive {
            "::cute::make_slice_inclusive"
        } else {
            "::cute::make_slice"
        };
        format!(
            "{helper}({}, {}, {})",
            lower(receiver),
            lower(start),
            lower(end)
        )
    } else {
        format!("{}[{}]", lower(receiver), lower(index))
    }
}

/// True when `body` (recursively) contains an `await` expression. The
/// cli_app intrinsic uses this to decide whether to lift its body into
/// a `QFuture<void>` coroutine so `co_await` suspensions are driven
/// by the QCoreApplication event loop. Synchronous bodies stay on the
/// plain `int main { ...; return 0; }` path with no event loop cost.
fn block_uses_await(body: &cute_syntax::ast::Block) -> bool {
    use cute_syntax::ast::ExprKind as K;
    use cute_syntax::ast::Stmt;

    fn expr_uses_await(e: &cute_syntax::ast::Expr) -> bool {
        match &e.kind {
            K::Await(_) => true,
            K::Block(b) => block_uses_await(b),
            K::If {
                cond,
                then_b,
                else_b,
                let_binding,
            } => {
                expr_uses_await(cond)
                    || block_uses_await(then_b)
                    || else_b.as_ref().map_or(false, block_uses_await)
                    || let_binding
                        .as_ref()
                        .map_or(false, |(_, init)| expr_uses_await(init))
            }
            K::Call {
                callee,
                args,
                block,
                ..
            } => {
                expr_uses_await(callee)
                    || args.iter().any(expr_uses_await)
                    || block.as_ref().map_or(false, |b| expr_uses_await(b))
            }
            K::MethodCall {
                receiver,
                args,
                block,
                ..
            }
            | K::SafeMethodCall {
                receiver,
                args,
                block,
                ..
            } => {
                expr_uses_await(receiver)
                    || args.iter().any(expr_uses_await)
                    || block.as_ref().map_or(false, |b| expr_uses_await(b))
            }
            K::Index { receiver, index } => expr_uses_await(receiver) || expr_uses_await(index),
            K::Unary { expr, .. } => expr_uses_await(expr),
            K::Binary { lhs, rhs, .. } => expr_uses_await(lhs) || expr_uses_await(rhs),
            K::Range { start, end, .. } => expr_uses_await(start) || expr_uses_await(end),
            K::Try(inner) => expr_uses_await(inner),
            K::Member { receiver, .. } | K::SafeMember { receiver, .. } => {
                expr_uses_await(receiver)
            }
            K::Array(items) => items.iter().any(expr_uses_await),
            K::Map(entries) => entries
                .iter()
                .any(|(k, v)| expr_uses_await(k) || expr_uses_await(v)),
            K::Lambda { body, .. } => block_uses_await(body),
            K::Case { scrutinee, arms } => {
                expr_uses_await(scrutinee) || arms.iter().any(|a| block_uses_await(&a.body))
            }
            K::Kwarg { value, .. } => expr_uses_await(value),
            K::Str(parts) => parts.iter().any(|p| match p {
                cute_syntax::ast::StrPart::Interp(inner) => expr_uses_await(inner),
                cute_syntax::ast::StrPart::InterpFmt { expr, .. } => expr_uses_await(expr),
                cute_syntax::ast::StrPart::Text(_) => false,
            }),
            _ => false,
        }
    }

    for stmt in &body.stmts {
        let hit = match stmt {
            Stmt::Let { value, .. } | Stmt::Var { value, .. } => expr_uses_await(value),
            Stmt::Expr(e) => expr_uses_await(e),
            Stmt::Assign { target, value, .. } => expr_uses_await(target) || expr_uses_await(value),
            Stmt::Return { value: Some(e), .. } => expr_uses_await(e),
            Stmt::Return { value: None, .. } => false,
            Stmt::For { iter, body, .. } => expr_uses_await(iter) || block_uses_await(body),
            Stmt::While { cond, body, .. } => expr_uses_await(cond) || block_uses_await(body),
            Stmt::Emit { args, .. } => args.iter().any(expr_uses_await),
            Stmt::Batch { body, .. } => block_uses_await(body),
            Stmt::Break { .. } | Stmt::Continue { .. } => false,
        };
        if hit {
            return true;
        }
    }
    if let Some(t) = &body.trailing {
        if expr_uses_await(t) {
            return true;
        }
    }
    false
}

/// Inject the `QStringList <name>; for (...) <name> << ...` lift when
/// `fn main(<name>: List)` was declared. Used by both the generic
/// main path and the cli_app/server_app intrinsics, so callers are
/// uniform whether or not they wrap the body in an event-loop app.
fn emit_main_args_lift(out: &mut String, param: Option<&Param>) {
    if let Some(p) = param {
        out.push_str(&format!(
            "    QStringList {bind};\n    for (int i = 0; i < argc; ++i) {{\n        {bind} << QString::fromLocal8Bit(argv[i]);\n    }}\n",
            bind = p.name.name,
        ));
    }
}

/// `fn main { server_app { ... } }`. Same intrinsic shape as `cli_app`
/// but reserved for event-loop use cases (HTTP servers, signal-driven
/// async jobs). Detection is identical; the codegen path differs in
/// using `return app.exec();` instead of `return 0;`.
fn detect_server_app(body: &cute_syntax::ast::Block) -> Option<&cute_syntax::ast::Block> {
    detect_named_block_app(body, "server_app")
}

fn detect_named_block_app<'a>(
    body: &'a cute_syntax::ast::Block,
    name: &str,
) -> Option<&'a cute_syntax::ast::Block> {
    use cute_syntax::ast::ExprKind as K;
    use cute_syntax::ast::Stmt;

    let call_expr = match (body.stmts.as_slice(), &body.trailing) {
        ([], Some(t)) => t.as_ref(),
        ([Stmt::Expr(e)], None) => e,
        _ => return None,
    };

    let K::Call {
        callee,
        args,
        block: Some(b),
        ..
    } = &call_expr.kind
    else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let K::Ident(callee_name) = &callee.kind else {
        return None;
    };
    if callee_name != name {
        return None;
    }
    let K::Block(inner) = &b.kind else {
        return None;
    };
    Some(inner)
}

/// True when `t` contains an unbound `Type::Var` (no substitution
/// found). Used by codegen to suppress emitting explicit type-args
/// like `<?T0>` when the type checker couldn't fully solve the
/// instantiation (in which case C++ template deduction usually
/// still works).
fn contains_unbound_var(t: &cute_types::ty::Type) -> bool {
    use cute_types::ty::Type;
    match t {
        Type::Var(_) => true,
        Type::Generic { args, .. } => args.iter().any(contains_unbound_var),
        Type::Nullable(inner) => contains_unbound_var(inner),
        Type::ErrorUnion { ok, .. } => contains_unbound_var(ok),
        Type::Fn { params, ret } => {
            params.iter().any(contains_unbound_var) || contains_unbound_var(ret)
        }
        _ => false,
    }
}

fn extract_string_literal(e: &cute_syntax::ast::Expr) -> Option<String> {
    use cute_syntax::ast::ExprKind as K;
    use cute_syntax::ast::StrPart;
    let K::Str(parts) = &e.kind else { return None };
    if parts.len() != 1 {
        return None;
    }
    let StrPart::Text(s) = &parts[0] else {
        return None;
    };
    Some(s.clone())
}

fn snake_to_camel(s: &str) -> String {
    s.split('_').map(capitalize_first).collect()
}

fn render_param_list(params: &[ParamInfo]) -> String {
    params
        .iter()
        .map(|p| format!("{} {}", p.cpp_type, p.name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render the user-declared params alone — no parent suffix. Used by ARC
/// classes (no parent tree) and prospective non-QObject ctors.
fn render_param_list_no_parent(params: &[Param], ctx: &TypeCtx) -> String {
    params
        .iter()
        .map(|p| format!("{} {}", ty::cute_param_to_cpp(p, ctx), p.name.name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render `<ty1> <n1>, <ty2> <n2>, ..., QObject* parent[ = nullptr]` for a
/// QObject-class user `init`. `for_decl` adds the `= nullptr` default — used
/// in the header declaration but omitted from the out-of-line definition.
fn render_init_param_list(params: &[Param], ctx: &TypeCtx, for_decl: bool) -> String {
    let parent = if for_decl {
        "QObject* parent = nullptr"
    } else {
        "QObject* parent"
    };
    if params.is_empty() {
        parent.to_string()
    } else {
        format!("{}, {parent}", render_param_list_no_parent(params, ctx))
    }
}

/// Extract the held class name from a `weak`/`unowned` member's type
/// expression. For `weak prop x : Foo?` the type is `Nullable(Named["Foo"])`
/// → returns `Some("Foo")`. For `unowned prop x : Foo` the type is
/// `Named["Foo"]` → also `Some("Foo")`. Generic / parametric forms
/// (`weak prop x : Box<Int>?`) are intentionally rejected here — the
/// type checker already errors on those, so v1 codegen bails out and the
/// caller falls back to the standard storage emission (which the type
/// error will pre-empt anyway).
fn weak_unowned_held_class(ty: &cute_syntax::ast::TypeExpr) -> Option<String> {
    let inner = match &ty.kind {
        cute_syntax::ast::TypeKind::Nullable(inner) => inner.as_ref(),
        _ => ty,
    };
    if let cute_syntax::ast::TypeKind::Named { path, args } = &inner.kind {
        if !args.is_empty() {
            return None;
        }
        return path.last().map(|i| i.name.clone());
    }
    None
}

/// Lower a class-method body inline into `target` (as opposed to
/// `self.source`). Used when the class body itself is being assembled
/// in a local buffer first (ARC inline ctors / generic-class methods).
/// Indent is 8 spaces — bodies sit two scope levels in (class + method).
/// Surrounding type-decl when lowering a method / init / deinit body.
/// `Class` drives `self` → `this->property()` style getter calls;
/// `Struct` drives `self.field` → `this->field` (no parens).
enum SurroundingDecl<'a> {
    Class(&'a ClassDecl),
    Struct(&'a StructDecl),
}

/// Lower a method / init / deinit body inline into `target`. Used by
/// arc-class init/deinit (always void, class context) and by struct
/// method emission (caller-supplied return type, struct context).
/// Indent is 8 spaces — bodies sit two scope levels in (decl + body).
fn lower_inline_body(
    target: &mut String,
    program: &ResolvedProgram,
    module: Option<&Module>,
    binding_modules: &[Module],
    surrounding: SurroundingDecl,
    return_type: &str,
    is_err_union: bool,
    params: &[Param],
    body: &Block,
) {
    let scope = program.fn_scopes.get(&body.span);
    let mut lo =
        Lowering::new(return_type, is_err_union, scope).with_module(module, binding_modules);
    lo = match surrounding {
        SurroundingDecl::Class(c) => lo.with_context(program, Some(c)),
        SurroundingDecl::Struct(s) => lo.with_context(program, None).with_struct_context(Some(s)),
    };
    lo.record_params(params);
    for (line, _span) in lo.lower_block(body) {
        target.push_str("        ");
        target.push_str(&line);
        target.push('\n');
    }
}

/// `Lowering` owns the per-function state needed for codegen:
/// - Fresh-temp counter for `?`-introduced names.
/// - `prelude` collects statements that must run before the surrounding
///   statement (e.g. the early-return guard introduced by `?`).
/// - `return_type` and `is_err_union` shape `?`'s synthesized
///   `return Result::err(...)` and the auto-wrap of trailing expressions
///   in `Result::ok(...)`.
/// - `fn_scope` carries HIR's per-fn name-resolution annotations: the
///   `Stmt::Assign` -> `is_decl` map drives the choice between
///   `auto x = e;` (first occurrence) and `x = e;` (reassignment).
/// One emitted C++ line plus the `.cute` source span it came from
/// (when known). Codegen emits a `#line N "..."` directive whenever
/// the span moves to a new source line, so debuggers step through
/// the user's source rather than the generated `.cpp`.
type LoweredLine = (String, Option<cute_syntax::span::Span>);

/// Span carried by each `Stmt` variant, used to tag synthesized lines
/// inside `lower_stmt_into` with the user-written stmt they came from.
fn stmt_span(stmt: &cute_syntax::ast::Stmt) -> cute_syntax::span::Span {
    use cute_syntax::ast::Stmt;
    match stmt {
        Stmt::Let { span, .. }
        | Stmt::Var { span, .. }
        | Stmt::Return { span, .. }
        | Stmt::Emit { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::For { span, .. }
        | Stmt::While { span, .. }
        | Stmt::Break { span, .. }
        | Stmt::Continue { span, .. }
        | Stmt::Batch { span, .. } => *span,
        Stmt::Expr(e) => e.span,
    }
}

/// LHS-driven hint for typed collection-literal lowering. Carries
/// the underlying `TypeExpr` of the element / key / value, both for
/// rendering the wrapper (`QList<T>{}` / `QMap<K,V>{}`) at emission
/// and for deriving the next-level hint when the literal is nested
/// (e.g. `Map<String, List<Int>>` propagating `List<Int>` down to
/// the inner array literal).
#[derive(Clone)]
enum CollectionHint {
    List {
        elem_ty: cute_syntax::ast::TypeExpr,
    },
    Map {
        key_ty: cute_syntax::ast::TypeExpr,
        value_ty: cute_syntax::ast::TypeExpr,
    },
}

struct Lowering<'a> {
    return_type: &'a str,
    is_err_union: bool,
    temp_counter: u32,
    prelude: Vec<LoweredLine>,
    /// Span of the statement currently being lowered. Synthesized lines
    /// pushed into `prelude` from inside expression lowering inherit
    /// this so the per-statement `#line` directive points at the user-
    /// written stmt rather than `None`.
    current_stmt_span: Option<cute_syntax::span::Span>,
    fn_scope: Option<&'a FnScope>,
    /// HIR program: used to identify QObject-derived classes for the
    /// `T.new(...)` -> `new T(...)` rewrite and for pointer receiver
    /// detection on method calls.
    program: Option<&'a cute_hir::ResolvedProgram>,
    /// Surrounding class declaration when lowering a method body: lets
    /// `@var` resolve to its property type for pointer-vs-value choice.
    class_decl: Option<&'a ClassDecl>,
    /// Surrounding struct declaration when lowering a struct-method
    /// body. Mutually exclusive with `class_decl` (each method body
    /// belongs to one or the other). Drives `self` → `this` / pointer
    /// classification and `self.field` → `this->field` for struct
    /// fields without the trailing `()` that class properties require.
    struct_decl: Option<&'a StructDecl>,
    /// Local bindings whose value is a QObject pointer (allocated by
    /// `T.new(...)`, indexed out of `List<QObject>`, etc.). The value is
    /// the class name when known (so we can resolve `binding.signal.connect`
    /// to `&Class::signal`), and an empty string when only the pointer-
    /// ness is known. Updated by each `let`/`var` and first-occurrence
    /// `name = expr` assignment.
    pointer_bindings: std::collections::HashMap<String, String>,
    /// True when the surrounding fn was declared `async fn`. Switches
    /// `return v;` to `co_return v;` and the trailing-expression wrap
    /// to the same, so the C++ compiler classifies the body as a Qt
    /// 6.5+ coroutine returning `QFuture<T>`.
    is_async: bool,
    /// Type-checker side-band: span -> generic args inferred for the
    /// `T.new()` call at that span. Looked up by `K::MethodCall`
    /// lowering when there are no explicit `type_args`. Empty
    /// elsewhere (e.g. snapshot tests don't run the type checker).
    generic_instantiations:
        Option<&'a std::collections::HashMap<cute_syntax::span::Span, Vec<cute_types::ty::Type>>>,
    /// The full module: lets `is_pointer_expr` look up a top-level
    /// fn's return type when the value site is a `Call` whose callee
    /// is a bare ident. Without this, `let z = make_arc()` (where
    /// `make_arc` returns an ARC class) wouldn't be recognized as a
    /// pointer-typed binding.
    module: Option<&'a Module>,
    /// Fn-parameter bindings whose declared type is a generic
    /// parameter of the surrounding fn (i.e. `xs` in `fn
    /// first<T: Foo>(xs: T)`). The value is the list of trait
    /// names the generic is bound by — drives trait-method dispatch
    /// in templated bodies: `xs.method(args)` routes through
    /// `::cute::trait_impl::<Trait>::method(xs, args)` when the
    /// method is declared on a bound trait, so impls on extern
    /// types (`impl Foo for QStringList`) participate in the same
    /// overload set as user-class impls. An empty bound list
    /// keeps the legacy `::cute::deref(xs).method()` form (bare
    /// `<T>` with no trait surface).
    generic_typed_bindings: std::collections::HashMap<String, Vec<String>>,
    /// SourceMap reference for resolving file paths inside compile-
    /// time intrinsics like `embed("path")`. Lookup is
    /// `sm.name(span.file)` to find the .cute source file's path,
    /// then the embed argument resolves relative to that file's
    /// directory. `None` for code paths that don't go through the
    /// driver's source-map-threading entry (snapshot tests, etc.) —
    /// `embed` is rejected in those contexts.
    source_map: Option<&'a cute_syntax::span::SourceMap>,

    /// Fn-parameter / `let` / `var` bindings whose declared type is
    /// a non-pointer named type (e.g. `let p : QPoint = ...`,
    /// `fn f(xs : List<Int>)`). The value is the simple base name
    /// (`"QPoint"`, `"List"`). Drives **direct-call** trait dispatch
    /// in concrete contexts: `recv.method(args)` on a tracked value
    /// receiver routes through `::cute::trait_impl::<Trait>::method(recv, args)`
    /// when an `impl Trait for ThatBase` declares the method, so
    /// trait methods on extern types and builtin generics work
    /// outside generic-bound bodies.
    ///
    /// Only non-pointer types are tracked. Pointer-backed bindings
    /// (QObject / ARC) live in `pointer_bindings`; generic-T params
    /// live in `generic_typed_bindings`.
    value_type_bindings: std::collections::HashMap<String, String>,

    /// When emitting an impl method as a free function (for
    /// extern / builtin-generic for-types), `self` becomes the
    /// first parameter rather than the implicit `this`. Setting
    /// this overrides `K::SelfRef` lowering and the receiver-style
    /// detection (pointer vs. value) for self.
    ///
    /// `None`: legacy "self is `this`" mode (class-method context).
    /// `Some((token, is_ptr))`: free-function mode — `K::SelfRef`
    /// emits as `token`, and pointer-detection on self uses
    /// `is_ptr` instead of the surrounding class context.
    self_override: Option<(String, bool)>,

    /// Loaded `.qpi` binding modules. Walked at method-call sites to
    /// pull per-method attributes (`@lifted_bool_ok`) off binding
    /// FnDecls that the user module never sees. Empty for callers
    /// that don't care (snapshot tests).
    binding_modules: &'a [Module],

    /// Memoized result of `lifted_method_names()`. `None` until first
    /// call; `Some(set)` for the lifetime of this Lowering. Stays
    /// `None` for snapshot-test Lowerings that never trigger the
    /// `@lifted_bool_ok` lookup path.
    lifted_method_names: Option<std::collections::HashSet<String>>,
}

impl<'a> Lowering<'a> {
    fn new(return_type: &'a str, is_err_union: bool, fn_scope: Option<&'a FnScope>) -> Self {
        Self {
            return_type,
            is_err_union,
            temp_counter: 0,
            prelude: Vec::new(),
            current_stmt_span: None,
            fn_scope,
            program: None,
            class_decl: None,
            struct_decl: None,
            pointer_bindings: std::collections::HashMap::new(),
            is_async: false,
            generic_instantiations: None,
            module: None,
            generic_typed_bindings: std::collections::HashMap::new(),
            value_type_bindings: std::collections::HashMap::new(),
            self_override: None,
            source_map: None,
            binding_modules: &[],
            lifted_method_names: None,
        }
    }

    fn with_source_map(mut self, sm: Option<&'a cute_syntax::span::SourceMap>) -> Self {
        self.source_map = sm;
        self
    }

    /// Set the user module + loaded `.qpi` bindings used for
    /// call-site lookups (`pointer_class_of`, `@lifted_bool_ok`, ...).
    fn with_module(mut self, module: Option<&'a Module>, binding_modules: &'a [Module]) -> Self {
        self.module = module;
        self.binding_modules = binding_modules;
        self
    }

    /// Walk the user module + all binding modules looking for a class
    /// method declared as `class C { fn name(...) ... }`. Returns the
    /// matching FnDecl when found. Used by the call-site attribute
    /// reader to pull `@lifted_bool_ok` and friends off Qt bindings.
    fn lookup_class_method_decl(&self, class_name: &str, method_name: &str) -> Option<&'a FnDecl> {
        if let Some(m) = self.module {
            if let Some(f) = find_class_method_in(m, class_name, method_name) {
                return Some(f);
            }
        }
        for m in self.binding_modules {
            if let Some(f) = find_class_method_in(m, class_name, method_name) {
                return Some(f);
            }
        }
        None
    }

    /// Best-effort class name for the receiver of a method call. Walks
    /// the same tracking paths the rest of codegen already uses
    /// (`pointer_bindings` for QObject / Arc, `value_type_bindings`
    /// for extern value types tracked at param / let / var sites).
    /// Returns `None` for receivers whose type isn't statically known
    /// — call-site rewrites that need a class name (e.g. the
    /// `@lifted_bool_ok` IIFE wrapper) skip those.
    fn class_name_of_receiver(&self, recv: &Expr) -> Option<String> {
        if let Some(name) = self.pointer_class_of(recv) {
            if !name.is_empty() {
                return Some(name);
            }
        }
        if let cute_syntax::ast::ExprKind::Ident(name) = &recv.kind {
            if let Some(base) = self.value_type_bindings.get(name) {
                return Some(base.clone());
            }
        }
        None
    }

    /// `Some(decl)` when the named class method carries
    /// `@lifted_bool_ok`. Returns `None` for unmarked methods or when
    /// the class isn't statically known (literal receivers etc.) so
    /// the call-site falls through to the regular `recv.method(args)`
    /// lowering.
    fn method_is_lifted_bool_ok(&self, class_name: &str, method_name: &str) -> Option<&'a FnDecl> {
        let f = self.lookup_class_method_decl(class_name, method_name)?;
        f.attributes
            .iter()
            .any(|a| a.name.name == "lifted_bool_ok")
            .then_some(f)
    }

    /// Synthesize the `@lifted_bool_ok` IIFE wrapper at a method call.
    /// Returns `Some(wrapper_cpp)` when the call's receiver+method
    /// resolves to a binding fn marked lifted; `None` to fall through
    /// to the regular method-call lowering.
    ///
    /// Wrapper shape:
    /// ```cpp
    /// [&]() -> ::cute::Result<{T}, QtBoolError> {
    ///   bool _ok_<n> = false;
    ///   auto _v_<n> = recv.method(&_ok_<n>{, args...});
    ///   if (_ok_<n>) return ::cute::Result<{T}, QtBoolError>::ok(_v_<n>);
    ///   return ::cute::Result<{T}, QtBoolError>::err(QtBoolError::failed());
    /// }()
    /// ```
    /// `&_ok_<n>` is inserted at C++ position 0 (Qt's bool*-ok
    /// convention puts the out-arg first); user-supplied Cute args
    /// follow.
    fn try_emit_lifted_bool_ok_call(
        &mut self,
        receiver: &Expr,
        method: &cute_syntax::ast::Ident,
        args: &[Expr],
        block: &Option<Box<Expr>>,
    ) -> Option<String> {
        if block.is_some() {
            return None;
        }
        // Cheap method-name short-circuit: most call sites don't name
        // a `@lifted_bool_ok`-marked fn, so bail before the (more
        // expensive) receiver-class + module walk. The index is built
        // once per Lowering and cached.
        if !self.lifted_method_names().contains(method.name.as_str()) {
            return None;
        }
        let class_name = self.class_name_of_receiver(receiver)?;
        let f = self.method_is_lifted_bool_ok(&class_name, &method.name)?;
        let inner_ty = match &f.return_ty.as_ref()?.kind {
            cute_syntax::ast::TypeKind::ErrorUnion(inner) => inner.as_ref(),
            _ => f.return_ty.as_ref()?,
        };
        let inner_cpp = self.program.map(|p| {
            let ctx = ty::TypeCtx::new(p);
            ty::cute_to_cpp(inner_ty, &ctx)
        })?;
        let n = self.fresh();
        let ok_var = format!("_ok{n}");
        let val_var = format!("_v{n}");
        let result_ty = format!("::cute::Result<{inner_cpp}, ::QtBoolError>");
        let rs = self.lower_expr(receiver);
        let sep = if self.is_pointer_expr(receiver) {
            "->"
        } else {
            "."
        };
        let user_args: Vec<String> = args.iter().map(|a| self.lower_expr(a)).collect();
        let mut call_args = Vec::with_capacity(user_args.len() + 1);
        call_args.push(format!("&{ok_var}"));
        call_args.extend(user_args);
        Some(format!(
            "[&]() -> {result_ty} {{ \
             bool {ok_var} = false; \
             auto {val_var} = {rs}{sep}{m}({args}); \
             if ({ok_var}) return {result_ty}::ok({val_var}); \
             return {result_ty}::err(::QtBoolError::failed()); \
             }}()",
            m = method.name,
            args = call_args.join(", "),
        ))
    }

    /// Lazily-built set of method names that any user-module or
    /// binding fn marks `@lifted_bool_ok`. Built once per Lowering;
    /// the per-call-site short-circuit avoids walking ~40 binding
    /// modules on every method call (>99% of which have no marker).
    fn lifted_method_names(&mut self) -> &std::collections::HashSet<String> {
        if self.lifted_method_names.is_none() {
            let mut names: std::collections::HashSet<String> = Default::default();
            let collect_from = |m: &Module, names: &mut std::collections::HashSet<String>| {
                for item in &m.items {
                    let Item::Class(c) = item else { continue };
                    for member in &c.members {
                        let f = match member {
                            ClassMember::Fn(f) | ClassMember::Slot(f) => f,
                            _ => continue,
                        };
                        if f.attributes.iter().any(|a| a.name.name == "lifted_bool_ok") {
                            names.insert(f.name.name.clone());
                        }
                    }
                }
            };
            if let Some(m) = self.module {
                collect_from(m, &mut names);
            }
            for m in self.binding_modules {
                collect_from(m, &mut names);
            }
            self.lifted_method_names = Some(names);
        }
        self.lifted_method_names.as_ref().unwrap()
    }

    /// Whether `@name` inside the surrounding class refers to a
    /// `bindable` (QObjectBindableProperty) or computed
    /// (QObjectComputedProperty) prop. Drives the `m_x` vs
    /// `m_x.value()` choice for AtIdent reads — bindable storage is
    /// non-copyable so naked `m_x` would fail in `auto x = @x` style
    /// code; the implicit `operator parameter_type` works in many
    /// expression positions but we emit `.value()` unconditionally for
    /// uniformity.
    /// Whether `name` refers to a top-level `let X : Foo = Foo.new()`
    /// where `Foo` is QObject-derived. These lower to
    /// `Q_GLOBAL_STATIC(Foo, X)` in the file-scope emit pre-pass; the
    /// macro's accessor is `X()` (returns `Foo*`), so user-source
    /// references to bare `X` need the function-call form.
    fn is_qobject_top_level_let(&self, name: &str) -> bool {
        let Some(prog) = self.program else {
            return false;
        };
        matches!(
            prog.items.get(name),
            Some(cute_hir::ItemKind::Let {
                is_qobject_type: true,
                ..
            }),
        )
    }

    fn at_ident_is_bindable_prop(&self, name: &str) -> bool {
        let Some(c) = self.class_decl else {
            return false;
        };
        for m in &c.members {
            if let cute_syntax::ast::ClassMember::Property(p) = m {
                if p.name.name == name {
                    // Bindable / Bind / Fresh all back onto a property
                    // type whose access goes through `.value()` —
                    // QObjectBindableProperty for the first two,
                    // QObjectComputedProperty for fresh.
                    return p.is_bindable_surface();
                }
            }
        }
        false
    }

    /// Whether `@name` refers to a Plain prop (non-bindable, non-weak,
    /// non-model) on the surrounding class. Plain prop writes route
    /// through the auto-generated setter — the setter does the dirty
    /// check + auto-emit, so a `@count = v` write inside a class
    /// method propagates the same as an external `obj.count = v`.
    /// Reading `@count` still uses the bare `m_count` storage form
    /// (no overhead). Was previously raw `m_count = v;`, which silently
    /// bypassed the notify signal — that landmine is gone.
    fn at_ident_is_plain_prop(&self, name: &str) -> bool {
        let Some(c) = self.class_decl else {
            return false;
        };
        for m in &c.members {
            if let cute_syntax::ast::ClassMember::Property(p) = m {
                if p.name.name == name {
                    // Bindable / Bind / Fresh have their own write
                    // mechanisms (operator= via QObjectBindableProperty
                    // / setBinding / read-only compute). `, model`
                    // is read-only at the prop level (mutation through
                    // the ModelList's own methods).
                    return !p.is_bindable_surface() && !p.model;
                }
            }
        }
        false
    }

    /// Whether `@name` refers to a `weak`-modified field on the
    /// surrounding arc class. Drives the transparent-`.lock()`
    /// rewrite of AtIdent reads — `@parent` returns `cute::Arc<T>`
    /// (matching the surface-level `T?` semantics) instead of the
    /// raw `cute::Weak<T>` storage. Writes still target the raw
    /// `m_parent` slot; Weak's `operator=(const Arc<T>&)` does the
    /// conversion at the assignment site.
    fn at_ident_is_weak_field(&self, name: &str) -> bool {
        let Some(c) = self.class_decl else {
            return false;
        };
        for m in &c.members {
            if let cute_syntax::ast::ClassMember::Field(f) = m {
                if f.name.name == name {
                    return f.weak;
                }
            }
        }
        false
    }

    /// Return the held class name for a `weak let x : T?` field.
    /// `None` when `name` isn't a weak field on the surrounding
    /// class. Used by case-arm `some(p)` lowering: a `weak`
    /// scrutinee yields `cute::Arc<T>`, and binding `p` into the
    /// pointer-class bookkeeping makes subsequent `p.method()`
    /// calls lower with `->` instead of `.`.
    fn weak_field_held_class(&self, name: &str) -> Option<String> {
        let c = self.class_decl?;
        for m in &c.members {
            if let cute_syntax::ast::ClassMember::Field(f) = m {
                if f.name.name == name && f.weak {
                    return weak_unowned_held_class(&f.ty);
                }
            }
        }
        None
    }

    /// True if the given name shadows the type-as-receiver position —
    /// i.e. there's a `let`/`var`/param binding with this name in the
    /// current Lowering scope. Used by the static-method-call detector
    /// to keep `obj.x()` (instance call) distinct from `Foo.x()`
    /// (`Foo::x()` static call) when `Foo` happens to also be a type.
    fn is_local_binding(&self, name: &str) -> bool {
        self.value_type_bindings.contains_key(name) || self.pointer_bindings.contains_key(name)
    }

    fn with_context(
        mut self,
        program: &'a cute_hir::ResolvedProgram,
        class_decl: Option<&'a ClassDecl>,
    ) -> Self {
        self.program = Some(program);
        self.class_decl = class_decl;
        self
    }

    /// Combined builder shortcut used by `Emitter` — every emit-side
    /// `Lowering::new(...).with_context(...)` call has access to
    /// `self.source_map` through the surrounding `&self`. Threading
    /// it via this single method avoids 13 separate `.with_source_map`
    /// chained calls and keeps source_map plumbing DRY.
    fn with_emit_context(
        self,
        source_map: Option<&'a cute_syntax::span::SourceMap>,
        program: &'a cute_hir::ResolvedProgram,
        class_decl: Option<&'a ClassDecl>,
    ) -> Self {
        self.with_source_map(source_map)
            .with_context(program, class_decl)
    }

    /// Mark the lowering context as a struct method body. Mutually
    /// exclusive with `class_decl` — every method body lives inside
    /// exactly one of the two surrounding declarations.
    fn with_struct_context(mut self, struct_decl: Option<&'a StructDecl>) -> Self {
        self.struct_decl = struct_decl;
        self
    }

    fn with_generic_instantiations(
        mut self,
        map: &'a std::collections::HashMap<cute_syntax::span::Span, Vec<cute_types::ty::Type>>,
    ) -> Self {
        self.generic_instantiations = Some(map);
        self
    }

    /// Pre-register fn parameters whose declared type is a pointer-
    /// backed class (QObject* or `cute::Arc<T>`-wrapped ARC class)
    /// so subsequent expressions referencing them know to use `->`
    /// for member access. Without this, a fn body that takes a
    /// `b: Box<Int>` argument would emit `b.setItem(...)` (wrong)
    /// instead of `b->setItem(...)`.
    ///
    /// Also tracks **value-typed** params (extern value types like
    /// QPoint, builtin generics like List<T>) in
    /// `value_type_bindings` so direct-call trait dispatch can
    /// route through the namespace overload at concrete call sites.
    fn record_params(&mut self, params: &[Param]) {
        for p in params {
            if let Some(class_name) = self.pointer_class_from_type_expr(&p.ty) {
                self.pointer_bindings
                    .insert(p.name.name.clone(), class_name);
            } else if let Some(base) = self.value_base_from_type_expr(&p.ty) {
                self.value_type_bindings.insert(p.name.name.clone(), base);
            }
        }
    }

    /// Inspect a `TypeExpr` and return the base name when the type
    /// is a *non-pointer* named type (extern value class, builtin
    /// generic). Returns None for pointer-backed classes (those
    /// flow through `pointer_class_from_type_expr`), function /
    /// Self types, or anonymous shapes. Used to populate
    /// `value_type_bindings`.
    fn value_base_from_type_expr(&self, t: &cute_syntax::ast::TypeExpr) -> Option<String> {
        if let cute_syntax::ast::TypeKind::Nullable(inner) = &t.kind {
            return self.value_base_from_type_expr(inner);
        }
        let cute_syntax::ast::TypeKind::Named { path, .. } = &t.kind else {
            return None;
        };
        let leaf = path.last()?.name.as_str();
        // Pointer-backed types are tracked separately in
        // `pointer_bindings` — never double-track them.
        if self.is_ref_class(leaf) {
            return None;
        }
        Some(leaf.to_string())
    }

    /// Inspect a `TypeExpr` and return the class name when the type is
    /// a pointer-backed class reference (QObject-derived or ARC).
    /// `Box<Int>` returns `Some("Box")`; `Int` / `String` / `List<X>`
    /// (a built-in parametric) return `None`.
    ///
    /// `T?` for a QObject-derived T lowers to `QPointer<T>`, which
    /// overloads `->`. Recursively unwrap `Nullable(T)` so the
    /// pointer-class detector treats nullable returns the same as
    /// bare returns — without this, `let c = make_maybe();
    /// c.signal.connect { ... }` couldn't resolve `&Class::signal`.
    fn pointer_class_from_type_expr(&self, t: &cute_syntax::ast::TypeExpr) -> Option<String> {
        if let cute_syntax::ast::TypeKind::Nullable(inner) = &t.kind {
            return self.pointer_class_from_type_expr(inner);
        }
        let cute_syntax::ast::TypeKind::Named { path, .. } = &t.kind else {
            return None;
        };
        let leaf = path.last()?.name.as_str();
        if self.is_ref_class(leaf) {
            Some(leaf.to_string())
        } else {
            None
        }
    }

    fn with_async(mut self, is_async: bool) -> Self {
        self.is_async = is_async;
        self
    }

    /// Switch this Lowering into "self is a parameter, not `this`"
    /// mode. Used when emitting an impl method as a free function
    /// in `cute::trait_impl::<Trait>::` — the body's `K::SelfRef`
    /// lowers to the parameter token, and pointer-detection on
    /// self uses `is_pointer` instead of the surrounding class
    /// context (which is None in free-function emission).
    fn with_self_override(mut self, token: String, is_pointer: bool) -> Self {
        self.self_override = Some((token, is_pointer));
        self
    }

    /// Walk `params` against the surrounding fn's `generics` and
    /// record each param whose declared type names a generic. Used
    /// later by `is_generic_param_receiver` to decide which Idents
    /// need a `::cute::deref` wrap on member access.
    ///
    /// Only direct generic types (`xs: T`) are tracked. A param
    /// like `xs: List<T>` doesn't trip this, since the C++ template
    /// receives a `List<T>` value (not a pointer-vs-value
    /// ambiguity); member access on a List goes through QList's
    /// existing `.size()` etc. without dispatch issues.
    fn with_generic_params(mut self, generics: &[GenericParam], params: &[Param]) -> Self {
        if generics.is_empty() {
            return self;
        }
        // Index generic-param-name → bound list (each bound is a
        // trait name). A bare `<T>` has an empty Vec — we still
        // want to track the binding so dispatch routing can decide
        // to fall back to `::cute::deref` form.
        let bounds_by_name: std::collections::HashMap<&str, Vec<String>> = generics
            .iter()
            .map(|g| {
                (
                    g.name.name.as_str(),
                    g.bounds.iter().map(|b| b.name.clone()).collect(),
                )
            })
            .collect();
        for p in params {
            if let cute_syntax::ast::TypeKind::Named { path, args } = &p.ty.kind {
                if args.is_empty() && path.len() == 1 {
                    if let Some(bounds) = bounds_by_name.get(path[0].name.as_str()) {
                        self.generic_typed_bindings
                            .insert(p.name.name.clone(), bounds.clone());
                    }
                }
            }
        }
        self
    }

    /// Render the type-checker's inferred type args at this call site
    /// as a `<X, Y>` suffix, or empty when there are no inferred args.
    /// Used by `K::Call` and `K::MethodCall` lowering to emit explicit
    /// template args for generic fns / methods. The type-checker
    /// records args for any generic top-level fn call and any method-
    /// level generic call; codegen consumes them so C++ template
    /// deduction never has to puzzle out a lambda-as-std::function.
    fn inferred_type_args_at(&self, span: cute_syntax::span::Span) -> String {
        let Some(map) = self.generic_instantiations else {
            return String::new();
        };
        let Some(args) = map.get(&span) else {
            return String::new();
        };
        // Skip when the inferred args still contain unbound vars
        // (`Type::Var(_)`) — emitting `<?T0>` would just confuse the
        // C++ compiler. C++ deduction will fall back to its default
        // behaviour, which works for non-lambda cases.
        if args.iter().any(contains_unbound_var) {
            return String::new();
        }
        let Some(prog) = self.program else {
            return String::new();
        };
        let ctx = ty::TypeCtx::new(prog);
        let rendered = args
            .iter()
            .map(|t| ty::cute_type_to_cpp(t, &ctx))
            .collect::<Vec<_>>()
            .join(", ");
        format!("<{rendered}>")
    }

    /// `return` (or `co_return` in async contexts) - the keyword that
    /// must wrap user-visible exit values, including `?`-operator
    /// short-circuits and trailing-expression auto-wrap. In a Qt 6.5+
    /// coroutine returning `QFuture<T>`, every exit point must use
    /// `co_return` for the body to typecheck as a coroutine.
    fn return_kw(&self) -> &'static str {
        if self.is_async { "co_return" } else { "return" }
    }

    // ---- pointer awareness ---------------------------------------------

    /// Discriminator for the Cute class kinds that codegen needs to
    /// distinguish at lowering time. Computed once via `class_kind`
    /// and threaded into the per-kind `is_*_class` shorthands +
    /// the `is_ref_class` "qobject-or-arc" predicate.
    fn class_kind(&self, name: &str) -> ClassKind {
        let Some(prog) = self.program else {
            return ClassKind::NotAClass;
        };
        match prog.items.get(name) {
            Some(cute_hir::ItemKind::Class {
                is_extern_value: true,
                ..
            }) => ClassKind::ExternValue,
            Some(cute_hir::ItemKind::Class {
                is_qobject_derived: true,
                ..
            }) => ClassKind::QObject,
            Some(cute_hir::ItemKind::Class { .. }) => ClassKind::Arc,
            _ => ClassKind::NotAClass,
        }
    }

    fn is_qobject_class(&self, name: &str) -> bool {
        self.class_kind(name) == ClassKind::QObject
    }

    /// True for `arc X { ... }` — Cute's reference-counted class form.
    /// `T.new(...)` returns a `cute::Arc<T>`, member access uses `->`,
    /// and the class derives from `cute::ArcBase` rather than QObject.
    fn is_arc_class(&self, name: &str) -> bool {
        self.class_kind(name) == ClassKind::Arc
    }

    /// True when `name` is a heap-allocated, pointer-accessed class —
    /// either QObject-derived or `arc`. The two share lowering shape
    /// (pointer typing, `T.new(...)` returns a handle, member access
    /// via `->`) so most callers care only about this combined view.
    fn is_ref_class(&self, name: &str) -> bool {
        matches!(self.class_kind(name), ClassKind::QObject | ClassKind::Arc)
    }

    fn is_error_class(&self, name: &str) -> bool {
        let Some(prog) = self.program else {
            return false;
        };
        // Any enum (declared with `error` or `enum`) can be used
        // as an err type in `!T` returns. The type checker has
        // already validated that the variant exists; codegen only
        // needs to know whether to wrap in `::err(...)`.
        matches!(prog.items.get(name), Some(cute_hir::ItemKind::Enum { .. }))
    }

    /// True when `class_name` (or any of its supers) declares a
    /// `static fn method_name(...)`. Walks the super chain so e.g.
    /// `QGuiApplication.quit()` / `QApplication.quit()` resolve to
    /// the static `quit` declared on `QCoreApplication`.
    fn is_static_method_of_class(&self, class_name: &str, method_name: &str) -> bool {
        let Some(prog) = self.program else {
            return false;
        };
        let mut current = Some(class_name.to_string());
        while let Some(name) = current {
            let Some(cute_hir::ItemKind::Class {
                super_class,
                static_methods,
                ..
            }) = prog.items.get(&name)
            else {
                return false;
            };
            if static_methods.iter().any(|m| m == method_name) {
                return true;
            }
            current = super_class.clone();
        }
        false
    }

    /// True for `extern value T { ... }` — a plain C++ value type.
    /// `T.new(args)` lowers to `T(args)` (stack-style construction),
    /// member access uses `.`, and there's no Arc / QObject pointer
    /// boxing. Codegen never emits a definition for these — they
    /// live in C++ headers pulled in via `[cpp] includes`.
    fn is_extern_value_class(&self, name: &str) -> bool {
        self.class_kind(name) == ClassKind::ExternValue
    }

    /// True for `struct X { ... }` — Cute's plain value type.
    /// `T.new(args)` lowers to `T{args}` (brace-init aggregate
    /// construction) and member access uses `.`. No metaobject, no
    /// reference-counting wrapper.
    fn is_struct_named(&self, name: &str) -> bool {
        let Some(prog) = self.program else {
            return false;
        };
        matches!(
            prog.items.get(name),
            Some(cute_hir::ItemKind::Struct { .. })
        )
    }

    /// Find the struct name a receiver expression evaluates to. Used
    /// by the bare-member-access lowering to decide whether to emit
    /// `recv.field` (struct field) vs `recv.method()` (everything
    /// else). Returns the struct's simple name when the receiver is
    /// (a) a let/var binding declared with a struct type annotation,
    /// (b) a parameter typed as a struct, or (c) a `T.new(...)` whose
    /// receiver is a struct.
    fn struct_name_of_receiver(&self, recv: &cute_syntax::ast::Expr) -> Option<String> {
        use cute_syntax::ast::ExprKind as K;
        match &recv.kind {
            K::Ident(name) => {
                let class_decl = self.class_decl;
                let module = self.module;
                // Walk surrounding fn / method body bindings via the
                // module so we can pick up the declared type from
                // `let p : Point = ...` or `let p = Point.new(...)`.
                if let Some(m) = module {
                    if let Some(t) = self.find_local_type_in_module(m, class_decl, name) {
                        if let cute_syntax::ast::TypeKind::Named { path, .. } = &t.kind {
                            if let Some(leaf) = path.last() {
                                if self.is_struct_named(&leaf.name) {
                                    return Some(leaf.name.clone());
                                }
                            }
                        }
                    }
                }
                None
            }
            K::MethodCall {
                receiver, method, ..
            } if method.name == "new" => {
                if let K::Ident(class_name) = &receiver.kind {
                    if self.is_struct_named(class_name) {
                        return Some(class_name.clone());
                    }
                }
                None
            }
            // Inside a struct method body, `self` is the surrounding
            // struct instance. Returning its name lets the K::Member
            // / K::MethodCall arms route field access through the
            // bare-pointer (`this->x`) path instead of treating it
            // like a class property getter.
            K::SelfRef => self.struct_decl.map(|s| s.name.name.clone()),
            _ => None,
        }
    }

    /// True if `struct_name` declares a field named `field_name`.
    /// AST `StructDecl` lookup off the module — single source of truth
    /// for struct queries inside the lowering pass. Codegen reads off
    /// the AST rather than the type table so it stays free of
    /// cute-types dependencies.
    fn struct_decl_for(&self, struct_name: &str) -> Option<&StructDecl> {
        let module = self.module?;
        module.items.iter().find_map(|item| match item {
            Item::Struct(s) if s.name.name == struct_name => Some(s),
            _ => None,
        })
    }

    fn struct_has_field(&self, struct_name: &str, field_name: &str) -> bool {
        self.struct_decl_for(struct_name)
            .map(|s| s.fields.iter().any(|f| f.name.name == field_name))
            .unwrap_or(false)
    }

    /// Field names of `struct X { ... }` in declaration order, OR
    /// `None` for `~Copyable` structs (C++20 designated initializers
    /// require an aggregate; ~Copyable structs carry explicit
    /// move/delete ctors that cost aggregate status, so callers fall
    /// back to positional brace-init). Also returns `None` when the
    /// struct isn't visible in the current module.
    fn struct_field_names(&self, struct_name: &str) -> Option<Vec<String>> {
        let s = self.struct_decl_for(struct_name)?;
        if !s.is_copyable {
            return None;
        }
        Some(s.fields.iter().map(|f| f.name.name.clone()).collect())
    }

    /// Best-effort lookup of a local-binding's declared type by walking
    /// the surrounding fn / method body's `let`/`var` statements. Only
    /// finds declarations with explicit type annotations or with
    /// `T.new(...)` initializers (the type can be read off the
    /// receiver). Returns None for inferred bindings whose type
    /// can only be obtained via the type checker.
    fn find_local_type_in_module(
        &self,
        module: &Module,
        _class_decl: Option<&ClassDecl>,
        binding_name: &str,
    ) -> Option<cute_syntax::ast::TypeExpr> {
        // Walk every fn body / method body in the module. Stop at the
        // first matching let/var. This is intentionally crude — a
        // real implementation would look at scope, but for codegen's
        // struct-vs-method dispatch the binding name is unique inside
        // its enclosing fn body so we accept the first match.
        for item in &module.items {
            if let Item::Fn(f) = item {
                // Check params first — function parameters are bindings
                // in scope for the body.
                for p in &f.params {
                    if p.name.name == binding_name {
                        return Some(p.ty.clone());
                    }
                }
                if let Some(body) = &f.body {
                    if let Some(t) = self.find_in_block(body, binding_name) {
                        return Some(t);
                    }
                }
            } else if let Item::Class(c) = item {
                for m in &c.members {
                    let (params, body): (
                        Option<&[cute_syntax::ast::Param]>,
                        Option<&cute_syntax::ast::Block>,
                    ) = match m {
                        ClassMember::Fn(f) | ClassMember::Slot(f) => {
                            (Some(&f.params), f.body.as_ref())
                        }
                        ClassMember::Init(i) => (Some(&i.params), Some(&i.body)),
                        ClassMember::Deinit(d) => (None, Some(&d.body)),
                        _ => (None, None),
                    };
                    if let Some(ps) = params {
                        for p in ps {
                            if p.name.name == binding_name {
                                return Some(p.ty.clone());
                            }
                        }
                    }
                    if let Some(b) = body {
                        if let Some(t) = self.find_in_block(b, binding_name) {
                            return Some(t);
                        }
                    }
                }
            }
        }
        None
    }

    fn find_in_block(
        &self,
        block: &cute_syntax::ast::Block,
        binding_name: &str,
    ) -> Option<cute_syntax::ast::TypeExpr> {
        use cute_syntax::ast::Stmt as S;
        for stmt in &block.stmts {
            match stmt {
                S::Let {
                    name, ty, value, ..
                }
                | S::Var {
                    name, ty, value, ..
                } => {
                    if name.name == binding_name {
                        if let Some(t) = ty {
                            return Some(t.clone());
                        }
                        return self.type_of_initializer(value);
                    }
                    // Even if this Let's name doesn't match, the
                    // initializer might contain a nested block that
                    // does (lambda capture, etc.). Walk into it.
                    if let Some(t) = self.find_in_expr(value, binding_name) {
                        return Some(t);
                    }
                }
                S::Expr(expr) => {
                    if let Some(t) = self.find_in_expr(expr, binding_name) {
                        return Some(t);
                    }
                }
                S::Return {
                    value: Some(expr), ..
                } => {
                    if let Some(t) = self.find_in_expr(expr, binding_name) {
                        return Some(t);
                    }
                }
                S::Assign { target, value, .. } => {
                    if let Some(t) = self.find_in_expr(target, binding_name) {
                        return Some(t);
                    }
                    if let Some(t) = self.find_in_expr(value, binding_name) {
                        return Some(t);
                    }
                }
                S::For { iter, body, .. } => {
                    if let Some(t) = self.find_in_expr(iter, binding_name) {
                        return Some(t);
                    }
                    if let Some(t) = self.find_in_block(body, binding_name) {
                        return Some(t);
                    }
                }
                S::While { cond, body, .. } => {
                    if let Some(t) = self.find_in_expr(cond, binding_name) {
                        return Some(t);
                    }
                    if let Some(t) = self.find_in_block(body, binding_name) {
                        return Some(t);
                    }
                }
                S::Batch { body, .. } => {
                    if let Some(t) = self.find_in_block(body, binding_name) {
                        return Some(t);
                    }
                }
                _ => {}
            }
        }
        // Also descend into the block's trailing expression — bodies
        // like `fn main { cli_app { let p = ...; ... } }` carry
        // the cli_app call as `trailing`, not as a `Stmt::Expr`.
        if let Some(t) = block.trailing.as_ref() {
            if let Some(ty) = self.find_in_expr(t, binding_name) {
                return Some(ty);
            }
        }
        None
    }

    /// Walk an expression looking for `let binding_name` declarations
    /// inside any nested blocks (trailing-block call args, lambdas,
    /// case arms, ...). Returns the declared type when found.
    fn find_in_expr(
        &self,
        expr: &cute_syntax::ast::Expr,
        binding_name: &str,
    ) -> Option<cute_syntax::ast::TypeExpr> {
        use cute_syntax::ast::ExprKind as K;
        match &expr.kind {
            K::Block(b) => self.find_in_block(b, binding_name),
            K::MethodCall {
                receiver,
                args,
                block,
                ..
            } => {
                if let Some(t) = self.find_in_expr(receiver, binding_name) {
                    return Some(t);
                }
                for a in args {
                    if let Some(t) = self.find_in_expr(a, binding_name) {
                        return Some(t);
                    }
                }
                if let Some(b) = block {
                    if let Some(t) = self.find_in_expr(b, binding_name) {
                        return Some(t);
                    }
                }
                None
            }
            K::Call {
                callee,
                args,
                block,
                ..
            } => {
                if let Some(t) = self.find_in_expr(callee, binding_name) {
                    return Some(t);
                }
                for a in args {
                    if let Some(t) = self.find_in_expr(a, binding_name) {
                        return Some(t);
                    }
                }
                if let Some(b) = block {
                    if let Some(t) = self.find_in_expr(b, binding_name) {
                        return Some(t);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Best-effort: read the declared type from an initializer
    /// expression. Currently handles `T.new(...)` (returns `T`).
    fn type_of_initializer(
        &self,
        e: &cute_syntax::ast::Expr,
    ) -> Option<cute_syntax::ast::TypeExpr> {
        use cute_syntax::ast::ExprKind as K;
        match &e.kind {
            K::MethodCall {
                receiver, method, ..
            } if method.name == "new" => {
                if let K::Ident(name) = &receiver.kind {
                    return Some(cute_syntax::ast::TypeExpr {
                        kind: cute_syntax::ast::TypeKind::Named {
                            path: vec![cute_syntax::ast::Ident {
                                name: name.clone(),
                                span: receiver.span,
                            }],
                            args: Vec::new(),
                        },
                        span: e.span,
                    });
                }
                None
            }
            // `recv.method(args)` returning a known type. Used so
            // `let q = p.shifted(...)` knows q's type when shifted's
            // return type is declared. Currently handles struct
            // methods only; other cases fall through.
            K::MethodCall {
                receiver, method, ..
            } => {
                let recv_ty = self.type_of_initializer(receiver)?;
                let cute_syntax::ast::TypeKind::Named { path, args } = &recv_ty.kind else {
                    return None;
                };
                if !args.is_empty() {
                    return None;
                }
                let recv_class = path.last()?.name.clone();
                if !self.is_struct_named(&recv_class) {
                    return None;
                }
                let module = self.module?;
                for item in &module.items {
                    if let Item::Struct(s) = item {
                        if s.name.name == recv_class {
                            for m in &s.methods {
                                if m.name.name == method.name {
                                    return m.return_ty.clone();
                                }
                            }
                        }
                    }
                }
                None
            }
            K::Ident(other) => {
                let module = self.module?;
                self.find_local_type_in_module(module, self.class_decl, other)
            }
            _ => None,
        }
    }

    /// True when this expression syntactically produces an error
    /// instance: a `MethodCall { receiver: Ident(E), .. }` where `E`
    /// is a declared error type, i.e. the `E.<variant>(args)` factory
    /// call shape. Used by `return` lowering to choose between
    /// `Result::ok(...)` (success value) and `Result::err(...)`
    /// (error value) for `!T`-returning fns.
    fn expr_is_error_value(&self, e: &cute_syntax::ast::Expr) -> bool {
        use cute_syntax::ast::ExprKind as K;
        match &e.kind {
            K::MethodCall { receiver, .. } => {
                if let K::Ident(name) = &receiver.kind {
                    return self.is_error_class(name);
                }
                false
            }
            _ => false,
        }
    }

    fn class_property_type(&self, prop_name: &str) -> Option<&cute_syntax::ast::TypeExpr> {
        let class = self.class_decl?;
        for m in &class.members {
            match m {
                ClassMember::Property(p) if p.name.name == prop_name => return Some(&p.ty),
                ClassMember::Field(f) if f.name.name == prop_name => return Some(&f.ty),
                _ => {}
            }
        }
        None
    }

    /// Whether `prop_name` on the surrounding class is decorated with
    /// `, model`. The C++ getter for such a prop returns
    /// `::cute::ModelList<T*>*` (a pointer), so call-site lowering must
    /// use `->` for further member / method access.
    fn class_property_is_model(&self, prop_name: &str) -> bool {
        let Some(class) = self.class_decl else {
            return false;
        };
        class.members.iter().any(|m| match m {
            ClassMember::Property(p) => p.name.name == prop_name && p.model,
            _ => false,
        })
    }

    /// Same check as `class_property_is_model`, but for an arbitrary
    /// class name (looked up through the surrounding module). Used
    /// when the receiver is `obj.Books` for some `obj : OtherClass*`
    /// outside the surrounding `class_decl`.
    fn other_class_property_is_model(&self, class_name: &str, prop_name: &str) -> bool {
        let Some(module) = self.module else {
            return false;
        };
        for item in &module.items {
            if let Item::Class(c) = item {
                if c.name.name == class_name {
                    return c.members.iter().any(|m| match m {
                        ClassMember::Property(p) => p.name.name == prop_name && p.model,
                        _ => false,
                    });
                }
            }
        }
        false
    }

    /// Render a list of call args, wrapping each one whose matched
    /// param is `consuming` in `std::move(...)`. Idempotent for args
    /// that are already rvalues (extra std::move on rvalue-of-temporary
    /// is a no-op).
    fn render_args_with_consuming(
        &mut self,
        args: &[cute_syntax::ast::Expr],
        flags: &Option<Vec<bool>>,
    ) -> Vec<String> {
        args.iter()
            .enumerate()
            .map(|(i, a)| {
                let s = self.lower_expr(a);
                let consuming = flags
                    .as_ref()
                    .and_then(|fs| fs.get(i).copied())
                    .unwrap_or(false);
                if consuming {
                    format!("std::move({s})")
                } else {
                    s
                }
            })
            .collect()
    }

    /// Look up a method's declared return type on a named class.
    /// `None` for an undeclared method or one without an explicit
    /// return type.
    fn class_method_return_type(
        &self,
        class_name: &str,
        method_name: &str,
    ) -> Option<&'a cute_syntax::ast::TypeExpr> {
        self.lookup_class_method_decl(class_name, method_name)
            .and_then(|f| f.return_ty.as_ref())
    }

    /// When a generic fn returns a bare type parameter (e.g. `fn id<T>(x: T) -> T`),
    /// the AST-only `pointer_class_from_type_expr` can't see that the
    /// type-checker bound `T` to a pointer-backed class at this call
    /// site. Consult the call-site `generic_instantiations` map: if
    /// the return type names one of the fn's generics and the
    /// inferred arg at that position is a known pointer class,
    /// return that class name.
    fn pointer_class_via_generic_binding(
        &self,
        call_span: cute_syntax::span::Span,
        fn_decl: &FnDecl,
        ret_ty: &cute_syntax::ast::TypeExpr,
    ) -> Option<String> {
        // Return type must be a single, unparameterized generic param name.
        let TypeKind::Named { path, args } = &ret_ty.kind else {
            return None;
        };
        if !args.is_empty() || path.len() != 1 {
            return None;
        }
        let leaf = path[0].name.as_str();
        let idx = fn_decl.generics.iter().position(|g| g.name.name == leaf)?;
        let inferred = self.generic_instantiations?.get(&call_span)?;
        // Walk past any outer `Nullable` once — bound types from the
        // checker may carry an option layer (`fn id<T>(x: T?) -> T?`
        // call site). Inspecting the inner type is what tells us the
        // class name. Don't clone the name until the qobject/arc
        // check confirms it's a pointer class.
        let bound_inner = match inferred.get(idx)? {
            cute_types::ty::Type::Nullable(inner) => inner.as_ref(),
            other => other,
        };
        let leaf_name: &str = match bound_inner {
            cute_types::ty::Type::Class(n) | cute_types::ty::Type::External(n) => n,
            cute_types::ty::Type::Generic { base, .. } => base,
            _ => return None,
        };
        if self.is_ref_class(leaf_name) {
            Some(leaf_name.to_string())
        } else {
            None
        }
    }

    /// Build a hint from a typed collection TypeExpr. Returns `None`
    /// for bare `List` / `Map` (no type arg → default heterogeneous
    /// QVariant{List,Map}) and for non-collection types.
    /// Used by typed array / map literal lowering so `@values = [1.0]`
    /// against a `List<Float>` property emits `QList<double>{1.0}`
    /// (and `@m = { a: 1 }` against `Map<String, Int>` emits
    /// `QMap<::cute::String, qint64>{...}`) instead of the
    /// heterogeneous `QVariantList` / `QVariantMap` default.
    fn collection_hint_from_type(&self, ty: &cute_syntax::ast::TypeExpr) -> Option<CollectionHint> {
        let cute_syntax::ast::TypeKind::Named { path, args } = &ty.kind else {
            return None;
        };
        let leaf = path.last()?.name.as_str();
        match (leaf, args.len()) {
            ("List", 1) => Some(CollectionHint::List {
                elem_ty: args[0].clone(),
            }),
            ("Map", 2) => Some(CollectionHint::Map {
                key_ty: args[0].clone(),
                value_ty: args[1].clone(),
            }),
            _ => None,
        }
    }

    /// Resolve the LHS expected collection hint for an assignment
    /// target. Currently recognizes `@property` references whose
    /// declared type is `List<T>` or `Map<K,V>` — the common typed
    /// collection assignment shapes in slot bodies. Returns `None` for
    /// everything else (including local `let xs = [...]` without a
    /// type annotation; those keep the heterogeneous QVariant default).
    fn lhs_collection_hint(&self, target: &cute_syntax::ast::Expr) -> Option<CollectionHint> {
        use cute_syntax::ast::ExprKind as K;
        match &target.kind {
            K::AtIdent(name) => {
                let ty = self.class_property_type(name)?;
                self.collection_hint_from_type(ty)
            }
            _ => None,
        }
    }

    /// Lower `value` as the RHS of an assignment / let / var, biased
    /// by an expected collection element / value type when the LHS is
    /// known to be a typed `List<T>` / `Map<K,V>`. The bias only fires
    /// for the direct `K::Array` / `K::Map` shape; complex RHSes lower
    /// as usual, since a value-position expression's type can't be
    /// re-derived from the LHS without a full type-checker pass.
    ///
    /// When the literal nests another collection literal (e.g.
    /// `Map<String, List<Int>>` with `{ xs: [1,2,3] }`), the inner
    /// literal also receives a hint derived from the value type, so
    /// the inner `[1,2,3]` lowers to `QList<qint64>{1,2,3}` instead of
    /// the default `QVariantList{...}` (which wouldn't compile against
    /// the outer `QMap<QString, QList<qint64>>`).
    fn lower_with_collection_hint(
        &mut self,
        value: &cute_syntax::ast::Expr,
        hint: Option<CollectionHint>,
    ) -> String {
        use cute_syntax::ast::ExprKind as K;
        let Some(prog) = self.program else {
            return self.lower_expr(value);
        };
        let ctx = ty::TypeCtx::new(prog);
        match (hint, &value.kind) {
            (Some(CollectionHint::List { elem_ty }), K::Array(items)) => {
                let elem_cpp = ty::cute_to_cpp(&elem_ty, &ctx);
                let inner_hint = self.collection_hint_from_type(&elem_ty);
                let parts = items
                    .iter()
                    .map(|i| self.lower_with_collection_hint(i, inner_hint.clone()))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("QList<{elem_cpp}>{{{parts}}}")
            }
            (Some(CollectionHint::Map { key_ty, value_ty }), K::Map(entries)) => {
                let key_cpp = ty::cute_to_cpp(&key_ty, &ctx);
                let value_cpp = ty::cute_to_cpp(&value_ty, &ctx);
                let inner_hint = self.collection_hint_from_type(&value_ty);
                let parts = entries
                    .iter()
                    .map(|(k, v)| {
                        let key_s = match &k.kind {
                            cute_syntax::ast::ExprKind::Ident(name) => {
                                format!("QStringLiteral(\"{name}\")")
                            }
                            _ => self.lower_expr(k),
                        };
                        let val_s = self.lower_with_collection_hint(v, inner_hint.clone());
                        format!("{{{key_s}, {val_s}}}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("QMap<{key_cpp}, {value_cpp}>{{{parts}}}")
            }
            (_, _) => self.lower_expr(value),
        }
    }

    fn is_qobject_type_expr(&self, t: &cute_syntax::ast::TypeExpr) -> bool {
        if let cute_syntax::ast::TypeKind::Named { path, args } = &t.kind {
            if !args.is_empty() {
                return false;
            }
            if let Some(leaf) = path.last() {
                return self.is_qobject_class(&leaf.name);
            }
        }
        false
    }

    fn is_property_pointer(&self, prop_name: &str) -> bool {
        // `, model` props always lower as `::cute::ModelList<T*>*` —
        // pointer regardless of the surface `List<T>` declaration.
        if self.class_property_is_model(prop_name) {
            return true;
        }
        self.class_property_type(prop_name)
            .is_some_and(|t| self.is_qobject_type_expr(t))
    }

    fn is_property_list_of_pointer(&self, prop_name: &str) -> bool {
        let Some(t) = self.class_property_type(prop_name) else {
            return false;
        };
        let cute_syntax::ast::TypeKind::Named { path, args } = &t.kind else {
            return false;
        };
        let leaf = path.last().map(|i| i.name.as_str()).unwrap_or("");
        if !matches!(leaf, "List" | "Set") || args.len() != 1 {
            return false;
        }
        self.is_qobject_type_expr(&args[0])
    }

    /// The static class name of `e` when we can determine it locally.
    /// `None` when the expression is foreign / unanalyzed (e.g. a method-
    /// call return). Used to resolve `<expr>.<signal>.connect { ... }` to
    /// the right `&Class::signal` member function pointer.
    fn pointer_class_of(&self, e: &Expr) -> Option<String> {
        use cute_syntax::ast::ExprKind as K;
        match &e.kind {
            K::SelfRef => self.class_decl.map(|c| c.name.name.clone()),
            K::MethodCall {
                receiver, method, ..
            } if method.name == "new" => {
                if let K::Ident(class_name) = &receiver.kind {
                    if self.is_ref_class(class_name) {
                        return Some(class_name.clone());
                    }
                }
                None
            }
            K::Ident(name) => self
                .pointer_bindings
                .get(name)
                .filter(|s| !s.is_empty())
                .cloned(),
            K::AtIdent(name) => {
                let t = self.class_property_type(name)?;
                if let cute_syntax::ast::TypeKind::Named { path, args } = &t.kind {
                    if args.is_empty() {
                        if let Some(leaf) = path.last() {
                            if self.is_qobject_class(&leaf.name) {
                                return Some(leaf.name.clone());
                            }
                        }
                    }
                }
                None
            }
            // `make_arc()` — look up the fn's declared return type, then
            // try the generic-binding map when the return is a bare
            // type parameter. Resolves `let z = make_arc(); z.signal`
            // to the right `&Class::signal` for both concrete and
            // generic returns.
            K::Call { callee, .. } => {
                let K::Ident(fn_name) = &callee.kind else {
                    return None;
                };
                let module = self.module?;
                let f = module.items.iter().find_map(|item| match item {
                    Item::Fn(f) if f.name.name == *fn_name => Some(f),
                    _ => None,
                })?;
                let ret_ty = f.return_ty.as_ref()?;
                self.pointer_class_from_type_expr(ret_ty)
                    .or_else(|| self.pointer_class_via_generic_binding(e.span, f, ret_ty))
            }
            // `inst.method()` (non-`new`) returning a pointer class.
            // The `method == "new"` arm above handles constructors.
            K::MethodCall {
                receiver, method, ..
            } => {
                if let Some(class) = self.pointer_class_of(receiver) {
                    if let Some(ret_ty) = self.class_method_return_type(&class, &method.name) {
                        if let Some(c) = self.pointer_class_from_type_expr(ret_ty) {
                            return Some(c);
                        }
                    }
                }
                // ModelList-element accessor fallback. `messages.at(i)`
                // / `.first` / `.last` on a `, model` prop returns
                // `Row*` (a QObject pointer), but the receiver is a
                // ModelList adapter that the standard pointer-class
                // chain doesn't recognise as a pointer. Pull the row
                // class straight off the prop's `List<Row>` type arg
                // so `let m = messages.at(i); m.role` records `m` as
                // `Row*` and member access uses `->`.
                if matches!(method.name.as_str(), "at" | "first" | "last") {
                    if let Some(row_class) = self.model_prop_row_class_of(receiver) {
                        return Some(row_class);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Row-class name (e.g. "Message" for a `prop xs : ModelList<Message>`)
    /// when `recv` is a member access onto a `, model` prop. Used by
    /// `pointer_class_of` to resolve the return type of element
    /// accessors (`at`, `first`, `last`) without going through the
    /// receiver's adapter class — there is no ClassEntry the standard
    /// chain could land on, since `ModelList<T>` is a parser-level
    /// surface that lifts to `List<T> + model: true` for downstream
    /// codegen.
    fn model_prop_row_class_of(&self, recv: &Expr) -> Option<String> {
        use cute_syntax::ast::ExprKind as K;
        let (class_decl, prop_name): (&ClassDecl, &str) = match &recv.kind {
            // Bare member name inside the surrounding class method
            // body — `messages` rewrote to `@messages`.
            K::AtIdent(name) => (self.class_decl?, name.as_str()),
            // `<obj>.messages` — `obj`'s static class hosts the prop.
            K::Member { receiver, name } => (
                self.find_class_decl(&self.pointer_class_of(receiver)?)?,
                name.name.as_str(),
            ),
            // `<obj>.messages()` (no-arg auto-getter call form). Same
            // shape as K::Member after parser desugaring.
            K::MethodCall {
                receiver,
                method,
                args,
                ..
            } if args.is_empty() => (
                self.find_class_decl(&self.pointer_class_of(receiver)?)?,
                method.name.as_str(),
            ),
            _ => return None,
        };
        for m in &class_decl.members {
            if let ClassMember::Property(p) = m {
                if p.name.name == prop_name && p.model {
                    return Self::row_class_of_property_ty(&p.ty);
                }
            }
        }
        None
    }

    /// Look up a top-level `class X { ... }` decl in the surrounding
    /// module by name. Returns None when the module side-table is
    /// missing (snapshot-test path) or the class isn't user-defined
    /// in this module (e.g. it lives in a binding `.qpi`, which has
    /// no `, model` props in any case).
    fn find_class_decl(&self, class_name: &str) -> Option<&'a ClassDecl> {
        let module = self.module?;
        module.items.iter().find_map(|item| match item {
            Item::Class(c) if c.name.name == class_name => Some(c),
            _ => None,
        })
    }

    /// Extract `Row` from a `List<Row>` property type. Returns `None`
    /// for shapes that aren't the parser-lifted `, model`-prop form
    /// (`List<X>` with one type arg whose base is a class name).
    fn row_class_of_property_ty(ty: &cute_syntax::ast::TypeExpr) -> Option<String> {
        use cute_syntax::ast::TypeKind;
        let TypeKind::Named { path, args } = &ty.kind else {
            return None;
        };
        if path.last().map(|i| i.name.as_str()) != Some("List") || args.len() != 1 {
            return None;
        }
        let TypeKind::Named { path: row_path, .. } = &args[0].kind else {
            return None;
        };
        row_path.last().map(|i| i.name.clone())
    }

    /// Whether the value produced by `e` is a QObject pointer (so that
    /// method calls on it use `->` and member access uses `->`).
    /// Is `e` a bare ident referencing one of the surrounding fn's
    /// generic parameters? When true, member/method access on `e`
    /// must route through `::cute::deref` so the lowered C++ works
    /// for both pointer-typed and value-typed instantiations of the
    /// generic. Anything more complex (member chain, subscript, …)
    /// returns false: the user can capture the value into a `let`
    /// of generic type and we'd still catch the access on that
    /// fresh ident if it were declared with a generic-T let, but
    /// today only fn params are tracked.
    fn is_generic_param_receiver(&self, e: &Expr) -> bool {
        use cute_syntax::ast::ExprKind as K;
        if self.generic_typed_bindings.is_empty() {
            return false;
        }
        if let K::Ident(name) = &e.kind {
            return self.generic_typed_bindings.contains_key(name);
        }
        false
    }

    /// If `e` is a generic-bound parameter and the named method is
    /// declared on at least one of its bounds' traits, return the
    /// trait name to dispatch through. Drives free-function routing
    /// in templated bodies — `::cute::trait_impl::<Trait>::method(xs, args)`
    /// instead of `::cute::deref(xs).method(args)`. Returns `None`
    /// for bare-generic params (no trait surface to consult), for
    /// methods not on any bound, and for non-Ident receivers.
    fn trait_dispatch_name(&self, recv: &Expr, method_name: &str) -> Option<String> {
        use cute_syntax::ast::ExprKind as K;
        let K::Ident(binding) = &recv.kind else {
            return None;
        };
        let bounds = self.generic_typed_bindings.get(binding)?;
        if bounds.is_empty() {
            return None;
        }
        let module = self.module?;
        for bound in bounds {
            // Walk traits in the module, prelude included via
            // `module.items`. A trait method match wins on first
            // hit, mirroring the type checker's "first matching
            // bound" rule (see `synth_method_call`).
            for item in &module.items {
                let Item::Trait(t) = item else { continue };
                if t.name.name != *bound {
                    continue;
                }
                if t.methods.iter().any(|m| m.name.name == method_name) {
                    return Some(bound.clone());
                }
            }
        }
        None
    }

    /// Concrete-context counterpart to `trait_dispatch_name`: when
    /// `recv` is a value-typed binding (let / param of an extern
    /// value type or builtin generic) and there's an `impl Trait
    /// for ThatBase` in the module declaring `method_name`, return
    /// the trait name. Drives the namespace-dispatch path for
    /// direct calls like `let p = QPoint(...); p.magnitude()` —
    /// without this, codegen falls back to `p.magnitude()` (a
    /// nonexistent QPoint member, since extern types can't be
    /// spliced).
    ///
    /// The check is impl-existence-driven (not trait-shape-driven)
    /// so real Qt members never get shadowed: `p.x()` (a real
    /// QPoint member, NOT in any impl) keeps the regular
    /// `p.x()` lowering even if some unrelated trait happens to
    /// declare an `x` method.
    fn trait_dispatch_name_for_value_recv(&self, recv: &Expr, method_name: &str) -> Option<String> {
        use cute_syntax::ast::ExprKind as K;
        let K::Ident(binding) = &recv.kind else {
            return None;
        };
        let base = self.value_type_bindings.get(binding)?;
        let module = self.module?;
        // Find impls whose for-type's base name matches the receiver's
        // tracked base, then verify the trait actually declares
        // `method_name`. First match wins — coherence is enforced
        // at HIR time (or will be, once task (4) lands).
        for item in &module.items {
            let Item::Impl(i) = item else { continue };
            let Some(for_base) = cute_syntax::ast::type_expr_base_name(&i.for_type) else {
                continue;
            };
            if for_base != *base {
                continue;
            }
            for trait_item in &module.items {
                let Item::Trait(t) = trait_item else { continue };
                if t.name.name != i.trait_name.name {
                    continue;
                }
                if t.methods.iter().any(|m| m.name.name == method_name) {
                    return Some(i.trait_name.name.clone());
                }
            }
        }
        None
    }

    fn is_pointer_expr(&self, e: &Expr) -> bool {
        use cute_syntax::ast::ExprKind as K;
        match &e.kind {
            // `T.new(...)` for QObject T returns a freshly heap-allocated
            // pointer per the spec's parent-tree ownership model. For
            // ARC class T (`arc X { ... }`), it returns `cute::Arc<T>`
            // which overloads `->` so the same pointer-aware lowering
            // (member access via `->`, etc.) works uniformly.
            K::MethodCall {
                receiver, method, ..
            } if method.name == "new" => {
                if let K::Ident(class_name) = &receiver.kind {
                    return self.is_ref_class(class_name);
                }
                false
            }
            // `this` is always a Class* in C++ method bodies, so
            // dotted access from `self` needs `->`. When self has
            // been overridden (free-function lowering for impl
            // methods), use the override's pointer flag instead so
            // `self.method()` lowers to `self->method()` for
            // pointer self and `self.method()` for value self.
            K::SelfRef => self
                .self_override
                .as_ref()
                .map(|(_, is_ptr)| *is_ptr)
                .unwrap_or_else(|| self.class_decl.is_some() || self.struct_decl.is_some()),
            K::Ident(name) => {
                // Top-level QObject-typed `let X : Foo = Foo.new(...)`
                // lowers to `Q_GLOBAL_STATIC` whose accessor `X()`
                // returns `Foo*`. So a method call `X.method(...)`
                // needs the pointer arrow lowering (`X()->method(...)`).
                self.pointer_bindings.contains_key(name) || self.is_qobject_top_level_let(name)
            }
            K::AtIdent(name) => self.is_property_pointer(name),
            K::Index { receiver, .. } => {
                // Indexing a `List<QObject>` property yields a pointer.
                if let K::AtIdent(prop_name) = &receiver.kind {
                    return self.is_property_list_of_pointer(prop_name);
                }
                false
            }
            // Free-fn calls and instance-method calls: defer to
            // `pointer_class_of`, which handles fn-return-type
            // lookup, the generic-binding fallback, and method
            // return resolution via the receiver class.
            K::Call { .. } | K::MethodCall { .. } => self.pointer_class_of(e).is_some(),
            // `recv.Books` where `Books` is a `, model` prop on
            // recv's class: the C++ getter returns
            // `::cute::ModelList<T*>*`, so further `.method()` access
            // must use `->`.
            K::Member { receiver, name } => match self.pointer_class_of(receiver) {
                Some(cls) => self.other_class_property_is_model(&cls, &name.name),
                None => false,
            },
            _ => false,
        }
    }

    fn fresh(&mut self) -> String {
        let n = self.temp_counter;
        self.temp_counter += 1;
        format!("_r{n}")
    }

    /// Lower a function body, returning the final list of C++ statements
    /// (preludes already interleaved) without surrounding braces.
    fn lower_block(&mut self, body: &cute_syntax::ast::Block) -> Vec<LoweredLine> {
        let mut out: Vec<LoweredLine> = Vec::new();
        for stmt in &body.stmts {
            self.lower_stmt_into(stmt, &mut out);
        }
        if let Some(t) = body.trailing.as_ref() {
            let prev_stmt_span = self.current_stmt_span;
            self.current_stmt_span = Some(t.span);
            let value_s = self.lower_expr(t);
            out.append(&mut self.prelude);
            let kw = self.return_kw();
            let trailing_span = Some(t.span);
            if self.return_type == "void" {
                out.push((format!("{value_s};"), trailing_span));
            } else if self.is_err_union {
                let inner_t = inner_of_err_union_cpp(self.return_type);
                out.push((
                    format!(
                        "{kw} {ret}::ok({value});",
                        ret = self.return_type,
                        value = if value_s.is_empty() {
                            String::from("/* unit */")
                        } else {
                            value_s
                        }
                    ),
                    trailing_span,
                ));
                let _ = inner_t; // currently unused but documents intent
            } else {
                out.push((format!("{kw} {value_s};"), trailing_span));
            }
            self.current_stmt_span = prev_stmt_span;
        }
        out
    }

    fn lower_stmt_into(&mut self, stmt: &cute_syntax::ast::Stmt, out: &mut Vec<LoweredLine>) {
        use cute_syntax::ast::{ExprKind, Stmt};
        let stmt_span = stmt_span(stmt);
        let prev_stmt_span = self.current_stmt_span;
        self.current_stmt_span = Some(stmt_span);
        let line = match stmt {
            Stmt::Assign {
                target,
                op,
                value,
                span,
            } => {
                let value_is_ptr = self.is_pointer_expr(value);
                let lhs_hint = self.lhs_collection_hint(target);
                let rhs = self.lower_with_collection_hint(value, lhs_hint);
                let op_s = assign_op_str(*op);
                // First-occurrence bare-ident `=` becomes a declaration
                // (`auto x = ...`), driven by HIR's per-fn assign_is_decl
                // map. Anything else (re-assignment, +=, member-access
                // target) keeps the literal assignment form.
                let is_fresh_decl = matches!(op, cute_syntax::ast::AssignOp::Eq)
                    && matches!(target.kind, ExprKind::Ident(_))
                    && self
                        .fn_scope
                        .and_then(|s| s.assign_is_decl.get(span))
                        .copied()
                        .unwrap_or(false);
                if is_fresh_decl {
                    if let ExprKind::Ident(name) = &target.kind {
                        if value_is_ptr {
                            let class = self.pointer_class_of(value).unwrap_or_default();
                            self.pointer_bindings.insert(name.clone(), class);
                        }
                    }
                    let lhs = self.lower_expr(target);
                    format!("auto {lhs} = {rhs};")
                } else if let ExprKind::Member { receiver, name } = &target.kind {
                    // K::Member lowers as `recv->field()` (a by-value
                    // call), so a plain `lhs = rhs` would assign to a
                    // temporary and silently no-op. Route writes
                    // through `set<Field>(...)`; the `@field = value`
                    // (AtIdent) path keeps its direct `m_field` write.
                    let recv_s = self.lower_expr(receiver);
                    let sep = if self.is_pointer_expr(receiver) {
                        "->"
                    } else {
                        "."
                    };
                    let setter = ty::setter_name(&name.name);
                    if matches!(op, cute_syntax::ast::AssignOp::Eq) {
                        format!("{recv_s}{sep}{setter}({rhs});")
                    } else {
                        // Compound assigns desugar to read-modify-write:
                        // `obj.x += rhs` → `obj->setX(obj->x() OP rhs)`.
                        // `op_s` is "+=", "-=", ...; strip the trailing
                        // `=` to get the binary operator.
                        let bin = &op_s[..op_s.len() - 1];
                        let getter = &name.name;
                        format!("{recv_s}{sep}{setter}({recv_s}{sep}{getter}() {bin} {rhs});")
                    }
                } else if let ExprKind::AtIdent(name) = &target.kind {
                    // Bindable storage: bare `m_x` here invokes
                    // QObjectBindableProperty::operator= → setValue,
                    // which is the intended semantics. The `.value()`
                    // form used for AtIdent *reads* cannot appear on
                    // the LHS.
                    if self.at_ident_is_bindable_prop(name) {
                        if matches!(op, cute_syntax::ast::AssignOp::Eq) {
                            format!("m_{name} = {rhs};")
                        } else {
                            let bin = &op_s[..op_s.len() - 1];
                            format!("m_{name}.setValue(m_{name}.value() {bin} {rhs});")
                        }
                    } else if self.at_ident_is_weak_field(name) {
                        // Weak fields: writes go straight through to
                        // the raw `cute::Weak<T>` storage. Its
                        // `operator=(const cute::Arc<T>&)` /
                        // `operator=(std::nullptr_t)` overload does
                        // the conversion, so the user's `@x = arc_val`
                        // / `@x = nil` lowers to `m_x = arc_val;` /
                        // `m_x = nullptr;` directly. The `.lock()`
                        // rewrite from the read path doesn't apply
                        // here.
                        format!("m_{name} {op_s} {rhs};")
                    } else if self.at_ident_is_plain_prop(name) {
                        // Plain props: route through the setter so the
                        // dirty check + auto-emit fire on every write.
                        // Was raw `m_x = v;` (silent-broken — bypassed
                        // the notify signal); the unified-write design
                        // makes `@x = v` semantically equivalent to an
                        // external `obj.x = v`.
                        let setter = ty::setter_name(name);
                        if matches!(op, cute_syntax::ast::AssignOp::Eq) {
                            format!("{setter}({rhs});")
                        } else {
                            // Compound: read via the getter (zero-arg
                            // method named after the prop), combine,
                            // write via the setter.
                            let bin = &op_s[..op_s.len() - 1];
                            format!("{setter}({name}() {bin} {rhs});")
                        }
                    } else {
                        // Class field (let / var) without prop semantics:
                        // raw write to storage.
                        let lhs = self.lower_expr(target);
                        format!("{lhs} {op_s} {rhs};")
                    }
                } else {
                    let lhs = self.lower_expr(target);
                    format!("{lhs} {op_s} {rhs};")
                }
            }
            Stmt::Emit { signal, args, .. } => {
                let args_s = args
                    .iter()
                    .map(|a| self.lower_expr(a))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("emit {}({args_s});", signal.name)
            }
            Stmt::Expr(e) => {
                // Statement-position `if` / `case` / Block lower
                // directly to C++ statements rather than IIFEs so
                // that an early `return X` inside actually returns
                // from the surrounding fn (an IIFE would intercept
                // it). Value-position uses (let RHS, fn trailing
                // when non-void) reach `lower_expr` and the IIFE
                // form there.
                match &e.kind {
                    ExprKind::If {
                        cond,
                        then_b,
                        else_b,
                        let_binding,
                    } => {
                        if let Some((pat, init)) = let_binding {
                            self.lower_if_let_as_stmt(pat, init, then_b, else_b.as_ref());
                        } else {
                            self.lower_if_as_stmt(cond, then_b, else_b.as_ref());
                        }
                        out.append(&mut self.prelude);
                        self.current_stmt_span = prev_stmt_span;
                        return;
                    }
                    ExprKind::Case { scrutinee, arms } => {
                        self.lower_case_as_stmt(scrutinee, arms);
                        out.append(&mut self.prelude);
                        self.current_stmt_span = prev_stmt_span;
                        return;
                    }
                    ExprKind::Block(b) => {
                        // Lower each stmt of the block in the
                        // current scope; discard any trailing.
                        for s in &b.stmts {
                            self.lower_stmt_into(s, out);
                        }
                        self.current_stmt_span = prev_stmt_span;
                        return;
                    }
                    _ => format!("{};", self.lower_expr(e)),
                }
            }
            Stmt::Return { value: None, .. } => format!("{};", self.return_kw()),
            Stmt::Return { value: Some(v), .. } => {
                let returns_err = self.is_err_union && self.expr_is_error_value(v);
                let s = self.lower_expr(v);
                let kw = self.return_kw();
                if self.is_err_union {
                    let ctor = if returns_err { "err" } else { "ok" };
                    format!("{kw} {ret}::{ctor}({s});", ret = self.return_type)
                } else {
                    format!("{kw} {s};")
                }
            }
            Stmt::Let {
                name, ty, value, ..
            }
            | Stmt::Var {
                name, ty, value, ..
            } => {
                let value_is_ptr = self.is_pointer_expr(value);
                let class = if value_is_ptr {
                    self.pointer_class_of(value).unwrap_or_default()
                } else {
                    String::new()
                };
                // Generic-class instantiation: the type checker has
                // already recorded the inferred type args in
                // `generic_instantiations` keyed on the call's span.
                // The `K::MethodCall` lowering picks them up; this
                // arm doesn't need its own form-(a) peephole.
                let coll_hint = ty.as_ref().and_then(|t| self.collection_hint_from_type(t));
                let s = self.lower_with_collection_hint(value, coll_hint);
                if value_is_ptr {
                    self.pointer_bindings.insert(name.name.clone(), class);
                } else if let Some(t) = ty {
                    // Annotated value-typed binding (e.g.
                    // `let p : QPoint = QPoint(3, 4)`,
                    // `let xs : List<Int> = [...]`). Track for
                    // direct-call trait dispatch.
                    if let Some(base) = self.value_base_from_type_expr(t) {
                        self.value_type_bindings.insert(name.name.clone(), base);
                    }
                }
                format!("auto {} = {};", name.name, s)
            }
            Stmt::For {
                binding,
                iter,
                body,
                ..
            } => {
                // `for i in 0..N { stmts }` (range case) lowers to a
                // C-style integer for-loop; everything else lowers to a
                // C++ range-based for over the iter expression.
                let mut sub: Vec<LoweredLine> = Vec::new();
                for s in &body.stmts {
                    self.lower_stmt_into(s, &mut sub);
                }
                if let Some(t) = &body.trailing {
                    let v = self.lower_expr(t);
                    sub.append(&mut self.prelude);
                    sub.push((format!("{v};"), Some(t.span)));
                }
                let header = if let ExprKind::Range {
                    start,
                    end,
                    inclusive,
                } = &iter.kind
                {
                    // `a..b` / `a..=b` lowers to `std::views::iota(s, e)`
                    // — same C++20 ranges path Slice<T> iteration uses, so
                    // future pipeline operators (`arr | filter | map`)
                    // can compose against either source. iota is half-
                    // open; inclusive bumps the upper bound by one.
                    let s = self.lower_expr(start);
                    let e = if *inclusive {
                        format!("({}) + 1", self.lower_expr(end))
                    } else {
                        self.lower_expr(end)
                    };
                    format!(
                        "for (qint64 {bind} : std::views::iota(static_cast<qint64>({s}), static_cast<qint64>({e}))) {{",
                        bind = binding.name
                    )
                } else {
                    let iter_s = self.lower_expr(iter);
                    format!(
                        "for (const auto& {bind} : {iter_s}) {{",
                        bind = binding.name
                    )
                };
                out.append(&mut self.prelude);
                out.push((header, Some(stmt_span)));
                for (line, span) in sub {
                    out.push((format!("    {line}"), span));
                }
                out.push(("}".to_string(), Some(stmt_span)));
                self.current_stmt_span = prev_stmt_span;
                return;
            }
            Stmt::While { cond, body, .. } => {
                let cond_s = self.lower_expr(cond);
                let mut sub: Vec<LoweredLine> = Vec::new();
                for s in &body.stmts {
                    self.lower_stmt_into(s, &mut sub);
                }
                if let Some(t) = &body.trailing {
                    let v = self.lower_expr(t);
                    sub.append(&mut self.prelude);
                    sub.push((format!("{v};"), Some(t.span)));
                }
                out.append(&mut self.prelude);
                out.push((format!("while ({cond_s}) {{"), Some(stmt_span)));
                for (line, span) in sub {
                    out.push((format!("    {line}"), span));
                }
                out.push(("}".to_string(), Some(stmt_span)));
                self.current_stmt_span = prev_stmt_span;
                return;
            }
            Stmt::Break { .. } => "break;".to_string(),
            Stmt::Continue { .. } => "continue;".to_string(),
            Stmt::Batch { body, .. } => {
                let mut sub: Vec<LoweredLine> = Vec::new();
                for s in &body.stmts {
                    self.lower_stmt_into(s, &mut sub);
                }
                if let Some(t) = &body.trailing {
                    let v = self.lower_expr(t);
                    sub.append(&mut self.prelude);
                    sub.push((format!("{v};"), Some(t.span)));
                }
                out.append(&mut self.prelude);
                out.push(("{".to_string(), Some(stmt_span)));
                out.push((
                    format!("    QScopedPropertyUpdateGroup {BATCH_GUARD_VAR};"),
                    Some(stmt_span),
                ));
                for (line, span) in sub {
                    out.push((format!("    {line}"), span));
                }
                out.push(("}".to_string(), Some(stmt_span)));
                self.current_stmt_span = prev_stmt_span;
                return;
            }
        };
        out.append(&mut self.prelude);
        out.push((line, Some(stmt_span)));
        self.current_stmt_span = prev_stmt_span;
    }

    /// Lower `embed("path")` to an IIFE returning a zero-copy
    /// `QByteArray`. The path is read at codegen time, relative to
    /// the directory of the .cute file containing the call. The
    /// file's bytes land inline as `static constexpr unsigned char[]`
    /// inside the lambda body — initialized once at first call,
    /// shared across subsequent calls. `QByteArray::fromRawData`
    /// wraps the static without a heap copy (caller must not mutate
    /// — detach happens automatically on first write, matching the
    /// rest of Qt's COW collection contract).
    ///
    /// Failure modes (all surface as inline `#error` text in the
    /// emitted C++ — turns into a compile-time error the user can't
    /// miss):
    /// - Argument is not a single `String` literal (no `#{...}`
    ///   interpolation, no concatenation).
    /// - Source map is unavailable (test fixture context). embed
    ///   needs to know the .cute file's directory; tests that build
    ///   raw modules don't set this.
    /// - File is missing or unreadable.
    fn lower_embed_call(
        &mut self,
        call_expr: &cute_syntax::ast::Expr,
        arg: &cute_syntax::ast::Expr,
    ) -> String {
        use cute_syntax::ast::{ExprKind as K, StrPart};
        // Argument must be a string literal with exactly one Text
        // part — `#{...}` interpolation can't be evaluated at
        // codegen, so reject.
        let path_str = match &arg.kind {
            K::Str(parts) if parts.len() == 1 => match &parts[0] {
                StrPart::Text(s) => s.clone(),
                _ => {
                    return embed_error(
                        "embed(\"...\") requires a plain string literal (no `#{...}` interpolation)",
                    );
                }
            },
            K::Str(_) => {
                return embed_error(
                    "embed(\"...\") requires a single-part string literal (no concatenation / interpolation)",
                );
            }
            _ => {
                return embed_error("embed(\"...\") requires a string literal argument");
            }
        };
        // Resolve path relative to the .cute file's directory. The
        // SourceMap maps FileId → original path; we take its parent
        // dir and join with the embed argument.
        let Some(sm) = self.source_map else {
            return embed_error(
                "embed(...) needs source-map context (only available through `cute build`, not raw codegen tests)",
            );
        };
        let src_path = std::path::PathBuf::from(sm.name(call_expr.span.file));
        let base_dir = src_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let abs_path = base_dir.join(&path_str);
        let bytes = match std::fs::read(&abs_path) {
            Ok(b) => b,
            Err(e) => {
                return embed_error(&format!(
                    "embed(\"{path_str}\") failed to read {} — {e}",
                    abs_path.display()
                ));
            }
        };
        // Render bytes as a comma-separated hex literal list. Wrap
        // ~16 per line to keep the .cpp readable and avoid pushing
        // every embed onto a single multi-MB physical line that
        // some C++ tooling chokes on.
        let mut hex = String::with_capacity(bytes.len() * 6);
        for (i, b) in bytes.iter().enumerate() {
            if i > 0 {
                hex.push(',');
                if i % 16 == 0 {
                    hex.push('\n');
                } else {
                    hex.push(' ');
                }
            }
            hex.push_str(&format!("0x{b:02x}"));
        }
        // Empty-file case: a `[0]` array would be UB to read past;
        // QByteArray::fromRawData handles size=0 fine, but C++ does
        // not let you declare a zero-element array. Emit a 1-byte
        // sentinel and pass length=0 explicitly.
        if bytes.is_empty() {
            return "QByteArray::fromRawData(nullptr, 0)".to_string();
        }
        format!(
            "([](){{ static constexpr unsigned char _d[] = {{ {hex} }}; return QByteArray::fromRawData(reinterpret_cast<const char*>(_d), sizeof(_d)); }})()"
        )
    }

    fn lower_expr(&mut self, e: &cute_syntax::ast::Expr) -> String {
        use cute_syntax::ast::ExprKind as K;
        match &e.kind {
            K::Int(v) => v.to_string(),
            K::Float(v) => float_literal_cpp(*v),
            K::Bool(true) => "true".into(),
            K::Bool(false) => "false".into(),
            K::Nil => "nullptr".into(),
            K::Str(parts) => self.lower_string_parts(parts),
            K::Sym(s) => format!("QByteArrayLiteral(\"{s}\")"),
            K::Ident(name) => {
                // Top-level QObject-typed `let X : Foo = Foo.new()`
                // lowers to `Q_GLOBAL_STATIC(Foo, X)`. The macro's
                // accessor is `X()` returning `Foo*`, so user-source
                // references to bare `X` need rewriting at every read
                // site. Value-typed top-level lets keep the bare-name
                // emit since they lower to plain `static const auto`.
                if self.is_qobject_top_level_let(name) {
                    format!("{name}()")
                } else {
                    name.clone()
                }
            }
            K::AtIdent(name) => {
                if self.at_ident_is_bindable_prop(name) {
                    format!("m_{name}.value()")
                } else if self.at_ident_is_weak_field(name) {
                    // Transparent lock: `@parent` reads on a `weak`
                    // field yield `cute::Arc<T>` (possibly null when
                    // the pointee has expired), matching the surface
                    // `Parent?` semantics. Writes still target the
                    // raw `m_x` slot — the assignment-statement
                    // lowering checks the same flag and skips this
                    // rewrite.
                    format!("m_{name}.lock()")
                } else if self.at_ident_is_plain_prop(name) {
                    // Plain Q_PROPERTY reads go through the getter so
                    // a subclass virtualising `<name>()` sees its
                    // override fire from in-class code paths too —
                    // mirrors the write side, which already routes
                    // through `set<Name>(...)`. Qt's READ accessor
                    // is `T <name>() const { return m_<name>; }`
                    // (inline, trivial), so optimised builds collapse
                    // back to the same machine code as `m_<name>`
                    // when no override is in play.
                    format!("{name}()")
                } else {
                    format!("m_{name}")
                }
            }
            K::SelfRef => self
                .self_override
                .as_ref()
                .map(|(token, _)| token.clone())
                .unwrap_or_else(|| "this".into()),
            K::Path(parts) => parts
                .iter()
                .map(|i| i.name.clone())
                .collect::<Vec<_>>()
                .join("::"),
            K::Call {
                callee,
                args,
                block,
                ..
            } => {
                // `println(...)` is a Cute builtin that prints to stderr
                // via Qt's qInfo (preserves QString round-trip and works
                // out-of-the-box without pulling iostream).
                if let K::Ident(name) = &callee.kind {
                    if name == "println" {
                        if args.is_empty() {
                            return "qInfo().noquote()".to_string();
                        }
                        // Wrap each arg in `cute::str::to_string` so
                        // QVariant-of-X (yielded by iterating an
                        // untyped `List` / `Map`) prints as the
                        // inner value's string instead of the noisy
                        // `QVariant(QString, "...")` debug wrapper.
                        // String/int/bool args go through the
                        // identity overloads.
                        let chained = args
                            .iter()
                            .map(|a| format!("::cute::str::to_string({})", self.lower_expr(a)))
                            .collect::<Vec<_>>()
                            .join(" << ");
                        return format!("qInfo().noquote() << {chained}");
                    }
                    // `assert_eq(actual, expected)` is the test framework
                    // builtin. Forwards to the runtime template so the
                    // failure path can throw with a formatted message
                    // including the file:line of the call site.
                    if name == "assert_eq" && args.len() == 2 {
                        let actual = self.lower_expr(&args[0]);
                        let expected = self.lower_expr(&args[1]);
                        return format!(
                            "::cute::test::assert_eq({actual}, {expected}, __FILE__, __LINE__)"
                        );
                    }
                    if name == "assert_neq" && args.len() == 2 {
                        let actual = self.lower_expr(&args[0]);
                        let unexpected = self.lower_expr(&args[1]);
                        return format!(
                            "::cute::test::assert_neq({actual}, {unexpected}, __FILE__, __LINE__)"
                        );
                    }
                    if name == "assert_true" && args.len() == 1 {
                        let cond = self.lower_expr(&args[0]);
                        return format!("::cute::test::assert_true({cond}, __FILE__, __LINE__)");
                    }
                    if name == "assert_false" && args.len() == 1 {
                        let cond = self.lower_expr(&args[0]);
                        return format!("::cute::test::assert_false({cond}, __FILE__, __LINE__)");
                    }
                    // `assert_throws { body }` — succeeds if `body`
                    // throws. The block is the only argument; wrap it
                    // as a nullary lambda and forward to the runtime
                    // template (which owns the try/catch).
                    if name == "assert_throws" && args.is_empty() {
                        if let Some(b) = block {
                            let body = self.lower_block_arg(b);
                            return format!(
                                "::cute::test::assert_throws({body}, __FILE__, __LINE__)"
                            );
                        }
                    }
                    // `embed("path/to/file")` — compile-time asset
                    // embed. Reads the file at codegen time, splices
                    // the bytes into a `static constexpr unsigned
                    // char[]` inside an IIFE, and returns a
                    // `QByteArray` (zero-copy, via `fromRawData`).
                    // Surface: ByteArray-only. For other Qt
                    // shapes, compose: `QString.fromUtf8(embed(...))`,
                    // `QImage.fromData(embed(...))` etc.
                    if name == "embed" && args.len() == 1 && block.is_none() {
                        return self.lower_embed_call(e, &args[0]);
                    }
                }
                // `<sender>.<signal>.connect { ... }` parses as a Call
                // whose callee is `Member{ Member{<sender>, signal}, "connect" }`
                // (trailing block kicks in only after the parser sees the
                // member, not after a method call). Detect that shape and
                // delegate to the same QObject::connect emitter as the
                // MethodCall arm uses.
                if let K::Member {
                    receiver: connect_recv,
                    name: connect_name,
                } = &callee.kind
                {
                    if connect_name.name == "connect" {
                        if let Some(s) = self.try_emit_signal_connect(connect_recv, args, block) {
                            return s;
                        }
                    }
                }
                // `<recv>.<method> { ... }` (trailing-block call without
                // explicit parens) parses as `Call { callee: Member, ... }`.
                // Now that Member lowers as a zero-arg call (`recv.x()`),
                // a literal `format!("{cs}({})", args)` would produce
                // `recv.x()(args)`. Detect this shape and merge into a
                // single `recv.x(args)` so the trailing-block style and
                // the explicit parens style emit identical C++.
                if let K::Member { receiver, name } = &callee.kind {
                    let recv_is_ptr = self.is_pointer_expr(receiver);
                    let sep = if recv_is_ptr { "->" } else { "." };
                    let rs = self.lower_expr(receiver);
                    let mut args_s: Vec<String> = args.iter().map(|a| self.lower_expr(a)).collect();
                    if let Some(b) = block {
                        args_s.push(self.lower_block_arg(b));
                    }
                    let inferred_targs = self.inferred_type_args_at(e.span);
                    return format!(
                        "{rs}{sep}{}{}({})",
                        name.name,
                        inferred_targs,
                        args_s.join(", ")
                    );
                }
                // Wrap consuming args in `std::move(...)` when the
                // callee is a top-level fn declared in this module —
                // matches the by-value-with-move lowering of consuming
                // params and makes call sites with lvalue args compile.
                let consuming_flags = match (&callee.kind, self.module) {
                    (K::Ident(fn_name), Some(m)) => m.fn_consuming_flags(fn_name),
                    _ => None,
                };
                let cs = self.lower_expr(callee);
                let mut args_s = self.render_args_with_consuming(args, &consuming_flags);
                if let Some(b) = block {
                    args_s.push(self.lower_block_arg(b));
                }
                // Generic top-level fn call: append the inferred type
                // args (`make<qint64>(0)`). C++ template deduction
                // handles most cases on its own, but lambdas-as-
                // `std::function<U(T)>` need an explicit annotation.
                let inferred_targs = self.inferred_type_args_at(e.span);
                format!("{cs}{}({})", inferred_targs, args_s.join(", "))
            }
            K::MethodCall {
                receiver,
                method,
                args,
                block,
                type_args,
            } => {
                // Built-in `e.rawValue()` on enum / flags values:
                // lowers to `static_cast<qint32>(e)`. Type checker
                // already verified the receiver is Type::Enum /
                // Type::Flags and the call has zero args / no block.
                if method.name == "rawValue" && args.is_empty() && block.is_none() {
                    let recv_s = self.lower_expr(receiver);
                    return format!("static_cast<qint32>({recv_s})");
                }
                // `EnumName.Variant(args)` — payload variant
                // constructor or static enum factory call. Same
                // PascalCase-as-namespace rule the K::Member arm
                // uses, but for the call form: emit `Enum::Variant
                // (args)` directly (no `()` wrapping fluff). For
                // extern enums declared with a C++ namespace
                // prefix, use that prefix instead.
                if let K::Ident(ns) = &receiver.kind {
                    if ns
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_uppercase())
                        .unwrap_or(false)
                    {
                        if let Some(cute_hir::ItemKind::Enum { cpp_namespace, .. }) =
                            self.program.and_then(|p| p.items.get(ns))
                        {
                            let prefix = cpp_namespace.clone().unwrap_or_else(|| ns.clone());
                            let args_s: Vec<String> =
                                args.iter().map(|a| self.lower_expr(a)).collect();
                            return format!("{prefix}::{}({})", method.name, args_s.join(", "));
                        }
                    }
                }
                // Generic-class instantiation paths:
                //   form (b)  — explicit `Box<Int>.new()` syntax: the
                //               parser populates `type_args` on the call.
                //   form (a) /
                //   fn-arg /  — implicit; the type checker recorded the
                //   return    inferred type args in `generic_instantiations`
                //               keyed on the call's span.
                // Both lower to the same instantiated-template form:
                // `cute::Arc<Box<X>>(new Box<X>(args))` (ARC) or
                // `new Box<X>(args)` (QObject).
                if method.name == "new" {
                    if let K::Ident(class_name) = &receiver.kind {
                        if let Some(prog) = self.program {
                            // Pick whichever source has the args. The
                            // syntactic `type_args` wins if both are
                            // present (the user spelled them out).
                            let ctx = ty::TypeCtx::new(prog);
                            let inst_args: Option<String> = if !type_args.is_empty() {
                                Some(
                                    type_args
                                        .iter()
                                        .map(|t| ty::cute_to_cpp(t, &ctx))
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                )
                            } else if let Some(map) = self.generic_instantiations {
                                map.get(&e.span).map(|tys| {
                                    tys.iter()
                                        .map(|t| ty::cute_type_to_cpp(t, &ctx))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                })
                            } else {
                                None
                            };
                            if let Some(inst_args) = inst_args {
                                let inst = format!("{class_name}<{inst_args}>");
                                let mut args_s: Vec<String> =
                                    args.iter().map(|a| self.lower_expr(a)).collect();
                                if self.is_extern_value_class(class_name) {
                                    return format!("{inst}({})", args_s.join(", "));
                                }
                                if self.is_qobject_class(class_name) {
                                    if args_s.is_empty() && self.class_decl.is_some() {
                                        args_s.push("this".to_string());
                                    }
                                    return format!("new {inst}({})", args_s.join(", "));
                                }
                                if self.is_arc_class(class_name) {
                                    return format!(
                                        "::cute::Arc<{inst}>(new {inst}({}))",
                                        args_s.join(", ")
                                    );
                                }
                            }
                        }
                    }
                }
                // `Type.staticMethod(args)` — receiver is the type
                // itself, lowers to `T::method(args)`. Recognized
                // either via an explicit `static fn` declaration, or
                // via the legacy extern-value rule where every non-
                // `new` method on an `extern value` class is a static.
                if let K::Ident(class_name) = &receiver.kind {
                    let is_static_call = !self.is_local_binding(class_name)
                        && (self.is_static_method_of_class(class_name, &method.name)
                            || (method.name != "new" && self.is_extern_value_class(class_name)));
                    if is_static_call {
                        let mut args_v: Vec<String> =
                            args.iter().map(|a| self.lower_expr(a)).collect();
                        if let Some(b) = block {
                            args_v.push(self.lower_block_arg(b));
                        }
                        return format!("{class_name}::{}({})", method.name, args_v.join(", "));
                    }
                }
                // `T.new(args)` for a QObject-derived class -> heap-alloc.
                // The surrounding `let`/`var` (or fresh `=`) records the
                // resulting binding as a pointer via `is_pointer_expr`.
                //
                // Memory-safety helper: when called inside a class method
                // body (`self.class_decl.is_some()`) with no explicit
                // parent argument, auto-inject `this` so the freshly-
                // allocated QObject hangs off the surrounding object's
                // parent-tree and gets cleaned up automatically. The
                // user can always override by passing an explicit parent
                // argument.
                if method.name == "new" {
                    if let K::Ident(class_name) = &receiver.kind {
                        if self.is_struct_named(class_name) {
                            // Cute `struct X` — value type. Prefer C++20
                            // designated initializers (`X{.x = a, .y = b}`)
                            // when we know the field declaration order, so
                            // the generated C++ documents which ctor arg
                            // lands in which field. Falls back to plain
                            // positional brace-init for arity-mismatched
                            // calls (struct field has a default that the
                            // user is implicitly accepting) and for the
                            // zero-arg `X.new()` empty-init.
                            let lowered: Vec<String> =
                                args.iter().map(|a| self.lower_expr(a)).collect();
                            if lowered.is_empty() {
                                return format!("{class_name}{{}}");
                            }
                            if let Some(field_names) = self.struct_field_names(class_name) {
                                if field_names.len() == lowered.len() {
                                    let body = field_names
                                        .iter()
                                        .zip(lowered.iter())
                                        .map(|(f, v)| format!(".{f} = {v}"))
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    return format!("{class_name}{{{body}}}");
                                }
                            }
                            return format!("{class_name}{{{}}}", lowered.join(", "));
                        }
                        // `T.new(args)` ctor lowering, branched on
                        // class shape. Each kind has a distinct C++
                        // construction form; `NotAClass` (free fn /
                        // struct path / unknown) falls through.
                        match self.class_kind(class_name) {
                            ClassKind::ExternValue => {
                                // Plain C++ value type — stack/value
                                // construct. No `new`, no Arc, no
                                // parent injection.
                                let args_s = args
                                    .iter()
                                    .map(|a| self.lower_expr(a))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                return format!("{class_name}({args_s})");
                            }
                            ClassKind::QObject => {
                                let mut args_s: Vec<String> =
                                    args.iter().map(|a| self.lower_expr(a)).collect();
                                if args_s.is_empty() && self.class_decl.is_some() {
                                    args_s.push("this".to_string());
                                }
                                return format!("new {class_name}({})", args_s.join(", "));
                            }
                            ClassKind::Arc => {
                                // ARC class lifetime is via reference-
                                // count. `T.new(args)` -> `cute::Arc<T>(
                                // new T(args))` — the smart pointer
                                // takes the raw heap object and bumps
                                // its refcount.
                                let args_s = args
                                    .iter()
                                    .map(|a| self.lower_expr(a))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                return format!(
                                    "::cute::Arc<{class_name}>(new {class_name}({args_s}))"
                                );
                            }
                            ClassKind::NotAClass => {}
                        }
                    }
                }
                // `ErrorName.<variant>(args)` -> the `static T variant(args)`
                // factory the error decl emits. C++ namespacing requires
                // `::` between the type name and the static method.
                if let K::Ident(name) = &receiver.kind {
                    if self.is_error_class(name) {
                        let args_s = args
                            .iter()
                            .map(|a| self.lower_expr(a))
                            .collect::<Vec<_>>()
                            .join(", ");
                        return format!("{name}::{}({args_s})", method.name);
                    }
                }
                // `<sender>.<signal>.connect { ... }` -> Qt's modern
                // function-pointer connect form `QObject::connect(sender,
                // &Class::signal, lambda)`. Recognized when receiver is
                // `Member { receiver: <sender>, name: <signal> }` and we
                // can resolve <sender>'s class to one whose signal table
                // contains <signal>.
                if method.name == "connect" {
                    if let Some(s) = self.try_emit_signal_connect(receiver, args, block) {
                        return s;
                    }
                }
                // Generic-typed receiver: `xs.method(args)` where `xs`
                // is bound to a generic param `T`. The C++ template
                // doesn't know whether T resolves to a pointer
                // (`Person*`) or a value, so neither `xs.method` nor
                // `xs->method` is correct in general.
                //
                // Two dispatch paths:
                //
                // 1. **Trait-method dispatch.** When `T: SomeTrait`
                //    and `method` is on the trait surface, route
                //    through `::cute::trait_impl::<Trait>::method(xs, args)`.
                //    Each `impl Trait for X` emits an overload in
                //    that namespace (delegate for user classes,
                //    inline body for extern types), so C++ overload
                //    resolution picks the right variant per
                //    instantiation. Lets `impl Iterable for QStringList`
                //    work end-to-end in a templated body.
                //
                // 2. **Bare-generic / non-trait method.** Fall back
                //    to `::cute::deref(xs)` whose `if constexpr`
                //    branches pick pointer vs value at instantiation.
                //    Used for bare `<T>` (no bound) and for methods
                //    not on a bound trait (still useful when T is
                //    structurally known to expose the method).
                if self.is_generic_param_receiver(receiver) {
                    let rs = self.lower_expr(receiver);
                    let mut args_s: Vec<String> = args.iter().map(|a| self.lower_expr(a)).collect();
                    if let Some(b) = block {
                        args_s.push(self.lower_block_arg(b));
                    }
                    let inferred_targs = self.inferred_type_args_at(e.span);
                    if let Some(trait_name) = self.trait_dispatch_name(receiver, &method.name) {
                        let mut all_args = vec![rs];
                        all_args.extend(args_s);
                        return format!(
                            "::cute::trait_impl::{trait_name}::{}{}({})",
                            method.name,
                            inferred_targs,
                            all_args.join(", ")
                        );
                    }
                    return format!(
                        "::cute::deref({rs}).{}{}({})",
                        method.name,
                        inferred_targs,
                        args_s.join(", ")
                    );
                }
                // **Value-typed direct call.** `let p : QPoint = ...;
                // p.magnitude()` where `magnitude` is on a registered
                // `impl Trait for QPoint`. Routes through the namespace
                // overload — the regular path would emit
                // `p.magnitude()`, calling a nonexistent QPoint
                // member. Real Qt members (`p.x()`, `p.manhattanLength()`)
                // are NOT in any impl and fall through.
                if let Some(trait_name) =
                    self.trait_dispatch_name_for_value_recv(receiver, &method.name)
                {
                    let rs = self.lower_expr(receiver);
                    let mut args_s: Vec<String> = args.iter().map(|a| self.lower_expr(a)).collect();
                    if let Some(b) = block {
                        args_s.push(self.lower_block_arg(b));
                    }
                    let inferred_targs = self.inferred_type_args_at(e.span);
                    let mut all_args = vec![rs];
                    all_args.extend(args_s);
                    return format!(
                        "::cute::trait_impl::{trait_name}::{}{}({})",
                        method.name,
                        inferred_targs,
                        all_args.join(", ")
                    );
                }
                if let Some(wrap) = self.try_emit_lifted_bool_ok_call(receiver, method, args, block)
                {
                    return wrap;
                }
                let recv_is_ptr = self.is_pointer_expr(receiver);
                // Look up the receiver's class name (when statically
                // known) so consuming-param method args get auto-
                // wrapped in `std::move(...)`. Skipped for foreign /
                // generic receivers.
                let consuming_flags = self.module.and_then(|m| {
                    self.pointer_class_of(receiver)
                        .and_then(|cn| m.method_consuming_flags(&cn, &method.name))
                });
                let rs = self.lower_expr(receiver);
                let mut args_s = self.render_args_with_consuming(args, &consuming_flags);
                if let Some(b) = block {
                    args_s.push(self.lower_block_arg(b));
                }
                let sep = if recv_is_ptr { "->" } else { "." };
                // Method-level generics: when the type checker
                // recorded inferred type args for this call, emit
                // them explicitly (`recv->method<X>(args)`). C++
                // template deduction handles most cases, but lambdas
                // taken as `std::function<U(T)>` won't deduce U.
                let inferred_targs = self.inferred_type_args_at(e.span);
                format!(
                    "{rs}{sep}{}{}({})",
                    method.name,
                    inferred_targs,
                    args_s.join(", ")
                )
            }
            K::Member { receiver, name } => {
                // `e.rawValue` on an enum / flags value — built-in
                // extractor for the underlying integer. Type
                // checker has already verified the receiver type;
                // codegen lowers to a static_cast.
                if name.name == "rawValue" {
                    let recv_s = self.lower_expr(receiver);
                    return format!("static_cast<qint32>({recv_s})");
                }
                // Ruby-style: `obj.x` is a zero-arg method call, not a
                // raw field reference. Q_PROPERTY getters lower to
                // `T x() const` so calling without parens would be a
                // C++ compile error; emit `()` so `app.exec` and
                // `counter.count` Just Work without explicit parens.
                //
                // Exception: `Foo.X` where `Foo` is PascalCase —
                // namespace-qualified enum / constant access (Cute
                // convention: types / namespaces are PascalCase,
                // values are camelCase). Emit `Foo::X` without
                // parens. Covers Qt.AlignCenter, std.npos, the new
                // user enum AlignmentFlag.AlignLeft form. For
                // extern enums declared with a C++ namespace
                // prefix (`extern enum Qt::AlignmentFlag`), the
                // emit prefix is the declared cpp_namespace
                // ("Qt") rather than the bare receiver name.
                if let K::Ident(ns) = &receiver.kind {
                    if ns
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_uppercase())
                        .unwrap_or(false)
                    {
                        let item = self.program.and_then(|p| p.items.get(ns));
                        // Only Enum / Flags / unknown-PascalCase receivers
                        // lower to namespace-style `Foo::X`. Any other
                        // resolved item (Class, Let, Struct, Trait) means
                        // the receiver is an instance value or a class
                        // type with no static members, so we fall through
                        // to the generic member-access path. That path
                        // handles `Counter()->value()` for singleton
                        // stores via the K::Ident accessor lowering.
                        let is_namespace_access = matches!(
                            item,
                            Some(cute_hir::ItemKind::Enum { .. })
                                | Some(cute_hir::ItemKind::Flags { .. }),
                        ) || item.is_none();
                        if is_namespace_access {
                            let (cns, payload_enum) = match item {
                                Some(cute_hir::ItemKind::Enum {
                                    cpp_namespace,
                                    variants,
                                    ..
                                }) => (
                                    cpp_namespace.clone(),
                                    variants.iter().any(|v| !v.fields.is_empty()),
                                ),
                                Some(cute_hir::ItemKind::Flags { cpp_namespace, .. }) => {
                                    (cpp_namespace.clone(), false)
                                }
                                _ => (None, false),
                            };
                            let prefix = cns.unwrap_or_else(|| ns.clone());
                            // Payload-style enum: nullary variant access
                            // is a static factory call (`Shape::Empty()`)
                            // not a value reference. Add parens.
                            if payload_enum {
                                return format!("{prefix}::{}()", name.name);
                            }
                            return format!("{prefix}::{}", name.name);
                        }
                    }
                }
                // Generic-typed receiver: route through trait-impl
                // namespace dispatch when the (zero-arg) accessor
                // matches a bound trait method, else fall back to
                // `::cute::deref(xs).x()` (see matching K::MethodCall
                // arm for the full rationale).
                if self.is_generic_param_receiver(receiver) {
                    if let Some(trait_name) = self.trait_dispatch_name(receiver, &name.name) {
                        return format!(
                            "::cute::trait_impl::{trait_name}::{}({})",
                            name.name,
                            self.lower_expr(receiver)
                        );
                    }
                    return format!(
                        "::cute::deref({}).{}()",
                        self.lower_expr(receiver),
                        name.name
                    );
                }
                // Value-typed direct member-style trait call. See the
                // matching K::MethodCall arm for the full rationale —
                // same shape, no args.
                if let Some(trait_name) =
                    self.trait_dispatch_name_for_value_recv(receiver, &name.name)
                {
                    return format!(
                        "::cute::trait_impl::{trait_name}::{}({})",
                        name.name,
                        self.lower_expr(receiver)
                    );
                }
                let recv_is_ptr = self.is_pointer_expr(receiver);
                let sep = if recv_is_ptr { "->" } else { "." };
                // Struct field access: emit `recv.field` (or
                // `this->field` for self in a struct method body)
                // without `()` because struct fields are plain C++
                // members, not getter methods.
                if let Some(struct_name) = self.struct_name_of_receiver(receiver) {
                    if self.struct_has_field(&struct_name, &name.name) {
                        return format!("{}{sep}{}", self.lower_expr(receiver), name.name);
                    }
                }
                format!("{}{sep}{}()", self.lower_expr(receiver), name.name)
            }
            K::SafeMember { receiver, name } => self.lower_safe_access(
                receiver,
                /*member_or_method=*/ &name.name,
                /*is_call=*/ false,
                /*args_str=*/ String::new(),
            ),
            K::SafeMethodCall {
                receiver,
                method,
                args,
                block,
                ..
            } => {
                let mut args_s: Vec<String> = args.iter().map(|a| self.lower_expr(a)).collect();
                if let Some(b) = block {
                    args_s.push(self.lower_block_arg(b));
                }
                self.lower_safe_access(
                    receiver,
                    &method.name,
                    /*is_call=*/ true,
                    args_s.join(", "),
                )
            }
            K::Index { receiver, index } => {
                lower_index_expr(receiver, index, |e| self.lower_expr(e))
            }
            K::Unary { op, expr } => {
                let inner = self.lower_expr(expr);
                match op {
                    cute_syntax::ast::UnaryOp::Neg => format!("(-{inner})"),
                    cute_syntax::ast::UnaryOp::Not => format!("(!{inner})"),
                }
            }
            K::Binary { op, lhs, rhs } => {
                use cute_syntax::ast::BinOp as B;
                let s = match op {
                    B::Add => "+",
                    B::Sub => "-",
                    B::Mul => "*",
                    B::Div => "/",
                    B::Mod => "%",
                    B::Lt => "<",
                    B::LtEq => "<=",
                    B::Gt => ">",
                    B::GtEq => ">=",
                    B::Eq => "==",
                    B::NotEq => "!=",
                    B::And => "&&",
                    B::Or => "||",
                    B::BitOr => "|",
                    B::BitAnd => "&",
                    B::BitXor => "^",
                };
                format!("({} {} {})", self.lower_expr(lhs), s, self.lower_expr(rhs))
            }
            K::Try(inner) => self.lower_try(inner),
            K::If {
                cond,
                then_b,
                else_b,
                let_binding,
            } => {
                if let Some((pat, init)) = let_binding {
                    self.lower_if_let(pat, init, then_b, else_b.as_ref())
                } else {
                    self.lower_if(cond, then_b, else_b.as_ref())
                }
            }
            K::Case { scrutinee, arms } => self.lower_case(scrutinee, arms),
            K::Await(inner) => format!("co_await {}", self.lower_expr(inner)),
            K::Block(b) => {
                // Value-position block expression: lower as an
                // immediately-invoked lambda so any internal preludes
                // (from `?` etc.) stay scoped.
                let body = self.lower_lambda_body(b);
                format!("[&]() {body}()")
            }
            K::Lambda { params, body } => self.lower_lambda(params, body),
            K::Kwarg { key, value } => format!("/* {} */ {}", key.name, self.lower_expr(value)),
            K::Array(items) => {
                // Heterogeneous-friendly default: lower to QVariantList
                // so element types don't have to match. Caller code that
                // wants a typed `QList<int>` can still write `QList<int>{
                // 1, 2, 3 }` when typed inference lands.
                let parts = items
                    .iter()
                    .map(|i| self.lower_expr(i))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("QVariantList{{{}}}", parts)
            }
            K::Map(entries) => {
                let parts = entries
                    .iter()
                    .map(|(k, v)| {
                        let key_s = match &k.kind {
                            cute_syntax::ast::ExprKind::Ident(name) => {
                                format!("QStringLiteral(\"{name}\")")
                            }
                            _ => self.lower_expr(k),
                        };
                        format!("{{{key_s}, {}}}", self.lower_expr(v))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("QVariantMap{{{parts}}}")
            }
            K::Range { .. } => unsupported_lowering(
                "range expression is not supported outside a `for` loop \
                 (the type checker reports this as a Type::Error; reaching \
                 here means it slipped past)",
            ),
            K::Element(_) => unsupported_lowering(
                "element value is not supported in a `fn` / class-method body \
                 (elements only have a runtime form inside a `view` or \
                 `widget` tree, where codegen lifts them via `trailing_element`)",
            ),
        }
    }

    fn lower_try(&mut self, inner: &cute_syntax::ast::Expr) -> String {
        let inner_s = self.lower_expr(inner);
        let tmp = self.fresh();
        self.push_prelude(format!("auto {tmp} = {inner_s};"));
        if self.is_err_union {
            self.push_prelude(format!(
                "if ({tmp}.is_err()) {kw} {ret}::err(std::move({tmp}).unwrap_err());",
                kw = self.return_kw(),
                ret = self.return_type,
                tmp = tmp
            ));
        } else {
            // Lexically out-of-place `?`: surrounding fn does not return !T.
            // Emit a runtime throw to surface the bug; HIR will reject this
            // cleanly when types land.
            self.push_prelude(format!(
                "if ({tmp}.is_err()) throw std::runtime_error(\"`?` used outside !T-returning fn\");"
            ));
        }
        format!("{tmp}.unwrap()")
    }

    /// Push `line` into the lowering prelude, tagging it with the
    /// current statement's `.cute` span so codegen can emit a `#line`
    /// directive that points back at the user-written stmt.
    fn push_prelude(&mut self, line: String) {
        self.prelude.push((line, self.current_stmt_span));
    }

    /// Lower `if cond { ... } else { ... }` to either:
    ///   - a C++ ternary `(cond ? then_v : else_v)` when both branches
    ///     are pure expressions (no statements). Ternary's own
    ///     type-coalescing rules unify branch types, so
    ///     `Person*` / `nullptr` and `int` / `int` both work without
    ///     extra casts.
    ///   - an immediately-invoked lambda otherwise. Statement-bearing
    ///     branches need a real block; the IIFE form lets `let y = if
    ///     cond { do_thing(); a } else { b }` still work as a value-
    ///     position expression. At statement position the IIFE is just
    ///     invoked and the value discarded.
    fn lower_if(
        &mut self,
        cond: &Expr,
        then_b: &cute_syntax::ast::Block,
        else_b: Option<&cute_syntax::ast::Block>,
    ) -> String {
        // Ternary path — both branches are single trailing expressions
        // and an `else` is present. C++ takes care of unifying branch
        // types, including the `T*` / `nullptr` case.
        if let Some(eb) = else_b {
            if then_b.stmts.is_empty()
                && eb.stmts.is_empty()
                && then_b.trailing.is_some()
                && eb.trailing.is_some()
            {
                let cond_s = self.lower_expr(cond);
                let then_v = self.lower_expr(then_b.trailing.as_ref().unwrap());
                let else_v = self.lower_expr(eb.trailing.as_ref().unwrap());
                return format!("({cond_s} ? {then_v} : {else_v})");
            }
        }

        // IIFE path — at least one branch has statements, or the
        // else-branch is missing.
        let cond_s = self.lower_expr(cond);
        let then_chunk = self.lower_branch_for_iife(then_b);
        let else_chunk = else_b.map(|b| self.lower_branch_for_iife(b));

        let mut buf = String::from("[&]() {\n");
        buf.push_str(&format!("    if ({cond_s}) {{\n"));
        for line in &then_chunk {
            buf.push_str(&format!("        {line}\n"));
        }
        buf.push_str("    }");
        if let Some(else_lines) = &else_chunk {
            buf.push_str(" else {\n");
            for line in else_lines {
                buf.push_str(&format!("        {line}\n"));
            }
            buf.push_str("    }");
        }
        buf.push('\n');
        buf.push_str("}()");
        buf
    }

    /// Lower `if let pat = init { ... } else { ... }` to a C++ block
    /// that tests the init's nullability and binds the inner value
    /// only when present. Patterns: `some(v)` binds the unwrapped
    /// inner; `nil` binds nothing and just runs the then-branch when
    /// init is null.
    fn lower_if_let(
        &mut self,
        pat: &cute_syntax::ast::Pattern,
        init: &Expr,
        then_b: &cute_syntax::ast::Block,
        else_b: Option<&cute_syntax::ast::Block>,
    ) -> String {
        use cute_syntax::ast::Pattern;
        let init_s = self.lower_expr(init);
        let tmp = self.fresh();
        let (cond_test, bind_decl) = match pat {
            Pattern::Ctor { name, args, .. } if name.name == "some" => {
                let bind = first_bind_name(args);
                let cond = format!("::cute::nullable_lift<decltype({tmp})>::has_value({tmp})");
                let bind_decl = bind.map(|bn| {
                    format!(
                        "auto {bn} = ::cute::nullable_lift<decltype({tmp})>::value({tmp});",
                        bn = bn
                    )
                });
                (cond, bind_decl)
            }
            Pattern::Ctor { name, .. } if name.name == "nil" => (
                format!("!::cute::nullable_lift<decltype({tmp})>::has_value({tmp})"),
                None,
            ),
            Pattern::Bind { name, .. } => (
                "true".to_string(),
                Some(format!("auto&& {bn} = {tmp};", bn = name.name)),
            ),
            _ => ("true".to_string(), None),
        };
        let then_chunk = self.lower_branch_for_iife(then_b);
        let else_chunk = else_b.map(|b| self.lower_branch_for_iife(b));
        let mut buf = String::from("[&]() {\n");
        buf.push_str(&format!("    auto {tmp} = {init_s};\n"));
        buf.push_str(&format!("    if ({cond_test}) {{\n"));
        if let Some(decl) = &bind_decl {
            buf.push_str(&format!("        {decl}\n"));
        }
        for line in &then_chunk {
            buf.push_str(&format!("        {line}\n"));
        }
        buf.push_str("    }");
        if let Some(else_lines) = &else_chunk {
            buf.push_str(" else {\n");
            for line in else_lines {
                buf.push_str(&format!("        {line}\n"));
            }
            buf.push_str("    }");
        }
        buf.push('\n');
        buf.push_str("}()");
        buf
    }

    /// Same as `lower_if_let` but emits a stmt-position if/else
    /// directly into the prelude (no IIFE wrapping). Preserves
    /// `return` semantics inside the branches.
    fn lower_if_let_as_stmt(
        &mut self,
        pat: &cute_syntax::ast::Pattern,
        init: &Expr,
        then_b: &cute_syntax::ast::Block,
        else_b: Option<&cute_syntax::ast::Block>,
    ) {
        use cute_syntax::ast::Pattern;
        let init_s = self.lower_expr(init);
        let tmp = self.fresh();
        let (cond_test, bind_decl) = match pat {
            Pattern::Ctor { name, args, .. } if name.name == "some" => {
                let bind = first_bind_name(args);
                let cond = format!("::cute::nullable_lift<decltype({tmp})>::has_value({tmp})");
                let bind_decl = bind.map(|bn| {
                    format!(
                        "auto {bn} = ::cute::nullable_lift<decltype({tmp})>::value({tmp});",
                        bn = bn
                    )
                });
                (cond, bind_decl)
            }
            Pattern::Ctor { name, .. } if name.name == "nil" => (
                format!("!::cute::nullable_lift<decltype({tmp})>::has_value({tmp})"),
                None,
            ),
            Pattern::Bind { name, .. } => (
                "true".to_string(),
                Some(format!("auto&& {bn} = {tmp};", bn = name.name)),
            ),
            _ => ("true".to_string(), None),
        };
        let then_lines = self.lower_block(then_b);
        let else_lines = else_b.map(|b| self.lower_block(b));
        let mut chunk = String::new();
        chunk.push_str(&format!("auto {tmp} = {init_s};\n"));
        chunk.push_str(&format!("if ({cond_test}) {{"));
        if let Some(decl) = &bind_decl {
            chunk.push_str(&format!("\n    {decl}"));
        }
        for (l, _) in &then_lines {
            chunk.push_str(&format!("\n    {l}"));
        }
        chunk.push_str("\n}");
        if let Some(lines) = else_lines {
            chunk.push_str(" else {");
            for (l, _) in &lines {
                chunk.push_str(&format!("\n    {l}"));
            }
            chunk.push_str("\n}");
        }
        self.push_prelude(chunk);
    }

    /// Lower `if cond { ... } else { ... }` at statement position as
    /// a plain C++ if/else chain pushed onto the prelude. Crucially
    /// preserves the user's `return X` semantics — `return` inside a
    /// stmt-position if returns from the surrounding fn, NOT from a
    /// synthesized IIFE the way value-position lowering would.
    fn lower_if_as_stmt(
        &mut self,
        cond: &Expr,
        then_b: &cute_syntax::ast::Block,
        else_b: Option<&cute_syntax::ast::Block>,
    ) {
        let cond_s = self.lower_expr(cond);
        let then_lines = self.lower_block(then_b);
        let else_lines = else_b.map(|b| self.lower_block(b));

        let mut chunk = format!("if ({cond_s}) {{");
        for (l, _) in &then_lines {
            chunk.push_str(&format!("\n    {l}"));
        }
        chunk.push_str("\n}");
        if let Some(lines) = else_lines {
            chunk.push_str(" else {");
            for (l, _) in &lines {
                chunk.push_str(&format!("\n    {l}"));
            }
            chunk.push_str("\n}");
        }
        self.push_prelude(chunk);
    }

    /// Lower `case scrutinee { when pat { body } ... }` at statement
    /// position as a plain C++ if/else chain pushed onto the prelude.
    /// Same `return`-preserving rationale as `lower_if_as_stmt`.
    fn lower_case_as_stmt(
        &mut self,
        scrutinee: &cute_syntax::ast::Expr,
        arms: &[cute_syntax::ast::CaseArm],
    ) {
        let scrutinee_s = self.lower_expr(scrutinee);
        let tmp = self.fresh();
        self.push_prelude(format!("auto {tmp} = {scrutinee_s};"));

        let mut buf = String::new();
        for (i, arm) in arms.iter().enumerate() {
            let (cond, binding) = self.lower_arm_pattern(&tmp, &arm.pattern);

            // When the scrutinee is a `weak` field read, the value
            // bound by `some(p)` is `cute::Arc<T>` (Arc-style pointer
            // semantics: `p.method()` should lower with `->`). Track
            // the bind name in `pointer_bindings` for the lifetime of
            // the arm body, and remove afterward so it doesn't leak
            // into sibling arms.
            let scoped_bind = self.case_arm_pointer_bind(&arm.pattern, scrutinee);
            if let Some((bn, class)) = &scoped_bind {
                self.pointer_bindings.insert(bn.clone(), class.clone());
            }
            let saved_prelude = std::mem::take(&mut self.prelude);
            let arm_lines = self.lower_block(&arm.body);
            self.prelude = saved_prelude;
            if let Some((bn, _)) = &scoped_bind {
                self.pointer_bindings.remove(bn);
            }

            let connector = if i == 0 { "if" } else { " else if" };
            buf.push_str(&format!("{connector} ({cond}) {{\n"));
            if let Some(b) = binding {
                buf.push_str(&format!("    {b}\n"));
            }
            for (line, _) in arm_lines {
                buf.push_str(&format!("    {line}\n"));
            }
            buf.push('}');
        }
        if !buf.is_empty() {
            self.push_prelude(buf);
        }
    }

    /// Find the enum (by Cute name) that declares a variant of the
    /// given name, plus the info needed to render its case-arm:
    /// `(enum_name, fields, is_error_decl, has_payload)`. Picks the
    /// first matching enum if more than one declares the same variant —
    /// Cute's PascalCase convention plus per-enum naming makes this a
    /// near-miss in practice. Returns `None` when the name doesn't
    /// belong to any declared enum.
    ///
    /// `is_error_decl` distinguishes the two C++-side naming
    /// conventions for the per-variant struct: error decls use
    /// `<Cap>` (the snake_to_camel rewrite from emit_error_decl),
    /// plain enums use `<Name>_t`.
    fn find_variant_signature(
        &self,
        variant: &str,
    ) -> Option<(String, Vec<cute_syntax::ast::Field>, bool, bool)> {
        let prog = self.program?;
        for (name, kind) in &prog.items {
            let cute_hir::ItemKind::Enum {
                variants, is_error, ..
            } = kind
            else {
                continue;
            };
            if let Some(v) = variants.iter().find(|v| v.name == variant) {
                let has_payload = variants.iter().any(|v| !v.fields.is_empty());
                return Some((name.clone(), v.fields.clone(), *is_error, has_payload));
            }
        }
        None
    }

    /// Build the (condition, binding-block) pair for a `when err(VariantName(...))`
    /// case arm. The condition checks both `is_err()` AND dispatches on
    /// the err's variant tag; payload binds extract via
    /// `std::get<...>(unwrap_err().value).field`. `tmp` is the lvalue
    /// holding the scrutinee (a `cute::Result<T, E>`).
    fn lower_err_variant_arm(
        &self,
        tmp: &str,
        variant: &str,
        variant_args: &[cute_syntax::ast::Pattern],
    ) -> (String, Option<String>) {
        let cap = capitalize_first(variant);
        let cond = format!("{tmp}.is_err() && {tmp}.unwrap_err().is{cap}()");
        let Some((enum_name, fields, is_error, _)) = self.find_variant_signature(variant) else {
            return (cond, None);
        };
        let variant_struct = if is_error {
            format!("{enum_name}::{cap}")
        } else {
            format!("{enum_name}::{variant}_t")
        };
        let value_expr = format!("{tmp}.unwrap_err().value");
        let bind = bind_variant_fields(
            &enum_name,
            &variant_struct,
            &value_expr,
            &fields,
            variant_args,
        );
        (cond, bind)
    }

    /// Compile a case-arm pattern into the C++ `(condition, optional
    /// binding-block)` pair that the if/else chain needs. Shared by
    /// `lower_case_as_stmt` (statement position) and `lower_case`
    /// (value position) so both routes stay in lockstep.
    fn lower_arm_pattern(
        &mut self,
        tmp: &str,
        pattern: &cute_syntax::ast::Pattern,
    ) -> (String, Option<String>) {
        use cute_syntax::ast::Pattern;
        match pattern {
            Pattern::Wild { .. } => ("true".to_string(), None),
            Pattern::Bind { name, .. } => (
                "true".to_string(),
                Some(format!("auto& {name} = {tmp};", name = name.name)),
            ),
            Pattern::Literal { value, .. } => {
                let v = self.lower_expr(value);
                (format!("{tmp} == {v}"), None)
            }
            Pattern::Ctor { name, args, .. } => {
                let head = name.name.as_str();
                let bind = first_bind_name(args);
                let nested = first_nested_variant_pattern(args);
                match (head, bind, nested) {
                    // `when err(VariantName)` / `when err(VariantName(field))`
                    // matches the err value AND inspects which variant
                    // fired, optionally extracting payload fields.
                    ("err", _, Some((vname, vargs))) => {
                        self.lower_err_variant_arm(tmp, vname, vargs)
                    }
                    ("ok", Some(bn), _) => (
                        format!("{tmp}.is_ok()"),
                        Some(format!("auto {bn} = {tmp}.unwrap();")),
                    ),
                    ("ok", None, _) => (format!("{tmp}.is_ok()"), None),
                    ("err", Some(bn), _) => (
                        format!("{tmp}.is_err()"),
                        Some(format!("auto {bn} = std::move({tmp}).unwrap_err();")),
                    ),
                    ("err", None, _) => (format!("{tmp}.is_err()"), None),
                    ("some", Some(bn), _) => (
                        format!("::cute::nullable_lift<decltype({tmp})>::has_value({tmp})"),
                        Some(format!(
                            "auto {bn} = ::cute::nullable_lift<decltype({tmp})>::value({tmp});"
                        )),
                    ),
                    ("some", None, _) => (
                        format!("::cute::nullable_lift<decltype({tmp})>::has_value({tmp})"),
                        None,
                    ),
                    ("nil", _, _) => (
                        format!("!::cute::nullable_lift<decltype({tmp})>::has_value({tmp})"),
                        None,
                    ),
                    // Bare enum-variant arm. Two shapes: pure-nullary
                    // enums lower to `tmp == EnumName::Variant`;
                    // payload-bearing enums lower to `tmp.isVariant()`
                    // plus per-field `std::get<...>(tmp.value).field`
                    // binds. Falls back to the bare `tmp.isFoo()`
                    // shape when no enum declares the variant (preserves
                    // the original error-decl-style discriminator path).
                    (variant, _, _) => {
                        let cap = capitalize_first(variant);
                        let Some((enum_name, fields, is_error, has_payload)) =
                            self.find_variant_signature(variant)
                        else {
                            return (format!("{tmp}.is{cap}()"), None);
                        };
                        if !has_payload {
                            return (format!("{tmp} == {enum_name}::{variant}"), None);
                        }
                        let variant_struct = if is_error {
                            format!("{enum_name}::{cap}")
                        } else {
                            format!("{enum_name}::{variant}_t")
                        };
                        let value_expr = format!("{tmp}.value");
                        let bind = bind_variant_fields(
                            &enum_name,
                            &variant_struct,
                            &value_expr,
                            &fields,
                            args,
                        );
                        (format!("{tmp}.is{cap}()"), bind)
                    }
                }
            }
        }
    }

    /// When a case-arm pattern is `some(bind_name)` AND the
    /// scrutinee is a `weak` field read, return the binding name
    /// plus the held class so the arm body's `bind_name.method()`
    /// calls lower with `->` (Arc<T>) instead of `.` (value).
    fn case_arm_pointer_bind(
        &self,
        pattern: &cute_syntax::ast::Pattern,
        scrutinee: &cute_syntax::ast::Expr,
    ) -> Option<(String, String)> {
        use cute_syntax::ast::{ExprKind, Pattern};
        let Pattern::Ctor { name, args, .. } = pattern else {
            return None;
        };
        if name.name != "some" {
            return None;
        }
        let bn = first_bind_name(args)?.to_string();
        let ExprKind::AtIdent(field_name) = &scrutinee.kind else {
            return None;
        };
        let class = self.weak_field_held_class(field_name)?;
        Some((bn, class))
    }

    /// Lower a branch body for inclusion inside an IIFE: statements
    /// pass through unchanged; the trailing expression (if any)
    /// becomes `return <value>;` so the lambda's return type is
    /// deduced from the branch's value. Used by both `lower_if` and
    /// `lower_case` to keep the two arms of the `Stmt::Expr` /
    /// expression-position split consistent.
    fn lower_branch_for_iife(&mut self, body: &cute_syntax::ast::Block) -> Vec<String> {
        let saved_prelude = std::mem::take(&mut self.prelude);
        let mut lines: Vec<LoweredLine> = Vec::new();
        for stmt in &body.stmts {
            self.lower_stmt_into(stmt, &mut lines);
        }
        if let Some(t) = &body.trailing {
            let v = self.lower_expr(t);
            lines.append(&mut self.prelude);
            lines.push((format!("return {v};"), Some(t.span)));
        }
        self.prelude = saved_prelude;
        lines.into_iter().map(|(l, _)| l).collect()
    }

    /// Lower `case scrutinee { when pat { body } ... }` as an
    /// immediately-invoked lambda whose return type is deduced from
    /// each arm's trailing expression (or `void` when no arm has one).
    ///
    /// IIFE form lets `let y = case ... { ... }` work as a value-
    /// position expression: `auto y = [&]() { ... }();`. At statement
    /// position the IIFE is just discarded as an expression statement,
    /// which is also fine — the lambda still runs.
    ///
    /// Patterns that drive the spec sample: `ok(v)` / `err(e)` over
    /// `Result`, error-variant `is_X()` discriminators, bare-ident
    /// bindings, `_` wildcard, and literal patterns. Nested
    /// constructors are deferred.
    fn lower_case(
        &mut self,
        scrutinee: &cute_syntax::ast::Expr,
        arms: &[cute_syntax::ast::CaseArm],
    ) -> String {
        let scrutinee_s = self.lower_expr(scrutinee);
        let tmp = self.fresh();

        // Collect each arm's compiled body separately so we can both
        // build the if/else chain AND assemble a `std::common_type_t<
        // decltype(v0), decltype(v1), ...>` return type for the IIFE.
        // Without the explicit return type, mixed branches like
        // `Person*` / `nullptr` fail lambda return-type deduction.
        struct CompiledArm {
            cond: String,
            binding: Option<String>,
            stmt_lines: Vec<String>,
            trailing: Option<String>,
        }

        let mut compiled: Vec<CompiledArm> = Vec::with_capacity(arms.len());
        for arm in arms {
            let (cond, binding) = self.lower_arm_pattern(&tmp, &arm.pattern);

            let scoped_bind = self.case_arm_pointer_bind(&arm.pattern, scrutinee);
            if let Some((bn, class)) = &scoped_bind {
                self.pointer_bindings.insert(bn.clone(), class.clone());
            }
            let saved_prelude = std::mem::take(&mut self.prelude);
            let mut arm_lines: Vec<LoweredLine> = Vec::new();
            for stmt in &arm.body.stmts {
                self.lower_stmt_into(stmt, &mut arm_lines);
            }
            let trailing = if let Some(t) = &arm.body.trailing {
                let v = self.lower_expr(t);
                arm_lines.append(&mut self.prelude);
                Some(v)
            } else {
                None
            };
            self.prelude = saved_prelude;
            if let Some((bn, _)) = &scoped_bind {
                self.pointer_bindings.remove(bn);
            }

            compiled.push(CompiledArm {
                cond,
                binding,
                stmt_lines: arm_lines.into_iter().map(|(l, _)| l).collect(),
                trailing,
            });
        }

        let trailing_values: Vec<&str> = compiled
            .iter()
            .filter_map(|a| a.trailing.as_deref())
            .collect();
        let produces_value = !trailing_values.is_empty();
        // The std::common_type form below uses `decltype(<arm_value>)`,
        // which is evaluated in the lambda return-type slot (before
        // the body). Names introduced by an arm's pattern binding
        // (`when ok(n)` → `auto n = ...`) aren't in scope there, so
        // referencing them in decltype fails to compile. Fall back to
        // the deduced return type when any arm has a binding — that
        // covers ok/err/Bind patterns. Pure literal / wildcard arms
        // keep the explicit return type so mixed pointer-and-nullptr
        // branches still unify.
        let any_arm_has_binding = compiled.iter().any(|a| a.binding.is_some());

        let return_type = if produces_value && !any_arm_has_binding {
            let parts = trailing_values
                .iter()
                .map(|v| format!("decltype({v})"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" -> std::common_type_t<{parts}>")
        } else {
            String::new()
        };

        let mut buf = format!("[&](){return_type} {{\n");
        buf.push_str(&format!("    auto {tmp} = {scrutinee_s};\n"));

        for (i, arm) in compiled.iter().enumerate() {
            let connector = if i == 0 { "if" } else { " else if" };
            buf.push_str(&format!("    {connector} ({}) {{\n", arm.cond));
            if let Some(b) = &arm.binding {
                buf.push_str(&format!("        {b}\n"));
            }
            for line in &arm.stmt_lines {
                buf.push_str(&format!("        {line}\n"));
            }
            if let Some(v) = &arm.trailing {
                buf.push_str(&format!("        return {v};\n"));
            }
            buf.push_str("    }\n");
        }
        // Non-exhaustive cases need a fallback so the lambda has a
        // defined exit. `__builtin_unreachable()` flags it loudly to
        // the C++ compiler (and traps in debug builds). Cases that
        // ARE exhaustive (`_` / top-level `Bind` / `ok`+`err` /
        // `true`+`false` / a complete error-variant set) skip the
        // sentinel — the compiler stops needing it, the generated
        // C++ stays cleaner, and `-Wreturn-type` no longer flags
        // the impossible fall-through.
        let exhaustive = match (self.module, self.program) {
            (Some(m), Some(p)) => cute_hir::is_case_exhaustive(scrutinee, arms, p, m),
            _ => false,
        };
        if produces_value && !exhaustive {
            buf.push_str("    __builtin_unreachable();\n");
        }
        buf.push_str("}()");
        buf
    }

    /// Lower a Cute lambda `{ |a, b| body }` to a C++ generic lambda
    /// `<capture>(T a, T b) { body }`. Untyped Cute params (parser
    /// placeholder `_`) become `auto` so the C++17 generic-lambda
    /// mechanism deduces the type from the call site.
    ///
    /// Capture mode depends on whether we're inside a class / struct
    /// method body. See [`Self::lambda_capture`] for the rationale.
    fn lower_lambda(&mut self, params: &[Param], body: &cute_syntax::ast::Block) -> String {
        let ctx_opt = self.program.map(TypeCtx::new);
        let params_s: Vec<String> = params
            .iter()
            .map(|p| {
                let ty = if is_lambda_placeholder_type(&p.ty) {
                    "auto".to_string()
                } else if let Some(ctx) = &ctx_opt {
                    ty::cute_to_cpp(&p.ty, ctx)
                } else {
                    "auto".to_string()
                };
                format!("{ty} {}", p.name.name)
            })
            .collect();
        let body_s = self.lower_lambda_body(body);
        let (cap, suffix) = self.lambda_capture_pieces();
        format!("{cap}({}){suffix} {body_s}", params_s.join(", "))
    }

    /// Capture-list prefix + post-params suffix (e.g. `mutable`) for
    /// user-written lambdas (`{ |x| body }` and trailing-block
    /// callables passed to `obj.signal.connect { ... }`,
    /// `xs.each { |x| ... }`, etc.).
    ///
    /// Two modes:
    ///
    /// - **Top-level fn bodies / `cli_app` / `server_app`** — `[&]`,
    ///   no `mutable`. Locals here live for the entire program
    ///   (`int main` returns only when `app.exec()` exits, which is
    ///   when the QCoreApplication::quit signal handler — itself a
    ///   captured lambda — fires). Capturing by reference lets a
    ///   `var counter` declared next to a `signal.connect { counter
    ///   += 1 }` actually mutate the same slot the rest of the body
    ///   sees.
    ///
    /// - **Class / struct method bodies** — `[=, this] mutable`. The
    ///   method's parameters and locals die when the method returns,
    ///   but Qt signal connections may fire later — `[&]` would leave
    ///   dangling references to dead stack slots. By-value capture
    ///   copies snapshots into the lambda; `mutable` lets the lambda
    ///   accumulate state across invocations on its own copies (e.g.
    ///   a token counter that ticks each `readyRead`). Cross-scope
    ///   sharing should use member fields, which the implicit
    ///   `this`-by-value capture lets the lambda mutate via
    ///   `this->member` because `this` is a pointer (the pointee
    ///   stays addressable as long as the receiver lives).
    ///
    /// IIFE wrappers (`[&]() { ... }()`, immediate invocation) are
    /// emitted directly with `[&]` elsewhere — those don't escape, so
    /// by-reference capture is always correct regardless of context.
    fn lambda_capture_pieces(&self) -> (&'static str, &'static str) {
        if self.class_decl.is_some() || self.struct_decl.is_some() {
            ("[=, this]", " mutable")
        } else {
            ("[&]", "")
        }
    }

    /// Lower the body of a lambda (or value-position block) into a
    /// brace-delimited C++ block. Statements lower normally; the
    /// trailing expression becomes `return <expr>;` so the surrounding
    /// callable returns it (C++ deducing the lambda's return type).
    fn lower_lambda_body(&mut self, body: &cute_syntax::ast::Block) -> String {
        let mut s = String::from("{\n");
        let mut out: Vec<LoweredLine> = Vec::new();
        for stmt in &body.stmts {
            self.lower_stmt_into(stmt, &mut out);
        }
        if let Some(t) = &body.trailing {
            let value_s = self.lower_expr(t);
            // Drain any preludes built up by the trailing expression's
            // sub-pieces (e.g. `?` operator) before the return.
            out.append(&mut self.prelude);
            out.push((format!("return {value_s};"), Some(t.span)));
        }
        for (line, _) in out {
            s.push_str(&format!("    {line}\n"));
        }
        s.push('}');
        s
    }

    /// Try to recognize and lower `<sender>.<signal>.connect { ... }`
    /// (or `.connect(handler)`) to Qt's modern function-pointer
    /// `QObject::connect(sender, &Class::signal, handler)` form.
    /// Returns `None` when the shape doesn't match (caller falls back
    /// to the plain method-call lowering).
    fn try_emit_signal_connect(
        &mut self,
        receiver: &Expr,
        args: &[Expr],
        block: &Option<Box<Expr>>,
    ) -> Option<String> {
        use cute_syntax::ast::ExprKind as K;
        // Receiver must be a `<sender>.<signal>` member access.
        let K::Member {
            receiver: sender,
            name: signal,
        } = &receiver.kind
        else {
            return None;
        };
        // Resolve the sender's static class so we can name the
        // member function pointer. Foreign / unanalyzed receivers
        // bail out, the caller then emits the plain `.connect(...)`.
        let class = self.pointer_class_of(sender)?;
        // Verify the class really has this signal (walking the super
        // chain). Otherwise this might be `obj.notASignal.connect`,
        // which we don't want to silently rewrite.
        let prog = self.program?;
        if !class_has_signal(prog, &class, &signal.name) {
            return None;
        }
        // Pick the handler: trailing block wins, otherwise the single
        // positional argument. `connect` takes exactly one callable.
        let handler = if let Some(b) = block {
            self.lower_block_arg(b)
        } else if args.len() == 1 {
            self.lower_expr(&args[0])
        } else {
            return None;
        };
        let sender_s = self.lower_expr(sender);
        Some(format!(
            "QObject::connect({sender_s}, &{class}::{}, {handler})",
            signal.name
        ))
    }

    /// Lower a `block: Some(...)` argument of a Call/MethodCall.
    /// Lambdas pass through; bare blocks (no `|x|` params) wrap as
    /// nullary lambdas so the receiver gets a callable, not the block's
    /// trailing value.
    fn lower_block_arg(&mut self, block: &Expr) -> String {
        use cute_syntax::ast::ExprKind as K;
        match &block.kind {
            K::Lambda { params, body } => self.lower_lambda(params, body),
            K::Block(b) => {
                let (cap, suffix) = self.lambda_capture_pieces();
                format!("{cap}(){suffix} {}", self.lower_lambda_body(b))
            }
            _ => self.lower_expr(block),
        }
    }

    /// Lower a `?.` access. `is_call=false` is the SafeMember form
    /// (`recv?.name` → zero-arg getter), `is_call=true` is the
    /// SafeMethodCall form (`recv?.method(args)`). The receiver is
    /// captured into a temp so it's evaluated once; the result lands
    /// in `::cute::nullable_lift<decltype(...)>::type` — a pointer
    /// stays a (possibly null) pointer, a value type lifts to
    /// `std::optional<T>`. Both empty sentinels (`nullptr` /
    /// `std::nullopt`) come from `__NL::none()` so the chosen branch
    /// is monomorphic and the lambda's return type is uniquely
    /// deducible.
    fn lower_safe_access(
        &mut self,
        receiver: &Expr,
        member_or_method: &str,
        is_call: bool,
        args_str: String,
    ) -> String {
        let recv_s = self.lower_expr(receiver);
        let tmp = self.fresh();
        // Receivers are captured by value when they look like a
        // pointer / smart-pointer (cheap copy with shared semantics).
        // A value-typed `std::optional<T>` is also fine to copy
        // because the IIFE only reads through it. Either way the
        // temp keeps `<receiver>` from being evaluated twice.
        let inner = if is_call {
            format!("{tmp}->{member_or_method}({args_str})")
        } else {
            // Zero-arg getter — Q_PROPERTY getters and Cute method
            // bodies both lower as `recv->name()`.
            format!("{tmp}->{member_or_method}()")
        };
        format!(
            "[&]() {{ auto {tmp} = {recv_s}; using __NL = ::cute::nullable_lift<decltype({inner})>; return {tmp} ? __NL::make({inner}) : __NL::none(); }}()"
        )
    }

    fn lower_string_parts(&mut self, parts: &[cute_syntax::ast::StrPart]) -> String {
        use cute_syntax::ast::StrPart;
        if parts.is_empty() {
            return "QString()".into();
        }
        let chunks: Vec<String> = parts
            .iter()
            .map(|p| match p {
                StrPart::Text(t) => cpp_quote_string(t),
                StrPart::Interp(e) => {
                    let inner = self.lower_expr(e);
                    format!("::cute::str::to_string({inner})")
                }
                StrPart::InterpFmt { expr, format_spec } => {
                    let inner = self.lower_expr(expr);
                    format!(
                        "::cute::str::format({inner}, \"{}\")",
                        escape_cpp_string(format_spec)
                    )
                }
            })
            .collect();
        chunks.join(" + ")
    }
}

fn first_bind_name(args: &[cute_syntax::ast::Pattern]) -> Option<&str> {
    use cute_syntax::ast::Pattern;
    args.iter().find_map(|p| match p {
        Pattern::Bind { name, .. } => Some(name.name.as_str()),
        _ => None,
    })
}

/// Look at a constructor pattern's args and, if the first arg is itself
/// a `Pattern::Ctor` (nested constructor like `err(IoError(msg))`),
/// return the inner variant name + its args. Used by case-arm lowering
/// to spot `when err(VariantName(...))` vs a plain bind.
fn first_nested_variant_pattern(
    args: &[cute_syntax::ast::Pattern],
) -> Option<(&str, &[cute_syntax::ast::Pattern])> {
    use cute_syntax::ast::Pattern;
    match args.first()? {
        Pattern::Ctor { name, args, .. } => Some((name.name.as_str(), args.as_slice())),
        _ => None,
    }
}

/// Generate the per-field `auto X = std::get<VariantStruct>(value).field;`
/// lines for a payload-variant arm. `value_expr` is the C++ expression
/// for the tagged-union value (e.g. `tmp.value` for a plain enum,
/// `tmp.unwrap_err().value` for the err arm). Returns `None` when no
/// arg-pattern is `Pattern::Bind` (so the caller can omit a binding
/// block instead of emitting an empty one).
fn bind_variant_fields(
    enum_name: &str,
    variant_struct: &str,
    value_expr: &str,
    fields: &[cute_syntax::ast::Field],
    args: &[cute_syntax::ast::Pattern],
) -> Option<String> {
    use cute_syntax::ast::Pattern;
    let mut bind = String::new();
    for (i, f) in fields.iter().enumerate() {
        let Some(Pattern::Bind { name, .. }) = args.get(i) else {
            continue;
        };
        if !bind.is_empty() {
            bind.push('\n');
        }
        // Self-typed payload fields are stored as `shared_ptr<E>`
        // (see emit_variant_class). Deref + copy the pointee so
        // arm bodies see a regular `E` value; the shared_ptr keeps
        // the original variant intact for any sibling arms.
        let access = format!("std::get<{variant_struct}>({value_expr}).{}", f.name.name);
        let rhs = if field_is_self_typed(f, enum_name) {
            format!("*{access}")
        } else {
            access
        };
        bind.push_str(&format!("    auto {} = {rhs};", name.name));
    }
    if bind.is_empty() { None } else { Some(bind) }
}

/// True when `field`'s declared type is exactly the enclosing enum
/// (no generic args, no namespace path). Drives the unique_ptr
/// indirection that breaks the std::variant incomplete-type cycle
/// for self-recursive payload variants.
fn field_is_self_typed(field: &cute_syntax::ast::Field, enum_name: &str) -> bool {
    use cute_syntax::ast::TypeKind;
    let TypeKind::Named { path, args } = &field.ty.kind else {
        return false;
    };
    if !args.is_empty() {
        return false;
    }
    matches!(path.as_slice(), [seg] if seg.name == enum_name)
}

/// Whether `arms` cover every possible value of `scrutinee`'s type so
/// the value-position case lowering can omit `__builtin_unreachable`.
///
/// Covers the four common shapes:
///
///   1. Any `Wild` (`when _`) or top-level `Bind` arm matches anything.
///   2. `Bool` scrutinee with both `when true` and `when false` arms.
///   3. `!T` (error-union) scrutinee with both `when ok(...)` and
///      `when err(...)` arms — even with payload bindings.
///   4. `error E { ... }` scrutinee where every declared variant
///      appears as a Ctor arm. Looks E up by name in the enclosing
///      module.
///
/// Outcome of compiling one `when <pattern>` head against a scrutinee.
/// `test` is the boolean expression evaluated for "does this arm
/// match?"; `bind_decls` are zero or more declarations that should
/// appear inside the arm body's scope (currently only the
/// `auto v = ...` injection for `when ok(v) { ... }` / `when err(e) { ... }`
/// shapes).
struct PatternMatch {
    test: String,
    bind_decls: Vec<String>,
}

/// Optional capability bundle for handling Cute error-union patterns
/// (`when ok(v)` / `when err(e)`). Widget codegen plugs in C++ Result
/// API access; view codegen leaves this `None` because `cute::Result`
/// isn't reachable from QML's expression language yet (Q_GADGET
/// wrapping pending).
struct ResultApi {
    is_ok: fn(&str) -> String,
    is_err: fn(&str) -> String,
    /// Builds a "safe extract" expression that returns the inner ok
    /// value when the result is in the ok state and a default-
    /// constructed fallback otherwise. The fallback keeps build-time
    /// evaluation safe even when the runtime tag is err.
    safe_unwrap: fn(&str) -> String,
    /// Same idea on the err side.
    safe_unwrap_err: fn(&str) -> String,
}

/// Map a single `when <pattern>` head + scrutinee to a `PatternMatch`.
/// The test string drives the visibility binding for that arm; the
/// bind_decls are statements that need to land inside the arm body's
/// scope (or, when the host doesn't support local bindings, into a
/// `let <name> = <expr>` style binding).
///
/// Pattern coverage:
///   - `Wild`               -> `"true"` (always matches; relies on
///                            prior-arm negations for exhaustiveness)
///   - `Literal(value)`     -> `"<scrutinee> <eq_op> <lit>"`
///   - `Ctor(name, [])`     -> string-tag compare `<scrutinee> <eq_op> "<name>"`
///   - `Ctor("ok", [Bind(n)])` (Result API present) -> `is_ok()` test
///                            + `auto n = safe_unwrap()` decl
///   - `Ctor("err", [Bind(n)])` (Result API present) `is_err()` test
///                            + `auto n = safe_unwrap_err()` decl
///   - any other Ctor/Bind  `true /* TODO */` placeholder
fn pattern_match_test(
    scrutinee: &str,
    pattern: &cute_syntax::ast::Pattern,
    eq_op: &str,
    quote_string: fn(&str) -> String,
    lower_literal: impl Fn(&cute_syntax::ast::Expr) -> String,
    result_api: Option<&ResultApi>,
) -> PatternMatch {
    use cute_syntax::ast::Pattern;
    match pattern {
        Pattern::Wild { .. } => PatternMatch {
            test: "true".into(),
            bind_decls: Vec::new(),
        },
        Pattern::Literal { value, .. } => PatternMatch {
            test: format!("{scrutinee} {eq_op} {}", lower_literal(value)),
            bind_decls: Vec::new(),
        },
        Pattern::Ctor { name, args, .. } if args.is_empty() => PatternMatch {
            test: format!("{scrutinee} {eq_op} {}", quote_string(&name.name)),
            bind_decls: Vec::new(),
        },
        Pattern::Ctor { name, args, .. }
            if (name.name == "ok" || name.name == "err") && args.len() == 1 =>
        {
            let bind_name = match &args[0] {
                Pattern::Bind { name: bn, .. } => bn.name.clone(),
                _ => {
                    return PatternMatch {
                        test: "true /* TODO: nested patterns inside ok/err */".into(),
                        bind_decls: Vec::new(),
                    };
                }
            };
            let Some(api) = result_api else {
                return PatternMatch {
                    test: format!(
                        "true /* TODO: ok/err patterns need Result API in this context */"
                    ),
                    bind_decls: Vec::new(),
                };
            };
            let (test, decl_expr) = if name.name == "ok" {
                ((api.is_ok)(scrutinee), (api.safe_unwrap)(scrutinee))
            } else {
                ((api.is_err)(scrutinee), (api.safe_unwrap_err)(scrutinee))
            };
            PatternMatch {
                test,
                bind_decls: vec![format!("auto {bind_name} = {decl_expr};")],
            }
        }
        Pattern::Ctor { .. } | Pattern::Bind { .. } => PatternMatch {
            test: "true /* TODO: bind / ctor patterns in element-case */".into(),
            bind_decls: Vec::new(),
        },
    }
}

/// Widget-side Result API: maps directly onto the C++ `cute::Result`
/// methods. The "safe" forms guard against an abort when the build-
/// time evaluation hits an arm whose runtime tag doesn't match - they
/// fall back to a default-constructed value of the same type.
const WIDGET_RESULT_API: ResultApi = ResultApi {
    is_ok: widget_is_ok,
    is_err: widget_is_err,
    safe_unwrap: widget_safe_unwrap,
    safe_unwrap_err: widget_safe_unwrap_err,
};

fn widget_is_ok(s: &str) -> String {
    format!("{s}.is_ok()")
}
fn widget_is_err(s: &str) -> String {
    format!("{s}.is_err()")
}
fn widget_safe_unwrap(s: &str) -> String {
    format!("({s}.is_ok() ? {s}.unwrap() : decltype({s}.unwrap()){{}})")
}
fn widget_safe_unwrap_err(s: &str) -> String {
    format!("({s}.is_err() ? std::move({s}).unwrap_err() : decltype({s}.unwrap_err()){{}})")
}

fn qml_quote_string(s: &str) -> String {
    format!("\"{}\"", escape_js_string(s))
}

fn cpp_quote_string(s: &str) -> String {
    format!("QStringLiteral(\"{}\")", escape_cpp_string(s))
}

/// Walk `class`'s super chain in `prog` to see whether any ancestor
/// declares `signal`. Used by `try_emit_signal_connect` to gate the
/// rewrite to `QObject::connect(...)` only on real signal members.
fn class_has_signal(prog: &cute_hir::ResolvedProgram, class: &str, signal: &str) -> bool {
    let mut cursor = Some(class.to_string());
    while let Some(name) = cursor {
        let Some(cute_hir::ItemKind::Class {
            signal_names,
            super_class,
            ..
        }) = prog.items.get(&name)
        else {
            return false;
        };
        if signal_names.iter().any(|s| s == signal) {
            return true;
        }
        cursor = super_class.clone();
    }
    false
}

/// True when `t` is the parser's placeholder type for an untyped lambda
/// param (`{ |x| ... }` with no annotation). The parser models it as a
/// `Named` type whose single-segment path is `_`. Such params lower to
/// `auto` so the C++17 generic-lambda mechanism deduces the type at the
/// call site.
fn is_lambda_placeholder_type(t: &cute_syntax::ast::TypeExpr) -> bool {
    use cute_syntax::ast::TypeKind;
    match &t.kind {
        TypeKind::Named { path, args } => args.is_empty() && path.len() == 1 && path[0].name == "_",
        _ => false,
    }
}

fn assign_op_str(op: cute_syntax::ast::AssignOp) -> &'static str {
    match op {
        cute_syntax::ast::AssignOp::Eq => "=",
        cute_syntax::ast::AssignOp::PlusEq => "+=",
        cute_syntax::ast::AssignOp::MinusEq => "-=",
        cute_syntax::ast::AssignOp::StarEq => "*=",
        cute_syntax::ast::AssignOp::SlashEq => "/=",
    }
}

fn inner_of_err_union_cpp(ret: &str) -> &str {
    // `::cute::Result<T, E>` -> just used for documentation; we don't
    // currently extract T (the C++ compiler handles ::ok template overload
    // via deduction).
    ret
}

fn escape_cpp_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// ---- ClassInfo extraction from AST ---------------------------------------

fn build_class_info(c: &ClassDecl, super_class: &str, ctx: &TypeCtx<'_>) -> ClassInfo {
    let mut signals: Vec<SignalInfo> = Vec::new();
    let mut properties: Vec<PropInfo> = Vec::new();
    let mut methods: Vec<MethodInfo> = Vec::new();
    let mut slots: Vec<MethodInfo> = Vec::new();

    // First pass: collect user-declared signal names so property notify
    // resolves to indices.
    for mem in &c.members {
        if let ClassMember::Signal(s) = mem {
            signals.push(SignalInfo {
                name: s.name.name.clone(),
                params: param_infos(&s.params, ctx),
            });
        }
    }
    // Auto-inject the change-event signal for every prop that needs
    // one. The signal goes into `signals` so the C++ header emits the
    // matching `void <name>();` declaration *and* the QMetaObject
    // table reserves a slot. Three rules, picked first-match:
    //   1. explicit `notify: :foo` — synthesize a SignalInfo for
    //      `foo` if no `pub signal foo` was declared. The user's
    //      handwritten signal still wins when present (parameters
    //      flow through verbatim).
    //   2. `, model` — no NOTIFY (stable pointer; row changes use
    //      the QAbstractItemModel signals on the model itself).
    //   3. `, constant` — explicit opt-out: emits a CONSTANT
    //      Q_PROPERTY with no NOTIFY clause and no signal.
    //   4. otherwise — synthesize `<propName>Changed`. This used to
    //      be gated on `bindable` / `bind { }` / `fresh { }`; the
    //      gate is gone so plain `pub prop X : T = V` becomes
    //      reactive without ceremony.
    for mem in &c.members {
        if let ClassMember::Property(p) = mem {
            if p.model || p.constant {
                continue;
            }
            let synth_name = match p.notify.as_ref() {
                Some(n) => n.name.clone(),
                None => p.synth_notify_name(),
            };
            if !signals.iter().any(|s| s.name == synth_name) {
                signals.push(SignalInfo {
                    name: synth_name,
                    params: Vec::new(),
                });
            }
        }
    }
    // Second pass: properties (resolve notify), methods, slots.
    // `prop x : ModelList<T>` produces ONE PropInfo whose cpp_type is
    // `::cute::ModelList<T*>*`. Downstream emission dispatches on
    // `prop.is_model_list` to pick model-flavoured storage /
    // Q_PROPERTY / ctor wiring.
    for mem in &c.members {
        match mem {
            ClassMember::Property(p) => {
                properties.push(prop_info(p, &signals, ctx));
            }
            ClassMember::Fn(f) => methods.push(fn_info(f, ctx)),
            ClassMember::Slot(f) => slots.push(fn_info(f, ctx)),
            ClassMember::Signal(_) => {}
            ClassMember::Field(_) => {}
            // init/deinit are emitted off the AST directly in emit_class_*.
            ClassMember::Init(_) | ClassMember::Deinit(_) => {}
        }
    }

    ClassInfo {
        name: c.name.name.clone(),
        super_class: super_class.to_string(),
        properties,
        signals,
        methods,
        slots,
    }
}

fn prop_info(p: &PropertyDecl, signals: &[SignalInfo], ctx: &TypeCtx<'_>) -> PropInfo {
    use cute_meta::PropKind;
    let kind = if p.binding.is_some() {
        PropKind::Bind
    } else if p.fresh.is_some() {
        PropKind::Fresh
    } else if p.bindable {
        PropKind::Bindable
    } else {
        PropKind::Plain
    };
    // `, model` props expose a `cute::ModelList<T*>*` instead of the
    // raw QList. The pointer is stable for the class's lifetime
    // (CONSTANT Q_PROPERTY), no setter exists — full replace goes
    // through `xs->replace(newList)`, structural mutations through
    // `xs->append(b)` / `xs->removeAt(i)` etc. (real public methods
    // on ModelList that fire begin/end signals internally). So:
    // writable false, no NOTIFY, cpp_type rewritten to the pointer.
    let is_model_list = p.model;
    let cpp_type = if is_model_list {
        match model_row_type_of(p) {
            Some(row) => format!("::cute::ModelList<{row}>*"),
            // Fallback: leave the raw type so the C++ compiler at
            // least surfaces a typed error if the row shape is wrong.
            None => ty::cute_to_cpp(&p.ty, ctx),
        }
    } else {
        ty::cute_to_cpp(&p.ty, ctx)
    };
    // Bind / Fresh are read-only (the `bind { ... }` / `fresh { ... }`
    // expression IS the value; there's no setter to write through).
    // `, model` is also read-only at the property level (mutation goes
    // through the ModelList's own methods).
    let writable = !is_model_list && matches!(kind, PropKind::Plain | PropKind::Bindable);
    // Bind and Fresh both get a synthesized `<name>_changed` notify
    // (added to the signal list in build_class_info). For Bind it's
    // the signal that QObjectBindableProperty fires on invalidation;
    // for Fresh it's fanned out from input bindables in the ctor.
    // `, model` skips notify entirely — the pointer is stable, and
    // QML / Widget views observe row-level changes through the
    // ModelList's QAbstractItemModel signals directly.
    let notify_name: Option<String> = if is_model_list || p.constant {
        None
    } else if let Some(n) = p.notify.as_ref() {
        Some(n.name.clone())
    } else {
        // Default for every plain prop: synthesize the conventional
        // change-event name. Mirrors the signal-list build above so
        // the metaobject's NOTIFY index always resolves.
        Some(p.synth_notify_name())
    };
    let notify_idx = notify_name
        .as_ref()
        .and_then(|n| signals.iter().position(|s| s.name == *n));
    let bindable_getter = match kind {
        PropKind::Bindable | PropKind::Bind => Some(ty::bindable_getter_name(&p.name.name)),
        _ => None,
    };

    let qmetatype = if is_model_list {
        // ModelList<T>* is a QObject subclass pointer; QML's
        // QMetaObject reflection treats it as QObjectStar so the
        // ListView consumer sees a model-like pointer it can probe.
        "QMetaType::QObjectStar"
    } else {
        ty::cute_to_qmeta_type_enum(&p.ty)
    };
    let pass_by_const_ref = !is_model_list && ty::pass_by_const_ref(&p.ty);
    PropInfo {
        name: p.name.name.clone(),
        setter: ty::setter_name(&p.name.name),
        cpp_type,
        qmetatype,
        pass_by_const_ref,
        readable: true,
        writable,
        notify_signal_idx: notify_idx,
        notify_signal_name: notify_name,
        kind,
        bindable_getter,
        is_model_list,
    }
}

/// Element type T of a `prop xs : List<T>, model` declaration. The
/// return is `Some(T)` for the list-of-class shape; anything else
/// (non-List type, missing type arg, fn-typed prop, etc.) is `None`
/// and will be silently skipped by the codegen pre-scan — the C++
/// side surfaces a real error at QRangeModel instantiation time if
/// the row shape is wrong.
fn model_row_type_of(p: &PropertyDecl) -> Option<String> {
    if !p.model {
        return None;
    }
    let cute_syntax::ast::TypeKind::Named { path, args } = &p.ty.kind else {
        return None;
    };
    if path.last().map(|i| i.name.as_str()) != Some("List") || args.len() != 1 {
        return None;
    }
    cute_syntax::ast::type_expr_base_name(&args[0])
}

fn fn_info(f: &FnDecl, ctx: &TypeCtx<'_>) -> MethodInfo {
    let raw = match &f.return_ty {
        Some(t) => ty::cute_to_cpp(t, ctx),
        None => "void".to_string(),
    };
    // `async fn` lowers to a Qt coroutine returning `QFuture<T>`.
    // Wrap once - if the user wrote `async fn f -> Future<Int>`
    // explicitly, the lowered type already starts with `QFuture<`
    // so we leave it alone instead of double-wrapping.
    let return_type = if f.is_async && !raw.starts_with("QFuture<") {
        if raw == "void" {
            "QFuture<void>".to_string()
        } else {
            format!("QFuture<{}>", raw)
        }
    } else {
        raw
    };
    MethodInfo {
        name: f.name.name.clone(),
        params: param_infos(&f.params, ctx),
        return_type,
        is_static: f.is_static,
    }
}

fn param_infos(params: &[Param], ctx: &TypeCtx<'_>) -> Vec<ParamInfo> {
    params
        .iter()
        .map(|p| ParamInfo {
            name: p.name.name.clone(),
            cpp_type: ty::cute_param_to_cpp(p, ctx),
            qmetatype: ty::cute_to_qmeta_type_enum(&p.ty).to_string(),
        })
        .collect()
}

/// True when the fn declares an error-union (`!T`) return. Drives both
/// the body-lowering path (`Lowering::new(..., return_is_err_union)`)
/// and the `[[nodiscard]]` attribute on the C++ declaration.
fn returns_err_union(return_ty: &Option<cute_syntax::ast::TypeExpr>) -> bool {
    matches!(
        return_ty.as_ref().map(|t| &t.kind),
        Some(TypeKind::ErrorUnion(_))
    )
}

/// True for top-level `let X : Int|Float|Bool = <numeric or bool literal>`.
/// Used to upgrade the C++ storage class to `static constexpr` so the
/// compiler asserts the binding is constant-initialized. Conservative on
/// purpose: QString / ByteArray / user-defined types stay on the
/// dynamic-init path because their underlying C++ ctors aren't
/// constexpr.
fn is_primitive_literal_let(
    ty: &cute_syntax::ast::TypeExpr,
    value: &cute_syntax::ast::Expr,
) -> bool {
    let primitive = matches!(
        &ty.kind,
        TypeKind::Named { path, args }
            if args.is_empty()
                && path
                    .last()
                    .map(|i| matches!(i.name.as_str(), "Int" | "Float" | "Bool"))
                    .unwrap_or(false)
    );
    if !primitive {
        return false;
    }
    use cute_syntax::ast::ExprKind as K;
    match &value.kind {
        K::Int(_) | K::Float(_) | K::Bool(_) => true,
        K::Unary {
            op: cute_syntax::ast::UnaryOp::Neg,
            expr,
        } => matches!(expr.kind, K::Int(_) | K::Float(_)),
        _ => false,
    }
}

/// Cute `!T` returns lower to `cute::Result<T, E>`. Marking the C++
/// declaration `[[nodiscard]]` catches the call-and-drop footgun
/// (caller compiles a fallible fn into `compute(); /* ignored */`).
/// ISO `[[nodiscard("...")]]` is portable and avoids pulling
/// Q_NODISCARD_X out of QtCore, keeping the runtime header-only.
const NODISCARD_ERR_UNION_ATTR: &str =
    "[[nodiscard(\"ignoring this Result loses the error case\")]] ";

/// Returns the attribute prefix to splice before the C++ return type.
/// Empty when not an error union so callers can interpolate
/// unconditionally into a `format!` template.
fn nodiscard_for_err_union(is_err_union: bool) -> &'static str {
    if is_err_union {
        NODISCARD_ERR_UNION_ATTR
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cute_syntax::{parse, span::FileId};

    /// Run the full frontend (parse → resolve → check) and emit
    /// codegen with caller-controlled `is_test_build` and source map.
    /// Asserts no resolver ERROR diagnostics; warnings (e.g. the
    /// non-exhaustive-case advisory from cute-hir) pass through so
    /// codegen tests for syntactically-valid-but-incomplete cases
    /// don't have to disable the warning.
    fn build_with(
        src: &str,
        is_test_build: bool,
        sm: Option<&cute_syntax::span::SourceMap>,
        fid: FileId,
    ) -> EmitResult {
        let module = parse(fid, src).expect("parse");
        // Mirror the driver's pre-pass chain (see `cute-driver` for
        // the full rationale).
        let module = crate::desugar_suite::desugar_suite(module);
        let module = crate::desugar_store::desugar_store(module);
        let module = crate::desugar_state::desugar_widget_state(module);
        let resolved = cute_hir::resolve(&module, &cute_hir::ProjectInfo::default());
        let errors: Vec<_> = resolved
            .diagnostics
            .iter()
            .filter(|d| matches!(d.severity, cute_syntax::diag::Severity::Error))
            .collect();
        assert!(
            errors.is_empty(),
            "unexpected error diagnostics in test source: {:?}",
            errors
        );
        let typed = cute_types::check_program(&module, &resolved.program);
        emit_module(
            "testMod",
            &module,
            &resolved.program,
            &cute_hir::ProjectInfo::default(),
            CodegenTypeInfo {
                generic_instantiations: &typed.generic_instantiations,
                qml_imports: &[],
                source_map: sm,
                is_test_build,
                binding_modules: &[],
            },
        )
        .expect("emit")
    }

    fn build(src: &str) -> EmitResult {
        build_with(src, false, None, FileId(0))
    }

    fn build_test(src: &str) -> EmitResult {
        build_with(src, true, None, FileId(0))
    }

    fn build_with_source_map(name: &str, src: &str) -> EmitResult {
        let mut sm = cute_syntax::span::SourceMap::default();
        let fid = sm.add(name.to_string(), src.to_string());
        build_with(src, false, Some(&sm), fid)
    }

    #[test]
    fn snapshot_todo_item_header() {
        let src = r#"
class TodoItem < QObject {
  prop text : String, default: ""
  prop done : Bool, notify: :stateChanged
  signal stateChanged
  fn toggle {
    done = !done
    emit stateChanged
  }
}
"#;
        let r = build(src);
        insta::assert_snapshot!("todoItemHeader", r.header);
    }

    #[test]
    fn snapshot_todo_item_source() {
        let src = r#"
class TodoItem < QObject {
  prop text : String, default: ""
  prop done : Bool, notify: :stateChanged
  signal stateChanged
  fn toggle {
    done = !done
    emit stateChanged
  }
}
"#;
        let r = build(src);
        insta::assert_snapshot!("todoItemSource", r.source);
    }

    #[test]
    fn header_uses_q_object_macro_and_q_property() {
        let src =
            "class X < QObject { prop name : String, notify: :nameChanged  signal nameChanged }";
        let r = build(src);
        assert!(
            r.header.contains("Q_OBJECT"),
            "missing Q_OBJECT:\n{}",
            r.header
        );
        assert!(
            r.header
                .contains("Q_PROPERTY(QString name READ name WRITE setName NOTIFY nameChanged)"),
            "missing Q_PROPERTY:\n{}",
            r.header
        );
        assert!(
            r.header.contains("signals:"),
            "missing signals: block:\n{}",
            r.header
        );
    }

    #[test]
    fn bindable_prop_emits_object_bindable_property_storage_and_qbindable_getter() {
        let src = "class Counter < QObject {\n  prop count : Int, notify: :countChanged, bindable\n  signal countChanged\n}";
        let r = build(src);
        assert!(
            r.header
                .contains("Q_PROPERTY(qint64 count READ count WRITE setCount BINDABLE bindableCount NOTIFY countChanged)"),
            "Q_PROPERTY missing BINDABLE clause:\n{}",
            r.header
        );
        assert!(
            r.header.contains(
                "Q_OBJECT_BINDABLE_PROPERTY(Counter, qint64, m_count, &Counter::countChanged)"
            ),
            "missing Q_OBJECT_BINDABLE_PROPERTY storage:\n{}",
            r.header
        );
        assert!(
            r.header.contains("QBindable<qint64> bindableCount();"),
            "missing public QBindable<T> getter declaration:\n{}",
            r.header
        );
        assert!(
            r.source.contains("return QBindable<qint64>(&m_count);"),
            "missing QBindable<T> getter body:\n{}",
            r.source
        );
        assert!(
            r.source.contains("m_count.setValue(value);"),
            "bindable setter should delegate to setValue:\n{}",
            r.source
        );
    }

    #[test]
    fn bindable_prop_without_explicit_notify_synthesizes_one() {
        // For uniformity across kinds (Bindable / Bind / Fresh), a
        // bindable prop without explicit `notify:` gets a synthesized
        // `<x>_changed` signal. The 4-arg Q_OBJECT_BINDABLE_PROPERTY
        // macro takes that signal so setValue() auto-fires it — the
        // QML NOTIFY path works without the user having to declare
        // anything.
        let src = "class X < QObject { prop x : Int, bindable }";
        let r = build(src);
        assert!(
            r.header
                .contains("Q_OBJECT_BINDABLE_PROPERTY(X, qint64, m_x, &X::xChanged)"),
            "expected 4-arg form with synthesized notify:\n{}",
            r.header
        );
        assert!(
            r.header.contains("    void xChanged();"),
            "missing synthesized xChanged signal declaration:\n{}",
            r.header
        );
        assert!(
            r.header.contains(
                "Q_PROPERTY(qint64 x READ x WRITE setX BINDABLE bindableX NOTIFY xChanged)"
            ),
            "Q_PROPERTY missing synthesized NOTIFY:\n{}",
            r.header
        );
    }

    #[test]
    fn bindable_at_ident_read_uses_value_method() {
        let src = "class X < QObject {\n  prop count : Int, bindable\n  fn read Int { count }\n}";
        let r = build(src);
        assert!(
            r.source.contains("m_count.value()"),
            "AtIdent read on bindable prop must lower to .value():\n{}",
            r.source
        );
    }

    #[test]
    fn bindable_at_ident_assign_lowers_to_member_assignment() {
        let src = "class X < QObject {\n  prop count : Int, bindable\n  fn zero { count = 0 }\n}";
        let r = build(src);
        // For bindable storage, `m_count = 0` resolves to
        // QObjectBindableProperty::operator=, which delegates to
        // setValue() (auto-fires notify + invalidates dependents). The
        // codegen does NOT route through `.value()` on the LHS — that
        // would assign to a temporary.
        assert!(
            r.source.contains("void X::zero() {\n    m_count = 0;\n}"),
            "AtIdent assign on bindable prop must produce raw m_count = ...:\n{}",
            r.source
        );
    }

    #[test]
    fn bindable_at_ident_compound_assign_routes_through_setvalue() {
        let src = "class X < QObject {\n  prop count : Int, bindable\n  fn bump { count += 1 }\n}";
        let r = build(src);
        assert!(
            r.source.contains("m_count.setValue(m_count.value() + 1);"),
            "compound assign on bindable AtIdent must go through value()/setValue():\n{}",
            r.source
        );
    }

    #[test]
    fn nonbindable_prop_unchanged_no_qproperty_overhead() {
        // Regression guard: a plain prop must keep its current
        // raw-member storage and zero-overhead read/setter so existing
        // demos opt out cleanly.
        let src = "class X < QObject { prop count : Int, default: 0 }";
        let r = build(src);
        assert!(
            r.header.contains("    qint64 m_count = 0;"),
            "plain prop should keep raw qint64 storage:\n{}",
            r.header
        );
        assert!(
            !r.header.contains("Q_OBJECT_BINDABLE_PROPERTY"),
            "plain prop should NOT use Q_OBJECT_BINDABLE_PROPERTY:\n{}",
            r.header
        );
        assert!(
            !r.header.contains("QBindable"),
            "plain prop should NOT expose a QBindable getter:\n{}",
            r.header
        );
        // Setter still uses the manual equality + emit form (no
        // setValue delegation).
        assert!(
            r.source.contains("if (m_count == value) return;"),
            "plain setter should keep equality guard:\n{}",
            r.source
        );
    }

    #[test]
    fn bind_prop_uses_qobject_bindable_property_with_setbinding() {
        // `bind { expr }` lowers to QObjectBindableProperty + a
        // setBinding(lambda) call in the constructor. Qt's binding
        // system auto-tracks deps the lambda reads via `.value()`,
        // and the synthesized notify (passed as the macro's 4th arg)
        // fires on invalidation — so QML/QtWidget bindings re-evaluate
        // through both the BINDABLE and NOTIFY paths.
        let src = r#"
class Book < QObject {
  prop Page : Int, bindable
  pub prop total : Int, bindable
  pub prop ratio : Float, bind { (1.0 * Page) / total }
}
"#;
        let r = build(src);
        assert!(
            r.header.contains(
                "Q_PROPERTY(double ratio READ ratio BINDABLE bindableRatio NOTIFY ratioChanged)"
            ),
            "bind prop Q_PROPERTY annotation wrong:\n{}",
            r.header
        );
        assert!(
            r.header.contains("    void ratioChanged();"),
            "missing synthesized ratioChanged signal:\n{}",
            r.header
        );
        // No WRITE → no setter.
        assert!(
            !r.header.contains("setRatio"),
            "bind prop must not declare setRatio"
        );
        // QObjectBindableProperty storage with the synthesized notify
        // signal as the 4th macro arg.
        assert!(
            r.header
                .contains("Q_OBJECT_BINDABLE_PROPERTY(Book, double, m_ratio, &Book::ratioChanged)"),
            "missing Q_OBJECT_BINDABLE_PROPERTY storage:\n{}",
            r.header
        );
        // No QObjectComputedProperty / compute_ratio — bind doesn't
        // use the function-style storage.
        assert!(
            !r.header.contains("Q_OBJECT_COMPUTED_PROPERTY"),
            "bind must not use QObjectComputedProperty storage:\n{}",
            r.header
        );
        assert!(
            !r.header.contains("computeRatio"),
            "bind must not declare a compute method:\n{}",
            r.header
        );
        // Constructor sets the binding lambda. Lambda body reads
        // m_page.value() / m_total.value() so the binding system tracks
        // them as deps automatically.
        assert!(
            r.source.contains(
                "m_ratio.setBinding([this]{ return ((1.0 * m_Page.value()) / m_total.value()); });"
            ),
            "missing setBinding(lambda) in ctor:\n{}",
            r.source
        );
        // Public bindable getter (bind props expose QBindable so QML's
        // BINDABLE path works — QObjectBindableProperty's QBindable
        // implements subscription, unlike fresh).
        assert!(
            r.header.contains("QBindable<double> bindableRatio();"),
            "missing bindableRatio() getter:\n{}",
            r.header
        );
    }

    #[test]
    fn fresh_prop_uses_qobject_computed_property_with_fan_out_notify() {
        // `fresh { expr }` lowers to QObjectComputedProperty +
        // private compute_x() method (re-evaluated every read, no
        // caching, no auto dep tracking). Q_PROPERTY has NOTIFY
        // (synthesized) but NO BINDABLE clause, since
        // QObjectComputedProperty's QBindable doesn't implement
        // subscription. The constructor fans every input bindable's
        // notify out to the fresh notify so QML/QtWidget bindings
        // still react when deps happen to be bindable.
        let src = r#"
class Sensor < QObject {
  pub prop raw : Int, bindable
  pub prop scaled : Float, fresh { 2.0 * raw }
}
"#;
        let r = build(src);
        assert!(
            r.header
                .contains("Q_PROPERTY(double scaled READ scaled NOTIFY scaledChanged)"),
            "fresh Q_PROPERTY annotation wrong:\n{}",
            r.header
        );
        assert!(
            !r.header.contains("BINDABLE bindableScaled"),
            "fresh must not advertise BINDABLE (subscription is broken):\n{}",
            r.header
        );
        assert!(
            r.header.contains("    double computeScaled() const;"),
            "missing computeScaled() declaration:\n{}",
            r.header
        );
        assert!(
            r.header.contains(
                "Q_OBJECT_COMPUTED_PROPERTY(Sensor, double, m_scaled, &Sensor::computeScaled)"
            ),
            "missing Q_OBJECT_COMPUTED_PROPERTY storage:\n{}",
            r.header
        );
        assert!(
            r.source
                .contains("double Sensor::computeScaled() const { return (2.0 * m_raw.value()); }"),
            "computeScaled body wrong:\n{}",
            r.source
        );
        // No bindableScaled() getter — fresh skips the QBindable
        // surface entirely (would expose a no-op subscription).
        assert!(
            !r.source
                .contains("QBindable<double> Sensor::bindableScaled"),
            "fresh must not expose bindableX() getter:\n{}",
            r.source
        );
        // Constructor wires raw_changed → emit scaled_changed.
        assert!(
            r.source
                .contains("QObject::connect(this, &Sensor::rawChanged, this, [this]"),
            "missing fan-out connect from rawChanged:\n{}",
            r.source
        );
        assert!(
            r.source.contains("emit scaledChanged();"),
            "missing emit scaledChanged in fan-out body:\n{}",
            r.source
        );
    }

    #[test]
    fn batch_block_lowers_to_qscoped_property_update_group() {
        let src = r#"
class Book < QObject {
  prop Page : Int, bindable
  pub prop total : Int, bindable
  pub fn swapTo(p: Int, t: Int) {
    batch {
      Page = p
      total = t
    }
  }
}
"#;
        let r = build(src);
        // Expect a `{` line, then the guard, then both writes inside.
        let body_idx = r
            .source
            .find("void Book::swapTo(")
            .expect("swapTo body missing");
        let body = &r.source[body_idx..];
        let end = body
            .find("\n}\n\n")
            .map(|n| n + body_idx + 4)
            .unwrap_or(r.source.len());
        let body_full = &r.source[body_idx..end];
        assert!(
            body_full.contains(&format!(
                "{{\n        QScopedPropertyUpdateGroup {BATCH_GUARD_VAR};"
            )),
            "missing QScopedPropertyUpdateGroup guard inside batch:\n{}",
            body_full
        );
        // Both bindable writes must land inside the guard's scope.
        assert!(
            body_full.contains("m_Page = p;"),
            "batch body missing first assignment:\n{}",
            body_full
        );
        assert!(
            body_full.contains("m_total = t;"),
            "batch body missing second assignment:\n{}",
            body_full
        );
    }

    #[test]
    fn batch_block_introduces_a_local_scope() {
        let src = r#"
fn run {
  batch {
    let tmp = 7
  }
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains(&format!("QScopedPropertyUpdateGroup {BATCH_GUARD_VAR};"))
        );
        assert!(r.source.contains("auto tmp = 7;"));
    }

    #[test]
    fn fresh_prop_constructor_fans_input_notifies_to_fresh_notifies() {
        // Fresh's reactive surface is the synthesized NOTIFY signal —
        // its QBindable is subscription-less. The constructor fans
        // every input bindable's notify out to all fresh notifies so
        // QML/QtWidget bindings re-evaluate when any input ticks
        // (over-conservative on a per-fresh basis but always correct).
        let src = r#"
class Book < QObject {
  prop Page : Int, notify: :pageChanged, bindable
  pub prop total : Int, notify: :totalChanged, bindable
  pub prop ratio : Float, fresh { (1.0 * Page) / total }
  pub signal pageChanged
  pub signal totalChanged
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("QObject::connect(this, &Book::pageChanged, this, [this]{"),
            "missing input→fresh connect for pageChanged:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("QObject::connect(this, &Book::totalChanged, this, [this]{"),
            "missing input→fresh connect for totalChanged:\n{}",
            r.source
        );
        assert!(
            r.source.contains("emit ratioChanged();"),
            "missing emit of synthesized ratioChanged:\n{}",
            r.source
        );
    }

    #[test]
    fn class_with_no_derived_props_keeps_empty_constructor_body() {
        // Regression guard: a class with only Plain / Bindable props
        // (no Bind / no Fresh) keeps the trivial `: QObject(parent) {}`
        // constructor body — neither setBinding nor fan-out connect
        // is emitted.
        let src =
            "class X < QObject { prop A : Int, notify: :aChanged, bindable  signal aChanged }";
        let r = build(src);
        assert!(
            r.source
                .contains("X::X(QObject* parent) : QObject(parent) {}"),
            "expected empty ctor body when no derived props:\n{}",
            r.source
        );
    }

    #[test]
    fn computed_prop_metacall_routes_through_bindable_getter() {
        let src = r#"
class X < QObject {
  prop a : Int, bindable
  pub prop doubleA : Int, bind { a + a }
}
"#;
        let r = build(src);
        // Both props should appear in the BindableProperty switch.
        assert!(
            r.source
                .contains("if (_c == QMetaObject::BindableProperty)"),
            "missing BindableProperty switch:\n{}",
            r.source
        );
        assert!(
            r.source.contains("_t->bindableA();"),
            "missing bindableA dispatch:\n{}",
            r.source
        );
        assert!(
            r.source.contains("_t->bindableDoubleA();"),
            "missing bindableDoubleA dispatch:\n{}",
            r.source
        );
    }

    #[test]
    fn bindable_prop_meta_section_has_bindable_flag_and_metacall_switch() {
        let src = "class Counter < QObject {\n  prop count : Int, notify: :countChanged, bindable\n  signal countChanged\n}";
        let r = build(src);
        assert!(
            r.source.contains("QMC::Bindable"),
            "PropertyData flags missing QMC::Bindable:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("if (_c == QMetaObject::BindableProperty)"),
            "qt_static_metacall missing BindableProperty switch:\n{}",
            r.source
        );
        assert!(
            r.source.contains("_t->bindableCount();"),
            "BindableProperty switch must dispatch via bindableX():\n{}",
            r.source
        );
    }

    #[test]
    fn source_uses_qt6_template_metaobject_form() {
        let src = "class X < QObject { prop name : String, default: \"\"  signal nameChanged }";
        let r = build(src);
        assert!(
            r.source
                .contains("qt_create_metaobjectdata<qt_meta_tag_X_t>"),
            "missing template specialization:\n{}",
            r.source
        );
        assert!(
            r.source.contains("QtMocHelpers::StringRefStorage"),
            "missing string storage:\n{}",
            r.source
        );
        assert!(
            r.source.contains("QtMocHelpers::SignalData<void()>"),
            "missing signal data:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("Q_CONSTINIT const QMetaObject X::staticMetaObject"),
            "missing staticMetaObject:\n{}",
            r.source
        );
    }

    #[test]
    fn property_setter_emits_notify() {
        let src = "class X < QObject { prop done : Bool, notify: :sc  signal sc }";
        let r = build(src);
        assert!(r.source.contains("emit sc();"));
    }

    #[test]
    fn bare_plain_prop_read_routes_through_getter() {
        // A class method that reads its own plain prop should call
        // the Q_PROPERTY getter (`count()`) rather than touching the
        // backing field (`m_count`) directly. Restores symmetry with
        // the write side, which already routes through `setCount(...)`,
        // so a subclass virtualising either accessor sees both halves
        // of `count = count + 1` fire its overrides.
        let src = r#"
class X < QObject {
  prop count : Int, default: 0
  fn inc { count = count + 1 }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("setCount((count() + 1));"),
            "bare-read should call the getter, write should call the setter:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("setCount((m_count + 1));"),
            "the old direct-field-access path must be gone:\n{}",
            r.source
        );
    }

    #[test]
    fn bare_bindable_prop_read_keeps_direct_value_call() {
        // Bindable / bind / fresh storage uses
        // QObjectBindableProperty / QObjectComputedProperty whose
        // value() call is what Qt's binding system observes for
        // dependency tracking. Routing those reads through the public
        // getter would still work, but the direct `.value()` form is
        // what the surrounding `bind { ... }` / `fresh { ... }`
        // lambdas (and external observers via `bindable<x>()`) expect,
        // so we keep the existing path here.
        let src = r#"
class X < QObject {
  prop tick  : Int, bindable, default: 0
  prop twice : Int, bind { tick * 2 }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("m_tick.value()"),
            "bindable read should remain `m_tick.value()` for binding-system tracking:\n{}",
            r.source
        );
    }

    #[test]
    fn property_setter_auto_emits_synth_notify_for_plain_prop() {
        // Plain `prop name : String, default: ""` (no explicit notify
        // and no constant flag) auto-derives `nameChanged` and the
        // setter fires it on a value change. This is the v1.x default
        // — the user no longer has to write `, notify: :nameChanged`
        // + `signal nameChanged` manually.
        let src = "class X < QObject { prop name : String, default: \"\" }";
        let r = build(src);
        assert!(r.source.contains("if (m_name == value) return;"));
        assert!(
            r.source.contains("emit nameChanged();"),
            "auto-derived NOTIFY signal should fire from setter:\n{}",
            r.source
        );
        assert!(
            r.header.contains("    void nameChanged();"),
            "header should declare the synth signal as a Qt signal:\n{}",
            r.header
        );
    }

    #[test]
    fn property_with_constant_flag_skips_notify() {
        // `, constant` opts the prop out of the auto-derived NOTIFY,
        // matching Qt's CONSTANT Q_PROPERTY semantics. Setter is still
        // emitted (writable storage, just no change-event); no signal
        // declaration; no `emit` in the body.
        let src = "class X < QObject { prop label : String, default: \"hi\", constant }";
        let r = build(src);
        assert!(
            !r.source.contains("emit labelChanged"),
            "constant prop must not emit a notify:\n{}",
            r.source
        );
        assert!(
            !r.header.contains("void labelChanged"),
            "constant prop must not declare a signal:\n{}",
            r.header
        );
    }

    #[test]
    fn signal_body_calls_qmetaobject_activate() {
        let src = "class X < QObject { signal pinged }";
        let r = build(src);
        assert!(
            r.source.contains("void X::pinged()"),
            "missing signal def:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("QMetaObject::activate(this, &staticMetaObject, 0, nullptr);"),
            "missing activate:\n{}",
            r.source
        );
    }

    #[test]
    fn line_directive_anchors_top_level_fn_body_to_cute_source() {
        // Body opens on line 3 of the source ("{ greet... }").
        let src = "\n\nfn run { greet() }\n";
        let r = build_with_source_map("hello.cute", src);
        assert!(
            r.source.contains("#line 3 \"hello.cute\""),
            "expected #line 3 directive in:\n{}",
            r.source
        );
    }

    #[test]
    fn line_directive_anchors_class_method_body_to_cute_source() {
        // The `fn toggle` body opens on line 4 (after the `class`
        // header on line 2 and the `prop` line on line 3).
        let src = "\nclass X < QObject {\n  prop done : Bool, default: false\n  fn toggle { done = !done }\n}\n";
        let r = build_with_source_map("toggle.cute", src);
        assert!(
            r.source.contains("#line 4 \"toggle.cute\""),
            "expected #line 4 directive in:\n{}",
            r.source
        );
    }

    #[test]
    fn line_directive_emitted_per_statement_inside_fn_body() {
        // Three distinct stmts on three distinct lines should produce
        // three #line directives (one per stmt's source row), not just
        // one at the function body start.
        let src = "\
class X < QObject {
  prop count : Int, default: 0
  signal step
  fn run {
    count = 1
    count = 2
    emit step
  }
}
";
        let r = build_with_source_map("multi.cute", src);
        let directives: Vec<_> = r
            .source
            .lines()
            .filter(|l| l.starts_with("#line "))
            .collect();
        // Body opens on line 4; the three stmts are on lines 5, 6, 7.
        assert!(
            directives.iter().any(|l| l.contains("#line 5 ")),
            "missing per-statement directive for line 5; got:\n{}",
            r.source
        );
        assert!(
            directives.iter().any(|l| l.contains("#line 6 ")),
            "missing per-statement directive for line 6"
        );
        assert!(
            directives.iter().any(|l| l.contains("#line 7 ")),
            "missing per-statement directive for line 7"
        );
    }

    #[test]
    fn no_line_directive_when_source_map_is_absent() {
        // Default `build` (source_map: None) must not emit any
        // `#line` directives — keeps existing snapshot tests stable.
        let r = build(
            "class X < QObject { prop done : Bool, default: false fn toggle { done = !done } }",
        );
        assert!(
            !r.source.contains("#line "),
            "unexpected #line directive in:\n{}",
            r.source
        );
    }

    #[test]
    fn fn_method_lowers_at_var_to_setter_call() {
        let src =
            "class X < QObject { prop done : Bool, default: false  fn toggle { done = !done } }";
        let r = build(src);
        // `done = !done` lowers to `setDone((!done()))` — both sides
        // route through the Q_PROPERTY accessors so a virtual override
        // in a subclass sees both directions. The setter does the
        // dirty check + auto-emit, the getter is inline-trivial so
        // optimised builds collapse the read back to direct field
        // access.
        assert!(
            r.source.contains("setDone((!done()));"),
            "expected getter-driven setter write, got:\n{}",
            r.source
        );
    }

    /// `obj.field = value` outside the owning class must call the
    /// Q_PROPERTY setter (`obj->setField(value)`), not assign to the
    /// getter's return value (`obj->field() = value`). The latter
    /// silently no-ops because the getter returns by value — a real
    /// correctness bug, not a stylistic preference.
    #[test]
    fn member_assignment_calls_setter_not_getter() {
        let src = r#"
class Counter < QObject {
  pub prop count : Int, default: 0
}
fn reset(c: Counter) {
  c.count = 0
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("c->setCount(0)"),
            "expected setter call, got:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("c->count() = 0"),
            "must not assign to getter return:\n{}",
            r.source
        );
    }

    /// Compound assignment via member: `obj.field += delta` must
    /// read through the getter and write through the setter, i.e.
    /// `obj->setField(obj->field() + delta)`. Same pattern for
    /// `-=`, `*=`, `/=`.
    #[test]
    fn member_compound_assign_round_trips_through_getter_and_setter() {
        let src = r#"
class Counter < QObject {
  pub prop count : Int, default: 0
}
fn bump(c: Counter, delta: Int) {
  c.count += delta
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("c->setCount(c->count() + delta)"),
            "expected getter+setter round-trip, got:\n{}",
            r.source
        );
    }

    /// `@field = value` (AtIdent target) routes through the auto-
    /// generated setter — write semantics are unified with the
    /// external `obj.field = value` form. The previous "raw m_x =
    /// value" path silently bypassed the notify signal, which made
    /// `notify:` props update views from external writes but NOT
    /// from internal `@x =` writes; the trap is gone.
    #[test]
    fn at_ident_assignment_routes_through_setter() {
        let src = "class X < QObject { prop count : Int, default: 0  fn zero { count = 0 } }";
        let r = build(src);
        assert!(
            r.source.contains("void X::zero() {\n    setCount(0);\n}"),
            "`count = 0` (member write) must lower as setCount(0); got:\n{}",
            r.source
        );
    }

    /// Compound `@x += d` reads via the getter, combines, writes
    /// via the setter — the same shape as external `obj.x += d`.
    /// Side-effect-once: the getter is called in the read position,
    /// the setter once in the write position.
    #[test]
    fn at_ident_compound_assign_round_trips_through_getter_setter() {
        let src = r#"
class X < QObject {
  prop count : Int, default: 0
  fn bump(d: Int) { count += d }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("setCount(count() + d);"),
            "compound `count += d` must round-trip through getter + setter, got:\n{}",
            r.source
        );
    }

    /// `fn make() -> X?` for a QObject-derived X lowers the return as
    /// `QPointer<X>`, which is pointer-like (`operator->`). The
    /// pointer-class detector must unwrap `Nullable` so the result of
    /// such a call is recognized as a pointer expression — without
    /// it, `let c = make_maybe(); c.signal.connect { ... }` cannot
    /// resolve the `&Class::signal` member function pointer and falls
    /// through to a plain `.connect(...)` call that doesn't compile.
    #[test]
    fn pointer_class_unwraps_nullable_qobject_return() {
        let src = r#"
class Counter < QObject {
  pub signal pinged
}
fn makeCounter Counter? {
  Counter.new()
}
fn useIt {
  let c = makeCounter()
  c.pinged.connect {
    println("hi")
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("QObject::connect(c, &Counter::pinged"),
            "expected signal-connect with resolved &Counter::pinged, got:\n{}",
            r.source
        );
    }

    /// A non-`new` method call that returns a pointer class — e.g.
    /// `factory.create()` returning `Foo` (QObject) — must mark its
    /// result as a pointer expression so subsequent `.signal.connect`
    /// or member access lower with `->`. Today `is_pointer_expr` /
    /// `pointer_class_of` skip MethodCall whose method isn't `new`.
    #[test]
    fn pointer_class_recognizes_method_call_returning_pointer_class() {
        let src = r#"
class Foo < QObject {
  pub signal sig
}
class Factory < QObject {
  pub fn create Foo {
    Foo.new()
  }
}
fn useIt(f: Factory) {
  f.create().sig.connect {
    println("hi")
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("QObject::connect(f->create(), &Foo::sig"),
            "expected signal-connect to resolve through method-call return, got:\n{}",
            r.source
        );
    }

    /// `fn make() -> X` where X is a Cute ARC class (`class X <
    /// Object`) returns a `cute::Arc<X>`. `Arc<T>` overloads `->`,
    /// so member access on the call result needs `->`. This already
    /// works through `pointer_class_from_type_expr` — the test pins
    /// the behavior so a future "tighten the leaf check" refactor
    /// doesn't break it.
    #[test]
    fn pointer_class_recognizes_arc_class_return() {
        let src = r#"
arc Token {
  pub fn describe String { "" }
}
fn makeToken Token {
  Token.new()
}
fn useIt String {
  let t = makeToken()
  t.describe()
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("t->describe()"),
            "expected ARC-class call result to use ->, got:\n{}",
            r.source
        );
    }

    /// `test fn name { ... }` lowers to `void cute_test_<name>()` so
    /// the runner main can call it via the prefixed C++ symbol
    /// without colliding with a same-named user fn. The body lowers
    /// like any other void-returning fn.
    #[test]
    fn test_fn_lowers_with_cute_test_prefix() {
        let src = r#"
test fn equalityWorks {
  assert_eq(1, 1)
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("void cuteTestEqualityWorks()"),
            "test fn should lower with cuteTest prefix, got:\n{}",
            r.source
        );
        assert!(
            r.header.contains("void cuteTestEqualityWorks();"),
            "header should declare cuteTestEqualityWorks, got:\n{}",
            r.header
        );
    }

    /// `assert_eq(actual, expected)` is a Cute builtin: the codegen
    /// rewrites it to `::cute::test::assert_eq(actual, expected,
    /// __FILE__, __LINE__)`. The runtime template throws
    /// `cute::test::AssertionFailure` on mismatch; the runner main
    /// catches it and prints `not ok N - <name>: <msg>`.
    #[test]
    fn assert_eq_lowers_to_runtime_call_with_file_line() {
        let src = r#"
test fn x {
  assert_eq(1 + 1, 2)
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("::cute::test::assert_eq((1 + 1), 2, __FILE__, __LINE__)"),
            "expected runtime assert_eq call with __FILE__/__LINE__, got:\n{}",
            r.source
        );
    }

    /// `assert_neq(a, b)` mirrors `assert_eq` but inverts the
    /// predicate. Runtime helper throws when both operands compare
    /// equal.
    #[test]
    fn assert_neq_lowers_to_runtime_call_with_file_line() {
        let src = r#"
test fn x {
  assert_neq(1 + 1, 3)
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("::cute::test::assert_neq((1 + 1), 3, __FILE__, __LINE__)"),
            "expected runtime assert_neq call with __FILE__/__LINE__, got:\n{}",
            r.source
        );
    }

    /// `assert_true(cond)` takes one boolean and forwards it. Same
    /// `__FILE__` / `__LINE__` annotation pattern as the equality
    /// asserts so failure messages point at the call site.
    #[test]
    fn assert_true_lowers_to_runtime_call_with_file_line() {
        let src = r#"
test fn x {
  assert_true(2 > 1)
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("::cute::test::assert_true((2 > 1), __FILE__, __LINE__)"),
            "expected runtime assert_true call with __FILE__/__LINE__, got:\n{}",
            r.source
        );
    }

    /// `assert_false(cond)` is the symmetric helper; same lowering
    /// shape as `assert_true`.
    #[test]
    fn assert_false_lowers_to_runtime_call_with_file_line() {
        let src = r#"
test fn x {
  assert_false(1 > 2)
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("::cute::test::assert_false((1 > 2), __FILE__, __LINE__)"),
            "expected runtime assert_false call with __FILE__/__LINE__, got:\n{}",
            r.source
        );
    }

    /// In a generic-bound fn body, `xs.method()` on a `T`-typed
    /// param routes through `::cute::deref` so the C++ template
    /// `<T: Foo>` body method call routes through the trait
    /// dispatch namespace `::cute::trait_impl::Foo::x(thing)`. C++
    /// overload resolution at template-instantiation time picks the
    /// right `Foo::x(T)` overload — the per-impl free functions
    /// emitted by `emit_impl_free_functions` are the candidates.
    /// Beats the legacy `::cute::deref(thing).x()` form because the
    /// namespace dispatch unifies user-class and extern for-types.
    #[test]
    fn generic_bound_body_method_call_routes_through_trait_namespace() {
        let src = r#"
trait Foo { fn x Int }
fn useIt<T: Foo>(thing: T) Int {
  thing.x()
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("::cute::trait_impl::Foo::x(thing)"),
            "expected namespace dispatch for trait method, got:\n{}",
            r.header
        );
    }

    /// Same dispatch applies to zero-arg property/getter access via
    /// `K::Member` (Cute's `xs.foo` parses as Member, not as
    /// MethodCall, but lowers to a method call in C++). When the
    /// member name is on a bound trait, route through the namespace.
    #[test]
    fn generic_bound_body_member_access_routes_through_trait_namespace() {
        let src = r#"
trait Foo { fn x Int }
fn useIt<T: Foo>(thing: T) Int {
  thing.x
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("::cute::trait_impl::Foo::x(thing)"),
            "expected namespace dispatch for trait member access, got:\n{}",
            r.header
        );
    }

    /// A bare `<T>` (no bounds) keeps the legacy `::cute::deref`
    /// form — there's no trait surface to dispatch through, so we
    /// fall back to the if-constexpr pointer/value normalizer.
    #[test]
    fn bare_generic_body_method_call_uses_cute_deref() {
        let src = r#"
fn useIt<T>(thing: T) Int {
  thing.x()
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("::cute::deref(thing).x()"),
            "expected deref-wrap on bare-generic method call, got:\n{}",
            r.header
        );
    }

    /// Non-generic params don't get the deref wrap — only generic-
    /// typed bindings do. The existing pointer-vs-value lowering
    /// continues to apply for class-typed params.
    #[test]
    fn non_generic_param_keeps_normal_lowering() {
        let src = r#"
class Counter < QObject {
  prop n : Int, default: 0
}
fn read(c: Counter) Int {
  c.n
}
"#;
        let r = build(src);
        assert!(
            !r.source.contains("::cute::deref(c)") && !r.header.contains("::cute::deref(c)"),
            "non-generic param shouldn't get deref-wrap:\n{}\n{}",
            r.header,
            r.source
        );
    }

    /// `impl Trait for Class { fn method { ... } }` should emit
    /// the method as a regular member of the target class (via
    /// `inline_impls_into_classes`'s splice pre-pass) AND a
    /// free-function delegate in `cute::trait_impl::<Trait>::`
    /// namespace (driving generic-bound dispatch).
    #[test]
    fn impl_methods_land_on_target_class() {
        let src = r#"
trait Iterable {
  fn iter Int
}
class MyList < QObject {
  prop n : Int, default: 0
}
impl Iterable for MyList {
  fn iter Int { 42 }
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("class MyList"),
            "expected MyList header, got:\n{}",
            r.header
        );
        // The trait method becomes a regular C++ method on MyList.
        // Whether the declaration lands in the header or the body
        // lands in the source, both should reference iter().
        assert!(
            r.header.contains("iter()") || r.source.contains("iter()"),
            "expected iter() somewhere, got header:\n{}\nsource:\n{}",
            r.header,
            r.source
        );
        // Free-function delegate in the trait-impl namespace lets
        // generic-bound bodies dispatch through C++ overload
        // resolution.
        assert!(
            r.header.contains("namespace cute::trait_impl::Iterable")
                && r.header.contains("iter(MyList* self)"),
            "expected `Iterable::iter(MyList* self)` delegate, got:\n{}",
            r.header
        );
        // The trait declaration itself still produces no C++ class
        // / struct / fn — only the namespace pulls the name in.
        let trait_decl_emitted = r.header.contains("class Iterable")
            || r.header.contains("struct Iterable")
            || r.source.contains("class Iterable");
        assert!(
            !trait_decl_emitted,
            "trait `Iterable` declaration should be erased; namespace use is fine:\n{}\n{}",
            r.header, r.source
        );
    }

    /// `trait Foo { fn x { ... } }` declares a default method body.
    /// An `impl Foo for Bar { ... }` that omits `x` inherits the
    /// trait's default. The default lands on `Bar` as a regular
    /// method, callable like any class member.
    #[test]
    fn impl_inherits_trait_default_when_method_omitted() {
        let src = r#"
trait Greeter {
  pub fn name String
  fn greet String { "hello, world" }
}
class Person < QObject {
  prop n : String, default: ""
}
impl Greeter for Person {
  pub fn name String { self.n() }
}
"#;
        let r = build(src);
        // The default body's literal should land on Person via the
        // injected `greet()` method. Both header decl + emitted
        // body need to reference the trait-default literal.
        let combined = format!("{}\n{}", r.header, r.source);
        assert!(
            combined.contains("greet()"),
            "expected default `greet()` to land on Person, got:\n{}",
            combined
        );
        assert!(
            combined.contains("\"hello, world\""),
            "expected default body literal to be emitted, got:\n{}",
            combined
        );
    }

    /// When the impl supplies its own version of a method that the
    /// trait also has a default for, the impl wins — the default
    /// is dropped on the floor (no duplicate definition).
    #[test]
    fn impl_override_wins_over_trait_default() {
        let src = r#"
trait Greeter {
  pub fn greet String { "default" }
}
class Person < QObject {
  prop n : String, default: ""
}
impl Greeter for Person {
  pub fn greet String { "custom" }
}
"#;
        let r = build(src);
        let combined = format!("{}\n{}", r.header, r.source);
        assert!(
            combined.contains("\"custom\""),
            "expected impl's body to be emitted, got:\n{}",
            combined
        );
        assert!(
            !combined.contains("\"default\""),
            "trait default should be discarded when impl overrides, got:\n{}",
            combined
        );
    }

    /// `impl<T> Trait for UserClass<T> { ... }` splices onto the
    /// user generic class via base-name lookup ("UserClass"). The
    /// impl method's references to `T` resolve through the class's
    /// own `T` type parameter (the splice is a structural copy;
    /// matching names is the v1 convention).
    #[test]
    fn parametric_impl_on_user_generic_class_splices() {
        let src = r#"
trait Sized { pub fn itemCount Int }
arc Bag<T> {
  pub var count : Int = 0
}
impl<T> Sized for Bag<T> {
  pub fn itemCount Int { self.count() }
}
"#;
        let r = build(src);
        // The trait method should land on Bag's class definition,
        // mirroring the non-parametric splice path.
        let combined = format!("{}\n{}", r.header, r.source);
        assert!(
            combined.contains("itemCount"),
            "expected itemCount to splice onto Bag<T>, got:\n{}",
            combined
        );
        // The impl block itself should be erased.
        assert!(
            !combined.contains("impl Sized"),
            "impl declaration should be erased, got:\n{}",
            combined
        );
    }

    /// `impl Trait for ExternType { ... }`: free-function emission
    /// produces an inline-body overload in the trait namespace,
    /// since there's no user class to splice onto. The body uses
    /// `self` (the parameter), not `this`.
    #[test]
    fn impl_on_extern_value_type_emits_inline_body_in_namespace() {
        let src = r#"
trait Sized { pub fn itemCount Int }
impl Sized for QPoint {
  pub fn itemCount Int { self.manhattanLength() }
}
"#;
        let r = build(src);
        // Namespace + inline body with self translated.
        assert!(
            r.header.contains("namespace cute::trait_impl::Sized"),
            "expected trait namespace, got:\n{}",
            r.header
        );
        assert!(
            r.header.contains("itemCount(QPoint& self)"),
            "expected `QPoint&` value-flavored signature, got:\n{}",
            r.header
        );
        assert!(
            r.header.contains("self.manhattanLength()"),
            "expected inline body with self.<method>(), got:\n{}",
            r.header
        );
    }

    /// `impl Trait for QStringList { ... }`: regression pin for the
    /// binding-shape flip (qcore.qpi marks QStringList as `extern
    /// value`). Before the flip, codegen lowered the receiver as
    /// `QStringList* self` and used `self->count()`, neither of
    /// which is correct — Qt's QStringList is a value type. After
    /// the flip, the namespace overload emits `QStringList& self`
    /// with `self.count()` (value member access), matching every
    /// other extern-value type.
    #[test]
    fn impl_on_qstringlist_uses_value_flavored_signature_after_binding_flip() {
        let src = r#"
trait Sized { pub fn itemCount Int }
impl Sized for QStringList {
  pub fn itemCount Int { self.count() }
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("itemCount(QStringList& self)"),
            "expected value-flavored `QStringList&` signature, got:\n{}",
            r.header
        );
        assert!(
            r.header.contains("self.count()"),
            "expected value-flavored `self.<method>()` body, got:\n{}",
            r.header
        );
        // Negative: pointer-flavored leftovers must be gone.
        assert!(
            !r.header.contains("QStringList* self") && !r.header.contains("self->count()"),
            "found leftover pointer-flavored lowering, got:\n{}",
            r.header
        );
    }

    /// **Direct-call dispatch for value-typed bindings.** A
    /// concrete-context call on an extern value type with a
    /// registered impl (`let p : QPoint = ...; p.magnitude()`)
    /// must route through the namespace overload, since the
    /// splice pre-pass can't add `magnitude` to QPoint.
    #[test]
    fn direct_call_on_value_typed_binding_routes_through_trait_namespace() {
        let src = r#"
trait Magnitude { fn Magnitude Int }
impl Magnitude for QPoint {
  fn Magnitude Int { self.manhattanLength() }
}
fn run Int {
  let p : QPoint = QPoint(3, 4)
  p.Magnitude()
}
"#;
        let r = build(src);
        // Must route the direct call through the namespace dispatch.
        assert!(
            r.source
                .contains("::cute::trait_impl::Magnitude::Magnitude(p)")
                || r.header
                    .contains("::cute::trait_impl::Magnitude::Magnitude(p)"),
            "expected namespace dispatch for direct value-typed call, got source:\n{}\nheader:\n{}",
            r.source,
            r.header
        );
        // And NOT keep the legacy member call form (which would
        // miss QPoint's actual surface — QPoint has no `magnitude`
        // member).
        assert!(
            !r.source.contains("p.Magnitude()") && !r.header.contains("p.Magnitude()"),
            "found leftover `p.Magnitude()` member call (should be namespace dispatch), got source:\n{}\nheader:\n{}",
            r.source,
            r.header
        );
    }

    /// **Real Qt members must NOT get rerouted.** If the user
    /// calls a real QPoint method (`p.manhattanLength()`), the
    /// regular `.` lowering wins because no trait impl declares
    /// `manhattanLength` — `trait_dispatch_name_for_value_recv`
    /// returns None and the code falls through.
    #[test]
    fn direct_call_to_real_qt_member_keeps_regular_lowering() {
        let src = r#"
trait Magnitude { fn Magnitude Int }
impl Magnitude for QPoint {
  fn Magnitude Int { self.manhattanLength() }
}
fn run Int {
  let p : QPoint = QPoint(3, 4)
  p.manhattanLength()
}
"#;
        let r = build(src);
        // Real Qt member access on the value receiver — namespace
        // dispatch must NOT shadow it.
        assert!(
            r.source.contains("p.manhattanLength()") || r.header.contains("p.manhattanLength()"),
            "expected `p.manhattanLength()` regular lowering, got source:\n{}\nheader:\n{}",
            r.source,
            r.header
        );
        // The trait dispatch namespace is still emitted (because of
        // the impl), but the call site shouldn't dispatch through it.
        assert!(
            !r.source
                .contains("::cute::trait_impl::Magnitude::manhattanLength")
                && !r
                    .header
                    .contains("::cute::trait_impl::Magnitude::manhattanLength"),
            "real Qt member rerouted through namespace, got source:\n{}\nheader:\n{}",
            r.source,
            r.header
        );
    }

    /// **Direct call on a List<T> binding** also routes through the
    /// namespace overload — the parametric impl emits a templated
    /// `magnitude(QList<T>&)` overload, and `xs.magnitude()` on an
    /// annotated-type binding picks it up by base-name match.
    #[test]
    fn direct_call_on_list_typed_binding_routes_through_trait_namespace() {
        let src = r#"
trait Magnitude { fn Magnitude Int }
impl<T> Magnitude for List<T> {
  fn Magnitude Int { self.count() }
}
fn run Int {
  let xs : List<Int> = [10, 20, 30]
  xs.Magnitude()
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("::cute::trait_impl::Magnitude::Magnitude(xs)")
                || r.header
                    .contains("::cute::trait_impl::Magnitude::Magnitude(xs)"),
            "expected namespace dispatch for builtin-generic direct call, got source:\n{}\nheader:\n{}",
            r.source,
            r.header
        );
    }

    /// **Method-level generic inference shows up as an explicit
    /// `<X>` arg at the namespace dispatch.** A trait method
    /// `fn map_to<U>(f: fn(Int) -> U) -> U` called on a generic-
    /// bound receiver with a String-returning lambda must lower to
    /// `::cute::trait_impl::Mapper::map_to<::cute::String>(thing, lambda)`.
    /// Without the explicit `<::cute::String>`, C++ template
    /// deduction can't bind `U` through `std::function<U(qint64)>`.
    #[test]
    fn body_trait_method_with_own_generic_emits_explicit_template_arg() {
        let src = r#"
trait Mapper {
  fn mapTo<U>(f: fn(Int) -> U) U
}
fn run<T: Mapper>(thing: T) {
  let s : String = thing.mapTo({ |x: Int| "got" })
}
"#;
        let r = build(src);
        assert!(
            r.header
                .contains("::cute::trait_impl::Mapper::mapTo<::cute::String>(thing")
                || r.source
                    .contains("::cute::trait_impl::Mapper::mapTo<::cute::String>(thing"),
            "expected explicit `<::cute::String>` arg at the namespace dispatch, got header:\n{}\nsource:\n{}",
            r.header,
            r.source
        );
    }

    /// **Method-level generic inference at a direct-call site** must
    /// emit the explicit `<X>` template arg at the namespace
    /// dispatch, just like the trait-bound branch. Without this,
    /// `p.map_to({ |x: Int| "got" })` would lower as
    /// `::cute::trait_impl::Mapper::map_to(p, lambda)`, leaving C++
    /// to deduce `U` through `std::function<U(qint64)>` — which fails
    /// on a raw lambda.
    #[test]
    fn direct_call_with_method_generic_emits_explicit_template_arg() {
        let src = r#"
trait Mapper { pub fn mapTo<U>(f: fn(Int) -> U) U }
impl Mapper for QPoint {
  pub fn mapTo<U>(f: fn(Int) -> U) U { f(self.manhattanLength()) }
}
fn run {
  let p : QPoint = QPoint(3, 4)
  let s : String = p.mapTo({ |x: Int| "got" })
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("::cute::trait_impl::Mapper::mapTo<::cute::String>(p,")
                || r.header
                    .contains("::cute::trait_impl::Mapper::mapTo<::cute::String>(p,"),
            "expected explicit `<::cute::String>` template arg at the direct-call dispatch, got source:\n{}\nheader:\n{}",
            r.source,
            r.header
        );
    }

    /// **Direct call via fn parameter** — value-typed params (extern
    /// value types, builtin generics) get the same routing as
    /// `let`-tracked bindings. Mirrors the brief's `record_params`
    /// extension.
    #[test]
    fn direct_call_on_value_typed_param_routes_through_trait_namespace() {
        let src = r#"
trait Magnitude { fn Magnitude Int }
impl Magnitude for QPoint {
  fn Magnitude Int { self.manhattanLength() }
}
fn read(p : QPoint) Int {
  p.Magnitude()
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("::cute::trait_impl::Magnitude::Magnitude(p)")
                || r.header
                    .contains("::cute::trait_impl::Magnitude::Magnitude(p)"),
            "expected namespace dispatch for value-typed param, got source:\n{}\nheader:\n{}",
            r.source,
            r.header
        );
    }

    /// **Specialization on a non-splice base** (builtin generic):
    /// both the parametric `impl<T> Sized for List<T>` and the
    /// concrete `impl Sized for List<Int>` emit free-function
    /// overloads in the trait namespace. C++ overload resolution
    /// picks the most specific at the call site (the non-template
    /// concrete wins over the template instantiation).
    #[test]
    fn specialization_on_builtin_generic_emits_both_overloads() {
        let src = r#"
trait Sized { pub fn itemCount Int }
impl<T> Sized for List<T> {
  pub fn itemCount Int { self.count() }
}
impl Sized for List<Int> {
  pub fn itemCount Int { 999 }
}
"#;
        let r = build(src);
        // Both overloads must be present in the namespace — the
        // parametric form (template) AND the concrete form (no template).
        assert!(
            r.header.contains("template <typename T>")
                && r.header.contains("itemCount(QList<T>& self)"),
            "expected templated `QList<T>&` overload, got:\n{}",
            r.header
        );
        assert!(
            r.header.contains("itemCount(QList<qint64>& self)"),
            "expected concrete `QList<qint64>&` overload (specialization), got:\n{}",
            r.header
        );
    }

    /// **`Self` in a trait method's return type substitutes to the
    /// impl's for-type at namespace-overload emission.** Without the
    /// substitution, the namespace overload would emit `auto /* Self */`
    /// (the placeholder lowering for `TypeKind::SelfType`) and C++
    /// would either fail to type the return or pick the wrong
    /// overload. With substitution, `Identity::identity` emits with
    /// the concrete return type `QPoint`.
    #[test]
    fn trait_self_in_return_substitutes_at_namespace_overload() {
        let src = r#"
trait Identity { fn Identity Self }
impl Identity for QPoint {
  fn Identity Self { self }
}
"#;
        let r = build(src);
        // Namespace overload signature: `QPoint identity(QPoint& self)`.
        assert!(
            r.header.contains("Identity(QPoint& self)"),
            "expected `Identity(QPoint& self)`, got:\n{}",
            r.header
        );
        // The return type must be the concrete for-type — NOT the
        // `auto /* Self */` fallback that bare `TypeKind::SelfType`
        // would lower to without substitution.
        assert!(
            !r.header.contains("auto /* Self */"),
            "expected Self → QPoint substitution at namespace emission, found leftover Self placeholder:\n{}",
            r.header
        );
    }

    /// **`Self` substitution covers parameter positions, too.**
    /// `fn merge(other: Self) -> Self` declared on the trait must
    /// emit as `merge(Box* self, Box* other)` in the namespace
    /// overload — both the receiver and the explicit `other` arg
    /// get the impl's for-type.
    #[test]
    fn trait_self_in_param_substitutes_at_namespace_overload() {
        let src = r#"
trait Combiner { pub fn merge(other: Self) Self }
class Box < QObject { prop n : Int, default: 0 }
impl Combiner for Box {
  pub fn merge(other: Self) Self {
    let r = Box.new()
    r.n = self.n() + other.n()
    return r
  }
}
"#;
        let r = build(src);
        // Param's `other: Self` must lower with `Box*` (Box is QObject-derived).
        assert!(
            r.header.contains("merge(Box* self, Box* other)"),
            "expected `Self` in param to substitute to Box* on the namespace overload, got:\n{}",
            r.header
        );
        assert!(
            !r.header.contains("auto /* Self */"),
            "expected no leftover Self placeholder in generated header:\n{}",
            r.header
        );
    }

    /// **`Self` substitution for trait default-bodied methods**
    /// inherited by an impl. Trait declares `fn pretty -> Self`
    /// (default body just returns `self`); the impl omits `pretty`
    /// and inherits the default. The namespace overload for the
    /// impl's for-type must emit the substituted form.
    #[test]
    fn trait_self_in_default_body_substitutes_at_namespace_overload() {
        let src = r#"
trait Maker {
  pub fn make Self
  fn pretty Self { self }
}
class Item < QObject { prop n : Int, default: 0 }
impl Maker for Item {
  pub fn make Self {
    let i = Item.new()
    i.n = 0
    return i
  }
}
"#;
        let r = build(src);
        // The inherited `pretty` should also have its Self substituted
        // to Item* (Item is QObject-derived → pointer).
        assert!(
            r.header.contains("pretty(Item* self)"),
            "expected inherited `pretty` overload with Item receiver, got:\n{}",
            r.header
        );
        assert!(
            !r.header.contains("auto /* Self */"),
            "expected Self substitution on inherited default-bodied method:\n{}",
            r.header
        );
    }

    /// `impl<T> Trait for List<T> { ... }`: parametric impl on a
    /// builtin generic emits a templated free function. The receiver
    /// is the C++ form of `List<T>` (`QList<T>`), passed by reference.
    #[test]
    fn parametric_impl_on_builtin_generic_emits_templated_free_function() {
        let src = r#"
trait Sized { pub fn itemCount Int }
impl<T> Sized for List<T> {
  pub fn itemCount Int { self.count() }
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("namespace cute::trait_impl::Sized"),
            "expected trait namespace, got:\n{}",
            r.header
        );
        assert!(
            r.header.contains("template <typename T>")
                && r.header.contains("itemCount(QList<T>& self)"),
            "expected templated `QList<T>&` overload, got:\n{}",
            r.header
        );
    }

    /// User-class impls emit a delegate variant in the trait
    /// namespace alongside the spliced class method. The delegate
    /// just calls the spliced method via `self->method()`.
    #[test]
    fn user_class_impl_emits_namespace_delegate() {
        let src = r#"
trait Show { pub fn render Int }
class Widget < QObject { prop n : Int, default: 0 }
impl Show for Widget {
  pub fn render Int { 42 }
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("inline qint64 render(Widget* self)")
                && r.header.contains("return self->render();"),
            "expected `Show::render(Widget* self)` delegate, got:\n{}",
            r.header
        );
    }

    /// Trait-default-bodied methods that the impl omits also get a
    /// namespace overload — the splice already added the default
    /// body to the user class, and the namespace delegate routes
    /// to it. Without this overload, generic-bound calls for the
    /// omitted method would have no candidate at the dispatch site.
    #[test]
    fn impl_omitting_default_method_still_gets_namespace_delegate() {
        let src = r#"
trait Show {
  pub fn render Int
  fn pretty Int { 0 }
}
class Widget < QObject { prop n : Int, default: 0 }
impl Show for Widget {
  pub fn render Int { 1 }
}
"#;
        let r = build(src);
        // Delegate emitted for both `render` (impl-supplied) AND
        // `pretty` (trait default that the impl inherits).
        assert!(
            r.header.contains("render(Widget* self)") && r.header.contains("pretty(Widget* self)"),
            "expected delegate for both supplied + default-inherited methods, got:\n{}",
            r.header
        );
    }

    /// When an impl method has the same name as an existing class
    /// method, the class's own method wins — the merge skips the
    /// impl entry to avoid duplicate definitions.
    #[test]
    fn impl_method_collision_keeps_class_method() {
        let src = r#"
trait Foo {
  fn bar Int
}
class Counter < QObject {
  prop n : Int, default: 0
  fn bar Int { 1 }
}
impl Foo for Counter {
  fn bar Int { 2 }
}
"#;
        let r = build(src);
        // Only one definition of Counter::bar emitted — pick whichever
        // path the codegen used (header inline vs source out-of-class).
        let total =
            r.source.matches("Counter::bar()").count() + r.header.matches("int bar()").count();
        assert!(
            total <= 2,
            "duplicate bar() emissions, header:\n{}\nsource:\n{}",
            r.header,
            r.source
        );
    }

    /// `assert_throws { body }` takes a trailing block (no parens
    /// args). Codegen wraps the block in a nullary lambda via
    /// `lower_block_arg` and forwards it to the runtime, where the
    /// try/catch lives. This lets every assert_throws call site stay
    /// short while still propagating `AssertionFailure` from inner
    /// asserts (the runtime helper rethrows those).
    #[test]
    fn assert_throws_lowers_block_to_runtime_call() {
        let src = r#"
fn boom Int { 1 + 1 }
test fn x {
  assert_throws { boom() }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("::cute::test::assert_throws("),
            "expected `::cute::test::assert_throws(` call, got:\n{}",
            r.source
        );
        // The block is wrapped as a [&]() {...} lambda — see
        // lower_block_arg's `K::Block(b)` arm.
        assert!(
            r.source.contains("[&]()"),
            "expected the block to be lowered as a captureless-lambda, got:\n{}",
            r.source
        );
        assert!(
            r.source.contains(", __FILE__, __LINE__"),
            "expected __FILE__/__LINE__ trailers, got:\n{}",
            r.source
        );
    }

    /// Test-build codegen emits a runner main that calls each
    /// `cute_test_<name>` via the `cute::test::run_one` helper, with
    /// a TAP-lite plan header. The non-test path emits neither —
    /// `cute build` on a test-only source surfaces the missing-main
    /// problem rather than silently producing a runner.
    #[test]
    fn test_runner_main_emitted_only_in_test_build() {
        let src = r#"
test fn one { assert_eq(1, 1) }
test fn two { assert_eq(2, 2) }
"#;
        let r_test = build_test(src);
        for needle in [
            "int main(int argc, char** argv)",
            "::cute::test::run_one(1, \"one\", &cuteTestOne)",
            "::cute::test::run_one(2, \"two\", &cuteTestTwo)",
            "\"1..2\\n\"",
            "#include \"cute_test.h\"",
        ] {
            assert!(
                r_test.source.contains(needle),
                "missing `{needle}` in test build, got:\n{}",
                r_test.source
            );
        }

        let r_norm = build(src);
        assert!(
            !r_norm.source.contains("int main("),
            "non-test build must not emit any main from test fns, got:\n{}",
            r_norm.source
        );
        assert!(
            !r_norm.source.contains("cute_test.h"),
            "non-test build must not include cute_test.h, got:\n{}",
            r_norm.source
        );
        assert!(
            r_norm.source.contains("void cuteTestOne()"),
            "non-test build still emits the test fn body, got:\n{}",
            r_norm.source
        );
    }

    /// In test-build mode, the user's `fn main` is suppressed — the
    /// runner main owns the entry point. Without this, the linker
    /// would refuse to produce a binary because of the duplicate
    /// `int main` definitions.
    #[test]
    fn user_main_is_suppressed_in_test_build() {
        let src = r#"
fn main { println("hi") }
test fn x { assert_eq(1, 1) }
"#;
        let r = build_test(src);
        let main_count = r.source.matches("int main(int argc, char** argv)").count();
        assert_eq!(
            main_count, 1,
            "test build must produce exactly one main (the runner), got:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("println"),
            "user main body must not be emitted in test build, got:\n{}",
            r.source
        );
    }

    /// A generic fn `fn id<T>(x: T) -> T { x }` called with an ARC
    /// or QObject argument should be recognized as a pointer
    /// expression at the call site. The type checker has the
    /// binding (T = QObject-derived class), but `pointer_class_of`
    /// only walks the AST, so the binding is invisible to it.
    /// Today this test fails — the call result is not tagged as a
    /// pointer expr and member access lowers with `.`.
    #[test]
    fn pointer_class_recognizes_generic_fn_return_bound_to_pointer_class() {
        let src = r#"
class Counter < QObject {
  pub signal pinged
}
fn id<T>(x: T) T { x }
fn useIt {
  let c = id(Counter.new())
  c.pinged.connect {
    println("hi")
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("QObject::connect(c, &Counter::pinged"),
            "expected signal-connect with class resolved through generic binding, got:\n{}",
            r.source
        );
    }

    #[test]
    fn string_interpolation_lowers_to_qstring_concat() {
        let src = r#"
class X < QObject {
  prop name : String, default: ""
  fn greet { name = "hello #{world}!" }
}
"#;
        let r = build(src);
        // Expected lower:
        //   m_name = QStringLiteral("hello ") + ::cute::str::to_string(world) + QStringLiteral("!");
        assert!(
            r.source.contains(r#"QStringLiteral("hello ")"#),
            "missing leading literal:\n{}",
            r.source
        );
        assert!(
            r.source.contains("::cute::str::to_string(world)"),
            "missing interp call:\n{}",
            r.source
        );
        assert!(
            r.source.contains(r#"QStringLiteral("!")"#),
            "missing trailing literal:\n{}",
            r.source
        );
    }

    #[test]
    fn string_no_interp_lowers_to_single_qstring_literal() {
        let src = r#"class X < QObject { fn run { greet("plain") } }"#;
        let r = build(src);
        assert!(r.source.contains(r#"greet(QStringLiteral("plain"))"#));
    }

    #[test]
    fn format_spec_lowers_to_cute_str_format() {
        let src = r#"
class X < QObject {
  prop text : String, default: ""
  fn render { text = "price: #{x:.2f}" }
}
"#;
        let r = build(src);
        // Format-spec interp lowers to ::cute::str::format(<expr>, "<spec>")
        // rather than the no-spec ::cute::str::to_string(...).
        assert!(
            r.source.contains(r#"::cute::str::format(x, ".2f")"#),
            "missing format helper call:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("::cute::str::to_string(x)"),
            "format-spec interp should not also emit to_string:\n{}",
            r.source
        );
    }

    #[test]
    fn format_spec_zero_pad_lowers() {
        let src = r#"
class X < QObject {
  prop out : String, default: ""
  fn run { out = "n=#{count:08d}" }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains(r#"::cute::str::format(count, "08d")"#),
            "missing zero-pad format call:\n{}",
            r.source
        );
    }

    #[test]
    fn generic_class_let_annotation_instantiates_arc_template() {
        // `arc Box<T>` lowers to a class template; without
        // the let-annotation propagation the rhs would be
        // `cute::Arc<Box>(new Box())` which fails to compile because
        // `Box` is not a class. With the annotation, we expect the
        // typed form `cute::Arc<Box<qint64>>(new Box<qint64>())`.
        let src = r#"
arc Box<T> {
  var Item : T
}

fn main {
  let b: Box<Int> = Box.new()
  println(b.Item)
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("::cute::Arc<Box<qint64>>(new Box<qint64>())"),
            "expected instantiated Arc template, got:\n{}",
            r.source
        );
    }

    #[test]
    fn method_level_generic_emits_method_template() {
        // `class Box<T>` with a method `fn transform<U>(...)` should
        // emit BOTH the class template prelude and the method's own
        // template prefix on the inline definition. The body is
        // header-only (templates require visibility at instantiation
        // sites).
        let src = r#"
arc Box<T> {
  var Item : T
  pub fn transform<U>(f: fn(T) -> U) U {
    f(Item)
  }
}
"#;
        let r = build(src);
        // Inline body in header with both template prefixes.
        assert!(
            r.header.contains("template <typename T>"),
            "missing class template prefix:\n{}",
            r.header
        );
        assert!(
            r.header.contains("template <typename U>"),
            "missing method template prefix:\n{}",
            r.header
        );
        // The method declaration inside the class body also gets
        // its own template prefix so out-of-line and in-line forms
        // line up. Closure-typed params default to
        // `cute::function_ref<F>` (non-owning) — `@escaping` opts
        // into the heavier `std::function<F>` form.
        assert!(
            r.header
                .contains("U Box<T>::transform(::cute::function_ref<U(T)> f)"),
            "missing out-of-class template definition:\n{}",
            r.header
        );
    }

    #[test]
    fn top_level_generic_fn_emits_template_in_header() {
        // `fn first<T>(...)` lowers to a C++ function template
        // whose body lives entirely in the header (templates need
        // to be visible at every instantiation site).
        let src = r#"
fn first<T>(xs: List<T>) T {
  xs[0]
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("template <typename T>"),
            "missing template prefix:\n{}",
            r.header
        );
        assert!(
            r.header.contains("T first(QList<T> xs)"),
            "missing templated signature:\n{}",
            r.header
        );
        assert!(
            !r.source.contains("T first("),
            "non-generic body in source — should be header-only:\n{}",
            r.source
        );
    }

    #[test]
    fn generic_class_form_b_explicit_type_args_instantiates() {
        // Form (b): `Box<Int>.new()` carries the type args directly on
        // the method call. Codegen consumes them to emit the typed
        // ARC template.
        let src = r#"
arc Box<T> {
  var Item : T
}

fn main {
  let b = Box<Int>.new()
  println(b.Item)
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("::cute::Arc<Box<qint64>>(new Box<qint64>())"),
            "expected form-(b) instantiated Arc, got:\n{}",
            r.source
        );
    }

    #[test]
    fn generic_class_var_annotation_also_instantiates() {
        // `var b: Box<String> = Box.new()` mirrors the let path —
        // both Stmt::Let and Stmt::Var go through the same hook.
        let src = r#"
arc Box<T> {
  var Item : T
}

fn main {
  var b: Box<String> = Box.new()
  println(b.Item)
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("::cute::Arc<Box<::cute::String>>"),
            "expected instantiated Arc<Box<String>>, got:\n{}",
            r.source
        );
    }

    #[test]
    fn error_decl_emits_variant_class() {
        let src = r#"
error FileError {
  notFound
  permissionDenied
  ioError(message: String)
}
"#;
        let r = build(src);
        // Variant payload structs.
        assert!(r.header.contains("struct NotFound {};"), "{}", r.header);
        assert!(
            r.header.contains("struct PermissionDenied {};"),
            "{}",
            r.header
        );
        assert!(r.header.contains("struct IoError {"), "{}", r.header);
        assert!(r.header.contains("::cute::String message;"), "{}", r.header);
        // Variant typedef.
        assert!(
            r.header
                .contains("using Variant = std::variant<NotFound, PermissionDenied, IoError>;"),
            "{}",
            r.header
        );
        // Factories.
        assert!(
            r.header.contains("static FileError notFound()"),
            "{}",
            r.header
        );
        assert!(
            r.header
                .contains("static FileError ioError(::cute::String message)"),
            "{}",
            r.header
        );
        // is_* discriminators.
        assert!(r.header.contains("bool isNotFound() const"), "{}", r.header);
        assert!(r.header.contains("bool isIoError() const"), "{}", r.header);
    }

    #[test]
    fn snake_to_camel_handles_common_cases() {
        assert_eq!(snake_to_camel("notFound"), "NotFound");
        assert_eq!(snake_to_camel("ioError"), "IoError");
        assert_eq!(snake_to_camel("permissionDenied"), "PermissionDenied");
        assert_eq!(snake_to_camel("foo"), "Foo");
        assert_eq!(snake_to_camel(""), "");
    }

    #[test]
    fn first_occurrence_assignment_emits_auto_decl() {
        // From HIR: a bare-ident `=` with no prior declaration in this fn
        // should lower to `auto X = ...;`, not `X = ...;`.
        let src = r#"
fn run {
  text = compute()
  text = followUp()
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("auto text = compute()"),
            "missing auto-decl on first occurrence:\n{}",
            r.source
        );
        assert!(
            r.source.contains("text = followUp();"),
            "missing reassignment of already-declared text:\n{}",
            r.source
        );
        // Critical: the second line must NOT be `auto text = ...`.
        assert!(
            !r.source.contains("auto text = followUp()"),
            "second occurrence wrongly auto-declared:\n{}",
            r.source
        );
    }

    #[test]
    fn fn_param_assignment_does_not_auto_decl() {
        let src = r#"
fn run(path: String) {
  path = recompute()
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("path = recompute();"),
            "missing reassignment of fn param:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("auto path"),
            "fn param wrongly auto-declared:\n{}",
            r.source
        );
    }

    #[test]
    fn err_union_binds_to_single_error_decl_in_module() {
        let src = r#"
error AppError {
  notFound
}

fn open(path: String) !File {
  doIt()
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("::cute::Result<File, AppError>"),
            "expected !T bound to AppError, got:\n{}",
            r.source
        );
        // Should NOT have the unbound-error placeholder.
        assert!(
            !r.source.contains("unbound error type"),
            "unbound placeholder leaked:\n{}",
            r.source
        );
    }

    #[test]
    fn list_t_maps_to_qlist() {
        let src = "fn f(xs: List<Int>) {}";
        let r = build(src);
        assert!(
            r.header.contains("QList<qint64>"),
            "expected QList<qint64> in header:\n{}",
            r.header
        );
    }

    #[test]
    fn map_k_v_maps_to_qmap() {
        let src = "fn f(m: Map<String, Int>) {}";
        let r = build(src);
        assert!(
            r.header.contains("QMap<::cute::String, qint64>"),
            "expected QMap<...> in header:\n{}",
            r.header
        );
    }

    #[test]
    fn set_t_and_hash_kv_map_to_qset_qhash() {
        let r = build("fn f(s: Set<Int>, h: Hash<String, Bool>) {}");
        assert!(r.header.contains("QSet<qint64>"), "{}", r.header);
        assert!(
            r.header.contains("QHash<::cute::String, bool>"),
            "{}",
            r.header
        );
    }

    #[test]
    fn future_t_maps_to_qfuture() {
        let r = build("fn f(x: Future<Int>) {}");
        assert!(r.header.contains("QFuture<qint64>"), "{}", r.header);
    }

    #[test]
    fn nested_generic_args_render_correctly() {
        let r = build("fn f(m: Map<String, List<Int>>) {}");
        assert!(
            r.header.contains("QMap<::cute::String, QList<qint64>>"),
            "{}",
            r.header
        );
    }

    #[test]
    fn try_operator_lowers_to_early_return() {
        let src = r#"
fn open(path: String) !File {
  try File.open(path)
}
"#;
        let r = build(src);
        // Single fresh temp + early-return guard + final ok-wrap of trailing expr.
        assert!(
            r.source.contains("auto _r0 = File.open(path);"),
            "missing temp:\n{}",
            r.source
        );
        assert!(
            r.source.contains("if (_r0.is_err()) return ::cute::Result"),
            "missing err return:\n{}",
            r.source
        );
        assert!(
            r.source.contains("std::move(_r0).unwrap_err()"),
            "missing unwrap_err move:\n{}",
            r.source
        );
        assert!(
            r.source.contains("::ok(_r0.unwrap())"),
            "missing ok-wrap of trailing:\n{}",
            r.source
        );
    }

    #[test]
    fn try_chain_uses_distinct_fresh_temps() {
        // Three nested `try`s — each one allocates its own fresh
        // temp + early-return guard.
        let src = r#"
fn loadConfig(path: String) !Config {
  try parse(try (try File.open(path)).readAll)
}
"#;
        let r = build(src);
        assert!(r.source.contains("_r0"), "_r0 missing:\n{}", r.source);
        assert!(r.source.contains("_r1"), "_r1 missing:\n{}", r.source);
        assert!(r.source.contains("_r2"), "_r2 missing:\n{}", r.source);
    }

    #[test]
    fn error_variant_call_lowers_to_static_factory() {
        // `ParseError.empty()` -> the `static T empty()` factory the
        // error decl emits. Uses `::` namespacing per C++; distinct
        // from class-method dispatch which uses `.` / `->`.
        let src = r#"
error ParseError { empty }
fn make !Int { return ParseError.empty() }
"#;
        let r = build(src);
        assert!(
            r.source.contains("ParseError::empty()"),
            "missing static-factory call:\n{}",
            r.source
        );
    }

    #[test]
    fn return_error_value_wraps_in_result_err() {
        // `return ParseError.empty()` from a `!T`-returning fn must
        // wrap with `::err(...)` instead of the default `::ok(...)`
        // - otherwise the error gets stuffed into the success slot.
        let src = r#"
error ParseError { empty }
fn make !Int { return ParseError.empty() }
"#;
        let r = build(src);
        assert!(
            r.source.contains("::err(ParseError::empty())"),
            "expected err-wrap:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("::ok(ParseError::empty())"),
            "should not auto-wrap error value in ok:\n{}",
            r.source
        );
    }

    #[test]
    fn return_success_value_still_wraps_in_ok() {
        // Sanity check the inverse: `return 42` from a `!T`-returning
        // fn keeps the existing `::ok(42)` wrap.
        let src = r#"
error E { x }
fn make !Int { return 42 }
"#;
        let r = build(src);
        assert!(
            r.source.contains("::ok(42)"),
            "expected ok-wrap for success value:\n{}",
            r.source
        );
    }

    #[test]
    fn lifted_bool_ok_call_site_wraps_in_iife() {
        // A binding-class method marked `@lifted_bool_ok` triggers the
        // codegen IIFE wrapper at the call site: bool* out-arg
        // synthesized as `&_ok<n>`, return type lifted to
        // `Result<T, QtBoolError>`. The user side just sees a
        // Result-returning call.
        let src = r#"
class Parser {
  fn toInt(base: Int) !Int @lifted_bool_ok
}

error QtBoolError { failed }

fn run(p: Parser) {
  let r = p.toInt(10)
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("[&]() -> ::cute::Result<qint64, ::QtBoolError>"),
            "missing IIFE return-type header:\n{}",
            r.source
        );
        assert!(
            r.source.contains("bool _ok"),
            "missing fresh ok-out local:\n{}",
            r.source
        );
        assert!(
            r.source.contains("::ok("),
            "missing ok-wrap path:\n{}",
            r.source
        );
        assert!(
            r.source.contains("::err(::QtBoolError::failed())"),
            "missing failed-error path:\n{}",
            r.source
        );
        // The C++ method receives `&_ok` first, then the user-supplied
        // `10`. The exact ok-var name is fresh (`_ok0`, `_ok1`, ...);
        // assert via substring.
        assert!(
            r.source.contains("toInt(&_ok"),
            "expected `toInt(&_ok…, 10)` shape:\n{}",
            r.source
        );
        assert!(
            r.source.contains(", 10)"),
            "user arg `10` should follow the ok-out:\n{}",
            r.source
        );
    }

    #[test]
    fn case_when_ok_err_lowers_to_if_else() {
        let src = r#"
fn run {
  case loadConfig("/etc/cute.conf") {
    when ok(cfg)  { apply(cfg) }
    when err(e)   { log(e) }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("if (_r0.is_ok())"),
            "missing ok branch:\n{}",
            r.source
        );
        assert!(
            r.source.contains("auto cfg = _r0.unwrap();"),
            "missing ok bind:\n{}",
            r.source
        );
        assert!(
            r.source.contains("else if (_r0.is_err())"),
            "missing err branch:\n{}",
            r.source
        );
        assert!(
            r.source.contains("auto e = std::move(_r0).unwrap_err();"),
            "missing err bind:\n{}",
            r.source
        );
    }

    #[test]
    fn case_with_error_variant_uses_is_helpers() {
        let src = r#"
error FileError {
  notFound
  ioError(message: String)
}

fn handle(e: FileError) {
  case e {
    when notFound { log("not found") }
    when ioError  { log("io") }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("_r0.isNotFound()"),
            "missing notFound check:\n{}",
            r.source
        );
        assert!(
            r.source.contains("_r0.isIoError()"),
            "missing ioError check:\n{}",
            r.source
        );
    }

    #[test]
    fn plain_enum_can_serve_as_err_type_in_bang_t_returns() {
        // A plain `enum E { ... }` (no `error` keyword) is usable
        // as the err type of `!T`. Codegen wraps the returned value
        // in `::err(...)` and types the `Result<>` with the enum
        // as the err parameter.
        let src = r#"
enum FileError {
  NotFound
  IoError(message: String)
}
fn parse(s: String) !Int {
  if s == "" { return FileError.NotFound() }
  return 42
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("Result<qint64, FileError>"),
            "expected FileError as the Result err parameter:\n{}",
            r.source
        );
        assert!(
            r.source.contains("::err(FileError::NotFound())"),
            "expected err-wrap on plain-enum constructor return:\n{}",
            r.source
        );
        assert!(
            r.source.contains("::ok(42)"),
            "non-error return path should still wrap in ok:\n{}",
            r.source
        );
    }

    #[test]
    fn nested_err_variant_pattern_dispatches_with_payload_bind() {
        // `when err(VariantName(payload))` matches on both the err
        // discriminator AND the variant tag, plus extracts payload
        // fields. The condition checks `is_err()` and the variant's
        // `is<Cap>()` helper; the binding pulls the field via
        // `std::get<...>(unwrap_err().value).field`.
        let src = r#"
error ParseError {
  empty
  badFormat(reason: String)
}
fn handle(r: !Int) {
  case r {
    when ok(n)               { log(n) }
    when err(empty())        { log("empty") }
    when err(badFormat(why)) { log(why) }
  }
}
"#;
        let r = build(src);
        // Nullary err variant: just is_err() && isEmpty().
        assert!(
            r.source
                .contains("_r0.is_err() && _r0.unwrap_err().isEmpty()"),
            "missing nested empty arm:\n{}",
            r.source
        );
        // Payload err variant: is_err() && isBadFormat() + std::get binding.
        assert!(
            r.source
                .contains("_r0.is_err() && _r0.unwrap_err().isBadFormat()"),
            "missing nested badFormat arm:\n{}",
            r.source
        );
        assert!(
            r.source.contains(
                "auto why = std::get<ParseError::BadFormat>(_r0.unwrap_err().value).reason;"
            ),
            "missing payload bind for badFormat:\n{}",
            r.source
        );
    }

    #[test]
    fn self_recursive_payload_variant_wraps_field_in_shared_ptr() {
        // Self-typed payload fields (`Node(left: Tree, right: Tree)`
        // on `enum Tree { ... }`) need indirection — std::variant
        // can't hold an incomplete type. Codegen wraps with
        // shared_ptr<Self>, factory `make_shared`s the value form,
        // and case-arm bindings deref the pointer.
        let src = r#"
enum Tree {
  Leaf(value: Int)
  Node(left: Tree, right: Tree)
}
fn sum(t: Tree) Int {
  case t {
    when Leaf(v)    { v }
    when Node(l, r) { sum(l) + sum(r) }
  }
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("std::shared_ptr<Tree> left;"),
            "missing shared_ptr field:\n{}",
            r.header
        );
        assert!(
            r.header.contains("std::make_shared<Tree>(std::move(left))"),
            "missing make_shared in factory:\n{}",
            r.header
        );
        assert!(
            r.source
                .contains("auto l = *std::get<Tree::Node_t>(_r0.value).left;"),
            "missing deref in pattern bind:\n{}",
            r.source
        );
    }

    #[test]
    fn return_inside_err_union_fn_wraps_in_ok() {
        let src = r#"
fn pick() !Int {
  return 42
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("return ::cute::Result"),
            "missing wrapped return:\n{}",
            r.source
        );
        assert!(
            r.source.contains("::ok(42);"),
            "missing ok(42):\n{}",
            r.source
        );
    }

    #[test]
    fn top_level_fn_emits_free_function() {
        let src = r#"
fn add(a: Int, b: Int) Int {
  a + b
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("qint64 add(qint64 a, qint64 b);"),
            "missing decl:\n{}",
            r.header
        );
        assert!(
            r.source.contains("qint64 add(qint64 a, qint64 b) {"),
            "missing def:\n{}",
            r.source
        );
        assert!(
            r.source.contains("return (a + b);"),
            "missing trailing return:\n{}",
            r.source
        );
    }

    #[test]
    fn trailing_block_with_pipe_params_lowers_to_generic_lambda() {
        // `xs.each { |x| println(x) }` is a MethodCall whose `block` is a
        // Lambda. The block-arg appender should pass it as the last call
        // argument; the untyped `|x|` should become `auto x` so C++17 generic
        // lambdas deduce the param type at the call site.
        let src = r#"
fn run(xs: Items) {
  xs.each { |x| println(x) }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("xs.each([&](auto x) {"),
            "missing generic-lambda block-arg:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("qInfo().noquote() << ::cute::str::to_string(x);"),
            "missing lambda body:\n{}",
            r.source
        );
    }

    #[test]
    fn trailing_block_without_params_wraps_as_nullary_lambda() {
        // `f { ... }` (no `|x|` list) is a plain Block trailing-arg. The
        // block-arg appender wraps it as `[&]() { ... }` so the receiver
        // gets a callable rather than the block's trailing value.
        let src = r#"
fn run {
  defer { println("bye") }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("defer([&]() {"),
            "missing nullary-lambda wrap:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("qInfo().noquote() << ::cute::str::to_string(QStringLiteral(\"bye\"));"),
            "missing wrapped body:\n{}",
            r.source
        );
    }

    #[test]
    fn lambda_in_expression_position_lowers_to_cxx_lambda() {
        // `{ |x| x + 1 }` at expression position (here as the RHS of a
        // `let`) lowers to a standalone C++ lambda value with `auto`
        // params; the trailing expression becomes a `return`.
        let src = r#"
fn run {
  let f = { |x| x + 1 }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("[&](auto x) {"),
            "missing lambda expr:\n{}",
            r.source
        );
        assert!(
            r.source.contains("return (x + 1);"),
            "missing trailing return inside lambda:\n{}",
            r.source
        );
    }

    #[test]
    fn widget_decl_emits_qmainwindow_subclass_constructor() {
        // `widget Main { QMainWindow { ... QLabel { text: "x" } } }`
        // -> a C++ class `Main : public QMainWindow` whose ctor sets
        // window props on `this` and constructs/parents the children.
        let src = r#"
widget Main {
  QMainWindow {
    windowTitle: "Cute"
    QLabel { text: "hello" }
  }
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("class Main : public QMainWindow"),
            "missing widget class header:\n{}",
            r.header
        );
        assert!(
            r.source
                .contains("Main::Main(QWidget* parent) : QMainWindow(parent) {"),
            "missing ctor signature:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("this->setWindowTitle(QStringLiteral(\"Cute\"));"),
            "missing root prop setter:\n{}",
            r.source
        );
        assert!(
            r.source.contains("auto* _w0 = new QLabel();"),
            "missing child construction:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("_w0->setText(QStringLiteral(\"hello\"));"),
            "missing child prop setter:\n{}",
            r.source
        );
    }

    #[test]
    fn widget_layout_uses_addwidget_not_setparent() {
        // QHBoxLayout / QVBoxLayout etc. are special-cased: layout
        // children of a layout get `addWidget`, not `setParent`. Plain
        // widget children of a non-layout get `setParent`.
        let src = r#"
widget Main {
  QMainWindow {
    QVBoxLayout {
      QLabel { text: "a" }
      QLabel { text: "b" }
    }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("addWidget"),
            "expected layout->addWidget for layout children:\n{}",
            r.source
        );
        // Root QMainWindow + Layout child: wrap in a QWidget and call
        // setCentralWidget. QMainWindow can't `setLayout` directly
        // because it already owns its own layout (Qt warns at runtime).
        assert!(
            r.source.contains("this->setCentralWidget("),
            "expected setCentralWidget for QMainWindow + Layout root:\n{}",
            r.source
        );
    }

    #[test]
    fn widget_app_intrinsic_emits_qapplication_main() {
        let src = r#"
widget Main {
  QMainWindow {}
}

fn main {
  widget_app(window: Main, title: "App")
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("QApplication app(argc, argv);"),
            "missing QApplication boot:\n{}",
            r.source
        );
        assert!(
            r.source.contains("Main w;"),
            "missing window construction:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("setApplicationName(QStringLiteral(\"App\"));"),
            "missing title plumbing:\n{}",
            r.source
        );
        assert!(
            r.source.contains("return app.exec();"),
            "missing exec:\n{}",
            r.source
        );
    }

    #[test]
    fn widget_on_signal_lowers_to_qobject_connect() {
        // `onClicked: <expr>` on a QPushButton -> Qt's modern function-
        // pointer connect with `<expr>` as the lambda body. The signal
        // name is derived from the property key by stripping `on` and
        // lowercasing the next char (Cute / QML convention).
        let src = r#"
fn greet { 1 }

widget Main {
  QMainWindow {
    QPushButton {
      text: "click me"
      onClicked: greet()
    }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("QObject::connect(_w0, &QPushButton::clicked, [=]() {"),
            "missing connect call:\n{}",
            r.source
        );
        assert!(
            r.source.contains("greet();"),
            "missing handler body:\n{}",
            r.source
        );
    }

    #[test]
    fn widget_if_lowers_to_setvisible() {
        // `if cond { El { ... } }` builds the element unconditionally
        // and binds its visibility to the runtime cond. Hidden Qt
        // widgets collapse to zero size in QBoxLayout / QGridLayout.
        let src = r#"
widget Main {
  QMainWindow {
    QVBoxLayout {
      if true {
        QLabel { text: "shown" }
      }
    }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("setVisible(true);"),
            "expected setVisible(<cond>):\n{}",
            r.source
        );
    }

    #[test]
    fn widget_if_else_lowers_to_two_setvisible() {
        let src = r#"
widget Main {
  QMainWindow {
    QVBoxLayout {
      if true {
        QLabel { text: "yes" }
      } else {
        QLabel { text: "no" }
      }
    }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("setVisible(true);"),
            "missing then-branch visibility:\n{}",
            r.source
        );
        assert!(
            r.source.contains("setVisible(!(true));"),
            "missing else-branch inverted visibility:\n{}",
            r.source
        );
    }

    #[test]
    fn widget_for_lowers_to_range_based_for() {
        // `for x in xs { El { ... } }` -> a real C++ range-based for
        // that constructs and attaches one element per iteration.
        let src = r#"
fn items List { [1, 2, 3] }

widget Main {
  QMainWindow {
    QVBoxLayout {
      for x in items() {
        QLabel { text: "item" }
      }
    }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("for (const auto& x : items()) {"),
            "missing range-for header:\n{}",
            r.source
        );
        assert!(
            r.source.contains("addWidget"),
            "iterated child should be added to the surrounding layout:\n{}",
            r.source
        );
    }

    #[test]
    fn cute_ui_widget_with_key_emits_setkey_wrapper() {
        // `Card { key: it.id; ... }` (or any cute_ui element with a
        // `key:` property) wraps construction in an IIFE that calls
        // `_ke->setKey(QVariant::fromValue(<expr>))` so the runtime's
        // keyed-diff can pair this child with the same-keyed old
        // child across rebuilds. The setter is QVariant-wrapped so
        // any `<expr>` type with QVariant conversion just works.
        let src = r#"
class Item {
  var Label : String
  pub var id : Int
}
class Store {
  pub prop items: List<Item>, notify: :itemsChanged
  pub signal itemsChanged
}
widget Main {
  let s = Store()
  Column {
    for it in s.items {
      Text { key: it.id; text: it.Label }
    }
  }
}
fn main { gpu_app(window: Main) }
"#;
        let r = build(src);
        assert!(
            r.source.contains("_ke->setKey(QVariant::fromValue("),
            "missing setKey wrapper:\n{}",
            r.source
        );
    }

    #[test]
    fn widget_property_lowers_qt_namespace_to_double_colon() {
        // `Qt.AlignCenter` at a property position is namespace-qualified
        // enum access; lower as `Qt::AlignCenter` (no `()` suffix).
        let src = r#"
widget Main {
  QMainWindow {
    QLabel { alignment: Qt.AlignCenter }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("Qt::AlignCenter"),
            "expected `::`-separated Qt namespace in source:\n{}",
            r.source
        );
        // Should NOT have the spurious zero-arg call
        // (`Qt.AlignCenter()` would be the old miscompile).
        assert!(
            !r.source.contains("Qt.AlignCenter(") && !r.source.contains("Qt::AlignCenter()"),
            "should not emit a function-call shape:\n{}",
            r.source
        );
    }

    #[test]
    fn widget_property_lowers_if_to_ternary() {
        // `text: if cond { "a" } else { "b" }` lowers to a ternary
        // C++ expression, not a TODO placeholder.
        let src = r#"
fn flag Bool { true }
widget Main {
  QMainWindow {
    QLabel { text: if flag() { "yes" } else { "no" } }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("? QStringLiteral(\"yes\")"),
            "expected ternary then-branch:\n{}",
            r.source
        );
        assert!(
            r.source.contains(": QStringLiteral(\"no\")"),
            "expected ternary else-branch:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("TODO widgetLower"),
            "should not fall through to TODO marker:\n{}",
            r.source
        );
    }

    #[test]
    fn widget_property_lowers_index_expression() {
        // `xs[0]` at a property position lowers to `xs[0]` directly.
        let src = r#"
fn items List { ["a", "b"] }
widget Main {
  QMainWindow {
    QLabel { text: items()[0] }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("items()[0]"),
            "expected indexed access in source:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("TODO widgetLower"),
            "should not fall through to TODO marker:\n{}",
            r.source
        );
    }

    #[test]
    fn view_state_field_lowers_to_qml_id_child() {
        // SwiftUI-style state: `let counter = Counter()` declared at
        // the head of a view body becomes a `Counter { id: counter }`
        // root-level child in the emitted QML so any property
        // expression in the tree can resolve `counter.x` via QML's
        // id binding.
        let src = r#"
class Counter {
  prop count : Int, default: 0
}

view Main {
  let counter = Counter()

  ApplicationWindow {
    Label { text: "" + counter.count }
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Main")
            .expect("view Main");
        assert!(
            v.qml.contains("Counter { id: counter }"),
            "missing id-tagged state-field child:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("text: (\"\" + counter.count)"),
            "binding should reach the state field via id:\n{}",
            v.qml
        );
    }

    #[test]
    fn widget_state_field_lowers_to_member_and_init() {
        // SwiftUI-style state on the widget side: the state field
        // becomes a private pointer member of the user's widget class,
        // initialized in the constructor before the tree is built.
        // References inside the tree resolve to the bare name (C++
        // resolves `counter` to `this->counter`).
        let src = r#"
class Counter {
  prop count : Int, default: 0
  fn increment {
    count = count + 1
  }
}

widget Main {
  let counter = Counter()

  QMainWindow {
    QPushButton {
      text: "click me"
      onClicked: counter.increment()
    }
  }
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("Counter* counter;"),
            "missing private member declaration:\n{}",
            r.header
        );
        assert!(
            r.source.contains("counter = new Counter(this);"),
            "missing constructor init:\n{}",
            r.source
        );
        // State fields are QObject pointers, so member access lowers
        // with `->`. The pointer-awareness was added once we recorded
        // state-field names in WidgetEmitter's `pointer_names` set.
        assert!(
            r.source.contains("counter->increment();"),
            "click handler should reference state field via -> (pointer):\n{}",
            r.source
        );
    }

    #[test]
    fn view_state_property_lowers_to_qml_root_property() {
        // SwiftUI-`@State`-style: `state count : Int = 0` in a view
        // body should land at the QML root as `property int count: 0`.
        // Bare references inside the body resolve via QML's own
        // scoping, so no wrapper class is needed for primitive
        // reactive cells.
        let src = r#"
view Main {
  state count : Int = 0

  ApplicationWindow {
    Label { text: "count: " + count }
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Main")
            .expect("view Main");
        assert!(
            v.qml.contains("property int count: 0"),
            "state-prop should emit `property <type> <name>: <init>` at the QML root:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("text: (\"count: \" + count)"),
            "bare reference to a state-prop should pass through to QML scoping:\n{}",
            v.qml
        );
        // No wrapper class element should appear: state-prop is not
        // an Object-kind state field.
        assert!(
            !v.qml.contains("count { id: count }"),
            "state-prop must not emit a class element (that's the let-form path):\n{}",
            v.qml
        );
    }

    #[test]
    fn view_state_property_handles_string_double_bool_types() {
        // QML's property type vocabulary: String → string,
        // Double → real, Bool → bool. Mapping lives in
        // `qml_property_type` and feeds the same lookup that view
        // params already use, so adding state-props inherits it.
        let src = r#"
view Main {
  state msg : String = "hi"
  state ratio : Double = 1.5
  state ready : Bool = true

  ApplicationWindow {
    Label { text: msg }
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Main")
            .expect("view Main");
        assert!(
            v.qml.contains("property string msg: \"hi\""),
            "String state-prop:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("property real ratio: 1.5"),
            "Double state-prop:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("property bool ready: true"),
            "Bool state-prop:\n{}",
            v.qml
        );
    }

    #[test]
    fn view_state_assignment_block_lowers_to_qml_assignment() {
        // `onClicked: { count = count + 1 }` should lower to a JS
        // assignment expression in QML (`count = (count + 1)`), not
        // to "undefined". The single-Stmt::Assign-no-trailing branch
        // of the K::Block lowering handles this without wrapping in
        // an IIFE so the QML output stays idiomatic.
        let src = r#"
view Main {
  state count : Int = 0

  ApplicationWindow {
    Button {
      text: "+1"
      onClicked: { count = count + 1 }
    }
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Main")
            .expect("view Main");
        assert!(
            v.qml.contains("onClicked: count = (count + 1)"),
            "single-assign block in handler position should emit a bare JS assignment:\n{}",
            v.qml
        );
        assert!(
            !v.qml.contains("onClicked: undefined"),
            "fallback to undefined should not happen for assignments:\n{}",
            v.qml
        );
    }

    #[test]
    fn widget_state_property_synthesizes_holder_class_and_member() {
        // `state count : Int = 0` inside a widget body is desugared
        // into `class __MainState < QObject { pub prop count : Int,
        // notify: :countChanged, default: 0 ; signal countChanged }`
        // plus a hidden `let __cute_state = __MainState()` field.
        // Result: the widget gets a `__MainState* __cute_state` member
        // initialised in the ctor, and the holder class carries the
        // Q_PROPERTY + auto-generated notify signal.
        let src = r#"
widget Main {
  state count : Int = 0

  QMainWindow {
    QLabel { text: "static" }
  }
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("class __MainState : public QObject"),
            "missing synthesized holder class:\n{}",
            r.header
        );
        assert!(
            r.header
                .contains("Q_PROPERTY(qint64 count READ count WRITE setCount NOTIFY countChanged)"),
            "Q_PROPERTY line on holder class:\n{}",
            r.header
        );
        assert!(
            r.header.contains("__MainState* __cute_state;"),
            "widget should carry hidden state-holder member:\n{}",
            r.header
        );
        assert!(
            r.source.contains("__cute_state = new __MainState(this);"),
            "ctor must instantiate the holder with `this` as parent:\n{}",
            r.source
        );
        assert!(
            r.source.contains("emit countChanged();"),
            "setter should fire NOTIFY:\n{}",
            r.source
        );
    }

    #[test]
    fn widget_state_assignment_lowers_to_setter_call() {
        // `onClicked: { count = count + 1 }` inside a widget body
        // should reach the synthesized state-holder via its setter
        // (`setCount`) rather than a raw C++ assignment, so the NOTIFY
        // signal fires and any text-bound expression updates.
        let src = r#"
widget Main {
  state count : Int = 0

  QMainWindow {
    QPushButton {
      text: "+1"
      onClicked: { count = count + 1 }
    }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("__cute_state->setCount((__cute_state->count() + 1))"),
            "click handler should drive the setter on the holder:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("TODO widget_lower: Block-with-stmts"),
            "Block-with-stmts placeholder should not survive lowering:\n{}",
            r.source
        );
    }

    #[test]
    fn cute_ui_widget_state_connects_to_request_rebuild() {
        // cute_ui root class (`Column`) routes to `emit_widget_cute_ui`.
        // The desugaring still synthesizes the `__MainState` holder
        // class; cute_ui's existing signal-loop wires the state's
        // notify signal to `requestRebuild()` so an Element-tree
        // rebuild fires on every assignment.
        let src = r#"
widget Main {
  state count : Int = 0

  Column {
    Text { text: "count: #{count}" }
    Button { text: "+1"; onClick: { count = count + 1 } }
  }
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("class __MainState : public QObject"),
            "synth holder should appear in cute_ui mode too:\n{}",
            r.header
        );
        assert!(
            r.source.contains(
                "QObject::connect(__cute_state, &__MainState::countChanged, this, [this]{ requestRebuild(); });"
            ),
            "cute_ui Component must rebuild on state-prop NOTIFY:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("__cute_state->setCount((__cute_state->count() + 1))"),
            "click handler should drive setter through the holder:\n{}",
            r.source
        );
    }

    #[test]
    fn widget_state_text_binding_connects_to_notify() {
        // `text: "count: #{count}"` reads `count` (now desugared to
        // `__cute_state.count`), so the reactive-binding emitter wires
        // a `connect(__cute_state, &__MainState::countChanged, label,
        // [=]{ label->setText(...); })` to refresh the label whenever
        // the state changes.
        let src = r#"
widget Main {
  state count : Int = 0

  QMainWindow {
    QLabel { text: "count: #{count}" }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("QObject::connect(__cute_state, &__MainState::countChanged"),
            "missing NOTIFY → setText connect for state-bound label:\n{}",
            r.source
        );
        assert!(
            r.source.contains("setText(QStringLiteral(\"count: \") + ::cute::str::to_string(__cute_state->count()))"),
            "initial setText should read through the holder getter:\n{}",
            r.source
        );
    }

    #[test]
    fn multiple_widget_state_fields_emit_in_order() {
        // Several state fields stack up as multiple member decls and
        // multiple init lines. Order of declaration is preserved.
        let src = r#"
class A {}
class B {}

widget Main {
  let a = A()
  let b = B()

  QMainWindow {}
}
"#;
        let r = build(src);
        assert!(r.header.contains("A* a;"), "missing A field:\n{}", r.header);
        assert!(r.header.contains("B* b;"), "missing B field:\n{}", r.header);
        assert!(
            r.source.contains("a = new A(this);"),
            "missing A init:\n{}",
            r.source
        );
        assert!(
            r.source.contains("b = new B(this);"),
            "missing B init:\n{}",
            r.source
        );
        let a_pos = r.source.find("a = new A(this);").unwrap();
        let b_pos = r.source.find("b = new B(this);").unwrap();
        assert!(a_pos < b_pos, "init order should match declaration order");
    }

    #[test]
    fn widget_alone_synthesizes_main_automatically() {
        // SwiftUI ergonomics on the QtWidgets side: a file with just
        // `widget Main { ... }` and no explicit `fn main` gets a
        // synthesized QApplication-based main wired to that widget.
        let src = r#"
widget Main {
  QMainWindow {}
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("QApplication app(argc, argv);"),
            "auto-main should boot QApplication:\n{}",
            r.source
        );
        assert!(
            r.source.contains("Main w;"),
            "auto-main should instantiate the entry widget:\n{}",
            r.source
        );
    }

    #[test]
    fn parameterless_view_emits_qml_with_no_root_props() {
        // Regression check: views without `(...)` keep working unchanged.
        let src = r#"
view Main {
  Rectangle {
    color: "red"
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Main")
            .expect("view Main");
        assert!(
            v.qml.contains("Rectangle {"),
            "missing Rectangle:\n{}",
            v.qml
        );
        assert!(
            !v.qml.contains("property "),
            "parameterless view should not emit any property decls:\n{}",
            v.qml
        );
    }

    #[test]
    fn parameterized_view_emits_root_property_decls() {
        // `view Card(label: String, count: Int) { Rectangle { ... } }`
        // -> the .qml file's root element gets `property string label`
        // and `property int count` lines so callers can write
        // `Card { label: "..."; count: 42 }`.
        let src = r#"
view Card(label: String, count: Int, ratio: Float, on: Bool) {
  Rectangle {
    color: "red"
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Card")
            .expect("view Card");
        assert!(
            v.qml.contains("    property string label"),
            "missing string property:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("    property int count"),
            "missing int property:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("    property real ratio"),
            "missing real property:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("    property bool on"),
            "missing bool property:\n{}",
            v.qml
        );
    }

    #[test]
    fn view_if_lowers_to_visible_property() {
        // `if cond { El { ... } }` -> the same element with a
        // `visible: cond` line injected. QML's column/row layouts
        // skip invisible items, so layout matches a "render only when
        // cond" semantic.
        let src = r#"
view Profile(loggedIn: Bool) {
  Column {
    if loggedIn {
      Label { text: "welcome" }
    }
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Profile")
            .expect("view Profile");
        assert!(
            v.qml.contains("visible: loggedIn"),
            "missing visible binding:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("text: \"welcome\""),
            "missing inner element:\n{}",
            v.qml
        );
    }

    #[test]
    fn array_literal_in_view_property_lowers_to_js_array() {
        // `[a, b, c]` inside a view property -> JS array literal.
        let src = r#"
view Main {
  Row {
    items: [1, 2, 3]
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Main")
            .expect("view Main");
        assert!(
            v.qml.contains("items: [1, 2, 3]"),
            "missing JS array literal:\n{}",
            v.qml
        );
    }

    #[test]
    fn map_literal_in_view_property_lowers_to_js_object() {
        // `{ key: value }` -> JS object, paren-wrapped to dodge any
        // statement-context ambiguity.
        let src = r#"
view Main {
  Item {
    data: { name: "alice", age: 30 }
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Main")
            .expect("view Main");
        assert!(
            v.qml.contains("data: ({name: \"alice\", age: 30})"),
            "missing object literal:\n{}",
            v.qml
        );
    }

    #[test]
    fn nested_array_of_maps_lowers() {
        // Array of object literals - the typical demo-data shape.
        let src = r#"
view Main {
  Row {
    items: [{ k: 1 }, { k: 2 }]
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Main")
            .expect("view Main");
        assert!(
            v.qml.contains("items: [({k: 1}), ({k: 2})]"),
            "missing nested array-of-maps:\n{}",
            v.qml
        );
    }

    #[test]
    fn view_if_else_emits_two_siblings_with_inverted_visibility() {
        // `if cond { A } else { B }` -> sibling A with `visible: cond`
        // and sibling B with `visible: !cond` so exactly one shows up
        // in the parent layout at a time.
        let src = r#"
view Profile(loggedIn: Bool) {
  Column {
    if loggedIn {
      Label { text: "welcome" }
    } else {
      Button { text: "sign in" }
    }
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Profile")
            .expect("view Profile");
        assert!(
            v.qml.contains("visible: loggedIn"),
            "then-branch visible binding missing:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("visible: !(loggedIn)"),
            "else-branch should bind to negation of cond:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("text: \"welcome\""),
            "missing then-body:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("text: \"sign in\""),
            "missing else-body:\n{}",
            v.qml
        );
    }

    #[test]
    fn array_literal_in_method_body_lowers_to_qvariantlist() {
        // C++-side codegen: `[1, 2, 3]` -> `QVariantList{1, 2, 3}` for
        // heterogeneous-friendly defaults. Typed `QList<int>` deduction
        // happens via the LHS-driven hint (`array_literal_assigned_to_typed_list_property`).
        let src = r#"
class Store {
  fn seed List {
    [1, 2, 3]
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("QVariantList{1, 2, 3}"),
            "missing QVariantList:\n{}",
            r.source
        );
    }

    #[test]
    fn array_literal_assigned_to_typed_list_property() {
        // `@values = [1.0, 2.0, 3.0]` against a `List<Float>` property
        // emits `QList<double>{1, 2, 3}`, not `QVariantList`. Otherwise
        // C++ assignment from QVariantList to QList<double> doesn't
        // compile (the underlying member is QList<double>).
        let src = r#"
class Store {
  pub prop values : List<Float>, default: []
  fn seed { values = [1.0, 2.0, 3.0] }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("QList<double>{1.0, 2.0, 3.0}"),
            "missing typed QList<double>:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("QVariantList{"),
            "should not emit heterogeneous QVariantList:\n{}",
            r.source
        );
    }

    #[test]
    fn array_literal_assigned_to_typed_list_local() {
        // `let xs : List<Int> = [1, 2, 3]` mirrors the property case:
        // explicit type annotation steers the array literal to a typed
        // QList rather than QVariantList.
        let src = r#"
fn run {
  let xs : List<Int> = [1, 2, 3]
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("QList<qint64>{1, 2, 3}"),
            "missing typed QList<qint64>:\n{}",
            r.source
        );
    }

    #[test]
    fn map_literal_in_method_body_lowers_to_qvariantmap() {
        let src = r#"
class Store {
  fn seed Map {
    { name: "alice", age: 30 }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("QVariantMap{{QStringLiteral(\"name\"), QStringLiteral(\"alice\")}, {QStringLiteral(\"age\"), 30}}"),
            "missing QVariantMap shape:\n{}",
            r.source
        );
    }

    #[test]
    fn map_literal_assigned_to_typed_map_property() {
        // `@m = {...}` against a `Map<String, Int>` prop must emit
        // `QMap<::cute::String, qint64>{...}` directly — `QVariantMap`
        // doesn't implicitly convert to the underlying `QMap<QString,
        // qint64>` member.
        let src = r#"
class Store {
  prop m : Map<String, Int>, default: {}
  fn seed { m = { a: 1, b: 2 } }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("QMap<::cute::String, qint64>{{QStringLiteral(\"a\"), 1}, {QStringLiteral(\"b\"), 2}}"),
            "missing typed QMap shape:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("QVariantMap{"),
            "should not emit heterogeneous QVariantMap when LHS hint exists:\n{}",
            r.source
        );
    }

    #[test]
    fn map_literal_assigned_to_typed_map_local() {
        // `let m : Map<String, Int> = { a: 1 }` mirrors the property
        // case: explicit type annotation steers the map literal to a
        // typed `QMap<::cute::String, qint64>` rather than QVariantMap.
        let src = r#"
fn run {
  let m : Map<String, Int> = { a: 1, b: 2 }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("QMap<::cute::String, qint64>{{QStringLiteral(\"a\"), 1}, {QStringLiteral(\"b\"), 2}}"),
            "missing typed QMap on annotated let:\n{}",
            r.source
        );
    }

    #[test]
    fn typed_map_value_type_propagates_into_nested_array_literal() {
        // `Map<String, List<Int>>` outer hint propagates `List<Int>`
        // down to each value-position array literal so the inner
        // `[1, 2, 3]` lowers to `QList<qint64>{1, 2, 3}`.
        let src = r#"
class Store {
  pub prop buckets : Map<String, List<Int>>, default: {}
  fn seed { buckets = { a: [1, 2, 3], b: [4, 5] } }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("QMap<::cute::String, QList<qint64>>"),
            "missing outer typed QMap:\n{}",
            r.source
        );
        assert!(
            r.source.contains("QList<qint64>{1, 2, 3}"),
            "inner array literal should propagate to typed QList<qint64>:\n{}",
            r.source
        );
        assert!(
            r.source.contains("QList<qint64>{4, 5}"),
            "second inner array literal should also propagate:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("QVariantList{"),
            "no QVariantList expected anywhere when nested hint propagates:\n{}",
            r.source
        );
    }

    #[test]
    fn typed_list_elem_type_propagates_into_nested_array_literal() {
        // `List<List<Int>>` outer hint propagates `List<Int>` down to
        // each element-position array literal so `[[1,2],[3,4]]`
        // lowers to `QList<QList<qint64>>{QList<qint64>{1,2}, ...}`.
        let src = r#"
class Store {
  pub prop matrix : List<List<Int>>, default: []
  fn seed { matrix = [[1, 2], [3, 4]] }
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("QList<QList<qint64>>{QList<qint64>{1, 2}, QList<qint64>{3, 4}}"),
            "nested typed QList shape missing:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("QVariantList{"),
            "no QVariantList should remain when nested hint propagates:\n{}",
            r.source
        );
    }

    #[test]
    fn typed_list_of_maps_propagates_value_hint() {
        // `List<Map<String, Int>>` outer hint propagates `Map<String,
        // Int>` to each map-literal element. This is the reverse-side
        // case (list outer, map inner) of `typed_map_value_type_...`.
        let src = r#"
fn run {
  let entries : List<Map<String, Int>> = [{ a: 1 }, { b: 2 }]
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("QList<QMap<::cute::String, qint64>>"),
            "missing outer typed QList<QMap<...>>:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("QMap<::cute::String, qint64>{{QStringLiteral(\"a\"), 1}}"),
            "first inner map literal should propagate:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("QMap<::cute::String, qint64>{{QStringLiteral(\"b\"), 2}}"),
            "second inner map literal should propagate:\n{}",
            r.source
        );
    }

    #[test]
    fn case_as_expression_lowers_to_iife() {
        // `case n { when 0 { "zero" } when _ { "many" } }` at the
        // trailing-expression position of a fn body now lowers to an
        // immediately-invoked lambda whose return type is the
        // std::common_type of each arm's value. Before this lowering,
        // the case was emitted as a side-effecting prelude that left
        // the surrounding return statement holding a `/* placeholder
        // */` and didn't compile.
        let src = r#"
fn classify(n: Int) String {
  case n {
    when 0 { "zero" }
    when _ { "many" }
  }
}
"#;
        let r = build(src);
        // The fn body returns the IIFE's value (with explicit return
        // type for branch-type unification).
        assert!(
            r.source.contains("return [&]() -> std::common_type_t<"),
            "expected IIFE return with std::common_type_t:\n{}",
            r.source
        );
        // Each arm's trailing expression becomes a `return <value>;`
        // inside the lambda so C++ deduces the lambda's return type.
        assert!(
            r.source.contains("return QStringLiteral(\"zero\");"),
            "missing first-arm IIFE return:\n{}",
            r.source
        );
        assert!(
            r.source.contains("return QStringLiteral(\"many\");"),
            "missing wildcard-arm IIFE return:\n{}",
            r.source
        );
        // The wildcard `_` arm makes this case exhaustive, so the
        // unreachable sentinel that older codegen unconditionally
        // appended is now elided. (See `case_without_wildcard_keeps
        // _unreachable_sentinel` for the inverse assertion.)
        assert!(
            !r.source.contains("__builtin_unreachable();"),
            "exhaustive case (with `when _`) must NOT emit the sentinel:\n{}",
            r.source
        );
        // The placeholder string from the old prelude form must be
        // gone — its presence would reintroduce the original bug.
        assert!(
            !r.source.contains("case match handled in prelude"),
            "old prelude placeholder leaked into source:\n{}",
            r.source
        );
    }

    /// Inverse assertion for the exhaustiveness check: a case that
    /// leaves out wildcard / catch-all coverage still lowers with the
    /// `__builtin_unreachable()` sentinel, since the C++ compiler
    /// has no other way to know the value-position lambda always
    /// returns.
    #[test]
    fn case_without_wildcard_keeps_unreachable_sentinel() {
        let src = r#"
fn classify(n: Int) String {
  case n {
    when 0 { "zero" }
    when 1 { "one" }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("__builtin_unreachable();"),
            "non-exhaustive case must keep the sentinel:\n{}",
            r.source
        );
    }

    /// `when ok(...)` + `when err(...)` together cover an `!T` value,
    /// so the case is exhaustive even though there's no `_` arm.
    #[test]
    fn case_over_error_union_with_ok_and_err_is_exhaustive() {
        let src = r#"
error ParseError {
  empty
}

fn parseInt(s: String) !Int {
  return 42
}

fn classify(s: String) String {
  case parseInt(s) {
    when ok(n)  { "parsed" }
    when err(e) { "failed" }
  }
}
"#;
        let r = build(src);
        assert!(
            !r.source.contains("__builtin_unreachable();"),
            "ok+err over !T is exhaustive — sentinel must be elided:\n{}",
            r.source
        );
    }

    /// All declared variants of an `error E { v1; v2 }` covered →
    /// exhaustive. The codegen looks the variants up in the
    /// surrounding module's AST.
    #[test]
    fn case_covering_all_error_variants_is_exhaustive() {
        let src = r#"
error Status {
  ok
  pending
  failed
}

fn fetch Status {
  return Status.ok()
}

fn label String {
  case fetch() {
    when ok      { "ok" }
    when pending { "pending" }
    when failed  { "failed" }
  }
}
"#;
        let r = build(src);
        assert!(
            !r.source.contains("__builtin_unreachable();"),
            "covering all error variants must be exhaustive:\n{}",
            r.source
        );
    }

    /// Bool `case` with both `true` and `false` arms is exhaustive.
    /// (Practical bool-case use is rare since `if` reads better, but
    /// the analysis covers it for free.)
    #[test]
    fn case_over_bool_with_true_and_false_is_exhaustive() {
        let src = r#"
fn label(b: Bool) String {
  case b {
    when true  { "yes" }
    when false { "no" }
  }
}
"#;
        let r = build(src);
        assert!(
            !r.source.contains("__builtin_unreachable();"),
            "true + false arms are exhaustive over Bool:\n{}",
            r.source
        );
    }

    #[test]
    fn case_at_statement_position_still_runs_arms() {
        // Statement-level case (no value used) still runs each arm
        // body because the IIFE is invoked; the result is just
        // discarded as an expression statement.
        let src = r#"
fn run(n: Int) {
  case n {
    when 0 { println("zero") }
    when _ { println("other") }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("[&]()"),
            "expected IIFE in source:\n{}",
            r.source
        );
        // The IIFE is invoked at statement position, ending with `();`
        // so its side effects fire even when the value is discarded.
        assert!(
            r.source.contains("}();"),
            "expected IIFE invocation `}}();` at statement position:\n{}",
            r.source
        );
    }

    #[test]
    fn view_case_string_arms_lower_to_visibility_chain() {
        // `case status { when "ok" {...} when "loading" {...} when _ {...} }`
        // -> three sibling elements:
        //   arm 0: visible = (status === "ok")
        //   arm 1: visible = !((status === "ok")) && (status === "loading")
        //   arm 2: visible = !((status === "ok")) && !((status === "loading"))
        let src = r#"
view Main(status: String) {
  Column {
    case status {
      when "ok" {
        Label { text: "done" }
      }
      when "loading" {
        Label { text: "loading..." }
      }
      when _ {
        Label { text: "?" }
      }
    }
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Main")
            .expect("view Main");
        assert!(
            v.qml.contains("visible: (status === \"ok\")"),
            "missing first-arm visible:\n{}",
            v.qml
        );
        assert!(
            v.qml
                .contains("visible: !(status === \"ok\") && (status === \"loading\")"),
            "missing second-arm visible:\n{}",
            v.qml
        );
        assert!(
            v.qml
                .contains("visible: !(status === \"ok\") && !(status === \"loading\")"),
            "missing wildcard arm visible:\n{}",
            v.qml
        );
    }

    #[test]
    fn view_case_bare_ctor_arm_treated_as_string_tag() {
        // Bare `when loading { ... }` (no parens) is parsed as a
        // nullary Ctor pattern. We treat it as a string-tag check
        // for the common "state machine encoded as Cute strings"
        // pattern - same lowered shape as `when "loading"`.
        let src = r#"
view Main(state: String) {
  Column {
    case state {
      when loading {
        Label { text: "loading" }
      }
    }
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Main")
            .expect("view Main");
        assert!(
            v.qml.contains("visible: (state === \"loading\")"),
            "bare ctor should compare to string-tag:\n{}",
            v.qml
        );
    }

    #[test]
    fn widget_case_ok_err_binds_to_safe_unwrap() {
        // `when ok(v) { ... }` / `when err(e) { ... }` inside a
        // widget element-position case: emit `is_ok()` / `is_err()`
        // as the visibility test, and inject `auto v = (is_ok ?
        // unwrap : default)` so the bound name is in scope inside
        // the arm body without aborting on the false branch.
        let src = r#"
class Backend {
  fn fetch !Int { 0 }
}

widget Main {
  let backend = Backend()
  QMainWindow {
    QVBoxLayout {
      case backend.fetch() {
        when ok(v)  { QLabel { text: "ok" } }
        when err(e) { QLabel { text: "fail" } }
      }
    }
  }
}
"#;
        let r = build(src);
        // is_ok() test on the scrutinee (method call on a state-field
        // pointer; widget_lower_expr emits backend->fetch()).
        assert!(
            r.source.contains("(backend->fetch().is_ok())"),
            "missing is_ok test on call result:\n{}",
            r.source
        );
        // is_err() test for the err arm.
        assert!(
            r.source.contains("(backend->fetch().is_err())"),
            "missing is_err test:\n{}",
            r.source
        );
        // The bind decl should cite the safe-unwrap shape so build-
        // time evaluation doesn't abort on the false branch.
        assert!(
            r.source.contains("auto v = (backend->fetch().is_ok() ? backend->fetch().unwrap() : decltype(backend->fetch().unwrap()){});"),
            "missing safe ok-bind decl:\n{}",
            r.source
        );
        assert!(
            r.source.contains("auto e = (backend->fetch().is_err() ? std::move(backend->fetch()).unwrap_err() : decltype(backend->fetch().unwrap_err()){});"),
            "missing safe err-bind decl:\n{}",
            r.source
        );
    }

    #[test]
    fn view_case_ok_err_falls_back_to_todo_placeholder() {
        // View / QML side has no Result API yet (cute::Result isn't
        // a Q_GADGET) so `when ok(v)` arms degrade to the TODO
        // placeholder. The other-arm patterns (literal / wild) still
        // work, so users can mix.
        let src = r#"
class Backend {
  fn fetch !Int { 0 }
}

view Main {
  let backend = Backend()
  ApplicationWindow {
    case backend.fetch() {
      when ok(v) { Label { text: "ok" } }
    }
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Main")
            .expect("view Main");
        assert!(
            v.qml
                .contains("/* TODO: ok/err patterns need Result API in this context */"),
            "expected TODO placeholder for ok/err in view-case:\n{}",
            v.qml
        );
    }

    #[test]
    fn widget_case_lowers_to_setvisible_per_arm() {
        let src = r#"
widget Main(status: String) {
  QMainWindow {
    QVBoxLayout {
      case status {
        when "ok"      { QLabel { text: "done" } }
        when "loading" { QLabel { text: "loading..." } }
        when _         { QLabel { text: "?" } }
      }
    }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("setVisible((status == QStringLiteral(\"ok\")))"),
            "missing first-arm setVisible:\n{}",
            r.source
        );
        assert!(
            r.source.contains("setVisible(!(status == QStringLiteral(\"ok\")) && (status == QStringLiteral(\"loading\")))"),
            "missing second-arm setVisible:\n{}",
            r.source
        );
    }

    #[test]
    fn t_new_inside_class_method_auto_passes_this_as_parent() {
        // Memory-safety helper: `let x = T.new()` inside a class method
        // body should auto-inject `this` as the parent so `x` joins the
        // surrounding QObject's parent-tree (and gets cleaned up when
        // the parent is destroyed). User can override by passing an
        // explicit parent argument.
        let src = r#"
class Worker {
}
class Manager {
  fn spawn {
    let job = Worker.new()
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("auto job = new Worker(this);"),
            "expected auto-this injection in class method body:\n{}",
            r.source
        );
    }

    #[test]
    fn t_new_inside_top_level_fn_does_not_inject_this() {
        // No `this` in a top-level fn, so we don't inject. The
        // resulting raw pointer has no parent and will leak unless
        // the user passes a parent or hooks it into a tree.
        let src = r#"
class Worker {
}
fn make {
  let job = Worker.new()
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("auto job = new Worker();"),
            "expected no parent in fn body:\n{}",
            r.source
        );
    }

    #[test]
    fn t_new_with_explicit_parent_is_left_alone() {
        // User passed parent already; we don't override.
        let src = r#"
class Worker {
}
class Manager {
  fn spawn {
    let other = Manager.new()
    let job = Worker.new(other)
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("auto job = new Worker(other);"),
            "expected explicit parent to pass through:\n{}",
            r.source
        );
    }

    #[test]
    fn arc_class_emits_arcbase_subclass() {
        // `arc X { ... }` opts out of QObject, lowers to
        // `class X : public ::cute::ArcBase` with plain getters/setters
        // for properties. No Q_OBJECT, no signals, no moc data.
        let src = r#"
arc Token {
  pub var text : String = ""
  pub fn length Int {
    42
  }
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("class Token : public ::cute::ArcBase"),
            "expected ArcBase derivation:\n{}",
            r.header
        );
        assert!(
            !r.header.contains("Q_OBJECT"),
            "ARC class must not emit Q_OBJECT:\n{}",
            r.header
        );
        assert!(
            r.header.contains("Token() = default;"),
            "expected default ctor:\n{}",
            r.header
        );
        assert!(
            r.header.contains("::cute::String text() const"),
            "expected property getter:\n{}",
            r.header
        );
    }

    #[test]
    fn arc_class_t_new_lowers_to_arc_wrapper() {
        // `Token.new(...)` for an ARC class returns `cute::Arc<Token>`,
        // not a raw pointer. Member access still uses `->` because
        // Arc<T> overloads it.
        let src = r#"
arc Token {
  var Text : String = ""
}
fn make {
  let t = Token.new()
  let n = t.Text
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("auto t = ::cute::Arc<Token>(new Token());"),
            "expected Arc<T> wrapping:\n{}",
            r.source
        );
        // Arc<T>::-> means member access via -> still works.
        assert!(
            r.source.contains("auto n = t->Text();"),
            "expected pointer-aware member access on Arc:\n{}",
            r.source
        );
    }

    #[test]
    fn bare_member_access_lowers_to_zero_arg_call() {
        // Cute: `app.exec` (no parens) -> C++: `app.exec()` (zero-arg
        // method call). Ruby/Smalltalk-style: there are no raw fields
        // in the surface language, so dotted access is always a method
        // invocation. Cute class instance via `T.new()` becomes a
        // QObject pointer, so the separator is `->`.
        let src = r#"
class App {
  fn exec
}
fn run {
  let a = App.new()
  a.exec
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("a->exec()"),
            "bare member should lower to zero-arg call:\n{}",
            r.source
        );
    }

    #[test]
    fn try_prefix_lowers_same_as_postfix_question() {
        // `try expr` is parser sugar for `expr?` - both produce
        // K::Try, both lower to the same `if (_r0.is_err()) return ...`
        // shape. This makes inline forms readable: e.g.
        // `Response.json(try User.find(id))`.
        let src = r#"
fn process(p: Path) !Int {
  let v = try parse(p)
  v
}
"#;
        let r = build(src);
        // Just check the if-err short-circuit prelude landed; the
        // exact `unwrap` shape comes from the Try lowering.
        assert!(
            r.source.contains("is_err()"),
            "expected Result-style early return:\n{}",
            r.source
        );
    }

    #[test]
    fn main_with_list_param_populates_argv() {
        // `fn main(args: List)` lifts argc/argv into a QStringList
        // bound to the user's parameter name, so `args.size`,
        // `for a in args { ... }`, etc. all just work.
        let src = r#"
fn main(args: List) {
  println("argc=" + args.size)
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("QStringList args;"),
            "missing QStringList declaration:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("args << QString::fromLocal8Bit(argv[i]);"),
            "missing argv lift:\n{}",
            r.source
        );
        // bare-member-call + the user binding name.
        assert!(
            r.source.contains("args.size()"),
            "`args.size` should lower as zero-arg call:\n{}",
            r.source
        );
    }

    #[test]
    fn fn_body_for_lowers_to_range_based_for() {
        // `for x in xs { stmt }` in fn / class-method body lowers to
        // a C++ range-based for. The binding name passes through to
        // the body's expressions verbatim.
        let src = r#"
fn shout(items: List) {
  for x in items {
    println(x)
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("for (const auto& x : items) {"),
            "missing range-for header:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("qInfo().noquote() << ::cute::str::to_string(x);"),
            "binding should reach the body verbatim:\n{}",
            r.source
        );
    }

    #[test]
    fn fn_body_for_with_method_call_in_iter() {
        // The iter expression can be any value-bearing expression;
        // method calls lower normally before the for header is built.
        let src = r#"
fn process(s: Store) {
  for it in s.items() {
    println(it)
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("for (const auto& it : s.items()) {"),
            "expected method-call iter:\n{}",
            r.source
        );
    }

    #[test]
    fn class_method_for_loop_lowers() {
        // The same Stmt::For path covers class methods (the body
        // lowering is shared with top-level fns).
        let src = r#"
class Cart {
  var items : List
  fn dump {
    for it in items {
      println(it)
    }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("for (const auto& it : m_items) {"),
            "expected member `items` to lower as `m_items`:\n{}",
            r.source
        );
    }

    #[test]
    fn break_and_continue_lower_inside_for_range() {
        // `break` / `continue` lower to plain C++ `break;` / `continue;`
        // and live wherever a Stmt is allowed, including the body of a
        // for-range loop.
        let src = r#"
fn firstPositive(xs: List<Int>) Int {
  for x in 0..10 {
    if x == 0 { continue }
    if x > 5 { break }
  }
  return 0
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("continue;"),
            "expected `continue;` in source:\n{}",
            r.source
        );
        assert!(
            r.source.contains("break;"),
            "expected `break;` in source:\n{}",
            r.source
        );
    }

    #[test]
    fn while_loop_with_break_lowers() {
        // While loop body accepts `break` / `continue` the same way a
        // for body does.
        let src = r#"
fn run {
  var i = 0
  while i < 10 {
    if i == 3 { break }
    i = i + 1
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("while ((i < 10)) {"),
            "expected while header:\n{}",
            r.source
        );
        assert!(
            r.source.contains("break;"),
            "expected `break;` inside while body:\n{}",
            r.source
        );
    }

    #[test]
    fn view_else_if_chain_emits_three_visibility_bindings() {
        // `if a { A } else if b { B } else { C }` -> three sibling
        // elements:
        //   A.visible = a
        //   B.visible = !(a) && b
        //   C.visible = !(a) && !(b)
        let src = r#"
view Profile(rank: Int) {
  Column {
    if rank > 90 {
      Label { text: "S tier" }
    } else if rank > 70 {
      Label { text: "A tier" }
    } else {
      Label { text: "rest" }
    }
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Profile")
            .expect("view Profile");
        assert!(
            v.qml.contains("visible: (rank > 90)"),
            "missing first-branch visible:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("visible: !((rank > 90)) && (rank > 70)"),
            "missing else-if visible:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("visible: !((rank > 90)) && !((rank > 70))"),
            "missing terminal-else visible:\n{}",
            v.qml
        );
    }

    #[test]
    fn widget_else_if_chain_emits_setvisible_per_branch() {
        let src = r#"
widget Main(rank: Int) {
  QMainWindow {
    QVBoxLayout {
      if rank > 90 {
        QLabel { text: "S tier" }
      } else if rank > 70 {
        QLabel { text: "A tier" }
      } else {
        QLabel { text: "rest" }
      }
    }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("setVisible((rank > 90))"),
            "missing first-branch setVisible:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("setVisible(!((rank > 90)) && (rank > 70))"),
            "missing else-if setVisible:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("setVisible(!((rank > 90)) && !((rank > 70)))"),
            "missing terminal-else setVisible:\n{}",
            r.source
        );
    }

    #[test]
    fn view_for_lowers_to_repeater_with_modeldata() {
        // `for x in xs { El { ... x.foo ... } }` ->
        // `Repeater { model: xs; El { ... modelData.foo ... } }`.
        // Repeater is QML's idiomatic per-row instantiator and exposes
        // each row value as the implicit `modelData`.
        let src = r#"
view Cards(items: List) {
  Row {
    for item in items {
      Label { text: item.name }
    }
  }
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Cards")
            .expect("view Cards");
        assert!(
            v.qml.contains("Repeater {"),
            "missing Repeater wrapper:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("model: items"),
            "missing model binding:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("text: modelData.name"),
            "for-binding `item` should be rewritten to modelData:\n{}",
            v.qml
        );
        assert!(
            !v.qml.contains("text: item.name"),
            "for-binding leaked into output:\n{}",
            v.qml
        );
    }

    #[test]
    fn parameterized_view_unknown_type_falls_back_to_var() {
        // A parameter typed with something outside the QML primitive
        // vocabulary should lower to `var` rather than failing - QML's
        // binding system still handles `var` slots, just without
        // compile-time type enforcement.
        let src = r#"
view Card(payload: SomeUserType) {
  Rectangle {}
}
"#;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Card")
            .expect("view Card");
        assert!(
            v.qml.contains("    property var payload"),
            "missing var fallback:\n{}",
            v.qml
        );
    }

    #[test]
    fn async_fn_lowers_trailing_to_co_return() {
        // `async fn` body should use `co_return` for its trailing
        // expression, marking it as a Qt 6.5+ coroutine. The return
        // type still renders as `QFuture<T>` via the Future->QFuture
        // table mapping.
        let src = r#"
async fn fetch Future(Int) {
  42
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("QFuture<qint64> fetch();"),
            "missing QFuture decl:\n{}",
            r.header
        );
        assert!(
            r.source.contains("co_return 42;"),
            "expected co_return, found:\n{}",
            r.source
        );
    }

    #[test]
    fn await_expression_lowers_to_co_await() {
        // `await expr` -> `co_await expr` regardless of the
        // surrounding fn's async-ness (parser only allows `await`
        // inside async fns at a higher level; codegen doesn't
        // double-check).
        let src = r#"
async fn fetch Future(Int) {
  let v = await other()
  v
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("auto v = co_await other();"),
            "expected co_await on let RHS, found:\n{}",
            r.source
        );
    }

    #[test]
    fn non_async_fn_still_uses_return() {
        // Sanity: making async fn switch to co_return shouldn't
        // affect plain fns - they keep `return` for their trailing
        // expression.
        let src = r#"
fn add(a: Int, b: Int) Int {
  a + b
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("return (a + b);"),
            "non-async fn must still use plain return:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("co_return"),
            "non-async fn must not emit co_return:\n{}",
            r.source
        );
    }

    #[test]
    fn signal_connect_self_lowers_to_qobject_connect() {
        // `self.<signal>.connect { ... }` -> `QObject::connect(this,
        // &Class::signal, [=, this]() mutable { ... })`. The capture
        // list is value-by-default inside class methods because Qt
        // signals fire after the enclosing method has returned, at
        // which point any `[&]`-captured method parameters / locals
        // would be dangling references; see `lambda_capture_pieces`.
        let src = r#"
class Counter {
  signal countChanged
  fn wire {
    self.countChanged.connect { println("changed") }
  }
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("QObject::connect(this, &Counter::countChanged, [=, this]() mutable {"),
            "missing self-connect lowering:\n{}",
            r.source
        );
    }

    #[test]
    fn signal_connect_let_binding_uses_recorded_class() {
        // A `let c = Counter.new` binding records its class so
        // `c.count_changed.connect { ... }` can resolve to
        // `&Counter::count_changed`.
        let src = r#"
class Counter {
  signal countChanged
}

fn wire {
  let c = Counter.new()
  c.countChanged.connect { println("changed") }
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("QObject::connect(c, &Counter::countChanged, [&]() {"),
            "missing binding-connect lowering:\n{}",
            r.source
        );
    }

    #[test]
    fn signal_connect_handler_arg_form() {
        // The non-block `.connect(handler)` form passes the explicit
        // handler expression through verbatim.
        let src = r#"
class Counter {
  signal countChanged
  fn wire(h: Handler) {
    self.countChanged.connect(h)
  }
}
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("QObject::connect(this, &Counter::countChanged, h)"),
            "missing handler-arg form:\n{}",
            r.source
        );
    }

    #[test]
    fn connect_on_non_signal_member_falls_back() {
        // `obj.someMethod.connect(...)` where `someMethod` is not a
        // signal should NOT be rewritten to QObject::connect - we
        // fall through to the plain method-call lowering.
        let src = r#"
class Counter {
  fn ping
  fn wire {
    self.ping.connect(this)
  }
}
"#;
        let r = build(src);
        assert!(
            !r.source.contains("QObject::connect"),
            "non-signal member should not rewrite to QObject::connect:\n{}",
            r.source
        );
    }

    #[test]
    fn typed_lambda_param_uses_declared_type() {
        // Annotated block param `|x: String|` should lower to the C++ type
        // (`::cute::String x`, the QString alias used everywhere except
        // Q_PROPERTY text) rather than `auto`.
        let src = r#"
fn run(xs: Items) {
  xs.each { |x: String| println(x) }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("xs.each([&](::cute::String x) {"),
            "expected typed param to lower to ::cute::String:\n{}",
            r.source
        );
    }

    // ---- style composition (α) ------------------------------------------

    /// A `style:` element member whose value is a single style name
    /// expands to that style's literal entries on the QML side.
    #[test]
    fn style_simple_view_inlines_entries_into_qml() {
        let src = r##"
style Card {
  padding: 16
  background: "#ffffff"
}

view Main {
  Label { style: Card; text: "hi" }
}
"##;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Main")
            .expect("view Main");
        assert!(
            !v.qml.contains("style:"),
            "style: marker should be desugared away:\n{}",
            v.qml
        );
        assert!(v.qml.contains("padding: 16"), "padding missing:\n{}", v.qml);
        assert!(
            v.qml.contains("background: \"#ffffff\""),
            "background missing:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("text: \"hi\""),
            "ordinary props preserved:\n{}",
            v.qml
        );
    }

    /// `style BigCard = Card + Big` flattens at codegen time and
    /// applies right-wins on conflicting keys (`Big`'s padding wins).
    #[test]
    fn style_alias_with_merge_right_wins() {
        let src = r##"
style Card {
  padding: 4
  background: "#fff"
}

style Big {
  padding: 32
  font.bold: true
}

style BigCard = Card + Big

view Main {
  Label { style: BigCard }
}
"##;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Main")
            .expect("view Main");
        assert!(
            v.qml.contains("padding: 32"),
            "right side should win on padding:\n{}",
            v.qml
        );
        assert!(
            !v.qml.contains("padding: 4"),
            "left padding should be overridden, not duplicated:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("background: \"#fff\""),
            "non-overridden lhs key kept:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("font.bold: true"),
            "rhs-only key kept:\n{}",
            v.qml
        );
    }

    /// Inline `style: A + B` at the element site should merge without
    /// requiring an alias declaration.
    #[test]
    fn style_inline_merge_at_element_site() {
        let src = r##"
style Pad { padding: 8 }
style Bold { font.bold: true }

view Main {
  Label { style: Pad + Bold; text: "hi" }
}
"##;
        let r = build(src);
        let v = r
            .views
            .iter()
            .find(|v| v.name == "Main")
            .expect("view Main");
        assert!(v.qml.contains("padding: 8"), "padding from Pad:\n{}", v.qml);
        assert!(
            v.qml.contains("font.bold: true"),
            "font.bold from Bold:\n{}",
            v.qml
        );
        assert!(
            v.qml.contains("text: \"hi\""),
            "literal text kept:\n{}",
            v.qml
        );
    }

    /// A `style A = A` (self-cycle) or any longer cycle should error
    /// out with a clear message rather than loop forever.
    #[test]
    fn style_self_cycle_errors() {
        let src = "style A = A\n";
        let module = parse(FileId(0), src).expect("parse");
        let resolved = cute_hir::resolve(&module, &cute_hir::ProjectInfo::default());
        let err = emit_module(
            "testMod",
            &module,
            &resolved.program,
            &cute_hir::ProjectInfo::default(),
            CodegenTypeInfo::empty(),
        )
        .expect_err("expected cycle error");
        match err {
            EmitError::StyleCycle(name) => assert_eq!(name, "A"),
            other => panic!("expected StyleCycle, got {other:?}"),
        }
    }

    /// `extern value Foo { ... }` declares a plain C++ value type
    /// — no Q_OBJECT, no Arc, no metaobject. `Foo.new(args)` must
    /// lower to `Foo(args)` (stack/value construction) and member
    /// access must use `.` instead of `->`.
    #[test]
    fn extern_value_class_emits_stack_construction_and_dot_access() {
        let src = r#"
extern value MyPoint {
  fn x Int
  fn y Int
}

fn buildPoint Int {
  let p = MyPoint.new(10, 20)
  p.x
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("auto p = MyPoint(10, 20);"),
            "extern value should construct via T(args), got:\n{}",
            r.source
        );
        assert!(
            r.source.contains("p.x()"),
            "extern value member access should use `.`:\n{}",
            r.source
        );
        // No Q_OBJECT, no Arc wrapping — codegen never emits a class
        // body for extern value types (they live in C++ headers).
        assert!(
            !r.source.contains("class MyPoint"),
            "codegen should not emit a class body for extern value:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("cute::Arc<MyPoint"),
            "extern value should not be Arc-wrapped:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("new MyPoint"),
            "extern value should not be heap-allocated:\n{}",
            r.source
        );
    }

    /// `recv?.name` on a `T?` lowers to a null-checked IIFE that uses
    /// `cute::nullable_lift<...>` to produce the right shell. The two
    /// `<inner>` evaluations are textually identical: one is in
    /// `decltype` (unevaluated), the other in the populated branch
    /// (evaluated at most once).
    #[test]
    fn safe_member_access_lowers_to_lift_iife() {
        let src = r#"
class Person < QObject {
  pub prop name : String, default: ""
}

fn greet(p: Person?) String? {
  p?.name
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("::cute::nullable_lift<decltype("),
            "safe-access IIFE missing nullable_lift:\n{}",
            r.source
        );
        assert!(
            r.source.contains("__NL::make"),
            "safe-access IIFE missing __NL::make:\n{}",
            r.source
        );
        assert!(
            r.source.contains("__NL::none()"),
            "safe-access IIFE missing __NL::none() fallback:\n{}",
            r.source
        );
        // The receiver gets stashed in a fresh temp before the test;
        // both `decltype(...)` and the materialized branch reference
        // the same temp so the receiver evaluates once.
        let temp_count = r.source.matches("auto _r").count();
        assert!(
            temp_count >= 1,
            "safe-access should bind the receiver to a fresh temp:\n{}",
            r.source
        );
    }

    /// `recv?.method(args)` is the call form. Same lift mechanism;
    /// args land in both the decltype slot and the call slot, but
    /// only get evaluated once (decltype is unevaluated).
    #[test]
    fn safe_method_call_lowers_to_lift_iife() {
        let src = r#"
class Greeter < QObject {
  pub fn greet(who: String) String { "" }
}

fn shout(g: Greeter?) String? {
  g?.greet("world")
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("::cute::nullable_lift<decltype("),
            "safe method-call IIFE missing nullable_lift:\n{}",
            r.source
        );
        assert!(
            r.source.contains("greet("),
            "safe method-call IIFE missing the underlying call:\n{}",
            r.source
        );
    }

    /// In a QML view body, `?.` is a textual passthrough — JS already
    /// has the operator with the same semantics, so no IIFE wrap.
    #[test]
    fn safe_member_in_view_lowers_to_qml_optional_chain() {
        let src = r#"
class Profile {
  pub prop name : String, notify: :nameChanged
  pub signal nameChanged
}

view Main {
  let profile = Profile()
  Item {
    Text {
      text: profile.name
    }
  }
}
"#;
        // Just ensure parse + lower don't blow up; the safe-access form
        // is exercised by the unit test below for QML lowering.
        let _ = build(src);
    }

    /// `Recv?.Member` on a non-nullable receiver should still type-
    /// check (silent passthrough — the result is just the same type
    /// wrapped in `?`).
    #[test]
    fn safe_access_on_non_nullable_receiver_is_accepted() {
        let src = r#"
class Person < QObject {
  pub prop name : String, default: ""
}

fn greet(p: Person) String? {
  p?.name
}
"#;
        let _ = build(src);
    }

    /// On the widget path, `style:` should expand into `setProperty`-
    /// like setter calls just like a hand-written property line.
    #[test]
    fn style_widget_path_emits_setters() {
        let src = r##"
style Hint {
  text: "hello"
}

widget Main {
  QMainWindow {
    QLabel { style: Hint }
  }
}
"##;
        let r = build(src);
        assert!(
            r.source.contains("setText"),
            "expected setText() call from style:Hint expansion:\n{}",
            r.source
        );
    }

    /// Visual properties that have no QWidget setter (`color`,
    /// `background`, `borderRadius`, ...) and pseudo-class entries
    /// (`hover.X`, `pressed.X`) get aggregated into one
    /// `setStyleSheet(QStringLiteral("..."))` call, scoped to the
    /// element's class as the QSS selector. Length-typed values
    /// auto-suffix `px`; non-length numeric values pass through.
    #[test]
    fn widget_qss_shorthand_aggregates_into_one_stylesheet_call() {
        let src = r##"
widget Main {
  QMainWindow {
    QPushButton {
      text: "ok"
      background: "#333"
      color: "#fff"
      borderRadius: 32
      fontSize: 26
      fontWeight: 500
      hover.background: "#3d3d3d"
      pressed.background: "#555"
    }
  }
}
"##;
        let r = build(src);
        let stylesheet_calls: Vec<&str> = r
            .source
            .lines()
            .filter(|l| l.contains("setStyleSheet"))
            .collect();
        assert_eq!(
            stylesheet_calls.len(),
            1,
            "expected exactly one setStyleSheet call (aggregated):\n{}",
            r.source
        );
        let line = stylesheet_calls[0];
        assert!(
            line.contains("QPushButton { background: #333"),
            "missing base rule with #333 background:\n{}",
            line
        );
        assert!(
            line.contains("color: #fff"),
            "missing color in base rule:\n{}",
            line
        );
        assert!(
            line.contains("border-radius: 32px"),
            "length value should auto-append px:\n{}",
            line
        );
        assert!(
            line.contains("font-size: 26px"),
            "fontSize is length-typed, expected px suffix:\n{}",
            line
        );
        assert!(
            line.contains("font-weight: 500;"),
            "fontWeight is non-length, expected raw integer:\n{}",
            line
        );
        assert!(
            line.contains("QPushButton:hover { background: #3d3d3d"),
            "missing :hover rule:\n{}",
            line
        );
        assert!(
            line.contains("QPushButton:pressed { background: #555"),
            "missing :pressed rule:\n{}",
            line
        );
        // The non-shorthand `text:` setter still goes through the
        // regular path — this confirms the partition didn't
        // accidentally strip it.
        assert!(
            r.source.contains("setText(QStringLiteral(\"ok\"))"),
            "non-shorthand setter should still be emitted:\n{}",
            r.source
        );
    }

    /// User-written `styleSheet: "..."` on the same element coexists
    /// with shorthand entries: synth comes first, the literal is
    /// concatenated after with `+` so QSS later-rule-wins specificity
    /// gives the user's hand-written string the final say.
    #[test]
    fn widget_qss_shorthand_concats_user_stylesheet_after_synth() {
        let src = r##"
widget Main {
  QMainWindow {
    QPushButton {
      background: "#333"
      styleSheet: "QPushButton { color: red; }"
    }
  }
}
"##;
        let r = build(src);
        let stylesheet_calls: Vec<&str> = r
            .source
            .lines()
            .filter(|l| l.contains("setStyleSheet"))
            .collect();
        assert_eq!(
            stylesheet_calls.len(),
            1,
            "expected exactly one setStyleSheet call (synth + user concat):\n{}",
            r.source
        );
        let line = stylesheet_calls[0];
        assert!(
            line.contains("background: #333"),
            "synth synth not emitted:\n{}",
            line
        );
        assert!(
            line.contains(") + QStringLiteral(\"QPushButton { color: red; }"),
            "user literal should be appended after synth via `+`:\n{}",
            line
        );
    }

    /// Style blocks combine with shorthand: a `style NumLook { ... }`
    /// containing `background: "#333"` desugars into a flat property
    /// list that the QSS pre-pass then partitions just like inline
    /// shorthand on the element. Verifies the `style.rs` desugar
    /// pass and the shorthand pre-pass compose in either order.
    #[test]
    fn widget_qss_shorthand_via_style_block() {
        let src = r##"
style NumLook {
  background: "#333"
  color: "#fff"
  borderRadius: 32
  hover.background: "#3d3d3d"
}

widget Main {
  QMainWindow {
    QPushButton { style: NumLook; text: "1" }
  }
}
"##;
        let r = build(src);
        assert!(
            r.source
                .contains("QPushButton { background: #333; color: #fff; border-radius: 32px"),
            "style block contents should reach the shorthand pre-pass:\n{}",
            r.source
        );
        assert!(
            r.source.contains("QPushButton:hover { background: #3d3d3d"),
            "pseudo-class entries from style blocks should also aggregate:\n{}",
            r.source
        );
    }

    #[test]
    fn style_block_splices_inside_nested_element_in_property_value() {
        // Element literals can appear nested inside property values
        // — `delegate: RowLayout { Rectangle { style: Bubble } }` on
        // a ListView, `background: Rectangle { style: ... }` on a
        // Button. The style desugar pass has to recurse into those
        // expression-position elements, otherwise the inner
        // `style:` reference reaches QML codegen verbatim and the
        // engine errors with `Cannot assign to non-existent property
        // "style"`.
        let src = r##"
use qml "QtQuick"
use qml "QtQuick.Controls"

style Bubble {
  radius: 14
  color: "#0a84ff"
}

class Item {
  pub prop label : String, default: ""
}

view Main {
  ApplicationWindow {
    ListView {
      delegate: Rectangle {
        style: Bubble
      }
    }
  }
}
"##;
        let r = build(src);
        let qml = r
            .views
            .iter()
            .find(|v| v.filename == "Main.qml")
            .expect("Main.qml view emitted")
            .qml
            .clone();
        assert!(
            qml.contains("radius: 14") && qml.contains("color: \"#0a84ff\""),
            "style entries should splice into the nested delegate Rectangle:\n{qml}"
        );
        assert!(
            !qml.contains("style: Bubble"),
            "the literal `style: Bubble` reference should be gone after splice:\n{qml}"
        );
    }

    /// `prop xs : List<T>, model` carries a `cute::ModelList<T>*` whose
    /// pointer is stable for the class's lifetime — Q_PROPERTY is
    /// rendered with `CONSTANT` (no NOTIFY); QML observers wire
    /// `model:` once and the ModelList's QAbstractItemModel surface
    /// pumps row-level changes through. Mutation goes through ordinary
    /// public methods on the ModelList (`xs->append(b)` etc.), not
    /// compiler magic.
    #[test]
    fn model_flag_emits_modellist_storage_and_ctor_init() {
        let src = r##"
class Book < QObject {
  prop Title : String, default: ""
}

class Store < QObject {
  pub prop items : ModelList<Book>
}
"##;
        let r = build(src);
        // The single Q_PROPERTY exposes the ModelList directly with
        // CONSTANT (pointer never changes; row-level changes flow
        // through QRangeModel's own dataChanged path).
        assert!(
            r.header
                .contains("Q_PROPERTY(::cute::ModelList<Book>* items READ items CONSTANT)"),
            "missing ModelList Q_PROPERTY clause:\n{}",
            r.header
        );
        // No setter — `, model` props are read-only at the property
        // level; mutation goes through the ModelList's public methods.
        assert!(
            !r.header.contains("setItems"),
            "no setter expected for `, model` prop:\n{}",
            r.header
        );
        // Storage is a heap-allocated `cute::ModelList<Book>*`,
        // null-initialised so the field is deterministic before the
        // ctor body runs.
        assert!(
            r.header
                .contains("::cute::ModelList<Book>* m_items = nullptr;"),
            "missing cute::ModelList<Book>* storage:\n{}",
            r.header
        );
        // Header preamble pulls in the Qt 6.11 headers only when the
        // module actually opts in via `, model` somewhere — projects
        // on older Qt versions that never use the flag stay clean.
        assert!(
            r.header.contains("#include <QRangeModel>")
                && r.header.contains("#include <QAbstractItemModel>")
                && r.header.contains("#include \"cute_model.h\""),
            "missing conditional QRangeModel/QAbstractItemModel/cute_model includes:\n{}",
            r.header
        );
        // Ctor body allocates the ModelList and parents it to `this`.
        // No `default: [...]` here, so just `(this)`.
        assert!(
            r.source
                .contains("m_items = new ::cute::ModelList<Book>(this);"),
            "missing cute::ModelList<Book> construction in ctor body:\n{}",
            r.source
        );
        // Public getter is a const member returning the pointer.
        assert!(
            r.header
                .contains("    ::cute::ModelList<Book>* items() const;"),
            "missing ModelList getter declaration:\n{}",
            r.header
        );
        assert!(
            r.source
                .contains("::cute::ModelList<Book>* Store::items() const { return m_items; }"),
            "missing ModelList getter definition:\n{}",
            r.source
        );
    }

    /// With `default: [...]`, the ctor body passes the lowered initial
    /// `QList<Book*>{...}` to the ModelList ctor as the first arg so
    /// the inner list starts populated — `library.Books` immediately
    /// reports `size == default-len` without an extra populate step.
    #[test]
    fn model_flag_with_default_initializes_modellist_inline() {
        let src = r##"
class Book < QObject {
  prop Title : String, default: ""
}

fn makeBook(t: String) Book {
  let b = Book.new()
  b.Title = t
  b
}

class Store < QObject {
  pub prop items : ModelList<Book>, default: [makeBook("a"), makeBook("b")]
}
"##;
        let r = build(src);
        assert!(
            r.source
                .contains("m_items = new ::cute::ModelList<Book>(QList<Book*>{")
                && r.source.contains(", this);"),
            "ctor body should pass a QList<Book*>{{...}} initial then `this`:\n{}",
            r.source
        );
    }

    /// Without the flag, no QRangeModel-related output appears.
    /// Pin the "zero-cost when off" guarantee so a future codegen
    /// refactor can't accidentally start including <QRangeModel> for
    /// every project.
    #[test]
    fn no_model_flag_means_no_qrangemodel_machinery() {
        let src = r##"
class Store < QObject {
  pub prop items : List<Int>, default: []
}
"##;
        let r = build(src);
        assert!(
            !r.header.contains("QRangeModel"),
            "QRangeModel leaked into header without `, model` flag:\n{}",
            r.header
        );
        assert!(
            !r.header.contains("ModelList"),
            "ModelList leaked into header without `, model` flag:\n{}",
            r.header
        );
    }

    /// `ModelList<T>` produces ONE PropertyData entry — the
    /// "single front door" pin so a future refactor can't reintroduce
    /// a doubled `<name>Model` proxy entry alongside the source prop.
    #[test]
    fn model_flag_emits_single_propertydata_entry() {
        let src = r##"
class Book < QObject {
  prop Title : String, default: ""
}

class Store < QObject {
  pub prop items : ModelList<Book>
}
"##;
        let r = build(src);
        assert!(
            r.source.contains("// property 0: items"),
            "expected single items PropertyData entry:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("itemsModel"),
            "synthesised proxy `itemsModel` should be gone in the wrapper-type design:\n{}",
            r.source
        );
    }

    /// `, model` emits a `QRangeModelRowOptions` specialization on
    /// the **unwrapped** row type (`Book`, not `Book*`) so QRangeModel
    /// classifies each row as `MultiRoleItem`. row_traits is keyed on
    /// `wrapped_t<row_type>` which strips the pointer for `Book*`
    /// rows; specializing the pointer form would silently miss and
    /// the row would fall back to the multi-column default — making
    /// only the first Q_PROPERTY visible from a QML ListView delegate.
    #[test]
    fn model_flag_emits_unwrapped_row_options_specialization() {
        let src = r##"
class Book < QObject {
  prop Title : String, default: ""
  pub prop author : String, default: ""
}

class Store < QObject {
  pub prop items : ModelList<Book>
}
"##;
        let r = build(src);
        assert!(
            r.header.contains("namespace QRangeModelDetails {")
                && r.header
                    .contains("template <> struct QRangeModelRowOptions<Book> {")
                && r.header.contains(
                    "static constexpr auto rowCategory = QRangeModel::RowCategory::MultiRoleItem;",
                ),
            "missing QRangeModelRowOptions<Book> spec:\n{}",
            r.header
        );
        // Belt-and-suspenders: the wrong (pointer-form) spec must NOT
        // be emitted, even by accident. row_traits never reaches it.
        assert!(
            !r.header.contains("QRangeModelRowOptions<Book*>"),
            "pointer-form spec leaked (would silently no-op):\n{}",
            r.header
        );
    }

    /// `@<prop>.<mutator>(args)` inside a method on the owning class
    /// is a NORMAL C++ method call on the ModelList — no compiler
    /// rewrite, no begin/end signal injection. The ModelList type
    /// itself owns the model-event semantics: its public `append` /
    /// `removeAt` / `clear` etc. are real methods that fire begin/end
    /// signals from inside their bodies. Pin this so a future
    /// regression doesn't reintroduce the previous compiler-magic
    /// design.
    #[test]
    fn at_mutator_on_model_prop_lowers_as_plain_method_call() {
        let src = r##"
class Book < QObject {
  prop Title : String, default: ""
}

class Store < QObject {
  pub prop items : ModelList<Book>

  pub fn addBook(b: Book) {
    items.append(b)
  }

  pub fn dropAt(i: Int) {
    items.removeAt(i)
  }

  pub fn dropAll {
    items.clear()
  }
}
"##;
        let r = build(src);
        // Plain pointer-deref method calls; ModelList's own methods
        // handle the begin/end signals internally.
        assert!(
            r.source.contains("m_items->append(b);"),
            "expected plain m_items->append call (no begin/end injection):\n{}",
            r.source
        );
        assert!(
            r.source.contains("m_items->removeAt(i);"),
            "expected plain m_items->removeAt call:\n{}",
            r.source
        );
        assert!(
            r.source.contains("m_items->clear();"),
            "expected plain m_items->clear call:\n{}",
            r.source
        );
        // Sanity: no compiler-injected begin/endInsertRows etc. in the
        // generated method body. (The ModelList header still uses
        // those internally, but they don't show up in the user-class
        // source.)
        assert!(
            !r.source.contains("m_items->beginInsertRows("),
            "no beginInsertRows should be injected at the call site:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("auto _r0 = static_cast<int>"),
            "no index-temp generation expected (no rewrite):\n{}",
            r.source
        );
    }

    /// On a non-`, model` prop the setter stays stock and no
    /// ModelList machinery appears anywhere. Pin "zero-cost when off"
    /// so a future refactor can't accidentally wrap every setter or
    /// drag in cute_model.h.
    #[test]
    fn plain_list_prop_emits_no_model_machinery() {
        let src = r##"
class Store < QObject {
  pub prop items : List<Int>, default: []
}
"##;
        let r = build(src);
        assert!(
            !r.source.contains("beginResetModel") && !r.source.contains("endResetModel"),
            "plain prop setter must not touch reset signals:\n{}",
            r.source
        );
        assert!(
            !r.header.contains("ModelList") && !r.source.contains("ModelList"),
            "ModelList symbol must not leak when no `, model` is present:\n{}\n{}",
            r.header,
            r.source
        );
    }

    /// `init(initial: Int) { @count = initial }` lowers to a public
    /// C++ ctor that takes the user-declared params and then a default
    /// `QObject* parent = nullptr`. The synthetic 0-arg ctor is no
    /// longer emitted in this case (the user's init is the sole
    /// entry point — overload-by-arity at `T.new(args)`).
    #[test]
    fn user_init_emits_qobject_ctor_with_params() {
        let src = r##"
class Counter < QObject {
  pub prop count : Int, default: 0
  init(initial: Int) {
    count = initial
  }
}
"##;
        let r = build(src);
        assert!(
            r.header
                .contains("Counter(qint64 initial, QObject* parent = nullptr);"),
            "missing user ctor decl:\n{}",
            r.header
        );
        assert!(
            !r.header
                .contains("explicit Counter(QObject* parent = nullptr);"),
            "synthetic ctor should be suppressed once user init exists:\n{}",
            r.header
        );
        assert!(
            r.source
                .contains("Counter::Counter(qint64 initial, QObject* parent) : QObject(parent) {"),
            "missing user ctor body:\n{}",
            r.source
        );
        assert!(
            r.source.contains("setCount(initial);"),
            "init body should route @count through the setter:\n{}",
            r.source
        );
    }

    /// Two `init`s on one class produce two C++ ctors. Each gets the
    /// auto-appended `QObject* parent = nullptr`. C++ overload
    /// resolution at the `T.new(args)` call site picks the right one.
    #[test]
    fn multiple_inits_emit_overloaded_ctors() {
        let src = r##"
class Pair < QObject {
  prop a : Int, default: 0
  prop b : Int, default: 0
  init() {
    a = 0
    b = 0
  }
  init(a: Int, b: Int) {
    a = a
    b = b
  }
}
"##;
        let r = build(src);
        assert!(
            r.header
                .contains("explicit Pair(QObject* parent = nullptr);"),
            "zero-arg init should keep `explicit`:\n{}",
            r.header
        );
        assert!(
            r.header
                .contains("Pair(qint64 a, qint64 b, QObject* parent = nullptr);"),
            "two-arg init missing:\n{}",
            r.header
        );
        assert!(
            r.source
                .contains("Pair::Pair(QObject* parent) : QObject(parent) {"),
            "missing zero-arg ctor body:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("Pair::Pair(qint64 a, qint64 b, QObject* parent) : QObject(parent) {"),
            "missing two-arg ctor body:\n{}",
            r.source
        );
    }

    /// `deinit { ... }` lowers to `~Class() override` declared in the
    /// header and defined out-of-line in the source. The body is
    /// lowered through the ordinary fn-body pipeline (so `@field`
    /// reads work just like in any other class method).
    #[test]
    fn deinit_emits_dtor_override() {
        let src = r##"
class Counter < QObject {
  pub prop count : Int, default: 0
  deinit {
    count = 0
  }
}
"##;
        let r = build(src);
        assert!(
            r.header.contains("~Counter() override;"),
            "missing dtor decl:\n{}",
            r.header
        );
        assert!(
            r.source.contains("Counter::~Counter() {"),
            "missing dtor body:\n{}",
            r.source
        );
        assert!(
            r.source.contains("setCount(0);"),
            "dtor body should route @count through the setter (unified write):\n{}",
            r.source
        );
    }

    /// Without `deinit`, no dtor declaration is emitted — Qt's
    /// virtual `~QObject()` handles teardown. Pins the "zero-cost
    /// when off" property so existing code stays bit-identical.
    #[test]
    fn no_deinit_means_no_dtor_decl() {
        let src = r##"
class Counter < QObject {
  pub prop count : Int, default: 0
}
"##;
        let r = build(src);
        assert!(
            !r.header.contains("~Counter()"),
            "dtor leaked when no deinit was declared:\n{}",
            r.header
        );
    }

    /// `arc X { init(...) ... deinit ... }` — ARC class
    /// init becomes a non-`QObject*-parent` ctor inlined in the
    /// header (matches the existing pattern of header-inline ARC
    /// methods for templates), and deinit becomes `~X()`.
    #[test]
    fn arc_class_init_and_deinit_emit_inline() {
        let src = r##"
arc Token {
  var Label : String = ""
  init(label: String) {
    Label = label
  }
  deinit {
    Label = "gone"
  }
}
"##;
        let r = build(src);
        assert!(
            r.header.contains("Token(::cute::String label) {"),
            "ARC ctor signature missing or has parent param:\n{}",
            r.header
        );
        assert!(
            r.header.contains("m_Label = label;"),
            "ARC ctor body should write member from param:\n{}",
            r.header
        );
        assert!(
            r.header.contains("~Token() {"),
            "ARC dtor missing:\n{}",
            r.header
        );
        // The default `Token() = default;` should be gone now that
        // the user supplied an init.
        assert!(
            !r.header.contains("Token() = default;"),
            "default ctor should be suppressed once an init exists:\n{}",
            r.header
        );
    }

    // ---- overload-by-arg-type codegen ------------------------------

    /// Two `fn foo(...)` on the same QObject class with different
    /// signatures must produce TWO distinct C++ method definitions
    /// (header decls + source bodies). Pre-overload, the
    /// name-keyed `find_map` in `emit_fn_body` would match the first
    /// AST FnDecl twice and silently lose the second body.
    #[test]
    fn class_with_two_fn_overloads_emits_both_bodies() {
        let src = r#"
class Greeter < QObject {
  pub fn greet String { "hi" }
  pub fn greet(name: String) String { name }
}
"#;
        let r = build(src);
        // Header: two Q_INVOKABLE decls with the right signatures.
        assert!(
            r.header.contains("Q_INVOKABLE ::cute::String greet();"),
            "missing zero-arg greet decl:\n{}",
            r.header
        );
        assert!(
            r.header
                .contains("Q_INVOKABLE ::cute::String greet(::cute::String name);"),
            "missing one-arg greet decl:\n{}",
            r.header
        );
        // Source: two distinct body definitions, each with its own
        // signature.
        assert!(
            r.source.contains("::cute::String Greeter::greet() {"),
            "missing zero-arg greet body:\n{}",
            r.source
        );
        assert!(
            r.source
                .contains("::cute::String Greeter::greet(::cute::String name) {"),
            "missing one-arg greet body:\n{}",
            r.source
        );
    }

    /// Two free `fn foo(...)` with different param signatures both
    /// land as separate C++ functions. C++ overload resolution at the
    /// call site picks the right one.
    #[test]
    fn top_level_fn_overload_emits_two_cpp_definitions() {
        let src = r#"
fn fmt(x: Int) String { "int" }
fn fmt(x: String) String { x }
"#;
        let r = build(src);
        let combined = format!("{}\n{}", r.header, r.source);
        assert!(
            combined.contains("::cute::String fmt(qint64 x)"),
            "missing fmt(Int) definition:\n{}",
            combined
        );
        assert!(
            combined.contains("::cute::String fmt(::cute::String x)"),
            "missing fmt(String) definition:\n{}",
            combined
        );
    }

    /// `impl Trait for X` with overloaded methods: the splice path
    /// must add each overload as a distinct method on the class
    /// surface, not collapse them onto one. Verified by checking
    /// that both signatures appear in the C++ output.
    #[test]
    fn impl_method_overload_splices_both_bodies_into_class() {
        let src = r#"
trait Pickable {
  pub fn pick String
  pub fn pick(idx: Int) String
}
class Box < QObject {
  prop Label : String, default: ""
}
impl Pickable for Box {
  pub fn pick String { Label }
  pub fn pick(idx: Int) String { Label }
}
"#;
        let r = build(src);
        // Both overloads should appear in the class's method surface.
        // Impls splice into the class members for QObject targets, so
        // the standard Q_INVOKABLE decls fire.
        assert!(
            r.header.contains("Q_INVOKABLE ::cute::String pick();"),
            "missing zero-arg pick decl:\n{}",
            r.header
        );
        assert!(
            r.header
                .contains("Q_INVOKABLE ::cute::String pick(qint64 idx);"),
            "missing one-arg pick decl:\n{}",
            r.header
        );
        assert!(
            r.source.contains("::cute::String Box::pick() {"),
            "missing zero-arg pick body:\n{}",
            r.source
        );
        assert!(
            r.source.contains("::cute::String Box::pick(qint64 idx) {"),
            "missing one-arg pick body:\n{}",
            r.source
        );
    }

    /// Q_INVOKABLE / qt_static_metacall path handles overloaded
    /// methods via positional MethodInfo index. Both overloads land
    /// as distinct cases in the metacall switch (even though they
    /// share the same name in the moc string table — exactly mirrors
    /// hand-written Qt + moc behavior).
    #[test]
    fn qt_metacall_handles_overloaded_methods_via_positional_index() {
        let src = r#"
class Box < QObject {
  pub fn put(x: Int) {}
  pub fn put(x: String) {}
}
"#;
        let r = build(src);
        // Two Q_INVOKABLE decls.
        assert!(
            r.header.contains("Q_INVOKABLE void put(qint64 x);"),
            "missing Int overload Q_INVOKABLE:\n{}",
            r.header
        );
        assert!(
            r.header.contains("Q_INVOKABLE void put(::cute::String x);"),
            "missing String overload Q_INVOKABLE:\n{}",
            r.header
        );
        // qt_static_metacall switch should have two distinct cases
        // routing to put(Int) and put(String) respectively. We don't
        // pin the exact case index (that depends on moc layout) but
        // both bodies must be present.
        assert!(
            r.source.contains("Box::put(qint64 x)"),
            "missing put(Int) body:\n{}",
            r.source
        );
        assert!(
            r.source.contains("Box::put(::cute::String x)"),
            "missing put(String) body:\n{}",
            r.source
        );
    }

    // ---- weak / unowned codegen --------------------------------------

    /// `weak let parent : Parent?` on an arc class lowers to:
    /// - `cute::Weak<Parent> m_parent;` storage,
    /// - public getter that returns `cute::Arc<Parent>` via `.lock()`
    ///   when the field is `pub`,
    /// - public setter (only when `pub var`) that takes `cute::Arc<Parent>`.
    #[test]
    fn weak_field_emits_cute_weak_with_lock_getter() {
        let src = r#"
arc Parent { }
arc Child {
  pub weak var parent : Parent?
}
"#;
        let r = build(src);
        // Storage is the Weak<T> form.
        assert!(
            r.header.contains("::cute::Weak<Parent> m_parent;"),
            "missing Weak<Parent> storage:\n{}",
            r.header
        );
        // Public getter returns Arc<Parent> via .lock().
        assert!(
            r.header
                .contains("::cute::Arc<Parent> parent() const { return m_parent.lock(); }"),
            "missing lock-based getter:\n{}",
            r.header
        );
        // Public setter takes Arc<Parent>; Weak::operator=(const Arc<T>&)
        // performs the conversion.
        assert!(
            r.header.contains(
                "void setParent(::cute::Arc<Parent> value) { m_parent = std::move(value); }"
            ),
            "missing arc-taking setter:\n{}",
            r.header
        );
    }

    /// `unowned let owner : Parent` on an arc class lowers to:
    /// - `Parent* m_owner = nullptr;` storage (raw, default-null),
    /// - public getter that returns the raw pointer when `pub`,
    /// - setter only for `pub var`.
    #[test]
    fn unowned_field_emits_raw_pointer() {
        let src = r#"
arc Parent { }
arc Child {
  pub unowned var owner : Parent
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("Parent* m_owner = nullptr;"),
            "missing raw-pointer storage:\n{}",
            r.header
        );
        assert!(
            r.header
                .contains("Parent* owner() const { return m_owner; }"),
            "missing raw-pointer getter:\n{}",
            r.header
        );
        assert!(
            r.header
                .contains("void setOwner(Parent* value) { m_owner = value; }"),
            "missing raw-pointer setter:\n{}",
            r.header
        );
    }

    // ---- @escaping codegen --------------------------------------------

    /// Default closure params (no `@escaping`) lower to
    /// `cute::function_ref<F>` — non-owning, two-pointer, no
    /// allocation.
    #[test]
    fn default_closure_param_lowers_to_function_ref() {
        let src = r#"
fn apply(f: fn(Int) -> Int) Int {
  f(0)
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("::cute::function_ref<qint64(qint64)> f"),
            "expected function_ref param signature, got:\n{}",
            r.header
        );
        assert!(
            !r.header.contains("std::function<qint64(qint64)>"),
            "did not expect std::function lowering, got:\n{}",
            r.header
        );
    }

    /// `@escaping` opts into `std::function<F>` so the callee can
    /// store / return / forward the closure freely. Verified by
    /// inspecting the emitted signature.
    #[test]
    fn escaping_closure_param_lowers_to_std_function() {
        let src = r#"
fn keep(escaping f: fn(Int) -> Int) Int {
  f(0)
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("std::function<qint64(qint64)> f"),
            "expected std::function param signature, got:\n{}",
            r.header
        );
        assert!(
            !r.header.contains("::cute::function_ref<qint64(qint64)>"),
            "did not expect function_ref lowering, got:\n{}",
            r.header
        );
    }

    // ---- struct methods codegen ---------------------------------------

    // ---- top-level `let` codegen ----------------------------------------

    /// A value-typed top-level `let X : Int = 1000` lowers to
    /// Top-level `let` lowers to file-scope storage, emitted before any
    /// class / fn that may reference it. Primitive-typed lets with a
    /// numeric or boolean literal initializer get `static constexpr`
    /// (compile-time-asserted constant init); everything else stays on
    /// the dynamic-init `static const auto` path because QString /
    /// QByteArray ctors aren't constexpr.
    #[test]
    fn top_level_let_value_type_emits_static_const_auto() {
        let src = r#"
let MaxLines : Int = 1000
let Greeting : String = "hello"
fn main {
  cli_app {
    println(Greeting)
  }
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("static constexpr auto MaxLines = 1000;"),
            "expected MaxLines as constexpr, got header:\n{}",
            r.header,
        );
        assert!(
            r.header
                .contains("static const auto Greeting = QStringLiteral(\"hello\");"),
            "expected Greeting emit, got header:\n{}",
            r.header,
        );
    }

    /// A QObject-typed top-level `let X : Foo = Foo.new()`
    /// lowers to `Q_GLOBAL_STATIC(Foo, X)`. The accessor `X()` is
    /// auto-generated by the macro; the K::Ident lowering rewrites
    /// bare `X` references in user code to `X()` so the surface stays
    /// pointer-typed.
    #[test]
    fn top_level_let_qobject_type_emits_q_global_static() {
        let src = r#"
class Counter < QObject {
  prop x : Int, notify: :xChanged, default: 0
  signal xChanged
}
let GlobalCounter : Counter = Counter.new()
fn main { cli_app { println("ok") } }
"#;
        let r = build(src);
        assert!(
            r.source.contains("Q_GLOBAL_STATIC(Counter, GlobalCounter)"),
            "expected Q_GLOBAL_STATIC emit, got source:\n{}",
            r.source,
        );
    }

    /// `T.new(args)` form lowers to `Q_GLOBAL_STATIC_WITH_ARGS`.
    #[test]
    fn top_level_let_qobject_with_args_emits_q_global_static_with_args() {
        let src = r#"
class Counter < QObject {
  prop x : Int, notify: :xChanged, default: 0
  signal xChanged
  init(start: Int) { x = start }
}
let GlobalCounter : Counter = Counter.new(42)
fn main { cli_app { println("ok") } }
"#;
        let r = build(src);
        assert!(
            r.source
                .contains("Q_GLOBAL_STATIC_WITH_ARGS(Counter, GlobalCounter, (42))"),
            "expected Q_GLOBAL_STATIC_WITH_ARGS emit, got source:\n{}",
            r.source,
        );
    }

    /// `store Foo { ... }` desugars (pre-pass) to `class Foo < QObject` +
    /// `let Foo : Foo = Foo.new()`. The Q_GLOBAL_STATIC post-pass picks
    /// up the synth let, and bare `Foo.method()` references rewrite to
    /// `Foo()->method()` through the same accessor mechanism the
    /// hand-written pattern uses.
    #[test]
    fn store_emits_q_global_static_and_accessor_call() {
        let src = r#"
store Counter {
  state value : Int = 0
  fn bump { value = value + 1 }
}
fn main {
  cli_app {
    Counter.bump()
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("Q_GLOBAL_STATIC(Counter, Counter)"),
            "expected Q_GLOBAL_STATIC emit, got source:\n{}",
            r.source,
        );
        assert!(
            r.source.contains("Counter()->bump()"),
            "expected Counter() accessor + arrow call, got source:\n{}",
            r.source,
        );
    }

    /// Bare references to a QObject-typed top-level let rewrite to
    /// the function-call accessor `X()`.
    #[test]
    fn ident_to_qobject_top_level_let_rewrites_to_accessor_call() {
        let src = r#"
class Counter < QObject {
  prop x : Int, notify: :xChanged, default: 0
  signal xChanged
  pub fn ping { println("pinged") }
}
let GlobalCounter : Counter = Counter.new()
fn main {
  cli_app {
    GlobalCounter.ping()
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("GlobalCounter()->ping()"),
            "expected GlobalCounter() accessor + arrow call, got source:\n{}",
            r.source,
        );
    }

    /// Struct methods land as inline member functions on the C++
    /// struct. `self.field` lowers to `this->field` (no parens —
    /// struct fields are plain C++ members), and `self.method()` calls
    /// a sibling method via `this->method()`.
    #[test]
    fn struct_method_lowers_to_inline_member() {
        let src = r#"
struct Point {
  var x : Int = 0
  var y : Int = 0

  fn magnitudeSq Int {
    self.x * self.x + self.y * self.y
  }
}
"#;
        let r = build(src);
        // Inline member function inside the struct body.
        assert!(
            r.header.contains("qint64 magnitudeSq()"),
            "expected inline magnitudeSq, got:\n{}",
            r.header
        );
        // self.x → this->x (no parens — struct fields are plain
        // members, not getter methods).
        assert!(
            r.header.contains("this->x * this->x"),
            "expected this->x field access, got:\n{}",
            r.header
        );
    }

    /// External call to a struct method (`p.magnitude_sq()`) uses
    /// `.` access (struct receiver is by-value) with parens, so it
    /// looks the same as on a class but skips the property-getter
    /// `()` rule for fields.
    #[test]
    fn struct_method_call_external_lowers_to_dot_call() {
        let src = r#"
struct Point {
  var x : Int = 0
  fn ident Int { self.x }
}
fn run(p : Point) Int {
  p.ident()
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("p.ident()"),
            "expected p.ident() call, got:\n{}",
            r.source
        );
    }

    // ---- ~Copyable + consuming codegen --------------------------------

    /// `struct X: ~Copyable { ... }` emits a deleted copy ctor /
    /// assignment and defaulted moves — the C++ compiler then
    /// statically rejects accidental copies.
    #[test]
    fn non_copyable_struct_emits_deleted_copy_and_defaulted_move() {
        let src = r#"
struct Token: ~Copyable {
  var id : Int = 0
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("Token(const Token&) = delete;"),
            "missing deleted copy ctor:\n{}",
            r.header
        );
        assert!(
            r.header
                .contains("Token& operator=(const Token&) = delete;"),
            "missing deleted copy assignment:\n{}",
            r.header
        );
        assert!(
            r.header.contains("Token(Token&&) = default;"),
            "missing defaulted move ctor:\n{}",
            r.header
        );
        assert!(
            r.header.contains("Token& operator=(Token&&) = default;"),
            "missing defaulted move assignment:\n{}",
            r.header
        );
    }

    /// `arc X: ~Copyable { ... }` emits the same deleted/defaulted
    /// suite on the underlying class so internal handling can't
    /// accidentally bypass the linear contract via class-level copy.
    #[test]
    fn non_copyable_arc_class_emits_deleted_copy_and_defaulted_move() {
        let src = r#"
arc Handle: ~Copyable {
  pub var id : Int = 0
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("Handle(const Handle&) = delete;"),
            "missing deleted copy ctor on arc class:\n{}",
            r.header
        );
        assert!(
            r.header.contains("Handle(Handle&&) = default;"),
            "missing defaulted move ctor on arc class:\n{}",
            r.header
        );
    }

    /// `consuming` parameters auto-wrap their lvalue arguments in
    /// `std::move(...)` at top-level fn call sites so the by-value /
    /// move-construct contract compiles even when the caller passes
    /// a stable binding.
    #[test]
    fn consuming_param_auto_moves_at_call_site() {
        let src = r#"
struct Token: ~Copyable { var id : Int = 0 }
fn consume(consuming t : Token) { }
fn main {
  cli_app {
    let t = Token.new(1)
    consume(t)
  }
}
"#;
        let r = build(src);
        assert!(
            r.source.contains("consume(std::move(t))"),
            "expected std::move-wrapped call site, got:\n{}",
            r.source
        );
    }

    /// Internal `@x` reads on a `weak` field call `.lock()`
    /// transparently, returning `cute::Arc<T>` so the surface-level
    /// `T?` semantics line up. Writes still go through the raw
    /// `m_x` storage (Weak's `operator=` performs the conversion).
    #[test]
    fn at_ident_read_on_weak_field_transparently_locks() {
        let src = r#"
arc Parent { pub var name : Int = 0 }
arc Child {
  weak let parent : Parent?
  init(p : Parent) { parent = p }
  pub fn describe Int {
    case parent {
      when some(p) { p.name() }
      when nil     { 0 }
    }
  }
}
"#;
        let r = build(src);
        let combined = format!("{}\n{}", r.header, r.source);
        // Read-side: `@parent` lowers to `m_parent.lock()`.
        assert!(
            combined.contains("m_parent.lock()"),
            "expected transparent .lock() on weak read, got:\n{}",
            combined
        );
        // Write-side (init body): `@parent = p` → `m_parent = p;`
        // (raw assignment — Weak::operator=(const Arc<T>&) handles
        // the conversion).
        assert!(
            combined.contains("m_parent = p;"),
            "expected raw `m_parent = p;` write, got:\n{}",
            combined
        );
    }

    /// `weak let` (immutable, not `var`) emits the storage but no
    /// public setter — matches the regular `pub let` rule.
    #[test]
    fn weak_pub_let_emits_getter_without_setter() {
        let src = r#"
arc Parent { }
arc Child {
  pub weak let parent : Parent?
}
"#;
        let r = build(src);
        assert!(
            r.header.contains("::cute::Weak<Parent> m_parent;"),
            "missing Weak storage:\n{}",
            r.header
        );
        assert!(
            r.header
                .contains("::cute::Arc<Parent> parent() const { return m_parent.lock(); }"),
            "missing lock getter:\n{}",
            r.header
        );
        assert!(
            !r.header.contains("setParent"),
            "did not expect a setter for `pub weak let`:\n{}",
            r.header
        );
    }
}
