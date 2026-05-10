//! `.qpi` (Cute Qt Package Interface) binding-file loader.
//!
//! Bindings describe the surface of a foreign C++ class to Cute's type
//! checker - method signatures, property types, signal parameters - so
//! calls like `widget.deleteLater()` resolve against a real definition
//! instead of falling through to `Type::External` soft-pass.
//!
//! The format is a strict subset of Cute syntax: `class Name { ... }`
//! with property / signal / fn members, no fn bodies. The Cute parser
//! handles it directly. Codegen never sees these classes - the driver
//! merges them into the HIR/type-check view but emits C++ only for
//! the user module.
//!
//! Stdlib bindings (under `stdlib/qt/`) are baked into the cute binary
//! via `include_str!` so users don't need the source tree.

use cute_syntax::{Module, ParseError, SourceMap, parse_binding};

#[derive(Debug, thiserror::Error)]
pub enum BindingError {
    #[error("failed to parse binding `{name}`: {message}")]
    Parse { name: String, message: String },
}

/// Parse a `.qpi` source string as a Cute module. `name` is used
/// both for the SourceMap entry (so spans land in a real file id)
/// and for diagnostics if the parse fails. Registering the source
/// in the map gives each binding a distinct `FileId` - critical for
/// the visibility check, which keys items by their declaring file
/// id and would otherwise treat all bindings as belonging to file 0
/// (= the entry user file).
pub fn parse_qpi(
    source_map: &mut SourceMap,
    name: &str,
    src: &str,
) -> Result<Module, BindingError> {
    let owned = src.to_string();
    let file_id = source_map.add(format!("<binding:{name}>"), owned);
    let stored = source_map.source(file_id);
    parse_binding(file_id, stored).map_err(|e: ParseError| BindingError::Parse {
        name: name.to_string(),
        message: format!("{e:?}"),
    })
}

/// Like `load_stdlib`, plus optionally the cute_ui bindings used by
/// `gpu_app` projects. Their class names (Window / Button / Row / Column /
/// ListView / TextField) collide with `qtquickcontrols.qpi`, so only the
/// driver — which knows the project's BuildMode — should set the flag.
pub fn load_stdlib_with_cute_ui(
    source_map: &mut SourceMap,
    enable_cute_ui: bool,
) -> Result<Vec<Module>, BindingError> {
    let mut modules = load_stdlib(source_map)?;
    if enable_cute_ui {
        let m = parse_qpi(source_map, "cute_ui.qpi", CUTE_UI_QPI)?;
        modules.push(m);
    }
    Ok(modules)
}

/// Built-in Qt stdlib bindings. The driver loads these before the user
/// source so type-check sees them as if they were declared up-front.
///
/// The QtWidgets bindings (qwidget / qmainwindow / qpushbutton / qlabel
/// / qlayout) are loaded unconditionally even when the program is a
/// QtQuick project. They cost nothing at codegen time (bindings emit
/// no C++) and only show up in diagnostics when the user actually
/// references them. Eagerly loading also keeps the type checker
/// honest: a typo like `widget.deletLater()` is caught the same way
/// regardless of whether the user wrote `view` or `widget`.
pub fn load_stdlib(source_map: &mut SourceMap) -> Result<Vec<Module>, BindingError> {
    let entries: &[(&str, &str)] = &[
        // Core QObject + value types come first because every
        // class binding below transitively depends on them
        // (super-class chain, parameter / return types).
        ("qcore.qpi", QCORE_QPI),
        ("qenums.qpi", QENUMS_QPI),
        ("qobject.qpi", QOBJECT_QPI),
        // Qt value types (QPoint, QSize, QRect, QColor, QDate,
        // QDateTime, QUrl, ...) bound via the `extern value` form.
        // Loaded right after the QObject root so subsequent
        // bindings can reference these in method signatures
        // (e.g. KFormat::formatRelativeDate(date: QDate, ...)).
        ("qvaluetypes.qpi", QVALUETYPES_QPI),
        // Filesystem inspection types — also `extern value`. Auto-
        // generated from typesystem.toml; the curated method allowlist
        // mirrors what the previous handcraft in qcore.qpi exposed.
        ("qbytearray.qpi", QBYTEARRAY_QPI),
        ("qregularexpression.qpi", QREGULAREXPRESSION_QPI),
        ("qfileinfo.qpi", QFILEINFO_QPI),
        ("qdir.qpi", QDIR_QPI),
        ("qfile.qpi", QFILE_QPI),
        ("qthread.qpi", QTHREAD_QPI),
        ("qprocess.qpi", QPROCESS_QPI),
        ("qenvironment.qpi", QENVIRONMENT_QPI),
        ("qclipboard.qpi", QCLIPBOARD_QPI),
        // Qt 6.11 QRangeModel: the QAbstractItemModel adapter that
        // wraps a `QList<T*>` of QObject items as a role-driven model
        // for QML views. Loaded unconditionally so `prop ..., model`
        // synthesis can reference the type.
        ("qrangemodel.qpi", QRANGEMODEL_QPI),
        ("modellist.qpi", MODELLIST_QPI),
        ("code_highlighter.qpi", CODE_HIGHLIGHTER_QPI),
        ("qwidget.qpi", QWIDGET_QPI),
        ("qmainwindow.qpi", QMAINWINDOW_QPI),
        ("qframe.qpi", QFRAME_QPI),
        ("qabstractscrollarea.qpi", QABSTRACTSCROLLAREA_QPI),
        ("qabstractitemmodel.qpi", QABSTRACTITEMMODEL_QPI),
        ("qabstractitemview.qpi", QABSTRACTITEMVIEW_QPI),
        ("qlistview.qpi", QLISTVIEW_QPI),
        ("qtreeview.qpi", QTREEVIEW_QPI),
        ("qtableview.qpi", QTABLEVIEW_QPI),
        ("qabstractbutton.qpi", QABSTRACTBUTTON_QPI),
        ("qabstractslider.qpi", QABSTRACTSLIDER_QPI),
        ("qslider.qpi", QSLIDER_QPI),
        ("qpushbutton.qpi", QPUSHBUTTON_QPI),
        ("qcheckbox.qpi", QCHECKBOX_QPI),
        ("qradiobutton.qpi", QRADIOBUTTON_QPI),
        ("qlabel.qpi", QLABEL_QPI),
        ("qlineedit.qpi", QLINEEDIT_QPI),
        ("qlayout.qpi", QLAYOUT_QPI),
        ("qprogressbar.qpi", QPROGRESSBAR_QPI),
        ("qabstractspinbox.qpi", QABSTRACTSPINBOX_QPI),
        ("qspinbox.qpi", QSPINBOX_QPI),
        ("qcombobox.qpi", QCOMBOBOX_QPI),
        ("qgroupbox.qpi", QGROUPBOX_QPI),
        ("qsplitter.qpi", QSPLITTER_QPI),
        ("qmenu.qpi", QMENU_QPI),
        ("qtextedit.qpi", QTEXTEDIT_QPI),
        ("qstatusbar.qpi", QSTATUSBAR_QPI),
        ("qlistwidget.qpi", QLISTWIDGET_QPI),
        // The aggregate `qwidgets_extra.qpi` from earlier Cute
        // versions has been broken into the per-class auto-gen
        // bindings above; loading binding classes is free at
        // codegen time so we keep them all unconditionally.
        ("qhttpserver.qpi", QHTTPSERVER_QPI),
        ("qcommandlineparser.qpi", QCOMMANDLINEPARSER_QPI),
        // Tier 2: surface area for typical Qt apps. Loading
        // unconditionally costs nothing at codegen (binding items
        // never emit C++) and lets the type checker catch typos
        // like `obj.delteLater()` even when the user code only
        // pulls in QtWidgets, QtQuick, or both.
        ("qtimer.qpi", QTIMER_QPI),
        ("qsettings.qpi", QSETTINGS_QPI),
        ("qjson.qpi", QJSON_QPI),
        ("qfiledialog.qpi", QFILEDIALOG_QPI),
        ("qnetwork.qpi", QNETWORK_QPI),
        ("qsql.qpi", QSQL_QPI),
        ("qtquickcontrols.qpi", QTQUICKCONTROLS_QPI),
        // Tier 3: graphics / multimedia / charts. QPainter and the
        // value-style classes (QPen, QBrush, QFont, QColor, ...) come
        // first because qtsvg and qtcharts reference them. QtCharts is
        // promoted from the per-demo binding so user code no longer
        // needs to redeclare its surfaces; linking still requires
        // `Qt6 COMPONENTS Charts` in cute.toml under [cmake].
        ("qpainter.qpi", QPAINTER_QPI),
        ("qguiapplication.qpi", QGUIAPPLICATION_QPI),
        ("qtsvg.qpi", QTSVG_QPI),
        ("qtprintsupport.qpi", QTPRINTSUPPORT_QPI),
        ("qtmultimedia.qpi", QTMULTIMEDIA_QPI),
        ("qtcharts.qpi", QTCHARTS_QPI),
        ("qtconcurrent.qpi", QTCONCURRENT_QPI),
        // Tier 4: KDE Frameworks 6 — non-QML C++ surfaces. These
        // are usable from `fn` bodies (KAboutData, KConfig, ...)
        // and don't need a `use qml` declaration. Loading is still
        // unconditional: the binding describes type surface only,
        // actual linking is opt-in via cute.toml [cmake].
        ("kcoreaddons.qpi", KCOREADDONS_QPI),
        ("kconfig.qpi", KCONFIG_QPI),
        ("knotifications.qpi", KNOTIFICATIONS_QPI),
        ("ki18n.qpi", KI18N_QPI),
        ("kio.qpi", KIO_QPI),
        // Note: foreign QML modules (Kirigami etc.) are NOT loaded
        // here. They opt in per-project via `use qml "..."` in source,
        // which the driver routes through `load_qml_module` below.
    ];
    entries
        .iter()
        .map(|(name, src)| {
            let mut m = parse_qpi(source_map, name, src)?;
            // Special-case: `QObject` itself is the root of the QObject
            // hierarchy. The Cute parser would default super=None for
            // `class QObject { ... }` and our resolver normally maps
            // None -> implicit-QObject - which would leave QObject as
            // its own parent, a self-loop. Force super to genuinely
            // None here so the chain walk in lookup_method terminates
            // cleanly.
            for item in m.items.iter_mut() {
                if let cute_syntax::ast::Item::Class(c) = item {
                    if c.name.name == "QObject" {
                        c.super_class = None;
                    }
                }
            }
            Ok(m)
        })
        .collect()
}

/// Per-URI metadata for a known QML module. `qpi_source` is `Some`
/// when the compiler bundles a `.qpi` for type-check (Kirigami,
/// future foreign modules); `None` for QML modules whose surface is
/// already covered by the always-loaded `qtquickcontrols.qpi` (the
/// QtQuick basics) or that don't need any type-check binding —
/// `use qml "QtQuick.Layouts"` for example just controls the QML
/// import line, the user's view body uses `Layout.*` attached
/// properties which Cute soft-passes regardless. `default_version`
/// is `None` for version-less modules (Qt 6 modular form).
pub struct QmlBinding {
    pub binding_name: &'static str,
    pub qpi_source: Option<&'static str>,
    pub default_version: Option<&'static str>,
}

/// Look up metadata for a QML module URI. Returns `None` for
/// unknown URIs — those still get an `import` line emitted in
/// the QML output (the user's `use qml` declaration is honoured),
/// but property / signal references on those types soft-pass at
/// type-check.
pub fn lookup_qml_module(uri: &str) -> Option<QmlBinding> {
    match uri {
        // QtQuick basics — version-less, type surface is loaded
        // by `load_stdlib` via qtquickcontrols.qpi (which actually
        // covers Item / Rectangle / Row / Column / Button / Label
        // / etc.). `use qml "QtQuick"` is what flips the import
        // line on for users who want their `.cute` source to read
        // self-contained.
        "QtQuick" => Some(QmlBinding {
            binding_name: "QtQuick",
            qpi_source: None,
            default_version: None,
        }),
        "QtQuick.Controls" => Some(QmlBinding {
            binding_name: "QtQuick.Controls",
            qpi_source: None,
            default_version: None,
        }),
        "QtQuick.Controls.Material" => Some(QmlBinding {
            binding_name: "QtQuick.Controls.Material",
            qpi_source: None,
            default_version: None,
        }),
        "QtQuick.Layouts" => Some(QmlBinding {
            binding_name: "QtQuick.Layouts",
            qpi_source: None,
            default_version: None,
        }),
        // Foreign QML modules with bundled bindings.
        "org.kde.kirigami" => Some(QmlBinding {
            binding_name: "kirigami.qpi",
            qpi_source: Some(KIRIGAMI_QPI),
            // Modern Kirigami (Qt 6) ships version-less; pinning a
            // version at the import line breaks loading.
            default_version: None,
        }),
        // KItemModels' QML façade. Source-level opt-in via
        // `use qml "org.kde.kitemmodels" as Kim`.
        "org.kde.kitemmodels" => Some(QmlBinding {
            binding_name: "kitemmodels.qpi",
            qpi_source: Some(KITEMMODELS_QPI),
            default_version: None,
        }),
        // KCoreAddons' QML façade — provides the `Format` singleton
        // and a QML `AboutData` view of the application's KAboutData.
        "org.kde.coreaddons" => Some(QmlBinding {
            binding_name: "kcoreaddons_qml.qpi",
            qpi_source: Some(KCOREADDONS_QML_QPI),
            default_version: None,
        }),
        _ => None,
    }
}

/// Load a foreign QML module's binding. The compiler resolves the
/// URI through `lookup_qml_module` (returning `None` for unknown
/// modules — those still get an import line in the QML output but
/// type-check soft-passes); when an alias is present, every class
/// declared in the binding is namespace-mangled with `<alias>_X`
/// so it doesn't clash with QtQuick.Controls or other modules that
/// share simple names.
pub fn load_qml_module(
    source_map: &mut SourceMap,
    uri: &str,
    alias: Option<&str>,
) -> Result<Option<Module>, BindingError> {
    let Some(binding) = lookup_qml_module(uri) else {
        return Ok(None);
    };
    let Some(qpi) = binding.qpi_source else {
        // Known URI without a bundled binding (QtQuick basics
        // etc.) — caller will still emit the import line, no
        // Module gets added to the type-check view.
        return Ok(None);
    };
    let mut m = parse_qpi(source_map, binding.binding_name, qpi)?;
    if let Some(prefix) = alias {
        apply_namespace_mangle(&mut m, prefix);
    }
    Ok(Some(m))
}

/// Rewrite every class declared in `module` to a `<prefix>_<original>`
/// name and rewrite intra-module class references the same way. References
/// to non-Kirigami types (QObject, String, ...) are left alone — the
/// .qpi was written without the namespace prefix specifically to keep
/// the source files readable.
fn apply_namespace_mangle(module: &mut Module, prefix: &str) {
    use cute_syntax::ast::{ClassMember, Item, TypeKind};

    // Pass 1: collect the set of class names declared in this file.
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in &module.items {
        if let Item::Class(c) = item {
            declared.insert(c.name.name.clone());
        }
    }

    // Pass 2: rewrite. We rename the class itself and any `< Super`
    // / property type / method param/return type whose leaf name is
    // in the declared set.
    let rewrite_path = |path: &mut Vec<cute_syntax::ast::Ident>| {
        if let Some(last) = path.last_mut() {
            if declared.contains(&last.name) {
                last.name = format!("{prefix}_{}", last.name);
            }
        }
    };
    fn rewrite_type_expr(
        t: &mut cute_syntax::ast::TypeExpr,
        declared: &std::collections::HashSet<String>,
        prefix: &str,
    ) {
        match &mut t.kind {
            TypeKind::Named { path, args } => {
                if let Some(last) = path.last_mut() {
                    if declared.contains(&last.name) {
                        last.name = format!("{prefix}_{}", last.name);
                    }
                }
                for a in args {
                    rewrite_type_expr(a, declared, prefix);
                }
            }
            TypeKind::Nullable(inner) | TypeKind::ErrorUnion(inner) => {
                rewrite_type_expr(inner, declared, prefix);
            }
            TypeKind::Fn { params, ret } => {
                for p in params {
                    rewrite_type_expr(p, declared, prefix);
                }
                rewrite_type_expr(ret, declared, prefix);
            }
            TypeKind::SelfType => {}
        }
    }
    let _ = rewrite_path;

    for item in module.items.iter_mut() {
        if let Item::Class(c) = item {
            c.name.name = format!("{prefix}_{}", c.name.name);
            if let Some(sup) = c.super_class.as_mut() {
                rewrite_type_expr(sup, &declared, prefix);
            }
            for m in c.members.iter_mut() {
                match m {
                    ClassMember::Property(p) => {
                        rewrite_type_expr(&mut p.ty, &declared, prefix);
                    }
                    ClassMember::Field(f) => {
                        rewrite_type_expr(&mut f.ty, &declared, prefix);
                    }
                    ClassMember::Signal(s) => {
                        for p in s.params.iter_mut() {
                            rewrite_type_expr(&mut p.ty, &declared, prefix);
                        }
                    }
                    ClassMember::Fn(f) | ClassMember::Slot(f) => {
                        for p in f.params.iter_mut() {
                            rewrite_type_expr(&mut p.ty, &declared, prefix);
                        }
                        if let Some(t) = f.return_ty.as_mut() {
                            rewrite_type_expr(t, &declared, prefix);
                        }
                    }
                    ClassMember::Init(i) => {
                        for p in i.params.iter_mut() {
                            rewrite_type_expr(&mut p.ty, &declared, prefix);
                        }
                    }
                    ClassMember::Deinit(_) => {}
                }
            }
        }
    }
}

const QCORE_QPI: &str = include_str!("../../../stdlib/qt/qcore.qpi");
const QENUMS_QPI: &str = include_str!("../../../stdlib/qt/qenums.qpi");
const QOBJECT_QPI: &str = include_str!("../../../stdlib/qt/qobject.qpi");
const QRANGEMODEL_QPI: &str = include_str!("../../../stdlib/qt/qrangemodel.qpi");
const MODELLIST_QPI: &str = include_str!("../../../stdlib/qt/modellist.qpi");
const CODE_HIGHLIGHTER_QPI: &str = include_str!("../../../stdlib/qt/code_highlighter.qpi");
const QVALUETYPES_QPI: &str = include_str!("../../../stdlib/qt/qvaluetypes.qpi");
const QBYTEARRAY_QPI: &str = include_str!("../../../stdlib/qt/qbytearray.qpi");
const QREGULAREXPRESSION_QPI: &str = include_str!("../../../stdlib/qt/qregularexpression.qpi");
const QFILEINFO_QPI: &str = include_str!("../../../stdlib/qt/qfileinfo.qpi");
const QDIR_QPI: &str = include_str!("../../../stdlib/qt/qdir.qpi");
const QFILE_QPI: &str = include_str!("../../../stdlib/qt/qfile.qpi");
const QTHREAD_QPI: &str = include_str!("../../../stdlib/qt/qthread.qpi");
const QPROCESS_QPI: &str = include_str!("../../../stdlib/qt/qprocess.qpi");
const QENVIRONMENT_QPI: &str = include_str!("../../../stdlib/qt/qenvironment.qpi");
const QCLIPBOARD_QPI: &str = include_str!("../../../stdlib/qt/qclipboard.qpi");
const QWIDGET_QPI: &str = include_str!("../../../stdlib/qt/qwidget.qpi");
const QMAINWINDOW_QPI: &str = include_str!("../../../stdlib/qt/qmainwindow.qpi");
const QFRAME_QPI: &str = include_str!("../../../stdlib/qt/qframe.qpi");
const QABSTRACTSCROLLAREA_QPI: &str = include_str!("../../../stdlib/qt/qabstractscrollarea.qpi");
const QABSTRACTITEMMODEL_QPI: &str = include_str!("../../../stdlib/qt/qabstractitemmodel.qpi");
const QABSTRACTITEMVIEW_QPI: &str = include_str!("../../../stdlib/qt/qabstractitemview.qpi");
const QLISTVIEW_QPI: &str = include_str!("../../../stdlib/qt/qlistview.qpi");
const QTREEVIEW_QPI: &str = include_str!("../../../stdlib/qt/qtreeview.qpi");
const QTABLEVIEW_QPI: &str = include_str!("../../../stdlib/qt/qtableview.qpi");
const QABSTRACTBUTTON_QPI: &str = include_str!("../../../stdlib/qt/qabstractbutton.qpi");
const QABSTRACTSLIDER_QPI: &str = include_str!("../../../stdlib/qt/qabstractslider.qpi");
const QSLIDER_QPI: &str = include_str!("../../../stdlib/qt/qslider.qpi");
const QPUSHBUTTON_QPI: &str = include_str!("../../../stdlib/qt/qpushbutton.qpi");
const QCHECKBOX_QPI: &str = include_str!("../../../stdlib/qt/qcheckbox.qpi");
const QRADIOBUTTON_QPI: &str = include_str!("../../../stdlib/qt/qradiobutton.qpi");
const QLABEL_QPI: &str = include_str!("../../../stdlib/qt/qlabel.qpi");
const QLINEEDIT_QPI: &str = include_str!("../../../stdlib/qt/qlineedit.qpi");
const QPROGRESSBAR_QPI: &str = include_str!("../../../stdlib/qt/qprogressbar.qpi");
const QABSTRACTSPINBOX_QPI: &str = include_str!("../../../stdlib/qt/qabstractspinbox.qpi");
const QSPINBOX_QPI: &str = include_str!("../../../stdlib/qt/qspinbox.qpi");
const QCOMBOBOX_QPI: &str = include_str!("../../../stdlib/qt/qcombobox.qpi");
const QGROUPBOX_QPI: &str = include_str!("../../../stdlib/qt/qgroupbox.qpi");
const QSPLITTER_QPI: &str = include_str!("../../../stdlib/qt/qsplitter.qpi");
const QMENU_QPI: &str = include_str!("../../../stdlib/qt/qmenu.qpi");
const QTEXTEDIT_QPI: &str = include_str!("../../../stdlib/qt/qtextedit.qpi");
const QSTATUSBAR_QPI: &str = include_str!("../../../stdlib/qt/qstatusbar.qpi");
const QLISTWIDGET_QPI: &str = include_str!("../../../stdlib/qt/qlistwidget.qpi");
const QLAYOUT_QPI: &str = include_str!("../../../stdlib/qt/qlayout.qpi");
const QHTTPSERVER_QPI: &str = include_str!("../../../stdlib/qt/qhttpserver.qpi");
const QCOMMANDLINEPARSER_QPI: &str = include_str!("../../../stdlib/qt/qcommandlineparser.qpi");
const QTIMER_QPI: &str = include_str!("../../../stdlib/qt/qtimer.qpi");
const QSETTINGS_QPI: &str = include_str!("../../../stdlib/qt/qsettings.qpi");
const QJSON_QPI: &str = include_str!("../../../stdlib/qt/qjson.qpi");
const QFILEDIALOG_QPI: &str = include_str!("../../../stdlib/qt/qfiledialog.qpi");
const QNETWORK_QPI: &str = include_str!("../../../stdlib/qt/qnetwork.qpi");
const QSQL_QPI: &str = include_str!("../../../stdlib/qt/qsql.qpi");
const QTQUICKCONTROLS_QPI: &str = include_str!("../../../stdlib/qt/qtquickcontrols.qpi");
const QPAINTER_QPI: &str = include_str!("../../../stdlib/qt/qpainter.qpi");
const QGUIAPPLICATION_QPI: &str = include_str!("../../../stdlib/qt/qguiapplication.qpi");
const QTSVG_QPI: &str = include_str!("../../../stdlib/qt/qtsvg.qpi");
const QTPRINTSUPPORT_QPI: &str = include_str!("../../../stdlib/qt/qtprintsupport.qpi");
const QTMULTIMEDIA_QPI: &str = include_str!("../../../stdlib/qt/qtmultimedia.qpi");
const QTCHARTS_QPI: &str = include_str!("../../../stdlib/qt/qtcharts.qpi");
const QTCONCURRENT_QPI: &str = include_str!("../../../stdlib/qt/qtconcurrent.qpi");
const KCOREADDONS_QPI: &str = include_str!("../../../stdlib/qt/kcoreaddons.qpi");
const KCONFIG_QPI: &str = include_str!("../../../stdlib/qt/kconfig.qpi");
const KNOTIFICATIONS_QPI: &str = include_str!("../../../stdlib/qt/knotifications.qpi");
const KI18N_QPI: &str = include_str!("../../../stdlib/qt/ki18n.qpi");
const KIO_QPI: &str = include_str!("../../../stdlib/qt/kio.qpi");
const KIRIGAMI_QPI: &str = include_str!("../../../stdlib/qt/kirigami.qpi");
const KITEMMODELS_QPI: &str = include_str!("../../../stdlib/qt/kitemmodels.qpi");
const KCOREADDONS_QML_QPI: &str = include_str!("../../../stdlib/qt/kcoreaddons_qml.qpi");
// Bindings used only by `gpu_app` projects; loaded conditionally because
// Window / Column / Row / Text / Button collide with qtquickcontrols.qpi.
const CUTE_UI_QPI: &str = include_str!("../../../stdlib/cute_ui/cute_ui.qpi");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qobject_binding_parses_and_has_methods() {
        let mut sm = SourceMap::default();
        let modules = load_stdlib(&mut sm).expect("load stdlib");
        assert!(!modules.is_empty());
        let qobject = modules
            .iter()
            .flat_map(|m| m.items.iter())
            .find_map(|i| match i {
                cute_syntax::ast::Item::Class(c) if c.name.name == "QObject" => Some(c),
                _ => None,
            })
            .expect("QObject class in stdlib");
        // Spot-check expected members.
        let method_names: Vec<&str> = qobject
            .members
            .iter()
            .filter_map(|m| match m {
                cute_syntax::ast::ClassMember::Fn(f) => Some(f.name.name.as_str()),
                _ => None,
            })
            .collect();
        assert!(method_names.contains(&"deleteLater"));
        assert!(method_names.contains(&"setParent"));
    }

    #[test]
    fn qobject_super_class_is_genuinely_none() {
        let mut sm = SourceMap::default();
        let modules = load_stdlib(&mut sm).expect("load stdlib");
        let qobject = modules
            .iter()
            .flat_map(|m| m.items.iter())
            .find_map(|i| match i {
                cute_syntax::ast::Item::Class(c) if c.name.name == "QObject" => Some(c),
                _ => None,
            })
            .unwrap();
        assert!(
            qobject.super_class.is_none(),
            "QObject is the root, no super"
        );
    }

    #[test]
    fn stdlib_does_not_auto_load_kirigami() {
        // Foreign QML modules (Kirigami etc.) are no longer baked into
        // load_stdlib's output — projects opt in via `use qml "..."`
        // in source, which the driver routes through load_qml_module.
        let mut sm = SourceMap::default();
        let modules = load_stdlib(&mut sm).expect("load stdlib");
        let class_names: std::collections::HashSet<String> = modules
            .iter()
            .flat_map(|m| m.items.iter())
            .filter_map(|i| match i {
                cute_syntax::ast::Item::Class(c) => Some(c.name.name.clone()),
                _ => None,
            })
            .collect();
        // No Kirigami classes in the default stdlib output.
        assert!(!class_names.contains("Kirigami_PageRow"));
        assert!(!class_names.contains("PageRow"));
    }

    #[test]
    fn load_qml_module_kirigami_with_alias_namespace_mangles() {
        let mut sm = SourceMap::default();
        let module = load_qml_module(&mut sm, "org.kde.kirigami", Some("Kirigami"))
            .expect("load qml module")
            .expect("kirigami URI is bundled");
        let class_names: std::collections::HashSet<String> = module
            .items
            .iter()
            .filter_map(|i| match i {
                cute_syntax::ast::Item::Class(c) => Some(c.name.name.clone()),
                _ => None,
            })
            .collect();
        for expected in [
            "Kirigami_ApplicationWindow",
            "Kirigami_Page",
            "Kirigami_PageRow",
            "Kirigami_ScrollablePage",
            "Kirigami_Theme",
        ] {
            assert!(
                class_names.contains(expected),
                "alias `Kirigami` should namespace-mangle `{expected}`"
            );
        }
    }

    #[test]
    fn load_qml_module_kirigami_without_alias_keeps_bare_names() {
        let mut sm = SourceMap::default();
        let module = load_qml_module(&mut sm, "org.kde.kirigami", None)
            .expect("load qml module")
            .expect("kirigami URI is bundled");
        let class_names: std::collections::HashSet<String> = module
            .items
            .iter()
            .filter_map(|i| match i {
                cute_syntax::ast::Item::Class(c) => Some(c.name.name.clone()),
                _ => None,
            })
            .collect();
        assert!(class_names.contains("PageRow"));
        assert!(class_names.contains("Page"));
        assert!(!class_names.contains("Kirigami_PageRow"));
    }

    #[test]
    fn load_qml_module_kitemmodels_namespace_mangles() {
        let mut sm = SourceMap::default();
        let module = load_qml_module(&mut sm, "org.kde.kitemmodels", Some("Kim"))
            .expect("load qml module")
            .expect("kitemmodels URI is bundled");
        let class_names: std::collections::HashSet<String> = module
            .items
            .iter()
            .filter_map(|i| match i {
                cute_syntax::ast::Item::Class(c) => Some(c.name.name.clone()),
                _ => None,
            })
            .collect();
        assert!(class_names.contains("Kim_SortFilterProxyModel"));
        assert!(class_names.contains("Kim_ConcatenateRowsProxyModel"));
    }

    #[test]
    fn load_qml_module_coreaddons_qml_namespace_mangles() {
        let mut sm = SourceMap::default();
        let module = load_qml_module(&mut sm, "org.kde.coreaddons", Some("KC"))
            .expect("load qml module")
            .expect("coreaddons QML URI is bundled");
        let class_names: std::collections::HashSet<String> = module
            .items
            .iter()
            .filter_map(|i| match i {
                cute_syntax::ast::Item::Class(c) => Some(c.name.name.clone()),
                _ => None,
            })
            .collect();
        assert!(class_names.contains("KC_Format"));
        assert!(class_names.contains("KC_AboutData"));
    }

    #[test]
    fn load_qml_module_unknown_uri_returns_none() {
        let mut sm = SourceMap::default();
        let res = load_qml_module(&mut sm, "com.example.unknown", None).expect("no parse error");
        assert!(
            res.is_none(),
            "unknown URIs return None — type-check soft-passes"
        );
    }

    #[test]
    fn qvaluetypes_loaded_as_extern_value_classes() {
        // Qt value types (QPoint, QSize, QRect, QColor, QDate, QUrl,
        // ...) ship as `extern value` classes — the type-check view
        // sees them as classes; codegen specifically lowers
        // `T.new(args)` to `T(args)` and member access via `.`.
        let mut sm = SourceMap::default();
        let modules = load_stdlib(&mut sm).expect("load stdlib");
        let extern_value_classes: std::collections::HashSet<String> = modules
            .iter()
            .flat_map(|m| m.items.iter())
            .filter_map(|i| match i {
                cute_syntax::ast::Item::Class(c) if c.is_extern_value => Some(c.name.name.clone()),
                _ => None,
            })
            .collect();
        for expected in [
            "QPoint",
            "QPointF",
            "QSize",
            "QSizeF",
            "QRect",
            "QRectF",
            "QColor",
            "QUrl",
            "QDate",
            "QDateTime",
        ] {
            assert!(
                extern_value_classes.contains(expected),
                "stdlib should bind `{expected}` as extern value"
            );
        }
    }

    #[test]
    fn tier4_kf6_bindings_parse_and_export_expected_classes() {
        // KCoreAddons / KConfig / KNotifications / KI18n / KIO are
        // loaded unconditionally (their bindings cost nothing at
        // codegen and link-time opt-in is via cute.toml [cmake]).
        // Spot-check that the type checker sees the surface user code
        // is most likely to call.
        let mut sm = SourceMap::default();
        let modules = load_stdlib(&mut sm).expect("load stdlib");
        let class_names: std::collections::HashSet<String> = modules
            .iter()
            .flat_map(|m| m.items.iter())
            .filter_map(|i| match i {
                cute_syntax::ast::Item::Class(c) => Some(c.name.name.clone()),
                _ => None,
            })
            .collect();
        for expected in [
            // KCoreAddons
            "KAboutData",
            "KAboutPerson",
            "KAboutLicense",
            "KFormat",
            // KConfig
            "KConfig",
            "KSharedConfig",
            "KConfigGroup",
            // KNotifications
            "KNotification",
            // KI18n
            "KLocalizedString",
            // KIO
            "KJob",
            "OpenUrlJob",
            "CommandLauncherJob",
            "CopyJob",
            "DeleteJob",
            "KFileItem",
        ] {
            assert!(
                class_names.contains(expected),
                "stdlib bindings should include `{expected}` after KF6 tier-4 expansion"
            );
        }
    }

    #[test]
    fn tier3_bindings_parse_and_export_expected_classes() {
        let mut sm = SourceMap::default();
        let modules = load_stdlib(&mut sm).expect("load stdlib");
        let class_names: std::collections::HashSet<String> = modules
            .iter()
            .flat_map(|m| m.items.iter())
            .filter_map(|i| match i {
                cute_syntax::ast::Item::Class(c) => Some(c.name.name.clone()),
                _ => None,
            })
            .collect();
        // Spot-check tier-3 surface: graphics, gui-app, charts,
        // multimedia, print, svg, concurrent.
        for expected in [
            "QPainter",
            "QPen",
            "QBrush",
            "QFont",
            "QColor",
            "QGuiApplication",
            "QClipboard",
            "QChart",
            "QChartView",
            "QPieSeries",
            "QLineSeries",
            "QValueAxis",
            "QSvgRenderer",
            "QPrinter",
            "QMediaPlayer",
            "QtConcurrent",
            "QFuture",
        ] {
            assert!(
                class_names.contains(expected),
                "stdlib bindings should include `{expected}`"
            );
        }
    }
}
