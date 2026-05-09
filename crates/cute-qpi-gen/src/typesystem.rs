//! `typesystem.toml` schema — declarative description of which C++
//! classes to turn into `.qpi` `extern value` blocks, and how to map
//! their methods.
//!
//! Each typesystem file is self-describing: it carries the include
//! / framework search paths libclang needs, the C++ → Cute type map,
//! and one entry per class with allowlist / exclude / param-rename
//! overrides. The CLI's `--header` / `--include` flags exist for
//! one-off probing; production runs feed `--typesystem` only.
//!
//! The schema is intentionally narrow — `extern value` only, no
//! QObject / Q_PROPERTY work. That is future scope (separate schema
//! namespace, separate code path).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Top-level shape of a `typesystem.toml` file.
#[derive(Debug, Default, Deserialize)]
pub struct TypeSystem {
    /// Optional template variable defaults. Any `${name}` reference
    /// inside `clang.includes`, `clang.frameworks`, or
    /// `classes[].header` is resolved against this map (with env
    /// overrides) at load time. References can be nested — `[vars]`
    /// entries themselves expand against earlier definitions, so
    /// `qt_core = "${qt_headers}/QtCore.framework/Headers"` works.
    ///
    /// A `[vars]` entry is overridden by the env var
    /// `CUTE_QPI_<UPPERCASE_NAME>` when set. Lets contributors on
    /// non-Homebrew Qt installs (Linux distro packages, custom Qt
    /// builds, MSVC) point the same typesystem at their own paths
    /// without editing the file.
    #[serde(default)]
    pub vars: BTreeMap<String, String>,

    /// Optional clang invocation hints. CLI flags override individual
    /// fields when set; otherwise the typesystem is the source of
    /// truth so the same `cute-qpi-gen --typesystem foo.toml` run
    /// reproduces exactly across machines (modulo the absolute paths
    /// users put inside).
    #[serde(default)]
    pub clang: ClangConfig,

    /// Extra C++ → Cute type-name mappings, layered on top of the
    /// built-in primitive table. Keys are the C++ display name as
    /// libclang reports it (after `const` and reference stripping —
    /// so `int` not `const int &`).
    ///
    /// The built-in table covers `bool` / `int` / floating point /
    /// `QString` / the QtCore value types that appear inside QPoint-
    /// shaped classes. Anything else (e.g. `QStringList`, `QByteArray`)
    /// must appear here or the method that uses it is silently dropped
    /// by the collector — which mirrors the conservative POC behavior.
    #[serde(default)]
    pub type_map: BTreeMap<String, String>,

    /// Per-class binding spec. Emit order in the output file follows
    /// declaration order here, so `[[classes]]` ordering is also a
    /// stylistic choice (group QPoint with QPointF, etc.).
    #[serde(default)]
    pub classes: Vec<ClassSpec>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ClangConfig {
    /// `-isystem` paths fed to clang. Each entry should be an
    /// absolute path; `~` expansion is the caller's responsibility.
    #[serde(default)]
    pub includes: Vec<PathBuf>,

    /// `-F` paths — framework search dirs, needed on macOS so that
    /// transitive `<QtCore/qfoo.h>`-style includes resolve. Without
    /// this, parsing any Qt header that pulls a sibling fails.
    #[serde(default)]
    pub frameworks: Vec<PathBuf>,

    /// `-std=` flag value. Defaults to `c++17` when unset, matching
    /// the rest of the Cute toolchain.
    #[serde(default)]
    pub std: Option<String>,
}

/// Which Cute-side declaration form to emit for this class.
///
/// - `Value` (default) — `extern value <Name> { ... }`. Plain C++
///   value type; pass-by-value, no Q_OBJECT machinery, no signals.
/// - `Object` — `class <Name> < <Super> { prop ... signal ... fn ...
///   }`. Q_OBJECT-derived class with Q_PROPERTY scraped from the
///   header source tokens (the macro is gone after preprocessor
///   expansion, so we scan tokens — see [`crate::clang_walk`]).
/// - `Enum` — `extern enum <Name> { ... }`. C++ enum binding:
///   variant names + values scraped from a libclang `EnumDecl`.
///   Cute side gets a distinct type with member-access lookup.
/// - `Flags` — `extern flags <Name> of <EnumName>`. QFlags<E>
///   companion type over an existing enum binding.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClassKind {
    #[default]
    Value,
    Object,
    Enum,
    Flags,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClassSpec {
    /// C++ class name as it appears in the header (`QPoint`, not
    /// `Qt::QPoint` — the POC doesn't disambiguate namespaces yet).
    pub name: String,

    /// Emission shape. Defaults to `value` for back-compat with
    /// older typesystems that don't set this field.
    #[serde(default)]
    pub kind: ClassKind,

    /// Cute-side super class name. Only meaningful for
    /// `kind = "object"` — value types don't inherit. When unset
    /// for an object class, the generator uses the C++ base
    /// detected via libclang (typically `QObject`).
    #[serde(default)]
    pub super_name: Option<String>,

    /// Header file the class is defined in. Absolute path. The same
    /// file is reparsed for every class entry it contains (libclang
    /// caches at the OS level so this is cheap), keeping each class
    /// spec independently runnable.
    pub header: PathBuf,

    /// Optional allowlist of method names. When set, only methods
    /// whose C++ name matches an entry in this list are emitted —
    /// every other public method is dropped, regardless of the
    /// generic filter rules.
    ///
    /// Three states:
    ///
    /// - **field omitted** (`None`) — accept all collected methods
    /// - **`include = []`** (`Some([])`) — drop every method
    ///   (useful for object classes that only want their
    ///   props / signals through Cute's prop synth, no explicit
    ///   `fn` lines)
    /// - **`include = [...]`** — keep only the listed names
    ///
    /// Signals always pass through regardless — the allowlist
    /// only gates regular `fn` lines.
    #[serde(default)]
    pub include: Option<Vec<String>>,

    /// Optional denylist. Only consulted when `include` is empty.
    /// Useful when a class's surface is mostly fine and you want to
    /// drop a handful of methods rather than enumerate the keepers.
    #[serde(default)]
    pub exclude: Vec<String>,

    /// Allow static C++ methods (e.g. `QDate::currentDate`,
    /// `QUrl::fromLocalFile`) into the binding. Defaults to false
    /// because most value-type bindings only want instance methods;
    /// flip to true on a per-class basis when you want the static
    /// surface too. The handcraft includes statics on QDate / QDateTime
    /// / QUrl, so those entries must set this.
    #[serde(default)]
    pub include_statics: bool,

    /// Per-method parameter renames, keyed by C++ method name. Each
    /// value is a positional list of Cute-side parameter names —
    /// position N replaces the libclang-reported name (or the `_`
    /// fallback when the header omitted the name). Use this when a
    /// header writes `QSize expandedTo(const QSize &) const` without
    /// naming the argument.
    ///
    /// If a method appears here but with fewer entries than C++
    /// arguments, only the leading positions are renamed.
    #[serde(default)]
    pub params: BTreeMap<String, Vec<String>>,

    /// Optional one-line comment emitted above the `extern value`
    /// block. Lets the typesystem carry the same prose context the
    /// handcrafted file used to (`# QPoint / QPointF — 2D
    /// coordinate. ...`).
    #[serde(default)]
    pub comment: Option<String>,

    /// `kind = "flags"` only — the underlying enum the flags
    /// type wraps. Lowers to `extern flags <name> of <flags_of>`.
    /// Ignored for value / object / enum kinds.
    #[serde(default)]
    pub flags_of: Option<String>,

    /// `kind = "enum"` / `kind = "flags"` only — C++ namespace
    /// prefix used at codegen time. Cute writes the variant access
    /// with the bare enum name (`AlignmentFlag.AlignLeft`); the
    /// typesystem stamps `Qt` here so the .qpi declares
    /// `cpp_namespace = "Qt"`. Codegen prefixes:
    /// `AlignmentFlag.AlignLeft` → `Qt::AlignLeft`.
    #[serde(default)]
    pub cpp_namespace: Option<String>,
}

impl TypeSystem {
    /// Read a typesystem.toml from disk and resolve `${var}`
    /// templates in path-shaped fields. Relative paths inside the
    /// toml are kept as-is; the caller resolves them against its
    /// own working directory.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let mut ts: TypeSystem =
            toml::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
        ts.expand_templates()
            .map_err(|e| format!("template expansion in {}: {e}", path.display()))?;
        Ok(ts)
    }

    /// Resolve env overrides + nested `${var}` references inside the
    /// `[vars]` table itself, then substitute every path-shaped
    /// field. Done in-place so downstream code never sees an
    /// unresolved `${...}` token.
    ///
    /// Variable resolution order (lowest precedence → highest):
    ///
    /// 1. **Built-in OS-aware defaults** — `qt_core` / `qt_gui` /
    ///    `qt_widgets` / `qt_charts` / `qt_frameworks` get a sensible
    ///    value per platform (macOS Homebrew framework layout vs
    ///    Linux distro `/usr/include/qt6/...` flat layout). User
    ///    typesystems don't need to declare these unless the host
    ///    has a non-standard install.
    /// 2. **`[vars]` table** in the typesystem.toml — overrides the
    ///    built-in default. Useful for project-specific variables.
    /// 3. **Environment** — `CUTE_QPI_<UPPERCASE_NAME>` overrides
    ///    both. Lets a contributor or CI set a path without editing
    ///    the typesystem.
    fn expand_templates(&mut self) -> Result<(), String> {
        // Layer 0: built-in OS-aware defaults. Only fill keys the
        // user didn't already declare in [vars].
        for (k, v) in builtin_platform_vars() {
            self.vars.entry(k).or_insert(v);
        }
        // Layer 1: env overrides take precedence over [vars] defaults.
        // The CUTE_QPI_<NAME> form mirrors the cargo / clang convention
        // and avoids stomping on unrelated env vars.
        for (k, v) in &self.vars.clone() {
            let env_key = format!("CUTE_QPI_{}", k.to_ascii_uppercase());
            if let Ok(env_val) = std::env::var(&env_key) {
                self.vars.insert(k.clone(), env_val);
            } else {
                // touch so the original default flows through expansion
                self.vars.insert(k.clone(), v.clone());
            }
        }
        // Layer 2: resolve `${X}` references inside [vars] itself.
        // A small fixed-point loop catches the chain
        // `qt_core = "${qt_headers}/.../Headers"` form. Bound iterations
        // so a typo like `a = "${a}"` errors instead of looping.
        for _ in 0..16 {
            let snapshot = self.vars.clone();
            let mut changed = false;
            for v in self.vars.values_mut() {
                let new = substitute(v, &snapshot)?;
                if new != *v {
                    *v = new;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // Layer 3: substitute path-shaped fields once.
        for inc in &mut self.clang.includes {
            *inc = substitute_path(inc, &self.vars)?;
        }
        for fwk in &mut self.clang.frameworks {
            *fwk = substitute_path(fwk, &self.vars)?;
        }
        for c in &mut self.classes {
            c.header = substitute_path(&c.header, &self.vars)?;
        }
        Ok(())
    }
}

/// Per-platform built-in defaults for the well-known Qt header
/// path variables. Filled into the typesystem's `[vars]` map ahead
/// of any user / env override, so a typesystem.toml that just
/// references `${qt_core}` works out of the box on macOS Homebrew
/// and Linux distro installs alike.
///
/// Platforms covered:
///
/// - **macOS** (`target_os = "macos"`): Homebrew framework layout
///   under `/opt/homebrew/lib/Qt<Module>.framework/Headers`.
///   Lowercase + `.h` filenames inside the framework's Headers
///   dir match the Linux casing, so user `header = "${qt_core}/qpoint.h"`
///   resolves identically.
/// - **Linux** (`target_os = "linux"`): distro-package layout under
///   `/usr/include/qt6/Qt<Module>` (no framework wrapper). Most
///   distros (Debian/Ubuntu/Fedora/Arch) ship this layout when the
///   Qt 6 dev packages are installed.
/// - **Other** (Windows / FreeBSD / unknown): no built-in defaults;
///   the user must declare `[vars]` in the typesystem or set
///   `CUTE_QPI_*` env vars explicitly.
fn builtin_platform_vars() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if cfg!(target_os = "macos") {
        let frameworks = "/opt/homebrew/lib";
        out.push(("qt_frameworks".into(), frameworks.into()));
        for (name, sub) in [
            ("qt_core", "QtCore"),
            ("qt_gui", "QtGui"),
            ("qt_widgets", "QtWidgets"),
            ("qt_charts", "QtCharts"),
            ("qt_quick", "QtQuick"),
            ("qt_qml", "QtQml"),
            ("qt_svg", "QtSvg"),
            ("qt_svg_widgets", "QtSvgWidgets"),
            ("qt_network", "QtNetwork"),
            ("qt_multimedia", "QtMultimedia"),
        ] {
            out.push((name.into(), format!("{frameworks}/{sub}.framework/Headers")));
        }
    } else if cfg!(target_os = "linux") {
        // Most distros (Debian / Ubuntu / Fedora / Arch / openSUSE)
        // install Qt 6 dev headers under `/usr/include/qt6/`. A few
        // (older Ubuntu) use `/usr/include/x86_64-linux-gnu/qt6/` —
        // those need an explicit override.
        let qt6 = "/usr/include/qt6";
        out.push(("qt_frameworks".into(), qt6.into()));
        for (name, sub) in [
            ("qt_core", "QtCore"),
            ("qt_gui", "QtGui"),
            ("qt_widgets", "QtWidgets"),
            ("qt_charts", "QtCharts"),
            ("qt_quick", "QtQuick"),
            ("qt_qml", "QtQml"),
            ("qt_svg", "QtSvg"),
            ("qt_svg_widgets", "QtSvgWidgets"),
            ("qt_network", "QtNetwork"),
            ("qt_multimedia", "QtMultimedia"),
        ] {
            out.push((name.into(), format!("{qt6}/{sub}")));
        }
    }
    out
}

/// `${name}` substitution. Unknown names error rather than silently
/// expand to empty — typos should fail loud.
fn substitute(s: &str, vars: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'$' && bytes[i + 1] == b'{' {
            let end = s[i + 2..]
                .find('}')
                .ok_or_else(|| format!("unterminated `${{` in {s:?}"))?;
            let name = &s[i + 2..i + 2 + end];
            let val = vars.get(name).ok_or_else(|| {
                format!(
                    "unknown var `{name}` (define under [vars] or set CUTE_QPI_{}=)",
                    name.to_ascii_uppercase()
                )
            })?;
            out.push_str(val);
            i += 2 + end + 1;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    Ok(out)
}

fn substitute_path(p: &Path, vars: &BTreeMap<String, String>) -> Result<PathBuf, String> {
    let s = p.to_str().ok_or_else(|| format!("non-utf8 path: {p:?}"))?;
    Ok(PathBuf::from(substitute(s, vars)?))
}
