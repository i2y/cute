//! QMetaObject moc-replacement generator targeting Qt 6.11 meta-object
//! template form.
//!
//! Qt 6.9+ moc emits a `qt_create_metaobjectdata<Tag>()` template
//! specialization plus a small set of `QtMocHelpers::{StringRefStorage,
//! UintData, SignalData, MethodData, PropertyData}` constructors instead
//! of the historical raw `qt_meta_data_*` uint arrays. Qt's `Q_OBJECT`
//! macro declares the surrounding template members
//! (`qt_create_metaobjectdata`, `qt_staticMetaObjectStaticContent` /
//! `RelocatingContent` variable templates, `staticMetaObject`,
//! `metaObject()`, `qt_metacast`, `qt_metacall`, `qt_static_metacall`),
//! and we provide the specialization plus the four function definitions.
//!
//! Cutec is still a moc replacement: we never invoke `moc` at build time.
//! We do use `Q_OBJECT` (one-line macro that Qt provides for users) to
//! anchor the template machinery without re-implementing Qt's private
//! variable templates ourselves.
//!
//! Reference: actual Qt 6.11 `moc` output for a hand-written `Q_OBJECT`
//! class - see `tests/e2e/qt-verify/` (vs running moc) for the diff
//! check that keeps this in sync.

use std::fmt::Write;

#[derive(Debug, Clone)]
pub struct ClassInfo {
    pub name: String,
    pub super_class: String,
    pub properties: Vec<PropInfo>,
    pub signals: Vec<SignalInfo>,
    pub methods: Vec<MethodInfo>,
    pub slots: Vec<MethodInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropKind {
    /// Plain non-bindable storage (`prop x : T`). External `obj.x = v`
    /// goes through the setter; `@x = v` (AtIdent) writes m_x directly.
    Plain,
    /// `prop x : T, bindable` — writable input bindable backed by
    /// `QObjectBindableProperty`. Setter delegates to `setValue`.
    Bindable,
    /// `prop x : T, bind { expr }` — derived (read-only) prop backed
    /// by `QObjectBindableProperty` whose binding is set in the
    /// constructor via `setBinding(lambda)`. Qt's binding system
    /// auto-tracks deps the lambda reads.
    Bind,
    /// `prop x : T, fresh { expr }` — function-style (read-only) prop
    /// backed by `QObjectComputedProperty`. The getter re-evaluates
    /// every read; deps are not auto-tracked. The synthesized
    /// `<x>_changed` notify is fanned out from the class's input
    /// bindables in the constructor so QML/QtWidget bindings still
    /// react when the inputs are bindable.
    Fresh,
}

#[derive(Debug, Clone)]
pub struct PropInfo {
    pub name: String,
    pub setter: String,
    pub cpp_type: String,
    /// `QMetaType::QString`, `QMetaType::Bool`, etc.
    pub qmetatype: &'static str,
    pub pass_by_const_ref: bool,
    pub readable: bool,
    pub writable: bool,
    pub notify_signal_idx: Option<usize>,
    pub notify_signal_name: Option<String>,
    pub kind: PropKind,
    /// `bindable<X>()` method name — precomputed by cute-codegen so
    /// cute-meta doesn't have to recapitulate the naming convention.
    /// `Some` only when the prop has a working QBindable surface
    /// (Bindable / Bind); `None` for Plain and Fresh.
    pub bindable_getter: Option<String>,
    /// True for a `prop xs : ModelList<T>` declaration. The cpp_type
    /// is `::cute::ModelList<T*>*`, storage is heap-allocated and parented,
    /// and the public surface is the ModelList itself (read access via
    /// the pointer; mutation through `xs->append(...)`, `xs->removeAt(...)`,
    /// `xs->clear()`, `xs->replace(...)` etc., each of which fires
    /// QAbstractItemModel signals via QRangeModel internals). No setter,
    /// no NOTIFY, Q_PROPERTY emitted as CONSTANT.
    pub is_model_list: bool,
}

impl PropInfo {
    /// True iff the prop's storage exposes a working `QBindable<T>`
    /// surface — i.e. Bindable or Bind. Fresh's QObjectComputedProperty
    /// has a QBindable but its subscription path is unimplemented, so
    /// QML's BINDABLE-driven dependency tracking would silently no-op
    /// there; callers should use the synthesized NOTIFY route instead.
    pub fn has_bindable_surface(&self) -> bool {
        matches!(self.kind, PropKind::Bindable | PropKind::Bind)
    }
}

#[derive(Debug, Clone)]
pub struct SignalInfo {
    pub name: String,
    pub params: Vec<ParamInfo>,
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub name: String,
    pub params: Vec<ParamInfo>,
    pub return_type: String,
    /// `true` for `static fn name(...)` declarations — class-scoped
    /// free functions with no implicit `self`. Header emission
    /// substitutes `static` for the usual `Q_INVOKABLE` decoration;
    /// the QMetaObject method table excludes static entries
    /// (Q_INVOKABLE is meaningless without a meta-object instance).
    pub is_static: bool,
}

#[derive(Debug, Clone)]
pub struct ParamInfo {
    pub name: String,
    pub cpp_type: String,
    pub qmetatype: String,
}

/// Build a `{{type, nameIdx}, ...}` ParametersArray literal for the
/// MethodData / SignalData constructor. Qt 6.11's MethodData ctor takes a
/// `std::array<FunctionParameter, N>` where `FunctionParameter = {typeIdx,
/// nameIdx}` — passing flat QMetaTypes only happens to compile for N=1
/// via aggregate-paren-init, so emit the full braced form for everything.
fn fn_params_array(params: &[ParamInfo], empty_idx: usize) -> String {
    // std::array<FunctionParameter, N> wraps a C-array; brace-elision is
    // unreliable for the function-arg case in Qt 6.11, so emit the
    // double-brace canonical form: outer {} starts the array, inner {}
    // starts the C-array, then per-param {type, nameIdx} aggregates.
    let mut out = String::from("{{");
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("{{{}, {empty_idx}}}", p.qmetatype));
    }
    out.push_str("}}");
    out
}

#[derive(Debug, Clone)]
pub struct MetaSection {
    /// Spliced into the class body. (Empty string in Qt 6.11 form: the
    /// Q_OBJECT macro provides everything; cute-codegen emits Q_OBJECT
    /// itself - cute-meta is responsible only for the .cpp side.)
    pub header_decls: String,
    /// Appended to the .cpp.
    pub source_defs: String,
}

pub fn emit_meta_section(info: &ClassInfo) -> MetaSection {
    MetaSection {
        header_decls: String::new(),
        source_defs: emit_source(info),
    }
}

fn emit_source(info: &ClassInfo) -> String {
    let mut s = String::new();
    let cls = &info.name;
    let tag = format!("qt_meta_tag_{cls}_t");

    // Tag struct. Real moc uses an Itanium-mangled name; ours is just the
    // class name suffixed with `_t`. The tag is local to the .cpp file
    // because it is in an anonymous namespace, so collisions cannot occur
    // across translation units.
    let _ = writeln!(s, "namespace {{");
    let _ = writeln!(s, "struct {tag} {{}};");
    let _ = writeln!(s, "}}  // namespace");
    let _ = writeln!(s);

    // String table contents: index 0 is the class name; then we collect
    // method names + property names + an empty-string sentinel for the
    // "tag" field (Qt's qt_method_tag, used for moc's @REVISION etc.).
    let mut strings: Vec<String> = Vec::new();
    let intern = |s: &str, table: &mut Vec<String>| -> usize {
        if let Some(i) = table.iter().position(|x| x == s) {
            return i;
        }
        let i = table.len();
        table.push(s.to_string());
        i
    };
    let class_idx = intern(cls, &mut strings);
    debug_assert_eq!(class_idx, 0);

    let mut signal_name_idx = Vec::with_capacity(info.signals.len());
    for sig in &info.signals {
        signal_name_idx.push(intern(&sig.name, &mut strings));
    }
    let empty_idx = intern("", &mut strings);
    let mut method_name_idx = Vec::with_capacity(info.methods.len());
    for m in &info.methods {
        method_name_idx.push(intern(&m.name, &mut strings));
    }
    let mut slot_name_idx = Vec::with_capacity(info.slots.len());
    for sl in &info.slots {
        slot_name_idx.push(intern(&sl.name, &mut strings));
    }
    let mut prop_name_idx = Vec::with_capacity(info.properties.len());
    for p in &info.properties {
        prop_name_idx.push(intern(&p.name, &mut strings));
    }

    // qt_create_metaobjectdata<tag>() template specialization.
    let _ = writeln!(
        s,
        "template <> constexpr inline auto {cls}::qt_create_metaobjectdata<{tag}>()"
    );
    let _ = writeln!(s, "{{");
    let _ = writeln!(s, "    namespace QMC = QtMocConstants;");
    let _ = writeln!(s, "    QtMocHelpers::StringRefStorage qt_stringData {{");
    for st in &strings {
        let _ = writeln!(s, "        \"{}\",", escape_for_c(st));
    }
    let _ = writeln!(s, "    }};");
    let _ = writeln!(s);

    // Methods (signals first, then user methods, then slots).
    let _ = writeln!(s, "    QtMocHelpers::UintData qt_methods {{");
    for (i, sig) in info.signals.iter().enumerate() {
        let sig_template = signal_template(&sig.params);
        let ret_meta = "QMetaType::Void";
        let name_idx = signal_name_idx[i];
        // SignalData<sig>(name_idx, tag_idx, access, return_metatype, [param_metatypes...])
        let mut args = format!("{name_idx}, {empty_idx}, QMC::AccessPublic, {ret_meta}");
        if !sig.params.is_empty() {
            args.push_str(", ");
            args.push_str(&fn_params_array(&sig.params, empty_idx));
        }
        let _ = writeln!(
            s,
            "        // signal {idx}: {name}",
            idx = i,
            name = sig.name
        );
        let _ = writeln!(
            s,
            "        QtMocHelpers::SignalData<{sig_template}>({args}),"
        );
    }
    for (i, m) in info.methods.iter().enumerate() {
        let mt = method_template(&m.return_type, &m.params);
        let ret_meta = qmetatype_for_return(&m.return_type);
        let name_idx = method_name_idx[i];
        let mut args = format!("{name_idx}, {empty_idx}, QMC::AccessPublic, {ret_meta}");
        if !m.params.is_empty() {
            args.push_str(", ");
            args.push_str(&fn_params_array(&m.params, empty_idx));
        }
        let _ = writeln!(
            s,
            "        // method {idx}: {name}",
            idx = info.signals.len() + i,
            name = m.name
        );
        let _ = writeln!(s, "        QtMocHelpers::MethodData<{mt}>({args}),");
    }
    for (i, sl) in info.slots.iter().enumerate() {
        let mt = method_template(&sl.return_type, &sl.params);
        let ret_meta = qmetatype_for_return(&sl.return_type);
        let name_idx = slot_name_idx[i];
        let mut args = format!("{name_idx}, {empty_idx}, QMC::AccessPublic, {ret_meta}");
        if !sl.params.is_empty() {
            args.push_str(", ");
            args.push_str(&fn_params_array(&sl.params, empty_idx));
        }
        let _ = writeln!(
            s,
            "        // slot {idx}: {name}",
            idx = info.signals.len() + info.methods.len() + i,
            name = sl.name
        );
        let _ = writeln!(s, "        QtMocHelpers::MethodData<{mt}>({args}),");
    }
    let _ = writeln!(s, "    }};");

    // Properties.
    let _ = writeln!(s, "    QtMocHelpers::UintData qt_properties {{");
    for (i, p) in info.properties.iter().enumerate() {
        let prop_t = prop_cpp_type_for_template(&p.cpp_type);
        let name_idx = prop_name_idx[i];
        let mut flags = String::from("QMC::DefaultPropertyFlags");
        if p.writable {
            flags.push_str(" | QMC::Writable");
        }
        // moc emits StdCppSet for properties whose setter follows the
        // `set<Name>` convention. cute-codegen's setter_name uses exactly
        // that convention, so we can always set the bit.
        flags.push_str(" | QMC::StdCppSet");
        // Bindable: tells QMetaProperty / QML's binding engine that
        // QMetaObject::BindableProperty access is supported. Set for
        // QObjectBindableProperty-backed kinds (Bindable + Bind);
        // skipped for Fresh because QObjectComputedProperty's QBindable
        // doesn't implement subscription (QML through BINDABLE would
        // silently drop dependency tracking — fall back to the NOTIFY
        // path the constructor wires up for fresh props instead).
        if p.has_bindable_surface() {
            flags.push_str(" | QMC::Bindable");
        }
        let mut args = format!("{name_idx}, {meta}, {flags}", meta = p.qmetatype);
        if let Some(idx) = p.notify_signal_idx {
            let _ = write!(args, ", {idx}");
        }
        let _ = writeln!(s, "        // property {i}: {name}", i = i, name = p.name);
        let _ = writeln!(s, "        QtMocHelpers::PropertyData<{prop_t}>({args}),");
    }
    let _ = writeln!(s, "    }};");
    let _ = writeln!(s, "    QtMocHelpers::UintData qt_enums {{}};");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "    return QtMocHelpers::metaObjectData<{cls}, {tag}>(QMC::MetaObjectFlag{{}}, qt_stringData, qt_methods, qt_properties, qt_enums);"
    );
    let _ = writeln!(s, "}}");
    let _ = writeln!(s);

    // staticMetaObject. The Q_OBJECT macro DECLARES it; we provide the
    // definition. In Qt 6.11 form the initializer points at variable
    // templates that derive from our qt_create_metaobjectdata.
    let _ = writeln!(
        s,
        "Q_CONSTINIT const QMetaObject {cls}::staticMetaObject = {{ {{"
    );
    let _ = writeln!(
        s,
        "    QMetaObject::SuperData::link<{}::staticMetaObject>(),",
        info.super_class
    );
    let _ = writeln!(
        s,
        "    {cls}::qt_staticMetaObjectStaticContent<{tag}>.stringdata,"
    );
    let _ = writeln!(
        s,
        "    {cls}::qt_staticMetaObjectStaticContent<{tag}>.data,"
    );
    let _ = writeln!(s, "    qt_static_metacall,");
    let _ = writeln!(s, "    nullptr,");
    let _ = writeln!(
        s,
        "    {cls}::qt_staticMetaObjectRelocatingContent<{tag}>.metaTypes,"
    );
    let _ = writeln!(s, "    nullptr");
    let _ = writeln!(s, "}} }};");
    let _ = writeln!(s);

    // metaObject().
    let _ = writeln!(s, "const QMetaObject* {cls}::metaObject() const");
    let _ = writeln!(s, "{{");
    let _ = writeln!(
        s,
        "    return QObject::d_ptr->metaObject ? QObject::d_ptr->dynamicMetaObject() : &staticMetaObject;"
    );
    let _ = writeln!(s, "}}");
    let _ = writeln!(s);

    // qt_metacast.
    let _ = writeln!(s, "void* {cls}::qt_metacast(const char* clname)");
    let _ = writeln!(s, "{{");
    let _ = writeln!(s, "    if (!clname) return nullptr;");
    let _ = writeln!(
        s,
        "    if (!std::strcmp(clname, {cls}::qt_staticMetaObjectStaticContent<{tag}>.strings))"
    );
    let _ = writeln!(s, "        return static_cast<void*>(this);");
    let _ = writeln!(
        s,
        "    return {sup}::qt_metacast(clname);",
        sup = info.super_class
    );
    let _ = writeln!(s, "}}");
    let _ = writeln!(s);

    // qt_metacall.
    let total_methods = info.signals.len() + info.methods.len() + info.slots.len();
    let _ = writeln!(
        s,
        "int {cls}::qt_metacall(QMetaObject::Call _c, int _id, void** _a)"
    );
    let _ = writeln!(s, "{{");
    let _ = writeln!(
        s,
        "    _id = {sup}::qt_metacall(_c, _id, _a);",
        sup = info.super_class
    );
    let _ = writeln!(s, "    if (_id < 0) return _id;");
    if total_methods > 0 {
        let _ = writeln!(s, "    if (_c == QMetaObject::InvokeMetaMethod) {{");
        let _ = writeln!(s, "        if (_id < {total_methods})");
        let _ = writeln!(s, "            qt_static_metacall(this, _c, _id, _a);");
        let _ = writeln!(s, "        _id -= {total_methods};");
        let _ = writeln!(s, "    }}");
        let _ = writeln!(
            s,
            "    if (_c == QMetaObject::RegisterMethodArgumentMetaType) {{"
        );
        let _ = writeln!(s, "        if (_id < {total_methods})");
        let _ = writeln!(
            s,
            "            *reinterpret_cast<QMetaType*>(_a[0]) = QMetaType();"
        );
        let _ = writeln!(s, "        _id -= {total_methods};");
        let _ = writeln!(s, "    }}");
    }
    if !info.properties.is_empty() {
        let pc = info.properties.len();
        let _ = writeln!(
            s,
            "    if (_c == QMetaObject::ReadProperty || _c == QMetaObject::WriteProperty"
        );
        let _ = writeln!(
            s,
            "            || _c == QMetaObject::ResetProperty || _c == QMetaObject::BindableProperty"
        );
        let _ = writeln!(
            s,
            "            || _c == QMetaObject::RegisterPropertyMetaType) {{"
        );
        let _ = writeln!(s, "        qt_static_metacall(this, _c, _id, _a);");
        let _ = writeln!(s, "        _id -= {pc};");
        let _ = writeln!(s, "    }}");
    }
    let _ = writeln!(s, "    return _id;");
    let _ = writeln!(s, "}}");
    let _ = writeln!(s);

    // qt_static_metacall.
    let _ = writeln!(
        s,
        "void {cls}::qt_static_metacall(QObject* _o, QMetaObject::Call _c, int _id, void** _a)"
    );
    let _ = writeln!(s, "{{");
    let _ = writeln!(s, "    (void)_a;");
    let _ = writeln!(s, "    auto* _t = static_cast<{cls}*>(_o);");
    if total_methods > 0 {
        let _ = writeln!(s, "    if (_c == QMetaObject::InvokeMetaMethod) {{");
        let _ = writeln!(s, "        switch (_id) {{");
        let mut idx = 0;
        for sig in &info.signals {
            let unpack = unpack_method_args(&sig.params);
            let arg_list = (0..sig.params.len())
                .map(|i| format!("a{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                s,
                "        case {idx}: {{ {unpack}_t->{name}({arg_list}); break; }}",
                idx = idx,
                unpack = unpack,
                name = sig.name,
                arg_list = arg_list
            );
            idx += 1;
        }
        for m in &info.methods {
            let unpack = unpack_method_args(&m.params);
            let arg_list = (0..m.params.len())
                .map(|i| format!("a{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                s,
                "        case {idx}: {{ {unpack}_t->{name}({arg_list}); break; }}",
                idx = idx,
                unpack = unpack,
                name = m.name,
                arg_list = arg_list
            );
            idx += 1;
        }
        for sl in &info.slots {
            let unpack = unpack_method_args(&sl.params);
            let arg_list = (0..sl.params.len())
                .map(|i| format!("a{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                s,
                "        case {idx}: {{ {unpack}_t->{name}({arg_list}); break; }}",
                idx = idx,
                unpack = unpack,
                name = sl.name,
                arg_list = arg_list
            );
            idx += 1;
        }
        let _ = writeln!(s, "        default: break;");
        let _ = writeln!(s, "        }}");
        let _ = writeln!(s, "    }}");

        // IndexOfMethod for signals (so QObject::connect's pointer-to-member
        // form can resolve the signal's index). Emit one per signal.
        // Format: indexOfMethod<void (Class::*)(args)>(...) - the template
        // parameter is a pointer-to-member-function type literally, so the
        // return type slot is bare `void` (Qt signals are always void).
        if !info.signals.is_empty() {
            let _ = writeln!(s, "    if (_c == QMetaObject::IndexOfMethod) {{");
            for (i, sig) in info.signals.iter().enumerate() {
                let plist = sig
                    .params
                    .iter()
                    .map(|p| p.cpp_type.clone())
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(
                    s,
                    "        if (QtMocHelpers::indexOfMethod<void ({cls}::*)({plist})>(_a, &{cls}::{name}, {i}))",
                    name = sig.name,
                );
                let _ = writeln!(s, "            return;");
            }
            let _ = writeln!(s, "    }}");
        }
    }
    if !info.properties.is_empty() {
        // ReadProperty
        let _ = writeln!(s, "    if (_c == QMetaObject::ReadProperty) {{");
        let _ = writeln!(s, "        void* _v = _a[0];");
        let _ = writeln!(s, "        switch (_id) {{");
        for (i, p) in info.properties.iter().enumerate() {
            let raw_t = if p.cpp_type == "::cute::String" {
                "QString".to_string()
            } else {
                p.cpp_type.clone()
            };
            let _ = writeln!(
                s,
                "        case {i}: *reinterpret_cast<{raw_t}*>(_v) = _t->{name}(); break;",
                name = p.name
            );
        }
        let _ = writeln!(s, "        default: break;");
        let _ = writeln!(s, "        }}");
        let _ = writeln!(s, "    }}");
        // WriteProperty (only writable props participate; computed
        // properties show up as readonly which is what Qt expects).
        let writable_count = info.properties.iter().filter(|p| p.writable).count();
        if writable_count > 0 {
            let _ = writeln!(s, "    if (_c == QMetaObject::WriteProperty) {{");
            let _ = writeln!(s, "        void* _v = _a[0];");
            let _ = writeln!(s, "        switch (_id) {{");
            for (i, p) in info.properties.iter().enumerate() {
                if !p.writable {
                    continue;
                }
                let raw_t = if p.cpp_type == "::cute::String" {
                    "QString".to_string()
                } else {
                    p.cpp_type.clone()
                };
                let _ = writeln!(
                    s,
                    "        case {i}: _t->{set}(*reinterpret_cast<{raw_t}*>(_v)); break;",
                    set = p.setter
                );
            }
            let _ = writeln!(s, "        default: break;");
            let _ = writeln!(s, "        }}");
            let _ = writeln!(s, "    }}");
        }
        // BindableProperty: routes `QObject::bindableProperty(idx)` and
        // QML's binding engine into a `QBindable<T>` constructed over
        // the underlying QObjectBindableProperty / QObjectComputedProperty.
        // qt_metacall already forwards `BindableProperty` here (cf.
        // qt_metacall above) — without this switch the call would no-op
        // and any QML binding through the moc-bindable path would either
        // segfault or silently disconnect.
        let bindable_count = info
            .properties
            .iter()
            .filter(|p| p.has_bindable_surface())
            .count();
        if bindable_count > 0 {
            let _ = writeln!(s, "    if (_c == QMetaObject::BindableProperty) {{");
            let _ = writeln!(s, "        void* _b = _a[0];");
            let _ = writeln!(s, "        switch (_id) {{");
            for (i, p) in info.properties.iter().enumerate() {
                if !p.has_bindable_surface() {
                    continue;
                }
                // `bindableX()` returns a `QBindable<T>` constructed
                // from `&m_x`; assigning to the output slot's
                // `QUntypedBindable` slices off the type tag (the iface
                // pointer carries the type, so the slice is safe).
                let getter = p
                    .bindable_getter
                    .as_deref()
                    .expect("has_bindable_surface() implies bindable_getter is Some");
                let _ = writeln!(
                    s,
                    "        case {i}: *static_cast<QUntypedBindable*>(_b) = _t->{getter}(); break;",
                );
            }
            let _ = writeln!(s, "        default: break;");
            let _ = writeln!(s, "        }}");
            let _ = writeln!(s, "    }}");
        }
    }
    let _ = writeln!(s, "}}");
    let _ = writeln!(s);

    // Signal bodies.
    for (i, sig) in info.signals.iter().enumerate() {
        emit_signal_body(&mut s, cls, sig, i);
    }

    s
}

fn emit_signal_body(s: &mut String, cls: &str, sig: &SignalInfo, idx: usize) {
    // Param shape must match the header's declaration verbatim — a
    // mismatched `const T&` vs `T` is a different overload at link
    // time. The header (cute-codegen `render_param_list`) emits
    // every param by value, so we mirror that here even for types
    // like QString where `const T&` would be the conventional Qt MOC
    // shape. Signals are typically called once with a single arg, so
    // copying a value into the activate path is negligible.
    let params = sig
        .params
        .iter()
        .map(|p| format!("{} {}", p.cpp_type, p.name))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(s, "// SIGNAL {idx}");
    let _ = writeln!(s, "void {cls}::{name}({params})", name = sig.name);
    let _ = writeln!(s, "{{");
    if sig.params.is_empty() {
        let _ = writeln!(
            s,
            "    QMetaObject::activate(this, &staticMetaObject, {idx}, nullptr);"
        );
    } else {
        let arg_array = sig
            .params
            .iter()
            .map(|p| format!("const_cast<void*>(static_cast<const void*>(&{}))", p.name))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(s, "    void* _a[] = {{ nullptr, {arg_array} }};");
        let _ = writeln!(
            s,
            "    QMetaObject::activate(this, &staticMetaObject, {idx}, _a);"
        );
    }
    let _ = writeln!(s, "}}");
    let _ = writeln!(s);
}

// ---- helpers -------------------------------------------------------------

fn escape_for_c(s: &str) -> String {
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

fn signal_template(params: &[ParamInfo]) -> String {
    let plist = params
        .iter()
        .map(|p| p.cpp_type.clone())
        .collect::<Vec<_>>()
        .join(", ");
    format!("void({plist})")
}

fn method_template(return_type: &str, params: &[ParamInfo]) -> String {
    let plist = params
        .iter()
        .map(|p| p.cpp_type.clone())
        .collect::<Vec<_>>()
        .join(", ");
    format!("{return_type}({plist})")
}

fn qmetatype_for_return(return_type: &str) -> String {
    if return_type == "void" {
        "QMetaType::Void".to_string()
    } else {
        // Best effort for builtin returns. Anything we don't know stays
        // QMetaType::Void with a TODO marker; HIR-aware codegen will refine.
        match return_type {
            "bool" => "QMetaType::Bool".to_string(),
            "qint64" => "QMetaType::LongLong".to_string(),
            "double" => "QMetaType::Double".to_string(),
            "::cute::String" | "QString" => "QMetaType::QString".to_string(),
            other => format!("QMetaType::Void /* TODO ret-meta for {other} */"),
        }
    }
}

fn prop_cpp_type_for_template(cpp_type: &str) -> String {
    // PropertyData<T> takes the storage type. cute::String aliases QString;
    // the template form needs the underlying QString to match.
    if cpp_type == "::cute::String" {
        "QString".to_string()
    } else {
        cpp_type.to_string()
    }
}

fn unpack_method_args(params: &[ParamInfo]) -> String {
    let mut out = String::new();
    for (i, p) in params.iter().enumerate() {
        let raw_t = if p.cpp_type == "::cute::String" {
            "QString".to_string()
        } else {
            p.cpp_type.clone()
        };
        // Index 0 of _a is the return slot; args start at 1.
        out.push_str(&format!(
            "auto& a{i} = *reinterpret_cast<{raw_t}*>(_a[{}]); ",
            i + 1
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_class() -> ClassInfo {
        ClassInfo {
            name: "TodoItem".into(),
            super_class: "QObject".into(),
            properties: vec![
                PropInfo {
                    name: "text".into(),
                    setter: "setText".into(),
                    cpp_type: "::cute::String".into(),
                    qmetatype: "QMetaType::QString",
                    pass_by_const_ref: true,
                    readable: true,
                    writable: true,
                    notify_signal_idx: None,
                    notify_signal_name: None,
                    kind: PropKind::Plain,
                    bindable_getter: None,
                    is_model_list: false,
                },
                PropInfo {
                    name: "done".into(),
                    setter: "setDone".into(),
                    cpp_type: "bool".into(),
                    qmetatype: "QMetaType::Bool",
                    pass_by_const_ref: false,
                    readable: true,
                    writable: true,
                    notify_signal_idx: Some(0),
                    notify_signal_name: Some("stateChanged".into()),
                    kind: PropKind::Plain,
                    bindable_getter: None,
                    is_model_list: false,
                },
            ],
            signals: vec![SignalInfo {
                name: "stateChanged".into(),
                params: vec![],
            }],
            methods: vec![MethodInfo {
                name: "toggle".into(),
                params: vec![],
                return_type: "void".into(),
                is_static: false,
            }],
            slots: vec![],
        }
    }

    #[test]
    fn meta_section_uses_qt6_template_form() {
        let m = emit_meta_section(&sample_class());
        let c = &m.source_defs;
        assert!(c.contains("struct qt_meta_tag_TodoItem_t"), "{c}");
        assert!(
            c.contains("template <> constexpr inline auto TodoItem::qt_create_metaobjectdata<qt_meta_tag_TodoItem_t>()"),
            "{c}"
        );
        assert!(c.contains("QtMocHelpers::StringRefStorage"), "{c}");
        assert!(c.contains("QtMocHelpers::SignalData<void()>"), "{c}");
        assert!(c.contains("QtMocHelpers::MethodData<void()>"), "{c}");
        assert!(c.contains("QtMocHelpers::PropertyData<QString>"), "{c}");
        assert!(c.contains("QtMocHelpers::PropertyData<bool>"), "{c}");
        assert!(
            c.contains("QMC::DefaultPropertyFlags | QMC::Writable | QMC::StdCppSet"),
            "{c}"
        );
        assert!(
            c.contains("Q_CONSTINIT const QMetaObject TodoItem::staticMetaObject"),
            "{c}"
        );
        assert!(
            c.contains("qt_staticMetaObjectStaticContent<qt_meta_tag_TodoItem_t>"),
            "{c}"
        );
        assert!(c.contains("void TodoItem::stateChanged()"), "{c}");
        assert!(
            c.contains("QMetaObject::activate(this, &staticMetaObject, 0, nullptr);"),
            "{c}"
        );
    }

    #[test]
    fn property_notify_emits_index_argument() {
        let m = emit_meta_section(&sample_class());
        let c = &m.source_defs;
        // Property `done` has notify_signal_idx 0 -> last arg in PropertyData<bool>(...) is `0`.
        let line = c
            .lines()
            .find(|l| l.contains("PropertyData<bool>"))
            .expect("done property line");
        assert!(
            line.trim_end().ends_with("0),"),
            "expected trailing notify-id arg, got: {line}"
        );
        // Property `text` has no notify -> no trailing arg.
        let line = c
            .lines()
            .find(|l| l.contains("PropertyData<QString>"))
            .expect("text property line");
        assert!(
            line.trim_end().ends_with("StdCppSet),"),
            "expected no trailing notify-id arg, got: {line}"
        );
    }

    #[test]
    fn header_decls_is_empty_in_qt6_template_form() {
        // Q_OBJECT macro now provides everything header-side; cute-meta no
        // longer splices anything into the class body.
        let m = emit_meta_section(&sample_class());
        assert!(
            m.header_decls.is_empty(),
            "expected empty, got: {:?}",
            m.header_decls
        );
    }
}
