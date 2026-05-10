//! Mapping Cute syntactic type expressions to C++ type names.
//!
//! Without name resolution / type-check we can only do a structural mapping.
//! Anything we can't reasonably guess (e.g. user class vs Qt class for
//! nullables) gets a documented TODO that HIR/types will refine.

use cute_hir::{ItemKind, ResolvedProgram};
use cute_syntax::ast::{TypeExpr, TypeKind};

/// Side-band context for type lowering: lets `cute_to_cpp` resolve `T?`
/// to either `QPointer<T>` (QObject-derived) or `std::optional<T>` (value
/// type), and bind `!T` to the module's error decl when there is exactly
/// one.
pub struct TypeCtx<'a> {
    pub program: &'a ResolvedProgram,
}

impl<'a> TypeCtx<'a> {
    pub fn new(program: &'a ResolvedProgram) -> Self {
        Self { program }
    }
}

/// Canonical C++ rendering of a Cute type used in declarations.
///
/// Built-in parametric names (List/Map/Set/Hash/Future/...) map directly
/// to their Qt counterparts. Class
/// names whose declarations are QObject-derived render as `T*` because
/// QObject subclasses are heap-only and parent-tree managed - storing
/// them by value in containers or properties would violate Qt's
/// non-copyable / non-movable invariant.
pub fn cute_to_cpp(ty: &TypeExpr, ctx: &TypeCtx<'_>) -> String {
    match &ty.kind {
        TypeKind::Named { path, args } => {
            let leaf = path.last().map(|i| i.name.as_str()).unwrap_or("?");
            // Bare-name shortcuts for collections: `List` (no type arg)
            // means "heterogeneous list" - lower to `QVariantList` so
            // QML's binding system can pass it through unchanged.
            // Same idea for `Map` / `Hash`. With explicit type args
            // (`List<Int>`, `Map<String, User>`) we emit the typed
            // QList/QMap/QHash form below.
            if args.is_empty() {
                let bare: Option<&str> = match leaf {
                    "List" => Some("QVariantList"),
                    "Map" | "Hash" => Some("QVariantMap"),
                    _ => None,
                };
                if let Some(b) = bare {
                    return b.to_string();
                }
            }
            let builtin: Option<&str> = match leaf {
                "String" => Some("::cute::String"),
                "Bool" => Some("bool"),
                "Int" => Some("qint64"),
                "Float" => Some("double"),
                "Void" => Some("void"),
                "ByteArray" => Some("QByteArray"),
                "List" => Some("QList"),
                "Map" => Some("QMap"),
                "Set" => Some("QSet"),
                "Hash" => Some("QHash"),
                "Future" => Some("QFuture"),
                "Slice" => Some("::cute::Slice"),
                "Url" => Some("QUrl"),
                "Regex" => Some("QRegularExpression"),
                "Date" => Some("QDate"),
                "Time" => Some("QTime"),
                "DateTime" => Some("QDateTime"),
                _ => None,
            };
            if let Some(base) = builtin {
                if args.is_empty() {
                    return base.to_string();
                }
                let a = args
                    .iter()
                    .map(|x| cute_to_cpp(x, ctx))
                    .collect::<Vec<_>>()
                    .join(", ");
                return format!("{base}<{a}>");
            }
            // Non-builtin: a user-declared class, struct, error, or
            // an unmodeled foreign type.
            if args.is_empty() {
                if is_qobject_named(leaf, ctx) {
                    return format!("{leaf}*");
                }
                if is_arc_class_named(leaf, ctx) {
                    return format!("::cute::Arc<{leaf}>");
                }
                return leaf.to_string();
            }
            // User-defined parametric (e.g. `Box<T>`).
            let a = args
                .iter()
                .map(|x| cute_to_cpp(x, ctx))
                .collect::<Vec<_>>()
                .join(", ");
            // ARC class instantiated with type args: `Box<Int>` lowers
            // to `::cute::Arc<Box<qint64>>` because every reference to
            // an ARC-class instance is held via the smart pointer.
            if is_arc_class_named(leaf, ctx) {
                return format!("::cute::Arc<{leaf}<{a}>>");
            }
            format!("{leaf}<{a}>")
        }
        TypeKind::Nullable(inner) => {
            if is_pointer_class(inner, ctx) {
                // QPointer is `QPointer<T>`, not `QPointer<T*>` — the
                // pointer is implicit. Render the bare class name
                // (with type args when present) instead of the
                // pointer form `cute_to_cpp` would add for a QObject
                // class in a value-position type slot.
                if let TypeKind::Named { path, args } = &inner.kind {
                    let leaf = path.last().map(|i| i.name.as_str()).unwrap_or("?");
                    if args.is_empty() {
                        return format!("QPointer<{leaf}>");
                    }
                    let a = args
                        .iter()
                        .map(|x| cute_to_cpp(x, ctx))
                        .collect::<Vec<_>>()
                        .join(", ");
                    return format!("QPointer<{leaf}<{a}>>");
                }
                format!("QPointer<{}>", cute_to_cpp(inner, ctx))
            } else {
                format!("std::optional<{}>", cute_to_cpp(inner, ctx))
            }
        }
        TypeKind::ErrorUnion(inner) => {
            let err = ctx
                .program
                .default_error_type
                .as_deref()
                .unwrap_or("::cute::String /* unbound error type */");
            format!("::cute::Result<{}, {err}>", cute_to_cpp(inner, ctx))
        }
        TypeKind::Fn { params, ret } => {
            let ps = params
                .iter()
                .map(|x| cute_to_cpp(x, ctx))
                .collect::<Vec<_>>()
                .join(", ");
            format!("std::function<{}({})>", cute_to_cpp(ret, ctx), ps)
        }
        TypeKind::SelfType => "auto /* Self */".to_string(),
    }
}

/// True when `name` is a class declared in the current module whose
/// super chain reaches `QObject`. Used to render bare `T` as `T*` in
/// codegen positions where Qt requires a heap pointer.
pub fn is_qobject_named(name: &str, ctx: &TypeCtx<'_>) -> bool {
    matches!(
        ctx.program.items.get(name),
        Some(cute_hir::ItemKind::Class {
            is_qobject_derived: true,
            ..
        })
    )
}

/// True when `name` is a Cute-side ARC class (`arc X { ... }`).
/// Distinguished from QObject-derived (parent-tree owned) and
/// from value-typed structs (`extern value X { ... }`). Type
/// positions referencing an ARC class need a `::cute::Arc<T>`
/// wrapper. `extern value` classes are foreign C++ value types
/// (`QPoint`, `QSize`, …) — they round-trip through Cute as
/// plain values, no smart-pointer wrapper.
pub fn is_arc_class_named(name: &str, ctx: &TypeCtx<'_>) -> bool {
    matches!(
        ctx.program.items.get(name),
        Some(cute_hir::ItemKind::Class {
            is_qobject_derived: false,
            is_extern_value: false,
            ..
        })
    )
}

/// Whether `inner` should lower a `T?` to a `QPointer<T>` (QObject-tree
/// owned, parent-tree-managed lifetime) versus `std::optional<T>` (value
/// type). Consults the resolved item table; falls back to the same Q-prefix
/// heuristic the HIR uses for Qt classes we have not yet bound.
fn is_pointer_class(ty: &TypeExpr, ctx: &TypeCtx<'_>) -> bool {
    let TypeKind::Named { path, .. } = &ty.kind else {
        return false;
    };
    let Some(name) = path.last() else {
        return false;
    };
    if let Some(ItemKind::Class {
        is_qobject_derived, ..
    }) = ctx.program.items.get(&name.name)
    {
        return *is_qobject_derived;
    }
    name.name.starts_with('Q')
}

/// QMetaType ID for the property-table `type` field. Returns the Qt 6
/// `QMetaType::Type` enumerator name (a symbolic string) so the C++
/// compiler resolves the integer value at build time.
pub fn cute_to_qmeta_type_enum(ty: &TypeExpr) -> &'static str {
    if let TypeKind::Named { path, .. } = &ty.kind {
        if let Some(name) = path.last() {
            return match name.name.as_str() {
                "Void" => "QMetaType::Void",
                "Bool" => "QMetaType::Bool",
                "Int" => "QMetaType::LongLong",
                "Float" => "QMetaType::Double",
                "String" => "QMetaType::QString",
                _ => "QMetaType::QObjectStar",
            };
        }
    }
    "QMetaType::Void"
}

/// Render a parameter's C++ type, honouring its `@escaping` flag. For a
/// closure-typed parameter without `@escaping` (the default), this
/// lowers to `::cute::function_ref<R(Args...)>` — a non-owning two-
/// pointer borrow that doesn't allocate. With `@escaping`, falls back
/// to the owning `std::function<R(Args...)>` so the callee can store /
/// return / forward the closure freely.
///
/// `consuming` parameters keep the type as-is (passed by value): the
/// callee takes ownership via the move constructor. Combined with
/// auto-`std::move(...)` wrapping at call sites (see
/// `render_args_with_consuming`), this gives Swift-style consuming
/// semantics — the caller's binding is moved-from after the call. For
/// `~Copyable` types where the copy ctor is deleted, by-value passing
/// is the only legal route, and the C++ compiler enforces that
/// statically.
///
/// Non-`Fn` parameter types are unaffected and route through `cute_to_cpp`.
pub fn cute_param_to_cpp(p: &cute_syntax::ast::Param, ctx: &TypeCtx<'_>) -> String {
    if !p.is_escaping {
        if let TypeKind::Fn { params, ret } = &p.ty.kind {
            let ps = params
                .iter()
                .map(|x| cute_to_cpp(x, ctx))
                .collect::<Vec<_>>()
                .join(", ");
            return format!("::cute::function_ref<{}({})>", cute_to_cpp(ret, ctx), ps);
        }
    }
    cute_to_cpp(&p.ty, ctx)
}

/// Whether a type, viewed as a property storage type, needs to be passed by
/// const-reference rather than by value.
pub fn pass_by_const_ref(ty: &TypeExpr) -> bool {
    if let TypeKind::Named { path, .. } = &ty.kind {
        if let Some(name) = path.last() {
            return matches!(name.name.as_str(), "String");
        }
    }
    false
}

/// Render a `cute_types::ty::Type` (post-lowering) to its C++ form,
/// mirroring `cute_to_cpp(TypeExpr)` but consuming the post-resolve
/// type representation. Used by codegen when the expected type at a
/// call site comes from the type-checker's instantiation map (where
/// only `Type` is available; the original `TypeExpr` is gone).
pub fn cute_type_to_cpp(t: &cute_types::ty::Type, ctx: &TypeCtx<'_>) -> String {
    use cute_types::ty::{Prim, Type};
    match t {
        Type::Prim(Prim::Bool) => "bool".to_string(),
        Type::Prim(Prim::Int) => "qint64".to_string(),
        Type::Prim(Prim::Float) => "double".to_string(),
        Type::Prim(Prim::String) => "::cute::String".to_string(),
        Type::Prim(Prim::Void) => "void".to_string(),
        Type::Prim(Prim::Nil) => "std::nullptr_t".to_string(),
        Type::Class(name) | Type::External(name) => {
            // Match the bare-name handling in `cute_to_cpp`: QObject-
            // derived classes lower to `T*` even when used as type
            // args, so a `Box<MyView>` becomes `Box<MyView*>`. ARC
            // classes wrap in `::cute::Arc<T>`.
            if is_qobject_named(name, ctx) {
                format!("{name}*")
            } else if is_arc_class_named(name, ctx) {
                format!("::cute::Arc<{name}>")
            } else {
                name.clone()
            }
        }
        Type::Enum(name) | Type::Flags(name) => {
            // Enums + flags lower to their declared C++ name; if the
            // typesystem stamped a `cpp_namespace` on the decl we
            // prefix it. The lookup happens in cute-codegen's
            // emit pass — at this layer we just emit the bare name.
            // Codegen for QFlags<E> wraps separately.
            name.clone()
        }
        Type::Generic { base, args } => {
            // Built-in parametric mapping (List/Map/Future/...) lives
            // alongside the user-defined fallback in the same table
            // `cute_to_cpp` uses for TypeExpr; route through it by
            // synthesizing a TypeExpr.
            let synth = synth_type_expr(t);
            let _ = base;
            let _ = args;
            cute_to_cpp(&synth, ctx)
        }
        Type::Nullable(inner) => {
            let synth = synth_type_expr(t);
            let _ = inner;
            cute_to_cpp(&synth, ctx)
        }
        Type::ErrorUnion { ok, err } => {
            format!("::cute::Result<{}, {err}>", cute_type_to_cpp(ok, ctx))
        }
        Type::Fn { params, ret } => {
            let ps = params
                .iter()
                .map(|p| cute_type_to_cpp(p, ctx))
                .collect::<Vec<_>>()
                .join(", ");
            format!("std::function<{}({ps})>", cute_type_to_cpp(ret, ctx))
        }
        Type::Sym => "QByteArray".to_string(),
        // SignalRef never reaches codegen: the `obj.signal.(dis)connect`
        // shape is rewritten earlier via try_emit_signal_(dis)connect.
        // Defensive: render as `void` so accidental reaches don't
        // generate well-formed but wrong template arguments.
        Type::SignalRef { .. } => "void".to_string(),
        Type::Var(_) | Type::Error | Type::Unknown => "auto".to_string(),
    }
}

/// Build a synthetic `TypeExpr` from a `Type` so we can reuse the
/// existing `cute_to_cpp` pipeline (built-in name mapping etc.) for
/// post-lowering types. Spans are blank — these expressions never
/// surface to diagnostics.
fn synth_type_expr(t: &cute_types::ty::Type) -> TypeExpr {
    use cute_syntax::ast::Ident;
    use cute_syntax::span::{FileId, Span};
    use cute_types::ty::{Prim, Type};
    let blank = Span::new(FileId(0), 0, 0);
    let named = |n: &str, args: Vec<TypeExpr>| TypeExpr {
        kind: TypeKind::Named {
            path: vec![Ident {
                name: n.to_string(),
                span: blank,
            }],
            args,
        },
        span: blank,
    };
    match t {
        Type::Prim(Prim::Bool) => named("Bool", vec![]),
        Type::Prim(Prim::Int) => named("Int", vec![]),
        Type::Prim(Prim::Float) => named("Float", vec![]),
        Type::Prim(Prim::String) => named("String", vec![]),
        Type::Prim(Prim::Void) => named("Void", vec![]),
        Type::Prim(Prim::Nil) => named("Nil", vec![]),
        Type::Class(n) | Type::External(n) | Type::Enum(n) | Type::Flags(n) => named(n, vec![]),
        Type::Generic { base, args } => named(base, args.iter().map(synth_type_expr).collect()),
        Type::Nullable(inner) => TypeExpr {
            kind: TypeKind::Nullable(Box::new(synth_type_expr(inner))),
            span: blank,
        },
        Type::ErrorUnion { ok, .. } => TypeExpr {
            kind: TypeKind::ErrorUnion(Box::new(synth_type_expr(ok))),
            span: blank,
        },
        Type::Fn { params, ret } => TypeExpr {
            kind: TypeKind::Fn {
                params: params.iter().map(synth_type_expr).collect(),
                ret: Box::new(synth_type_expr(ret)),
            },
            span: blank,
        },
        Type::Sym | Type::SignalRef { .. } | Type::Var(_) | Type::Error | Type::Unknown => {
            named("Void", vec![])
        }
    }
}

/// Property setter naming convention: `text` -> `setText`.
pub fn setter_name(prop_name: &str) -> String {
    let mut chars = prop_name.chars();
    let first = chars.next().unwrap_or('_').to_uppercase().to_string();
    format!("set{first}{}", chars.collect::<String>())
}

/// QBindable<T> getter naming convention: `text` -> `bindableText`.
/// Mirrors Qt's own `QObject::bindableObjectName` convention. Used in
/// the `Q_PROPERTY(... BINDABLE bindableX ...)` annotation and as the
/// public method name on `class : QObject` Cute classes.
pub fn bindable_getter_name(prop_name: &str) -> String {
    let mut chars = prop_name.chars();
    let first = chars.next().unwrap_or('_').to_uppercase().to_string();
    format!("bindable{first}{}", chars.collect::<String>())
}

/// Computed-property re-evaluation method name: `ratio` ->
/// `computeRatio`. Used by `Q_OBJECT_COMPUTED_PROPERTY` as the
/// "binding callable" template arg, and emitted privately on the
/// owning class.
pub fn compute_method_name(prop_name: &str) -> String {
    let mut cs = prop_name.chars();
    let cap = cs
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + cs.as_str())
        .unwrap_or_default();
    format!("compute{cap}")
}
