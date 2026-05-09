//! libclang traversal — turns a header + class spec into a list of
//! `Method` records ready for emit.
//!
//! Stays close to the original POC's filter rules, with three
//! configurable knobs that the typesystem controls:
//!
//! - `include` allowlist (pre-emit gate)
//! - `exclude` denylist (pre-emit gate, only when allowlist absent)
//! - `include_statics` (relaxes the "drop static methods" filter)
//!
//! Param-name renames happen in [`emit`](crate::emit) so this layer
//! stays pure traversal — easier to reason about when libclang's
//! shape changes between Qt versions.

use cute_qpi_gen::types::{CollectedClass, CuteType, EnumVariantInfo, Method, Param, Property};
use cute_qpi_gen::typesystem::{ClangConfig, ClassKind, ClassSpec, TypeSystem};

use clang::{Clang, Entity, EntityKind, Index, TypeKind};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Top-level entry: drives libclang for every `[[classes]]` entry in
/// the typesystem and returns the collected method list per class.
/// Errors propagate per-file (a parse failure on one class is fatal —
/// we don't want to silently emit a partial binding).
pub fn collect(ts: &TypeSystem) -> Result<Vec<CollectedClass>, String> {
    let clang = Clang::new().map_err(|e| format!("libclang init failed: {e}"))?;
    let index = Index::new(
        &clang, /*exclude_pch=*/ false, /*diagnostics=*/ false,
    );
    let std_flag = ts.clang.std.clone().unwrap_or_else(|| "c++17".to_string());

    let mut out = Vec::with_capacity(ts.classes.len());
    let type_map = build_type_map(ts);
    for spec in &ts.classes {
        out.push(collect_one(&index, &std_flag, &ts.clang, spec, &type_map)?);
    }
    Ok(out)
}

/// Variant for the legacy `--header` / `--class` ad-hoc path. Builds
/// a one-class typesystem on the fly so the same machinery handles
/// both modes — keeps the difference at the CLI layer only.
pub fn collect_one_off(
    header: PathBuf,
    class_name: String,
    includes: Vec<PathBuf>,
    std_flag: String,
    kind: ClassKind,
) -> Result<Vec<CollectedClass>, String> {
    let mut clang_cfg = ClangConfig {
        includes,
        frameworks: Vec::new(),
        std: Some(std_flag),
    };
    if let Some(parent) = header.parent() {
        if let Some(fwk) = walk_up_to_frameworks(parent) {
            clang_cfg.frameworks.push(fwk);
        }
    }
    let spec = ClassSpec {
        name: class_name,
        kind,
        super_name: None,
        header,
        include: None,
        exclude: Vec::new(),
        include_statics: false,
        params: BTreeMap::new(),
        comment: None,
        flags_of: None,
        cpp_namespace: None,
    };
    let ts = TypeSystem {
        vars: BTreeMap::new(),
        clang: clang_cfg,
        type_map: BTreeMap::new(),
        classes: vec![spec],
    };
    collect(&ts)
}

fn collect_one(
    index: &Index<'_>,
    std_flag: &str,
    clang_cfg: &ClangConfig,
    spec: &ClassSpec,
    type_map: &BTreeMap<String, String>,
) -> Result<CollectedClass, String> {
    let mut args: Vec<String> = vec![format!("-std={std_flag}"), "-x".into(), "c++".into()];
    for inc in &clang_cfg.includes {
        args.push("-isystem".into());
        args.push(inc.display().to_string());
    }
    for fwk in &clang_cfg.frameworks {
        args.push("-F".into());
        args.push(fwk.display().to_string());
    }
    if clang_cfg.frameworks.is_empty() {
        if let Some(parent) = spec.header.parent() {
            if let Some(fwk) = walk_up_to_frameworks(parent) {
                args.push("-F".into());
                args.push(fwk.display().to_string());
            }
        }
    }

    let tu = index
        .parser(&spec.header)
        .arguments(&args)
        .parse()
        .map_err(|e| format!("clang parse failed for {}: {e}", spec.header.display()))?;

    // Enum / Flags decls have a separate libclang shape (EnumDecl
    // rather than ClassDecl). Resolve and extract here, ahead of
    // the class-body path.
    if matches!(spec.kind, ClassKind::Enum) {
        let enum_entity = find_enum(tu.get_entity(), &spec.name).ok_or_else(|| {
            format!(
                "enum `{}` not found in {}",
                spec.name,
                spec.header.display()
            )
        })?;
        let variants = collect_enum_variants(enum_entity);
        return Ok(CollectedClass {
            spec: spec.clone(),
            methods: Vec::new(),
            signals: Vec::new(),
            properties: Vec::new(),
            detected_super: None,
            enum_variants: variants,
        });
    }
    if matches!(spec.kind, ClassKind::Flags) {
        // Flags: just emit the typedef. No libclang work needed —
        // the underlying enum is referenced by name; the typesystem
        // already declared `flags_of`.
        return Ok(CollectedClass {
            spec: spec.clone(),
            methods: Vec::new(),
            signals: Vec::new(),
            properties: Vec::new(),
            detected_super: None,
            enum_variants: Vec::new(),
        });
    }

    let target = find_class(tu.get_entity(), &spec.name).ok_or_else(|| {
        format!(
            "class `{}` not found in {}",
            spec.name,
            spec.header.display()
        )
    })?;

    // For object classes we need source-text knowledge that the AST
    // alone doesn't preserve: which methods are inside `signals:`
    // (Qt's signals access section is just `public` to libclang) and
    // what Q_PROPERTY macros declare (the macro is gone after
    // preprocessor expansion). Both are recovered by tokenising the
    // class body. Value classes skip the work.
    let scrape = if matches!(spec.kind, ClassKind::Object) {
        scrape_class_body(target, type_map)
    } else {
        ClassBodyScrape::default()
    };

    let (methods, signals) = collect_public_methods(target, spec, type_map, &scrape.signal_lines);

    Ok(CollectedClass {
        spec: spec.clone(),
        methods,
        signals,
        properties: scrape.properties,
        detected_super: scrape.super_name,
        enum_variants: Vec::new(),
    })
}

/// Walk the translation unit looking for an `EnumDecl` whose
/// simple name matches `name`. Returns the entity for value
/// extraction.
fn find_enum<'tu>(root: Entity<'tu>, name: &str) -> Option<Entity<'tu>> {
    let mut found: Option<Entity<'tu>> = None;
    fn walk<'tu>(e: Entity<'tu>, target: &str, out: &mut Option<Entity<'tu>>) {
        if out.is_some() {
            return;
        }
        e.visit_children(|child, _| {
            if out.is_some() {
                return clang::EntityVisitResult::Break;
            }
            if matches!(child.get_kind(), EntityKind::EnumDecl)
                && child.get_name().as_deref() == Some(target)
            {
                *out = Some(child);
                return clang::EntityVisitResult::Break;
            }
            // Recurse into namespaces / classes / structs (Qt enums
            // often live nested under `class QSlider { ... enum
            // TickPosition { ... } }`).
            if matches!(
                child.get_kind(),
                EntityKind::Namespace | EntityKind::ClassDecl | EntityKind::StructDecl
            ) {
                walk(child, target, out);
            }
            clang::EntityVisitResult::Continue
        });
    }
    walk(root, name, &mut found);
    found
}

/// Extract `(name, optional explicit-value source text)` for
/// every enumerator in an EnumDecl. The explicit value is the
/// source-verbatim splice of any `= <expr>` clause; absent values
/// fall through to C++'s default-progression at codegen time.
fn collect_enum_variants(enum_entity: Entity<'_>) -> Vec<EnumVariantInfo> {
    let mut out = Vec::new();
    enum_entity.visit_children(|child, _| {
        if matches!(child.get_kind(), EntityKind::EnumConstantDecl) {
            let name = child.get_name().unwrap_or_default();
            // Source text of the explicit `= expr` part, when
            // present. `get_range()` covers the whole enumerator
            // (`AlignLeft = 0x0001`); we need just the right of
            // the `=`. Easiest path: grab the full text and split.
            let value_text = enum_constant_value_text(child);
            out.push(EnumVariantInfo { name, value_text });
        }
        clang::EntityVisitResult::Continue
    });
    out
}

/// Source-text helper for `collect_enum_variants`. Reads the
/// enumerator's source range, finds `=`, returns everything
/// after it (whitespace-trimmed). Returns None when the
/// enumerator has no explicit value clause.
fn enum_constant_value_text(child: Entity<'_>) -> Option<String> {
    let range = child.get_range()?;
    let tokens = range.tokenize();
    let eq_idx = tokens.iter().position(|t| t.get_spelling() == "=")?;
    let after: Vec<String> = tokens[eq_idx + 1..]
        .iter()
        .map(|t| t.get_spelling())
        .collect();
    if after.is_empty() {
        None
    } else {
        Some(after.join(" "))
    }
}

/// Walk parent directories of a Qt framework header until we find a
/// directory whose entries include `*.framework`. Returns that parent
/// (the directory you pass to `-F`). For
/// `/opt/homebrew/lib/QtCore.framework/Headers/qpoint.h` this
/// returns `/opt/homebrew/lib`.
fn walk_up_to_frameworks(start: &std::path::Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    while let Some(parent) = cur.parent() {
        if let Ok(entries) = std::fs::read_dir(parent) {
            if entries
                .flatten()
                .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("framework"))
            {
                return Some(parent.to_path_buf());
            }
        }
        cur = parent.to_path_buf();
    }
    None
}

fn find_class<'tu>(root: Entity<'tu>, name: &str) -> Option<Entity<'tu>> {
    let mut found: Option<Entity<'tu>> = None;
    root.visit_children(|e, _| {
        if found.is_some() {
            return clang::EntityVisitResult::Break;
        }
        if matches!(e.get_kind(), EntityKind::ClassDecl | EntityKind::StructDecl)
            && e.get_name().as_deref() == Some(name)
            && e.is_definition()
        {
            found = Some(e);
            return clang::EntityVisitResult::Break;
        }
        clang::EntityVisitResult::Recurse
    });
    found
}

fn collect_public_methods(
    class: Entity<'_>,
    spec: &ClassSpec,
    type_map: &BTreeMap<String, String>,
    signal_lines: &std::collections::BTreeSet<u32>,
) -> (Vec<Method>, Vec<Method>) {
    // `class` defaults to private accessibility, `struct` to public.
    let mut current_access = if matches!(class.get_kind(), EntityKind::ClassDecl) {
        clang::Accessibility::Private
    } else {
        clang::Accessibility::Public
    };

    let allow: Option<&[String]> = spec.include.as_deref();
    let deny = &spec.exclude;
    // Source order preserved by visitation; overloads stay grouped
    // because libclang emits sibling methods consecutively in the
    // class body. The allowlist reorder pass below uses this Vec
    // (not a map) so that reorder can produce a stable, predictable
    // output that mirrors the typesystem's `include = [...]` line.
    //
    // The bool flag tracks whether this method was emitted from
    // inside a `signals:` access section. Defaults expand to many
    // Method records sharing the same bool — they all came from
    // the same C++ source line so the signal tag carries through.
    let mut collected: Vec<(Method, bool)> = Vec::new();

    class.visit_children(|e, _| {
        if let Some(a) = e.get_accessibility() {
            current_access = a;
        }
        if current_access != clang::Accessibility::Public {
            return clang::EntityVisitResult::Continue;
        }
        if !matches!(e.get_kind(), EntityKind::Method) {
            return clang::EntityVisitResult::Continue;
        }
        let raw_name = e.get_name().unwrap_or_default();
        // Operator overloads — symbolic in C++, no idiomatic Cute
        // surface yet. Drop unconditionally.
        if raw_name.starts_with("operator") {
            return clang::EntityVisitResult::Continue;
        }
        // MOC-injected sanity-check methods (`qt_check_for_QGADGET_macro`,
        // `qt_metacall`, `qt_metacast`, ...). These appear on every
        // Q_OBJECT / Q_GADGET class and have no useful Cute-side
        // surface — drop them ahead of the include/exclude check so
        // each typesystem entry doesn't have to.
        if raw_name.starts_with("qt_") {
            return clang::EntityVisitResult::Continue;
        }
        // Static-method gate: opt-in per typesystem class entry.
        if e.is_static_method() && !spec.include_statics {
            return clang::EntityVisitResult::Continue;
        }
        // Detect signal status now (before the include / exclude
        // gate) so signals always come through regardless of the
        // user's `fn` allowlist. Signals are a separate output
        // axis from regular methods — there's no reason for the
        // typesystem author to have to enumerate them in `include`.
        let is_signal = e
            .get_location()
            .map(|loc| loc.get_spelling_location().line)
            .map(|l| signal_lines.contains(&l))
            .unwrap_or(false);
        if !is_signal {
            if let Some(list) = allow {
                if !list.iter().any(|n| n == &raw_name) {
                    return clang::EntityVisitResult::Continue;
                }
            } else if deny.iter().any(|n| n == &raw_name) {
                return clang::EntityVisitResult::Continue;
            }
        }
        let Some(ret_ty) = e.get_result_type() else {
            return clang::EntityVisitResult::Continue;
        };
        // Drop `T &mutator()` style accessors — they alias mutate the
        // receiver and don't translate cleanly to Cute's value-type
        // discipline. Const lvalue refs are fine and stripped by the
        // type mapper.
        if matches!(ret_ty.get_kind(), TypeKind::LValueReference) && !ret_ty.is_const_qualified() {
            return clang::EntityVisitResult::Continue;
        }
        let Some(cute_ret) = map_type(ret_ty, type_map) else {
            return clang::EntityVisitResult::Continue;
        };
        let mut params: Vec<Param> = Vec::new();
        let mut defaults: Vec<bool> = Vec::new();
        let mut bad_param = false;
        // Index of the first `bool* ok = nullptr`-shaped parameter.
        // When present, emit a lifted variant in addition to the
        // raw form, with the bool* dropped and the return type
        // wrapped in `!`.
        let mut bool_ptr_param_idx: Option<usize> = None;
        if let Some(args) = e.get_arguments() {
            for a in args {
                let pname = a.get_name().unwrap_or_else(|| "_".into());
                // `QPrivateSignal` is Qt's internal sentinel arg added
                // to every Q_OBJECT signal so MOC can recognise the
                // declaration (`void timeout(QPrivateSignal);`). Cute
                // users never type it; drop it from the param list so
                // signals come out with their public arity.
                if let Some(t) = a.get_type() {
                    let display = strip_const(&t.get_display_name());
                    if display.trim() == "QPrivateSignal" {
                        continue;
                    }
                }
                // A C++ default-arg shows up as an Expression child of
                // the ParmDecl (TypeRef / NamespaceRef siblings get
                // filtered out by `is_expression`). Used downstream to
                // expand `foo(a, b = 1)` into both `foo(a)` and
                // `foo(a, b: Int)` so Cute call sites can omit the
                // default just as C++ does.
                let has_default = a.get_children().iter().any(|c| c.is_expression());
                // Detect Qt's `bool* ok = nullptr` out-parameter
                // pattern: a pointer to bool with a default of
                // `nullptr`. Most parsing methods on Qt value types
                // (`QString::toInt`, `QLocale::toDouble`, ...) use
                // it to flag whether the conversion succeeded.
                // Flagging the position lets the lifted `!T` variant
                // be emitted alongside the raw form.
                let is_bool_ptr_ok = a
                    .get_type()
                    .map(|t| {
                        if !matches!(t.get_kind(), TypeKind::Pointer) {
                            return false;
                        }
                        let Some(pointee) = t.get_pointee_type() else {
                            return false;
                        };
                        let display = strip_const(&pointee.get_display_name());
                        display.trim() == "bool"
                    })
                    .unwrap_or(false);
                if is_bool_ptr_ok && has_default && bool_ptr_param_idx.is_none() {
                    bool_ptr_param_idx = Some(params.len());
                }
                let mapped = a.get_type().and_then(|t| map_type(t, type_map));
                let pty = match mapped {
                    Some(t) if !matches!(t, CuteType::Void) => t,
                    _ => {
                        // Unmappable / void parameter. If it (and every
                        // following arg) has a C++ default, we can still
                        // emit the prefix overload — `host()` survives
                        // even when `host(ComponentFormattingOptions)`
                        // can't be expressed. Otherwise the whole method
                        // is unrepresentable; drop it.
                        if has_default {
                            break;
                        }
                        bad_param = true;
                        break;
                    }
                };
                params.push(Param {
                    name: pname,
                    ty: pty,
                });
                defaults.push(has_default);
            }
        }
        if bad_param {
            return clang::EntityVisitResult::Continue;
        }
        // Trailing-default expansion: in C++, only trailing parameters
        // can be default — once a param has a default, every later one
        // must too. Emit one Method per valid call arity (shortest
        // first so the most-common form leads in the .qpi listing).
        let trailing = defaults.iter().rev().take_while(|d| **d).count();
        let max_arity = params.len();
        let min_arity = max_arity - trailing;
        // When a `bool* ok = nullptr` out-param was detected and the
        // method returns a non-void value, every emitted overload is
        // the lifted `!T @lifted_bool_ok` shape. The raw form (which
        // would expose the bool* to Cute as a misleading `Bool` param)
        // is dropped so users only see the Result-returning API.
        let lift_only = bool_ptr_param_idx.is_some() && !matches!(cute_ret, CuteType::Void);
        if !lift_only {
            for arity in min_arity..=max_arity {
                collected.push((
                    Method {
                        name: raw_name.clone(),
                        params: params[..arity].to_vec(),
                        return_ty: cute_ret.clone(),
                        lifted_bool_ok: false,
                    },
                    is_signal,
                ));
            }
        }
        // Lifted variant for the bool*-ok parameter pattern. Drop
        // the bool* parameter and return `!ValueType` instead of
        // the raw return. Useful for parsing-style methods where
        // the bool indicates success — Cute callers get
        // `case s.toInt() { when ok(n) ... when err(e) ... }` for
        // free instead of having to plumb an out-bool.
        //
        // Skip when the underlying return is `void` (lifting `!void`
        // adds no information). For each call arity that *includes*
        // the bool* position, emit one lifted entry with that param
        // removed. The arity range matches the raw-form expansion
        // above so callers who pass extra trailing
        // args can still hit the lifted overload.
        if let Some(idx) = bool_ptr_param_idx {
            if !matches!(cute_ret, CuteType::Void) {
                let lift_min = (idx + 1).max(min_arity);
                for arity in lift_min..=max_arity {
                    let mut lifted_params: Vec<Param> = params[..arity].to_vec();
                    if idx < lifted_params.len() {
                        lifted_params.remove(idx);
                    }
                    collected.push((
                        Method {
                            name: raw_name.clone(),
                            params: lifted_params,
                            return_ty: cute_ret.clone(),
                            lifted_bool_ok: true,
                        },
                        is_signal,
                    ));
                }
            }
        }
        clang::EntityVisitResult::Continue
    });

    // Allowlist mode: reorder to mirror the typesystem `include`
    // sequence so the editorial layout is deterministic and matches
    // the file's reading order. All overloads of a name stay grouped
    // (Cute's overload-by-arg-type picks the right one at call site).
    // Signals bypass the allowlist entirely and append in source
    // order so the typesystem author doesn't have to enumerate them.
    let ordered: Vec<(Method, bool)> = if let Some(list) = allow {
        let mut out = Vec::with_capacity(collected.len());
        for n in list {
            for entry in &collected {
                if !entry.1 && &entry.0.name == n {
                    out.push(entry.clone());
                }
            }
        }
        for entry in &collected {
            if entry.1 {
                out.push(entry.clone());
            }
        }
        out
    } else {
        collected
    };

    // Partition into signals vs regular methods. Allowlist filtering
    // already happened above — both halves respect it. Also dedup by
    // (name, Cute-side type signature): Cute can't have two methods
    // with the same surface, and certain Qt 6 chrono-overload pairs
    // collapse to the same signature once mapped (the display_name
    // for `std::chrono::milliseconds` resolves through to plain
    // `int` in libclang in some cases — we filter what we can in
    // map_type and dedup the rest here as a safety net).
    let mut methods = Vec::new();
    let mut signals = Vec::new();
    let mut seen_methods: std::collections::BTreeSet<String> = Default::default();
    let mut seen_signals: std::collections::BTreeSet<String> = Default::default();
    for (m, is_signal) in ordered {
        let sig = method_signature(&m);
        let (bucket, seen) = if is_signal {
            (&mut signals, &mut seen_signals)
        } else {
            (&mut methods, &mut seen_methods)
        };
        if seen.insert(sig) {
            bucket.push(m);
        }
    }
    (methods, signals)
}

fn method_signature(m: &Method) -> String {
    let types: Vec<String> = m
        .params
        .iter()
        .map(|p| match &p.ty {
            CuteType::Named(s) => s.clone(),
            CuteType::Void => "Void".to_string(),
        })
        .collect();
    // Include the lifted-bool-ok flag in the dedup key so a lifted
    // variant doesn't collide with the raw form when both happen to
    // have the same Cute-side parameter signature (e.g. raw
    // `toShort()` and lifted `toShort() !Int` collapse to the same
    // (name, param-types) tuple after the bool* is dropped).
    let suffix = if m.lifted_bool_ok { "!" } else { "" };
    format!("{}({}){}", m.name, types.join(","), suffix)
}

#[derive(Default, Debug)]
struct ClassBodyScrape {
    /// Set of source line numbers that fall inside a `signals:` /
    /// `Q_SIGNALS:` access section. AST methods on these lines are
    /// emitted as Cute `signal X(...)` rather than `fn X(...)`.
    signal_lines: std::collections::BTreeSet<u32>,
    /// One entry per `Q_PROPERTY(type name READ ... WRITE ... NOTIFY ...)`
    /// macro that the type-mapper could resolve. Properties whose
    /// C++ type isn't in the type map are dropped silently — the
    /// underlying READ/WRITE methods still survive as plain `fn`.
    properties: Vec<Property>,
    /// First public/private base class detected via libclang's
    /// `BaseSpecifier` children. `super_name` from the typesystem
    /// overrides this when set.
    super_name: Option<String>,
}

/// Tokenise the class body and extract everything the AST alone
/// can't carry: signals: range, Q_PROPERTY macros, and the C++ base
/// class. The token stream is the only place these survive — the
/// preprocessor expands `Q_PROPERTY` away before libclang's AST
/// pass, and `signals:` is just `#define signals public` so the
/// access-section info is lost too.
fn scrape_class_body(class: Entity<'_>, type_map: &BTreeMap<String, String>) -> ClassBodyScrape {
    let mut out = ClassBodyScrape::default();

    // Walk the translation unit collecting enum-typed identifier
    // names so the Q_PROPERTY token scrape can resolve type names
    // it sees as text (e.g. `Qt::TimerType`) without having to
    // re-query libclang per property. Both the unqualified name
    // (`TimerType`) and the simple-namespace-qualified form
    // (`Qt::TimerType`) go in.
    let mut enum_names: std::collections::BTreeSet<String> = Default::default();
    let tu_root = class.get_translation_unit().get_entity();
    collect_enum_names(tu_root, None, &mut enum_names);

    // Detect base class via the AST (BaseSpecifier child entities).
    // For Q_OBJECT classes there's almost always exactly one base
    // (single-inheritance is the Qt convention); take the first.
    for child in class.get_children() {
        if matches!(child.get_kind(), EntityKind::BaseSpecifier) {
            if let Some(t) = child.get_type() {
                let display = t.get_display_name();
                let name = strip_const(&display);
                let trimmed = name.trim();
                // Strip any `Qt::` namespace prefix for the few cases
                // that surface here — Cute names mirror the bare class.
                let bare = trimmed.rsplit("::").next().unwrap_or(trimmed);
                out.super_name = Some(bare.to_string());
                break;
            }
        }
    }

    let Some(range) = class.get_range() else {
        return out;
    };
    let tokens = range.tokenize();

    // Pass 1: signals: line ranges. Walk linearly; when we see
    // `signals` `:` or `Q_SIGNALS` `:` or `Q_SIGNAL` (the per-method
    // marker), open an active range until the next access keyword.
    // Lines inside the range are added to the signal_lines set.
    let mut in_signals = false;
    let mut last_line = 0u32;
    for (i, tok) in tokens.iter().enumerate() {
        let kind = tok.get_kind();
        let spelling = tok.get_spelling();
        let line = tok.get_location().get_spelling_location().line;
        if matches!(kind, clang::token::TokenKind::Keyword)
            && (spelling == "public" || spelling == "private" || spelling == "protected")
        {
            // Closes any active signals: section. The Qt `slots:`
            // form expands to plain access keywords too, so we
            // never see it as a separate token.
            in_signals = false;
        }
        if matches!(kind, clang::token::TokenKind::Identifier)
            && (spelling == "signals" || spelling == "Q_SIGNALS")
        {
            // Followed by `:` opens the section.
            if let Some(next) = tokens.get(i + 1) {
                if next.get_spelling() == ":" {
                    in_signals = true;
                }
            }
        }
        if in_signals && line != last_line {
            out.signal_lines.insert(line);
            last_line = line;
        }
    }

    // Pass 2: Q_PROPERTY macros. Each scan starts at a Q_PROPERTY
    // identifier token, expects `(`, then collects tokens until the
    // matching `)`. Inside, the format is:
    //   <TypeTokens...> <Name> [<Keyword> <Identifier>]*
    // where Keyword is one of READ / WRITE / NOTIFY / RESET /
    // BINDABLE / MEMBER / DESIGNABLE / SCRIPTABLE / STORED / USER /
    // CONSTANT / FINAL / REQUIRED / REVISION / PRIVATE.
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].get_spelling() != "Q_PROPERTY" {
            i += 1;
            continue;
        }
        i += 1;
        // Find opening `(`
        if i >= tokens.len() || tokens[i].get_spelling() != "(" {
            continue;
        }
        i += 1;
        // Collect tokens until matching `)`. Q_PROPERTY bodies don't
        // have nested parens in practice, but handle depth anyway.
        let body_start = i;
        let mut depth = 1usize;
        while i < tokens.len() && depth > 0 {
            match tokens[i].get_spelling().as_str() {
                "(" => depth += 1,
                ")" => depth -= 1,
                _ => {}
            }
            if depth > 0 {
                i += 1;
            }
        }
        if depth != 0 {
            // Unterminated; bail on this property but keep scanning.
            continue;
        }
        let body = &tokens[body_start..i];
        i += 1; // skip the closing `)`
        if let Some(prop) = parse_property_body(body, type_map, &enum_names) {
            out.properties.push(prop);
        }
    }

    out
}

/// Walk the translation unit collecting every enum's bare name
/// plus its `Outer::Name` qualifications up two levels. Catches
/// the Qt-typical patterns `Qt::TimerType`,
/// `QSlider::TickPosition`, `Qt::Orientation`, and so on.
///
/// Also collects typedef names that resolve to either an enum or
/// `QFlags<...>` — covers `using QString = QFlags<Qt::AlignmentFlag>`
/// shaped declarations that propagate through Q_PROPERTYs.
///
/// Uses libclang's `visit_children` (depth-first) rather than
/// `get_children` because the TU root only enumerates its direct
/// children that way.
fn collect_enum_names(
    root: Entity<'_>,
    _parent_name: Option<&str>,
    out: &mut std::collections::BTreeSet<String>,
) {
    fn record(
        e: Entity<'_>,
        parent_name: Option<&str>,
        out: &mut std::collections::BTreeSet<String>,
    ) {
        let Some(name) = e.get_name() else { return };
        out.insert(name.clone());
        if let Some(parent) = parent_name {
            out.insert(format!("{parent}::{name}"));
        }
    }
    fn walk(
        e: Entity<'_>,
        parent_name: Option<&str>,
        out: &mut std::collections::BTreeSet<String>,
    ) {
        e.visit_children(|child, _| {
            match child.get_kind() {
                EntityKind::EnumDecl => record(child, parent_name, out),
                EntityKind::TypedefDecl | EntityKind::TypeAliasDecl => {
                    if let Some(under) = child.get_typedef_underlying_type() {
                        let display = under.get_canonical_type().get_display_name();
                        let is_enum = under
                            .get_declaration()
                            .map(|d| matches!(d.get_kind(), EntityKind::EnumDecl))
                            .unwrap_or(false);
                        if is_enum || display.starts_with("QFlags<") {
                            record(child, parent_name, out);
                        }
                    }
                }
                EntityKind::Namespace
                | EntityKind::ClassDecl
                | EntityKind::StructDecl
                | EntityKind::ClassTemplate => {
                    let next_parent = child.get_name();
                    walk(child, next_parent.as_deref(), out);
                }
                _ => {}
            }
            clang::EntityVisitResult::Continue
        });
    }
    walk(root, None, out);
}

/// Splits a `Q_PROPERTY(...)` body into type, name, and ignored
/// modifiers. Returns None when the type doesn't map to a Cute
/// type — the caller silently drops the property in that case
/// (the underlying READ/WRITE methods still surface as `fn`).
fn parse_property_body(
    body: &[clang::token::Token<'_>],
    type_map: &BTreeMap<String, String>,
    enum_names: &std::collections::BTreeSet<String>,
) -> Option<Property> {
    // Modifier keywords mark the boundary between the (type, name)
    // prefix and the rest. The NAME is the last token before the
    // first modifier.
    const MODIFIERS: &[&str] = &[
        "READ",
        "WRITE",
        "NOTIFY",
        "RESET",
        "BINDABLE",
        "MEMBER",
        "DESIGNABLE",
        "SCRIPTABLE",
        "STORED",
        "USER",
        "CONSTANT",
        "FINAL",
        "REQUIRED",
        "REVISION",
        "PRIVATE",
    ];
    let first_mod = body
        .iter()
        .position(|t| MODIFIERS.contains(&t.get_spelling().as_str()))?;
    if first_mod < 2 {
        return None;
    }
    let prefix = &body[..first_mod];
    let name_tok = prefix.last()?;
    let type_toks = &prefix[..prefix.len() - 1];
    // Reconstruct the C++ type as a single string (whitespace-joined),
    // then map. Multi-token types like `Qt::TimerType`, `QList<int>`,
    // `int*` collapse here.
    let type_str: String = type_toks
        .iter()
        .map(|t| t.get_spelling())
        .collect::<Vec<_>>()
        .join(" ");
    let normalized = type_str
        .replace(" ::", "::")
        .replace(":: ", "::")
        .trim()
        .to_string();
    let bare = normalized
        .rsplit("::")
        .next()
        .unwrap_or(&normalized)
        .trim()
        .to_string();
    // Try the type_map first — typesystem-bound enums
    // (`Qt::AlignmentFlag` → Cute `AlignmentFlag`) take precedence
    // over the generic enum→Int auto-fallback below. Without this
    // order, `Q_PROPERTY(Qt::AlignmentFlag alignment ...)` would
    // always lower as `prop alignment : Int` even when the user
    // bound the matching `extern enum` in qenums.toml.
    if let Some(mapped) = type_map
        .get(&normalized)
        .or_else(|| type_map.get(&bare))
        .cloned()
    {
        return Some(Property {
            name: name_tok.get_spelling(),
            ty: CuteType::Named(mapped),
        });
    }
    // `Qt::Alignment` is a typedef for `QFlags<Qt::AlignmentFlag>`.
    // The token-text the property scrape sees is `Qt::Alignment`
    // (the typedef name), not the expanded QFlags form. We don't
    // have libclang here to resolve the typedef, but we can
    // recognise common Qt flag-typedef names by trying the
    // unqualified-name + "Flag" suffix in the type_map. For
    // `Qt::Alignment`: bare = "Alignment", try bare + "Flag" =
    // "AlignmentFlag" → AlignmentFlag (bound enum).
    let with_flag = format!("{bare}Flag");
    if let Some(mapped) = type_map.get(&with_flag).cloned() {
        return Some(Property {
            name: name_tok.get_spelling(),
            ty: CuteType::Named(mapped),
        });
    }
    // Auto-fallback: unbound enum types lower to Int — same rule
    // map_type uses on the AST side. Both the qualified
    // (`Qt::TimerType`) and the bare (`TimerType`) form are tried.
    if enum_names.contains(&normalized) || enum_names.contains(&bare) {
        return Some(Property {
            name: name_tok.get_spelling(),
            ty: CuteType::Named("Int".to_string()),
        });
    }
    None
}

/// Built-in type table — overlaid by `[type_map]` from the
/// typesystem AND by the names of every `[[classes]]` entry the
/// same typesystem declares (so a typesystem that binds class A
/// can have class B's methods reference A by name without the
/// user having to repeat A in `[type_map]`). The built-in
/// covers what's needed to bind the
/// QtCore value-type family without any user config; users add to
/// it (not replace) when binding wider modules.
fn build_type_map(ts: &TypeSystem) -> BTreeMap<String, String> {
    let mut m: BTreeMap<String, String> = [
        ("bool", "Bool"),
        ("int", "Int"),
        ("qint32", "Int"),
        ("qint64", "Int"),
        ("long", "Int"),
        ("long long", "Int"),
        ("qsizetype", "Int"),
        ("float", "Float"),
        ("double", "Float"),
        ("qreal", "Float"),
        ("QString", "String"),
        // Qt 6.4+ replaced many `const QString &` parameters with
        // `QAnyStringView` / `QStringView` / `QLatin1StringView` —
        // all string-shaped at the Cute level.
        ("QAnyStringView", "String"),
        ("QStringView", "String"),
        ("QLatin1StringView", "String"),
        ("QStringList", "QStringList"),
        ("QRgb", "Int"),
        ("QPoint", "QPoint"),
        ("QPointF", "QPointF"),
        ("QSize", "QSize"),
        ("QSizeF", "QSizeF"),
        ("QRect", "QRect"),
        ("QRectF", "QRectF"),
        ("QLine", "QLine"),
        ("QLineF", "QLineF"),
        ("QMargins", "QMargins"),
        ("QMarginsF", "QMarginsF"),
        ("QColor", "QColor"),
        ("QUrl", "QUrl"),
        ("QDate", "QDate"),
        ("QDateTime", "QDateTime"),
        // Qt class-form types (Arc-tracked in Cute) that appear
        // as method parameters / return types on object-form
        // bindings (QPushButton's icon, QLabel's pixmap, ...).
        // Bound elsewhere in stdlib as `class X { ... }`; the
        // type_map just teaches the auto-gen which Cute name to
        // emit when one of these appears in a signature.
        ("QPixmap", "QPixmap"),
        ("QImage", "QImage"),
        ("QIcon", "QIcon"),
        ("QFont", "QFont"),
        ("QPen", "QPen"),
        ("QBrush", "QBrush"),
        ("QPainter", "QPainter"),
        ("QIODevice", "QIODevice"),
        ("QByteArray", "QByteArray"),
        ("QVariant", "QVariant"),
        ("QObject", "QObject"),
        ("QWidget", "QWidget"),
        ("QLayout", "QLayout"),
        // Qt namespace enums bound in stdlib/qt/qenums.qpi —
        // listed here so per-class typesystems (qlabel.toml,
        // qslider.toml, ...) can lower their enum-typed
        // Q_PROPERTYs to the matching Cute enum type instead of
        // falling through to the generic `Int` shape. Keep this
        // in sync with `stdlib/qt/typesystem/qenums.toml`. Both
        // qualified (`Qt::AlignmentFlag`) and bare
        // (`AlignmentFlag`) keys are registered — Q_PROPERTY
        // scrape sees the source-text form (often qualified),
        // QFlags<X> unwrap sees the inner enum's bare name.
        ("Qt::AlignmentFlag", "AlignmentFlag"),
        ("AlignmentFlag", "AlignmentFlag"),
        ("Qt::Orientation", "Orientation"),
        ("Orientation", "Orientation"),
        ("Qt::TimerType", "TimerType"),
        ("TimerType", "TimerType"),
        ("Qt::CheckState", "CheckState"),
        ("CheckState", "CheckState"),
        ("Qt::TextFormat", "TextFormat"),
        ("TextFormat", "TextFormat"),
        ("Qt::CursorShape", "CursorShape"),
        ("CursorShape", "CursorShape"),
        ("Qt::FocusPolicy", "FocusPolicy"),
        ("FocusPolicy", "FocusPolicy"),
        ("Qt::ScrollBarPolicy", "ScrollBarPolicy"),
        ("ScrollBarPolicy", "ScrollBarPolicy"),
        ("Qt::MouseButton", "MouseButton"),
        ("MouseButton", "MouseButton"),
        ("Qt::Key", "Key"),
        ("Key", "Key"),
        ("Qt::WindowState", "WindowState"),
        ("WindowState", "WindowState"),
        // Class-nested Qt enums (qslider.h, qlineedit.h, qframe.h,
        // qabstractitemview.h). Same shape as the Qt:: namespace
        // enums above — qualified + bare keys both registered.
        ("QSlider::TickPosition", "TickPosition"),
        ("TickPosition", "TickPosition"),
        ("QLineEdit::EchoMode", "EchoMode"),
        ("EchoMode", "EchoMode"),
        ("QFrame::Shape", "Shape"),
        ("Shape", "Shape"),
        ("QFrame::Shadow", "Shadow"),
        ("Shadow", "Shadow"),
        ("QAbstractItemView::SelectionMode", "SelectionMode"),
        ("SelectionMode", "SelectionMode"),
        ("QAbstractItemView::SelectionBehavior", "SelectionBehavior"),
        ("SelectionBehavior", "SelectionBehavior"),
        ("QAbstractItemView::ScrollMode", "ScrollMode"),
        ("ScrollMode", "ScrollMode"),
        ("QLayoutItem", "QLayoutItem"),
        ("QAbstractItemModel", "QAbstractItemModel"),
        ("QAbstractButton", "QAbstractButton"),
        ("QAbstractSlider", "QAbstractSlider"),
        ("QFrame", "QFrame"),
        ("QAbstractScrollArea", "QAbstractScrollArea"),
        ("QAbstractItemView", "QAbstractItemView"),
        ("QPushButton", "QPushButton"),
        ("QLabel", "QLabel"),
        ("QLineEdit", "QLineEdit"),
        ("QHeaderView", "QHeaderView"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    // Self-register every class the typesystem declares — saves the
    // user from having to add `"QNetworkRequest" = "QNetworkRequest"`
    // entries by hand whenever one bound class references another.
    // Enum / Flags entries also register their fully-qualified C++
    // name (e.g. `Qt::AlignmentFlag` → `AlignmentFlag`) so
    // Q_PROPERTY scraping can lower an enum-typed property to the
    // Cute enum type rather than the int fallback.
    for c in &ts.classes {
        m.entry(c.name.clone()).or_insert_with(|| c.name.clone());
        if matches!(c.kind, ClassKind::Enum | ClassKind::Flags) {
            if let Some(ns) = &c.cpp_namespace {
                let qualified = format!("{ns}::{}", c.name);
                m.entry(qualified).or_insert_with(|| c.name.clone());
            }
        }
    }
    for (k, v) in &ts.type_map {
        m.insert(k.clone(), v.clone());
    }
    m
}

fn map_type(ty: clang::Type<'_>, map: &BTreeMap<String, String>) -> Option<CuteType> {
    // Strip outer reference / pointer layers — Cute's surface has
    // no `&` or `*` and treats both `QWidget *` and `QWidget` (and
    // `const QPoint &p` and `QPoint p`) identically. QObject-class
    // types are Arc-tracked at the Cute level, so a `QWidget*`
    // parameter on the C++ side maps to the same Cute name as a
    // `QWidget` value.
    let mut t = ty;
    loop {
        match t.get_kind() {
            TypeKind::LValueReference | TypeKind::RValueReference | TypeKind::Pointer => {
                t = t.get_pointee_type()?;
            }
            _ => break,
        }
    }
    let display = t.get_display_name();
    let key = strip_const(&display);
    if key == "void" {
        return Some(CuteType::Void);
    }
    // Reject `std::chrono::*` types — Qt 6 added chrono-typed
    // overloads alongside the int ones (`setInterval(milliseconds)`
    // alongside `setInterval(int msec)`). For reasons that aren't
    // fully clear (libclang's display_name on chrono typedefs
    // sometimes resolves all the way to the underlying integer
    // type), the display string isn't reliable for these — fall
    // back to dedup-by-Cute-signature later in the pipeline.
    if key.contains("chrono::") {
        return None;
    }
    // Try the type_map first — this catches typesystem-bound enums
    // (`Qt::AlignmentFlag` → Cute `AlignmentFlag`) before the
    // generic enum→Int auto-fallback below picks them up. Without
    // this order, every Q_PROPERTY typed as a Qt enum would lower
    // as `Int` even when the user explicitly bound the matching
    // `extern enum` in qenums.toml.
    if let Some(mapped) = map.get(key.trim()).cloned() {
        return Some(CuteType::Named(mapped));
    }
    // Same fallback for QFlags<E> — try the type_map for the
    // qualified `Qt::Alignment` shape first via the canonical
    // display name.
    let canonical = t.get_canonical_type().get_display_name();
    let canonical_key = strip_const(&canonical);
    if let Some(mapped) = map.get(canonical_key.trim()).cloned() {
        return Some(CuteType::Named(mapped));
    }
    // QFlags<X> wraps an enum X; try the inner X in the type_map
    // and return that. So `Qt::Alignment = QFlags<Qt::AlignmentFlag>`
    // lowers to Cute `AlignmentFlag` (the bound enum) rather than
    // the generic `Int` fallback. Strip the `QFlags<...>` wrapper
    // by simple text manipulation — works for Qt's standard form.
    if let Some(inner) = canonical_key.trim().strip_prefix("QFlags<") {
        let inner = inner.trim_end_matches('>').trim();
        if let Some(mapped) = map.get(inner).cloned() {
            return Some(CuteType::Named(mapped));
        }
        // Also try the bare last-segment form (`AlignmentFlag`
        // when the inner is `Qt::AlignmentFlag`).
        if let Some(bare) = inner.rsplit("::").next() {
            if let Some(mapped) = map.get(bare).cloned() {
                return Some(CuteType::Named(mapped));
            }
        }
    }
    // Auto-fallback for unbound enums: int-shaped at the ABI
    // level, and Qt's idiom is to pass them as the enum type
    // (`Qt::Alignment`, `Qt::TextFormat`, ...) at compile time
    // but use them interchangeably with int at the call site.
    // Map every unbound enum type to Cute's `Int` so a Q_PROPERTY
    // like `Qt::TimerType timerType` lands as `prop timerType :
    // Int` instead of being silently dropped.
    if let Some(decl) = t.get_declaration() {
        if matches!(decl.get_kind(), EntityKind::EnumDecl) {
            return Some(CuteType::Named("Int".to_string()));
        }
    }
    // QFlags<EnumName> wraps an enum into a bit-or-able set;
    // also int-shaped at the ABI level. Detect via the canonical
    // type display name (the typedef-stripped form), which shows
    // up as `QFlags<...>`.
    if canonical.starts_with("QFlags<") {
        return Some(CuteType::Named("Int".to_string()));
    }
    None
}

fn strip_const(s: &str) -> String {
    s.replace("const ", "").replace("&", "").trim().to_string()
}
