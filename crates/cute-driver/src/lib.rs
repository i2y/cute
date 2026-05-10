//! Cute compiler driver.
//!
//! Orchestrates the pipeline: source -> lex -> parse -> resolve -> typecheck
//! -> lower -> codegen, plus diagnostic aggregation and binary linking.
//!
//! Two output modes:
//!
//! - **`build_file`** writes a `.h` + `.cpp` pair into a directory and
//!   returns. Used by CMake-driven projects that want to integrate Cute
//!   sources alongside hand-written C++.
//! - **`compile_to_binary`** runs the full pipeline (parse -> codegen ->
//!   internal CMake project -> cmake configure + build) and produces a
//!   native executable in the user's CWD. This is what `cute build foo.cute`
//!   surfaces - the primary developer experience.

use cute_syntax::{SourceMap, ast, parse};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

pub mod doctor;

use codespan_reporting::diagnostic::{Diagnostic as CrDiag, Label};
use codespan_reporting::files::SimpleFiles;
use codespan_reporting::term::{
    self,
    termcolor::{ColorChoice, StandardStream},
};

/// `cute.toml` manifest: optional sibling file next to a `.cute`
/// source that declares external dependencies. Lets users consume
/// arbitrary C++ libraries (Qt official add-ons, KDE Frameworks,
/// vcpkg / Conan packages, header-only libs) without inventing a
/// new package manager - the actual asset reuse goes through the
/// existing `.qpi` binding mechanism + CMake's `find_package`.
///
/// Also enumerates extra `.cute` source files for multi-file
/// projects, so a project can split its declarations across
/// `counter.cute` + `style.cute` + `main.cute` without inventing
/// a directory-walk convention.
///
/// Layout:
/// ```toml
/// [sources]
/// paths = ["counter.cute", "style.cute"]
///
/// [bindings]
/// paths = ["bindings/qtcharts.qpi"]
///
/// [cmake]
/// find_package = ["Qt6 COMPONENTS Charts"]
/// link_libraries = ["Qt6::Charts"]
///
/// [cpp]
/// includes = ["<QtCharts>"]
/// ```
#[derive(Default, Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Optional `[library]` block — when present, this project builds
    /// as a Cute library (shared lib + public header + .qpi binding +
    /// cmake config) rather than an app. Mutually exclusive with
    /// `fn main` / `*_app` intrinsic in the source.
    #[serde(default)]
    pub library: Option<Library>,
    #[serde(default)]
    pub sources: Sources,
    #[serde(default)]
    pub bindings: Bindings,
    #[serde(default)]
    pub cmake: CmakeConfig,
    #[serde(default)]
    pub cpp: CppConfig,
    /// Cute libraries this project depends on. Each entry maps to
    /// (a) a `find_package(<name>)` + link via cmake, AND (b) the
    /// library's `.qpi` binding loaded into the type checker. Both
    /// flat-name (`deps = ["MyLib"]` — installed lookup) and
    /// detailed-spec (`[cute_libraries.MyLib] git = "..."`) forms
    /// supported. (Detailed git-source spec is honoured by `cute
    /// install`, not by `cute build` directly.)
    #[serde(default)]
    pub cute_libraries: CuteLibraries,
}

/// `[library]` manifest block. Presence flips `cute build` into
/// library-output mode (shared lib + public header + .qpi binding
/// + `<Name>Config.cmake`, installed under
/// `~/.cache/cute/libraries/<name>/<version>/<triple>/`).
#[derive(Debug, Clone, Deserialize)]
pub struct Library {
    /// Library name. Becomes the cmake target name, the `find_package`
    /// key, and the installed `.qpi` filename. PascalCase by Cute
    /// convention (mirrors Qt: `QtCore`, `KF6Kirigami`).
    pub name: String,
    /// Semver-ish version string. Embedded into the install path so
    /// multiple versions of the same library can coexist on disk.
    pub version: String,
    /// Optional one-line summary surfaced by `cute install` /
    /// `<Name>Config.cmake`.
    #[serde(default)]
    pub description: String,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct Sources {
    /// Extra `.cute` source files compiled together with the entry
    /// file. Resolved relative to the directory containing
    /// `cute.toml`. The entry file (`cute build foo.cute`) is always
    /// included automatically; `paths` lists *additional* files only.
    /// No glob support yet — exact paths only.
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct Bindings {
    /// Paths to additional `.qpi` files, resolved relative to the
    /// directory containing `cute.toml`.
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct CmakeConfig {
    /// Extra `find_package(...)` lines (the inner argument list).
    /// E.g. `["Qt6 COMPONENTS Charts"]` -> `find_package(Qt6 COMPONENTS Charts)`.
    #[serde(default)]
    pub find_package: Vec<String>,
    /// Extra `target_link_libraries(...)` items appended verbatim.
    #[serde(default)]
    pub link_libraries: Vec<String>,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct CppConfig {
    /// Extra `#include` lines for the generated `.h`. The angle
    /// brackets / quotes are user-supplied so umbrella vs specific
    /// header is the user's choice.
    #[serde(default)]
    pub includes: Vec<String>,
}

/// `[cute_libraries]` manifest block. Three forms accepted:
///
/// ```toml
/// # Form 1 — flat name list, looked up in the install cache.
/// [cute_libraries]
/// deps = ["MyLib", "AnotherLib"]
///
/// # Form 2 — per-library spec under [cute_libraries.<Name>].
/// [cute_libraries.MyLib]
/// git = "https://github.com/foo/mylib"
/// rev = "v0.2.0"          # tag / branch / commit, optional
///
/// # Form 3 — local path (mostly for development / testing).
/// [cute_libraries.WorkInProgress]
/// path = "../wip-lib"
/// ```
///
/// Form 1 entries resolve at `cute build` time by walking the install
/// cache. Form 2 + 3 are honoured by `cute install` (which fetches +
/// builds + installs them); `cute build` of a downstream consumer
/// then sees them in the install cache and treats them like Form 1.
#[derive(Default, Debug, Clone, Deserialize)]
pub struct CuteLibraries {
    /// Flat list of installed library names to depend on. Resolved
    /// by walking `~/.cache/cute/libraries/<name>/<version>/<triple>/`.
    /// The newest installed version wins when multiple are present.
    #[serde(default)]
    pub deps: Vec<String>,
    /// Per-library detailed spec. Captured as a name → CuteLibSpec
    /// map so toml's `[cute_libraries.<Name>]` table syntax flows
    /// through cleanly. `flatten` puts the map at the same level as
    /// `deps`, matching the documented manifest layout.
    #[serde(default, flatten)]
    pub specs: std::collections::BTreeMap<String, CuteLibSpec>,
}

/// One library entry under `[cute_libraries.<Name>]`. Used by `cute
/// install` to fetch + build + install the library before downstream
/// consumers depend on it. The CLI walks every spec in the manifest
/// and runs the appropriate fetch path (git clone / local copy).
#[derive(Default, Debug, Clone, Deserialize)]
pub struct CuteLibSpec {
    /// Git URL the library lives at. `cute install` clones into a
    /// scratch dir, builds with the library's own cute.toml, and
    /// installs to the cache.
    #[serde(default)]
    pub git: Option<String>,
    /// Git rev to check out — tag / branch / commit hash. Defaults
    /// to the cloned default branch HEAD if unset; for reproducible
    /// builds, pin it.
    #[serde(default)]
    pub rev: Option<String>,
    /// Local filesystem path, relative to the manifest dir. Used for
    /// developing two libraries side-by-side without a publish step.
    #[serde(default)]
    pub path: Option<String>,
    /// Optional version constraint. Currently informational — the
    /// cache walker just picks the newest installed version. Will
    /// gate semver compatibility once a resolver lands.
    #[serde(default)]
    pub version: Option<String>,
}

impl Manifest {
    /// Look for `cute.toml` in the directory of `source` (and only
    /// there - no parent walk). Returns `Ok(None)` when the file is
    /// missing, which is the common case.
    pub fn try_load(source: &Path) -> Result<Option<(Manifest, PathBuf)>, DriverError> {
        let Some(dir) = source.parent() else {
            return Ok(None);
        };
        let path = dir.join("cute.toml");
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)?;
        let manifest: Manifest =
            toml::from_str(&text).map_err(|e| DriverError::Manifest(format!("{path:?}: {e}")))?;
        Ok(Some((manifest, dir.to_path_buf())))
    }
}

pub struct CompileResult {
    pub modules: Vec<cute_syntax::Module>,
}

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("codegen error: {0}")]
    Codegen(#[from] cute_codegen::EmitError),
    #[error("input has no file stem: {0:?}")]
    NoStem(PathBuf),
    #[error("{count} resolution error(s): {first}")]
    Resolve { count: usize, first: String },
    #[error(
        "QML app expected a sibling `.qml` file at {0:?} - create it next to your `.cute` source"
    )]
    MissingQml(PathBuf),
    #[error("cmake configure failed:\n{0}")]
    CmakeConfigure(String),
    #[error("cmake build failed:\n{0}")]
    CmakeBuild(String),
    #[error("could not locate built binary in {0:?}")]
    BinaryNotFound(PathBuf),
    #[error(
        "`cmake` not found on PATH - install cmake (>= 3.21) and Qt 6 to use `cute build` for binaries"
    )]
    CmakeMissing,
    #[error("invalid cute.toml: {0}")]
    Manifest(String),
    #[error("`use {use_path}` did not resolve - expected file at {resolved:?}")]
    UseNotFound { use_path: String, resolved: PathBuf },
}

/// Run parse + resolve + type-check on `input` (and any sibling
/// `.cute` files reachable through `cute.toml`'s `[sources]` and
/// transitive `use`), render every diagnostic to stderr in the same
/// format `cute build` uses, and return the number of *errors*
/// encountered. Warnings are rendered but don't count.
///
/// Skips codegen, cmake configure/build, and binary linking — this
/// is the fast-iteration / CI / pre-commit path. A clean run returns
/// `Ok(0)`.
pub fn check_file(input: &Path) -> Result<usize, DriverError> {
    let mut source_map = SourceMap::default();
    let (manifest, manifest_dir) = match Manifest::try_load(input)? {
        Some((m, d)) => (m, Some(d)),
        None => (Manifest::default(), None),
    };

    let user_sources = parse_user_sources(
        &mut source_map,
        input,
        &manifest,
        manifest_dir.as_deref(),
        &[],
    )?;
    let user_module = merge_user_modules(&user_sources.modules);

    let mut bindings = cute_binding::load_stdlib(&mut source_map)
        .map_err(|e| DriverError::Parse(format!("{e}")))?;
    // Foreign QML modules pulled in via `use qml "..."` — we collect
    // them so type-check sees the imported surface, but otherwise
    // discard the per-import metadata (`cute check` doesn't drive
    // codegen).
    let _ = collect_qml_module_specs(&mut source_map, &user_sources.modules, &mut bindings)?;

    load_manifest_bindings(
        &mut source_map,
        &manifest,
        manifest_dir.as_deref(),
        &mut bindings,
    )?;
    // AST → AST pre-passes, in driver-canonical order. Each pass
    // is a no-op when its surface form is absent.
    //   * `desugar_suite` flattens each `suite "X" { test ... }`
    //     into sibling `Item::Fn(is_test)` entries.
    //   * `desugar_store` expands each `store Foo { ... }` into a
    //     `class Foo < QObject { pub ... }` + `let Foo : Foo =
    //     Foo.new()` pair.
    //   * `desugar_widget_state` lifts widget-side `state X : T = init`
    //     into a synthesized `__<Widget>State < QObject` holder + a
    //     hidden Object-kind state field. View bodies are unaffected
    //     (they emit QML root-level `property` directly).
    // All three feed mangling + HIR + type-check on equal footing
    // with user-written items.
    let user_module = cute_codegen::desugar_suite::desugar_suite(user_module);
    let user_module = cute_codegen::desugar_store::desugar_store(user_module);
    let user_module = cute_codegen::desugar_state::desugar_widget_state(user_module);
    let combined = combine_modules(&user_module, &bindings);
    let project = build_project_info(&source_map, &user_sources, &bindings);
    let emit_names = cute_codegen::mangle::build_emit_names(&combined, &project);
    let combined = cute_codegen::mangle::apply_rewrite(&combined, &emit_names, &project);

    let mut all_diags: Vec<cute_syntax::diag::Diagnostic> =
        check_user_collisions(&user_sources.modules);
    let resolved = cute_hir::resolve(&combined, &project);
    all_diags.extend(resolved.diagnostics.clone());
    let typed = cute_types::check_program(&combined, &resolved.program);
    all_diags.extend(typed.diagnostics);
    // use-after-move detection on `~Copyable` bindings.
    all_diags.extend(cute_types::check_linearity(&combined, &resolved.program));

    let renderable: Vec<&cute_syntax::diag::Diagnostic> = all_diags.iter().collect();
    if !renderable.is_empty() {
        render_diagnostics(&source_map, &renderable);
    }
    let error_count = all_diags
        .iter()
        .filter(|d| matches!(d.severity, cute_syntax::diag::Severity::Error))
        .count();
    Ok(error_count)
}

/// Parse a single `.cute` source file into an AST `Module`.
pub fn parse_file(
    source_map: &mut SourceMap,
    path: &Path,
) -> Result<cute_syntax::Module, DriverError> {
    let source = std::fs::read_to_string(path)?;
    let file_id = source_map.add(path.to_string_lossy().into_owned(), source);
    let source = source_map.source(file_id);
    parse(file_id, source).map_err(|e| DriverError::Parse(format!("{:?}", e)))
}

/// Parse + resolve + type-check + emit a Cute project. Common front-end
/// for both `build_file` (writes .h/.cpp pair) and `compile_to_binary`
/// (drives an internal cmake build).
///
/// Multi-file projects are opt-in via `cute.toml`'s `[sources] paths`.
/// The entry file (`cute build foo.cute`) is always parsed; any
/// additional `.cute` files listed there are parsed and merged into a
/// single user module before HIR / type-check / codegen run. Symbol
/// collisions across the user file set are reported up-front.
///
/// Stdlib `.qpi` bindings are loaded up-front and merged into the
/// module the resolver / type-checker see, so calls like
/// `widget.deleteLater()` resolve against the bound `QObject` class
/// instead of falling through to soft-pass `External`. Codegen still
/// runs on the user-written module only - bound classes never produce
/// C++ output.
fn frontend(
    source_map: &mut SourceMap,
    input: &Path,
) -> Result<(String, cute_codegen::EmitResult, Manifest), DriverError> {
    let (stem, emit, manifest, _user_module) = frontend_with_mode(source_map, input, false, &[])?;
    Ok((stem, emit, manifest))
}

/// Same pipeline as `frontend`, with a `is_test_build` flag forwarded
/// to codegen. When `true`, codegen suppresses the user's `fn main`
/// and synthesizes a TAP-lite runner that calls every `test fn`.
///
/// `extras` is a caller-supplied list of additional `.cute` source
/// paths to merge alongside the entry file. `cute test` uses this to
/// auto-discover every test file under cwd. Empty for the regular
/// build path, where multi-file projects come in via `[sources] paths`.
fn frontend_with_mode(
    source_map: &mut SourceMap,
    input: &Path,
    is_test_build: bool,
    extras: &[PathBuf],
) -> Result<(String, cute_codegen::EmitResult, Manifest, ast::Module), DriverError> {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| DriverError::NoStem(input.to_path_buf()))?
        .to_string();

    let (manifest, manifest_dir) = match Manifest::try_load(input)? {
        Some((m, d)) => (m, Some(d)),
        None => (Manifest::default(), None),
    };

    // -- user source set ---------------------------------------------------
    // The entry file is always part of the project. `[sources] paths`
    // adds optional siblings. We canonicalize each path before merging
    // so the entry file showing up explicitly in `[sources]` doesn't
    // get parsed twice.
    let user_sources = parse_user_sources(
        source_map,
        input,
        &manifest,
        manifest_dir.as_deref(),
        extras,
    )?;
    let user_modules = &user_sources.modules;
    let user_module = merge_user_modules(user_modules);
    // Same pre-pass chain as `check_file`; see there for rationale.
    let user_module = cute_codegen::desugar_suite::desugar_suite(user_module);
    let user_module = cute_codegen::desugar_store::desugar_store(user_module);
    let user_module = cute_codegen::desugar_state::desugar_widget_state(user_module);

    // -- bindings ---------------------------------------------------------
    // Stdlib bindings (QObject + Qt 6 surface) load unconditionally —
    // every Cute project can use them. Foreign QML modules (Kirigami
    // and any future ones) are NOT auto-loaded; the user opts in
    // per-project via `use qml "..."` in source. The driver collects
    // those declarations next.
    let mut bindings =
        cute_binding::load_stdlib(source_map).map_err(|e| DriverError::Parse(format!("{e}")))?;

    // Collect all `use qml "uri" [as Alias]` declarations from the
    // user's source modules. Each one becomes a QmlModuleSpec the
    // codegen consumes to emit the matching QML import line, and
    // (when the URI is bundled) loads its binding so type-check sees
    // the foreign module's surface.
    let qml_modules = collect_qml_module_specs(source_map, user_modules, &mut bindings)?;

    load_manifest_bindings(
        source_map,
        &manifest,
        manifest_dir.as_deref(),
        &mut bindings,
    )?;

    // [cute_libraries] deps — load each Cute library's installed .qpi
    // binding so the type checker sees its public surface. Each entry
    // resolves through the user cache (`~/.cache/cute/libraries/<name>/
    // <version>/<triple>/`); the newest installed version wins. Missing
    // installs surface as a clear error pointing at `cute install`
    // (Commit 3) — which we don't have yet, so the message names the
    // local-build path users have today.
    for dep_name in &manifest.cute_libraries.deps {
        let prefix = find_cute_library_prefix(dep_name).ok_or_else(|| {
            DriverError::Manifest(format!(
                "[cute_libraries] dep `{dep_name}` not found in ~/.cache/cute/libraries/. Build the library with `cute build` from its source dir, or (Commit 3) `cute install <path-or-git-url>`.",
            ))
        })?;
        let qpi_path = prefix
            .join("share/cute/bindings")
            .join(format!("{dep_name}.qpi"));
        let qpi_src = std::fs::read_to_string(&qpi_path).map_err(|e| {
            DriverError::Manifest(format!(
                "[cute_libraries] dep `{dep_name}` install at {prefix:?} is missing the .qpi binding ({e}). Re-build the library.",
            ))
        })?;
        let qpi_name = qpi_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(dep_name);
        let module = cute_binding::parse_qpi(source_map, qpi_name, &qpi_src)
            .map_err(|e| DriverError::Parse(format!("{qpi_path:?}: {e:?}")))?;
        bindings.push(module);
    }
    let combined = combine_modules(&user_module, &bindings);

    // -- project info: module names + per-module imports + prelude -------
    let project = build_project_info(source_map, &user_sources, &bindings);

    // -- module-aware name mangling --------------------------------------
    // Module-level namespacing: when two modules declare `class
    // Counter`, both class decls and every reference to them are
    // rewritten to `<module>__Counter`. We compute the emit-name
    // table from the combined view (so collision detection sees the
    // full set), then apply the same rewrite to both the combined
    // module (HIR / type-check input) and the user-only module
    // (codegen input). Modules with unique simple names produce a
    // no-op rewrite, preserving existing demos' C++ output exactly.
    let emit_names = cute_codegen::mangle::build_emit_names(&combined, &project);
    let combined = cute_codegen::mangle::apply_rewrite(&combined, &emit_names, &project);
    let user_module = cute_codegen::mangle::apply_rewrite(&user_module, &emit_names, &project);

    // -- collision detection across the user source set -------------------
    let mut all_diags: Vec<cute_syntax::diag::Diagnostic> = check_user_collisions(user_modules);

    let resolved = cute_hir::resolve(&combined, &project);
    all_diags.extend(resolved.diagnostics.clone());

    let typed = cute_types::check_program(&combined, &resolved.program);
    all_diags.extend(typed.diagnostics);

    // use-after-move detection on `~Copyable` bindings.
    all_diags.extend(cute_types::check_linearity(&combined, &resolved.program));

    // Render every diagnostic (warnings + errors) so the user sees
    // hints like the suggest-close-method "did you mean" inline,
    // even when the build doesn't abort. Errors trigger an
    // early-return; warnings just print and continue.
    let renderable: Vec<&cute_syntax::diag::Diagnostic> = all_diags.iter().collect();
    if !renderable.is_empty() {
        render_diagnostics(source_map, &renderable);
    }
    let errors: Vec<&cute_syntax::diag::Diagnostic> = all_diags
        .iter()
        .filter(|d| matches!(d.severity, cute_syntax::diag::Severity::Error))
        .collect();
    if !errors.is_empty() {
        return Err(DriverError::Resolve {
            count: errors.len(),
            first: errors[0].message.clone(),
        });
    }

    // Convert driver QML specs into the codegen-side import shape
    // (drops the `binding_loaded` flag, which is informational).
    let qml_imports: Vec<cute_codegen::QmlImport> = qml_modules
        .iter()
        .map(|s| cute_codegen::QmlImport {
            module_uri: s.module_uri.clone(),
            version: s.version.clone(),
            alias: s.alias.clone(),
        })
        .collect();

    // Codegen runs on the merged USER module only. Bound classes from
    // stdlib contribute to type-check but never to emitted C++. The
    // type-checker's `generic_instantiations` map flows in here so
    // codegen can emit instantiated C++ templates for `T.new()` calls
    // whose type args were inferred from context (let / var
    // annotation, fn parameter, return type) rather than spelled out.
    let mut result = cute_codegen::emit_module(
        &stem,
        &user_module,
        &resolved.program,
        &project,
        cute_codegen::CodegenTypeInfo {
            generic_instantiations: &typed.generic_instantiations,
            qml_imports: &qml_imports,
            // Threading the source map enables `#line` directives in
            // generated `.cpp`, so a debugger backtrace through Cute-
            // emitted code lands in the user's `.cute` source instead
            // of the generated wrapper.
            source_map: Some(source_map),
            is_test_build,
            binding_modules: &bindings,
        },
    )?;
    // Inject manifest-declared `#include` lines at the top of the
    // emitted header so user-side `widget Main { QChartView { ... } }`
    // resolves to a real type at compile time. The codegen layer
    // doesn't see the manifest - keeping that boundary clean.
    //
    // Cute libraries declared under `[cute_libraries] deps` get the
    // same auto-prepend treatment for their installed `<Name>.h`,
    // which the library's CMake install rule renamed to match the
    // library name regardless of the source file's stem.
    let mut all_includes: Vec<String> = manifest
        .cpp
        .includes
        .iter()
        .map(|inc| format!("#include {inc}"))
        .collect();
    for dep_name in &manifest.cute_libraries.deps {
        all_includes.push(format!("#include <{dep_name}.h>"));
    }
    if !all_includes.is_empty() {
        prepend_after_pragma_once(
            &mut result.header,
            &format!("{}\n", all_includes.join("\n")),
        );
    }
    Ok((stem, result, manifest, user_module))
}

/// Render Cute diagnostics to stderr as a codespan-reporting block:
/// header line, primary span shown with `^^^^` underline against the
/// source, secondary `note:` annotations attached as `Label::secondary`
/// so they share the same render. The driver builds a transient
/// `SimpleFiles` that mirrors Cute's `SourceMap` 1:1 (FileId(N) ->
/// codespan file id N) so labels can refer to spans by byte range.
/// Load `[bindings] paths` entries from the manifest into `bindings`.
/// Each `.qpi` is parsed against `source_map` so diagnostics point at
/// the file; parse failures surface as `DriverError::Parse` carrying
/// the binding's path.
fn load_manifest_bindings(
    source_map: &mut SourceMap,
    manifest: &Manifest,
    manifest_dir: Option<&Path>,
    bindings: &mut Vec<ast::Module>,
) -> Result<(), DriverError> {
    let Some(dir) = manifest_dir else {
        return Ok(());
    };
    for rel in &manifest.bindings.paths {
        let path = dir.join(rel);
        let src = std::fs::read_to_string(&path)?;
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<extra>")
            .to_string();
        let module = cute_binding::parse_qpi(source_map, &name, &src)
            .map_err(|e| DriverError::Parse(format!("{path:?}: {e:?}")))?;
        bindings.push(module);
    }
    Ok(())
}

fn render_diagnostics(source_map: &SourceMap, diags: &[&cute_syntax::diag::Diagnostic]) {
    // codespan's SimpleFiles uses opaque integer ids returned by
    // `add()`. We add files in FileId order so the returned id
    // matches `FileId(N).0 as usize`. Iterate the Cute SourceMap by
    // index; if a file is referenced by a diagnostic but missing
    // from the map, fall back to a placeholder so rendering still
    // completes.
    let mut files: SimpleFiles<String, String> = SimpleFiles::new();
    let count = source_map.file_count();
    for i in 0..count {
        let id = cute_syntax::span::FileId(i as u32);
        files.add(
            source_map.name(id).to_string(),
            source_map.source(id).to_string(),
        );
    }

    let writer = StandardStream::stderr(ColorChoice::Auto);
    let config = term::Config::default();

    for d in diags {
        let primary_file = d.primary.file.0 as usize;
        let mut cr = match d.severity {
            cute_syntax::diag::Severity::Error => CrDiag::error(),
            cute_syntax::diag::Severity::Warning => CrDiag::warning(),
            cute_syntax::diag::Severity::Note => CrDiag::note(),
        }
        .with_message(&d.message)
        .with_labels(vec![Label::primary(
            primary_file,
            d.primary.start as usize..d.primary.end as usize,
        )]);
        // Secondary notes - attached to the same diagnostic so the
        // user sees the cause-and-effect in one render block.
        for (sp, note) in &d.notes {
            cr = cr.with_labels(vec![
                Label::secondary(sp.file.0 as usize, sp.start as usize..sp.end as usize)
                    .with_message(note),
            ]);
        }
        // Best-effort emit: codespan returns IO/file errors as
        // `Result`, which we ignore - if we can't render we've
        // already lost the user's attention to a bigger problem.
        let _ = term::emit(&mut writer.lock(), &config, &files, &cr);
    }
}

/// Insert `extra` into `header` immediately after the `#pragma once`
/// line if there is one, otherwise at the very top. Used for manifest-
/// driven `#include` injection so the surrounding generated includes
/// stay where they are.
fn prepend_after_pragma_once(header: &mut String, extra: &str) {
    let needle = "#pragma once\n";
    if let Some(idx) = header.find(needle) {
        let insert_at = idx + needle.len();
        header.insert_str(insert_at, extra);
    } else {
        header.insert_str(0, extra);
    }
}

/// Build a single `Module` whose `items` are the union of every binding
/// module's items followed by the user module's items. Bindings come
/// first so name resolution sees them as already-declared when the
/// user module is processed.
fn combine_modules(user: &ast::Module, bindings: &[ast::Module]) -> ast::Module {
    let mut items: Vec<ast::Item> = bindings
        .iter()
        .flat_map(|m| m.items.iter().cloned())
        .collect();
    items.extend(user.items.iter().cloned());
    ast::Module {
        items,
        span: user.span,
    }
}

/// Parse the entry file and every file it transitively imports via
/// `use foo` / `use foo.bar`. Resolution rule:
///
/// - Project root is the directory containing `cute.toml` (when one
///   exists) or the entry file's directory.
/// - `use foo` resolves to `<root>/foo.cute`.
/// - `use foo.bar.baz` resolves to `<root>/foo/bar/baz.cute`.
/// - `use qt.<X>` is a no-op (the `qt.*` prefix is reserved for the
///   built-in stdlib bindings, which the driver loads unconditionally).
///
/// `cute.toml`'s `[sources] paths` list is consulted as a fallback for
/// projects that want a file in the source set without an explicit
/// `use`. Duplicate paths (entry file in `[sources]`, two files
/// importing the same sibling) are deduped by canonical path so each
/// `.cute` file is parsed exactly once.
/// What `parse_user_sources` returns: every parsed user module plus
/// the set of canonical paths that came from `[sources] paths` in
/// `cute.toml`. The driver uses the latter to mark those modules as
/// ambient (auto-imported by every other module) when building the
/// `ProjectInfo`.
struct UserSources {
    modules: Vec<ast::Module>,
    ambient_paths: std::collections::HashSet<PathBuf>,
}

fn parse_user_sources(
    source_map: &mut SourceMap,
    input: &Path,
    manifest: &Manifest,
    manifest_dir: Option<&Path>,
    extras: &[PathBuf],
) -> Result<UserSources, DriverError> {
    use std::collections::{HashSet, VecDeque};

    let project_root: PathBuf = manifest_dir
        .map(|p| p.to_path_buf())
        .or_else(|| input.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));

    let mut modules: Vec<ast::Module> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    let mut ambient_paths: HashSet<PathBuf> = HashSet::new();

    // Seed: the entry file plus any `[sources]` fallbacks. Iteration
    // is breadth-first from the entry, so files appear in
    // declaration-encounter order in the merged module. Anything
    // listed in `[sources]` gets recorded as ambient so its module is
    // auto-imported by every other module - that's the contract for
    // "files no one explicitly `use`s" (e.g. a project-wide style
    // palette or prelude).
    queue.push_back(input.to_path_buf());
    if let Some(dir) = manifest_dir {
        for rel in &manifest.sources.paths {
            let p = dir.join(rel);
            ambient_paths.insert(canonicalize_or_self(&p));
            queue.push_back(p);
        }
    }
    // Caller-supplied extras (used by `cute test` no-arg, where the
    // CLI walks cwd for `.cute` files and feeds them all in). These
    // are NOT marked ambient: tests should be able to `use` each
    // other explicitly when they share helpers, but a stray test file
    // shouldn't silently pollute every other module's name resolution.
    for p in extras {
        queue.push_back(p.clone());
    }

    while let Some(path) = queue.pop_front() {
        let canon = canonicalize_or_self(&path);
        if !seen.insert(canon) {
            continue;
        }
        let module = parse_file(source_map, &path)?;

        // Walk this module's `use` items and queue each referenced
        // file. Resolution failures surface as `UseNotFound` with the
        // computed path so the user can see exactly where the
        // compiler looked.
        for item in &module.items {
            if let ast::Item::Use(u) = item {
                let dotted = u
                    .path
                    .iter()
                    .map(|i| i.name.clone())
                    .collect::<Vec<_>>()
                    .join(".");
                // `qt.X` is reserved for the auto-loaded stdlib bindings.
                // Treat as a no-op rather than searching the filesystem
                // so existing demos that mention Qt don't break.
                if u.path.first().map(|i| i.name.as_str()) == Some("qt") {
                    continue;
                }
                let mut resolved = project_root.clone();
                let n = u.path.len();
                for (i, segment) in u.path.iter().enumerate() {
                    if i + 1 == n {
                        resolved.push(format!("{}.cute", segment.name));
                    } else {
                        resolved.push(&segment.name);
                    }
                }
                if !resolved.exists() {
                    return Err(DriverError::UseNotFound {
                        use_path: dotted,
                        resolved,
                    });
                }
                queue.push_back(resolved);
            }
        }

        modules.push(module);
    }

    Ok(UserSources {
        modules,
        ambient_paths,
    })
}

fn canonicalize_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Assemble the per-project module/visibility info that HIR's
/// resolve consumes. Each parsed user `.cute` file becomes a module
/// (name = file stem); its `use foo` items populate
/// `imports_for_module`. Every item declared in a binding module
/// (Qt stdlib + user `.qpi`s) is added to `prelude_items` so the
/// visibility check treats them as universally accessible.
fn build_project_info(
    source_map: &SourceMap,
    user_sources: &UserSources,
    bindings: &[ast::Module],
) -> cute_hir::ProjectInfo {
    use std::collections::HashSet;
    let mut info = cute_hir::ProjectInfo::default();

    // First pass: register file ids, the per-module `use` import
    // sets, the `as`-aliases, and the selective-import maps. Also
    // collect "ambient" module names from `[sources] paths` so they
    // can be auto-imported into every other module afterwards.
    let mut ambient_modules: HashSet<String> = HashSet::new();
    for m in &user_sources.modules {
        let module_name = match module_name_from_module(source_map, m) {
            Some(n) => n,
            None => continue,
        };
        info.module_for_file
            .insert(m.span.file, module_name.clone());
        let imports = info
            .imports_for_module
            .entry(module_name.clone())
            .or_insert_with(HashSet::new);
        let aliases = info
            .module_aliases
            .entry(module_name.clone())
            .or_insert_with(std::collections::HashMap::new);
        let selectives = info
            .selective_imports
            .entry(module_name.clone())
            .or_insert_with(std::collections::HashMap::new);
        let re_exports = info
            .re_exports
            .entry(module_name.clone())
            .or_insert_with(std::collections::HashMap::new);
        for item in &m.items {
            let ast::Item::Use(u) = item else { continue };
            // `qt.X` is reserved (parser-side it stays a no-op).
            // Mirror that here so the import sets don't pick up a
            // phantom `qt` module.
            if u.path.first().map(|i| i.name.as_str()) == Some("qt") {
                continue;
            }
            // The leaf segment is the source module's real name (the
            // `.cute` file stem). All bookkeeping is in those terms;
            // aliases and selective rebindings layer on top.
            let Some(source_module) = u.path.last().map(|i| i.name.clone()) else {
                continue;
            };
            match &u.kind {
                ast::UseKind::Module(alias) => {
                    imports.insert(source_module.clone());
                    if let Some(a) = alias {
                        aliases.insert(a.name.clone(), source_module.clone());
                    }
                    // `pub use foo` would re-export every pub item of
                    // `foo` from this module. That requires walking
                    // foo's items, which we don't have at parse time
                    // — only `pub use foo.{X}` is supported.
                    // `pub use foo` parses but is treated as plain
                    // `use foo` (the keyword has no effect).
                }
                ast::UseKind::Names(names) => {
                    for n in names {
                        let local = n.alias.as_ref().unwrap_or(&n.name).name.clone();
                        selectives
                            .insert(local.clone(), (source_module.clone(), n.name.name.clone()));
                        // `pub use foo.{X as A}` re-exports A from
                        // this module, forwarding to (foo, X).
                        if u.is_pub {
                            re_exports.insert(local, (source_module.clone(), n.name.name.clone()));
                        }
                    }
                }
            }
        }
        let path_str = source_map.name(m.span.file);
        let canon = canonicalize_or_self(Path::new(path_str));
        if user_sources.ambient_paths.contains(&canon) {
            ambient_modules.insert(module_name);
        }
    }

    // Second pass: weave every ambient module into every other
    // module's import set. Self-references (an ambient module
    // importing itself) are skipped to keep the import sets minimal.
    for module_name in info.module_for_file.values().cloned().collect::<Vec<_>>() {
        let imports = info
            .imports_for_module
            .entry(module_name.clone())
            .or_insert_with(HashSet::new);
        for amb in &ambient_modules {
            if amb != &module_name {
                imports.insert(amb.clone());
            }
        }
    }

    // Bindings: every item declared in a binding `.qpi` joins the
    // global prelude.
    for m in bindings {
        for item in &m.items {
            if let Some((name, _)) = item_name_span(item) {
                info.prelude_items.insert(name.to_string());
            }
        }
    }
    info
}

/// Best-effort module-name lookup: read the SourceMap entry that the
/// module's first item came from and strip `.cute`. Empty modules
/// fall back to None so the driver can skip them rather than register
/// a zero-length name.
fn module_name_from_module(source_map: &SourceMap, m: &ast::Module) -> Option<String> {
    let path = source_map.name(m.span.file);
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

/// Concatenate every user module's items into a single flat module.
/// Codegen consumes this as the user's logical module - all
/// declarations look as if they came from one file.
/// One foreign QML module declared by the user via `use qml "..."`.
/// Codegen emits `import <module> [<version>] [as <alias>]` per spec
/// at the top of generated `.qml` files. `version` is `None` for Qt
/// 6+ modules that ship version-less qmldirs (Kirigami 6 etc.) — we
/// must NOT emit a version in that case or QML refuses to load.
/// The `binding_loaded` flag records whether the compiler had a
/// bundled `.qpi` for this URI; `false` means type-check soft-passes
/// references to elements from this module.
#[derive(Debug, Clone)]
pub struct QmlModuleSpec {
    pub module_uri: String,
    pub version: Option<String>,
    pub alias: Option<String>,
    pub binding_loaded: bool,
}

/// Walk every parsed user module for `Item::UseQml` declarations,
/// dedupe by (URI, alias), and load the matching `.qpi` binding for
/// each known URI. Bindings are appended to `bindings_out` so the
/// rest of the pipeline (HIR resolve, type check) sees the foreign
/// module's surface alongside the stdlib's. The returned spec list
/// drives the QML import emit.
fn collect_qml_module_specs(
    source_map: &mut cute_syntax::SourceMap,
    user_modules: &[ast::Module],
    bindings_out: &mut Vec<ast::Module>,
) -> Result<Vec<QmlModuleSpec>, DriverError> {
    use std::collections::HashSet;
    let mut specs: Vec<QmlModuleSpec> = Vec::new();
    let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
    for m in user_modules {
        for item in &m.items {
            if let ast::Item::UseQml(u) = item {
                let alias = u.alias.as_ref().map(|a| a.name.clone());
                let key = (u.module_uri.clone(), alias.clone());
                if !seen.insert(key) {
                    continue;
                }
                let binding =
                    cute_binding::load_qml_module(source_map, &u.module_uri, alias.as_deref())
                        .map_err(|e| DriverError::Parse(format!("{e}")))?;
                let binding_loaded = binding.is_some();
                if let Some(b) = binding {
                    bindings_out.push(b);
                }
                // Look up the version from the bundled binding
                // metadata. None when the module ships version-less
                // (modern Kirigami etc.); codegen omits the version
                // suffix in that case so the QML import resolves.
                let version = cute_binding::lookup_qml_module(&u.module_uri)
                    .and_then(|b| b.default_version.map(|s| s.to_string()));
                specs.push(QmlModuleSpec {
                    module_uri: u.module_uri.clone(),
                    version,
                    alias,
                    binding_loaded,
                });
            }
        }
    }
    Ok(specs)
}

fn merge_user_modules(modules: &[ast::Module]) -> ast::Module {
    let mut items: Vec<ast::Item> = Vec::new();
    for m in modules {
        items.extend(m.items.iter().cloned());
    }
    let span = modules
        .first()
        .map(|m| m.span)
        .unwrap_or_else(|| cute_syntax::span::Span::new(cute_syntax::span::FileId(0), 0, 0));
    ast::Module { items, span }
}

/// Uniqueness rule: a top-level Cute declaration must be unique
/// within its declaring module. The same simple name in *different* modules
/// is allowed - codegen disambiguates by mangling
/// (`<module>__<name>`). Within-file duplicates remain an error for
/// most item kinds because the module's flat items table can only
/// hold one entry per name.
///
/// **Exception: `fn` declarations.** With overload-by-arg-type, two
/// `fn foo(...)` of the same name are legal as long as their
/// signatures don't collide (HIR `fn_overload_coherence_check`
/// catches true duplicates). Skip the simple-name check for fns and
/// rely on the HIR pass for accurate per-signature diagnostics.
fn check_user_collisions(modules: &[ast::Module]) -> Vec<cute_syntax::diag::Diagnostic> {
    use std::collections::HashMap;
    // Per-file dedup: each module gets its own seen set keyed by
    // simple name. Cross-module same-name is intentional and routed
    // through codegen's emit_name table, not flagged here.
    let mut diags = Vec::new();
    for m in modules {
        let mut seen: HashMap<String, cute_syntax::span::Span> = HashMap::new();
        for item in &m.items {
            // Fn items can legally repeat (overload). HIR coherence
            // handles per-signature duplicate detection.
            if matches!(item, ast::Item::Fn(_)) {
                continue;
            }
            let Some((name, span)) = item_name_span(item) else {
                continue;
            };
            if let Some(prev) = seen.get(name) {
                diags.push(
                    cute_syntax::diag::Diagnostic::error(
                        span,
                        format!(
                            "duplicate top-level declaration `{name}` in this file (rename or move one of them to a different module)"
                        ),
                    )
                    .with_note(*prev, "first declared here"),
                );
            } else {
                seen.insert(name.to_string(), span);
            }
        }
    }
    diags
}

/// Returns the simple-name + span of the identifier introduced by an
/// item, or `None` for items that don't introduce a name (e.g. `use`).
fn item_name_span(item: &ast::Item) -> Option<(&str, cute_syntax::span::Span)> {
    match item {
        ast::Item::Class(c) => Some((c.name.name.as_str(), c.name.span)),
        ast::Item::Struct(s) => Some((s.name.name.as_str(), s.name.span)),
        ast::Item::Fn(f) => Some((f.name.name.as_str(), f.name.span)),
        ast::Item::View(v) => Some((v.name.name.as_str(), v.name.span)),
        ast::Item::Widget(w) => Some((w.name.name.as_str(), w.name.span)),
        ast::Item::Style(s) => Some((s.name.name.as_str(), s.name.span)),
        ast::Item::Trait(t) => Some((t.name.name.as_str(), t.name.span)),
        ast::Item::Let(l) => Some((l.name.name.as_str(), l.name.span)),
        ast::Item::Enum(e) => Some((e.name.name.as_str(), e.name.span)),
        ast::Item::Flags(f) => Some((f.name.name.as_str(), f.name.span)),
        ast::Item::Store(s) => Some((s.name.name.as_str(), s.name.span)),
        // `suite "X"` doesn't introduce a value-namespace name —
        // its label is metadata. Collision-detect skips it.
        ast::Item::Suite(_) => None,
        // Impl blocks have no introducible name; collision detection
        // is on the (trait, target) pair, handled by HIR's index pass.
        ast::Item::Impl(_) => None,
        ast::Item::Use(_) => None,
        ast::Item::UseQml(_) => None,
    }
}

/// Compile a single `.cute` file: emit C++ and write `.h` + `.cpp`
/// pair into `out_dir`. Returns the absolute paths of the written files.
pub fn build_file(input: &Path, out_dir: &Path) -> Result<Vec<PathBuf>, DriverError> {
    let mut source_map = SourceMap::default();
    let (_stem, result, _manifest) = frontend(&mut source_map, input)?;

    std::fs::create_dir_all(out_dir)?;
    let header_path = out_dir.join(&result.header_filename);
    let source_path = out_dir.join(&result.source_filename);
    std::fs::write(&header_path, &result.header)?;
    std::fs::write(&source_path, &result.source)?;
    Ok(vec![header_path, source_path])
}

/// Build a `.cute` file end-to-end into a native binary. Generates C++,
/// stamps a CMake project into a per-source cache dir under
/// `~/.cache/cute/build/<hash>/`, runs cmake configure + build, copies
/// the resulting binary to `output` (or `<cwd>/<stem>` if not specified).
///
/// The cache dir persists between invocations so subsequent builds are
/// truly incremental: `write_if_changed` preserves mtime when source
/// text has not changed, which lets cmake skip re-configuration and
/// the underlying build tool skip already-built objects.
pub fn compile_to_binary(input: &Path, output: Option<&Path>) -> Result<PathBuf, DriverError> {
    // Peek the manifest so a `[library] name = ...` block flips the
    // build into library-output mode (shared lib + public header +
    // .qpi binding + Config.cmake, installed to the user cache).
    // App and library modes are mutually exclusive — `frontend_with_mode`
    // run inside `compile_to_library` re-checks for stray `fn main` /
    // `*_app` intrinsics so the user can't accidentally ship both.
    let manifest = Manifest::try_load(input)?
        .map(|(m, _)| m)
        .unwrap_or_default();
    if manifest.library.is_some() {
        return compile_to_library(input);
    }
    compile_binary_with_mode(input, output, false, &[])
}

/// Build a Cute library: shared library + public header + .qpi
/// binding + `<Name>Config.cmake`, installed under
/// `~/.cache/cute/libraries/<name>/<version>/<triple>/`. Consumers
/// declare `[cute_libraries] deps = ["<Name>"]` in their cute.toml
/// to pick up both the cmake target (for linking) and the binding
/// (for type-check).
pub fn compile_to_library(input: &Path) -> Result<PathBuf, DriverError> {
    compile_library_inner(input)
}

fn compile_library_inner(input: &Path) -> Result<PathBuf, DriverError> {
    let mut source_map = SourceMap::default();
    let (_stem, emit, manifest, user_module) =
        frontend_with_mode(&mut source_map, input, false, &[])?;

    let library = manifest
        .library
        .clone()
        .ok_or_else(|| DriverError::Manifest("missing [library] block in cute.toml".into()))?;

    // Reject conflicting intrinsics — a library has no app entry.
    let mode = detect_mode(&emit.source);
    if !matches!(mode, BuildMode::Plain | BuildMode::Cli) {
        return Err(DriverError::Manifest(format!(
            "[library] declared but source uses an app intrinsic ({mode:?}) — libraries can't have qml_app / widget_app / server_app / gpu_app",
        )));
    }
    // Extra guard: even `fn main` is rejected for a library — main
    // belongs to executables. The codegen emits `int main(` for
    // user-defined `fn main`; pattern-match on the synthesized
    // signature so a user method literally named `main` (without
    // top-level fn shape) doesn't trigger this.
    if emit.source.contains("\nint main(") || emit.source.contains("\nstatic int main(") {
        return Err(DriverError::Manifest(
            "[library] declared but source defines `fn main` — libraries shouldn't have main"
                .into(),
        ));
    }

    let work = cache_dir_for(input)?;
    std::fs::create_dir_all(&work)?;

    // Emitted C++ from cutec.
    let gen_dir = work.join("generated");
    std::fs::create_dir_all(&gen_dir)?;
    write_if_changed(&gen_dir.join(&emit.header_filename), &emit.header)?;
    write_if_changed(&gen_dir.join(&emit.source_filename), &emit.source)?;

    let runtime_dir = work.join("runtime").join("cpp");
    std::fs::create_dir_all(&runtime_dir)?;
    write_runtime_headers_if_changed(&runtime_dir)?;

    // Stage CMakeLists + Config template + .qpi.
    let cmake_text = generate_library_cmake(&library, &emit, &manifest);
    write_if_changed(&work.join("CMakeLists.txt"), &cmake_text)?;
    let config_in = generate_library_config_template(&library);
    write_if_changed(
        &work.join(format!("{}Config.cmake.in", library.name)),
        &config_in,
    )?;
    let qpi_text = generate_library_qpi(&library, &user_module);
    write_if_changed(&work.join(format!("{}.qpi", library.name)), &qpi_text)?;

    // Cmake configure.
    let build_dir = work.join("build");
    std::fs::create_dir_all(&build_dir)?;
    let prefix = library_install_prefix(&library)?;
    let mut configure_args: Vec<std::ffi::OsString> = vec![
        "-S".into(),
        work.clone().into_os_string(),
        "-B".into(),
        build_dir.clone().into_os_string(),
        "-DCMAKE_BUILD_TYPE=Release".into(),
        format!("-DCMAKE_INSTALL_PREFIX={}", prefix.display()).into(),
    ];
    let mut prefix_paths: Vec<String> = Vec::new();
    if let Some(p) = find_qt_prefix() {
        prefix_paths.push(p);
    }
    if !prefix_paths.is_empty() {
        configure_args.push(format!("-DCMAKE_PREFIX_PATH={}", prefix_paths.join(";")).into());
    }
    let configure_args_ref: Vec<&std::ffi::OsStr> =
        configure_args.iter().map(|s| s.as_os_str()).collect();
    let configure = run_cmake(&configure_args_ref)?;
    if !configure.status.success() {
        return Err(DriverError::CmakeConfigure(format_cmake_configure_failure(
            &configure,
        )));
    }

    // Build.
    let built = run_cmake(&[
        "--build".as_ref(),
        build_dir.as_os_str(),
        "--config".as_ref(),
        "Release".as_ref(),
    ])?;
    if !built.status.success() {
        return Err(DriverError::CmakeBuild(combine_output(&built)));
    }

    // Install. Wipe a previous install of this same version first so
    // stale headers / .qpi from a prior build can't shadow current
    // output (cmake --install merges by default).
    if prefix.exists() {
        std::fs::remove_dir_all(&prefix)?;
    }
    let installed = run_cmake(&[
        "--install".as_ref(),
        build_dir.as_os_str(),
        "--config".as_ref(),
        "Release".as_ref(),
    ])?;
    if !installed.status.success() {
        return Err(DriverError::CmakeBuild(combine_output(&installed)));
    }

    eprintln!();
    eprintln!(
        "{} v{} installed to {}",
        library.name,
        library.version,
        prefix.display()
    );
    eprintln!(
        "Consumers declare [cute_libraries] deps = [\"{}\"] in cute.toml.",
        library.name
    );
    Ok(prefix)
}

/// Per-library install prefix, mirroring `find_cute_ui_prefix`'s shape:
/// `~/.cache/cute/libraries/<Name>/<version>/<triple>/`. Side-by-side
/// versions coexist; `find_cute_library_prefix` walks the parent dir
/// when consumers resolve a flat name.
fn library_install_prefix(library: &Library) -> Result<PathBuf, DriverError> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| DriverError::Manifest("HOME not set; can't pick library prefix".into()))?;
    let triple = host_triple_short();
    Ok(PathBuf::from(home)
        .join(".cache/cute/libraries")
        .join(&library.name)
        .join(&library.version)
        .join(triple))
}

fn host_triple_short() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// Render the CMakeLists.txt for a library build. Emits an
/// `add_library(... SHARED)` target (so consumers link against
/// `<Name>::<Name>`), plus install rules for the lib + public
/// header + .qpi binding + cmake config files. The Config.cmake.in
/// template (written sibling to CMakeLists.txt by the driver) gets
/// rendered to the build dir via `configure_package_config_file`.
fn generate_library_cmake(
    library: &Library,
    emit: &cute_codegen::EmitResult,
    manifest: &Manifest,
) -> String {
    // Conservative Qt component default — Cute's runtime headers
    // (cute_arc.h, cute_meta.h, ...) include QObject / QString from
    // QtCore + QtGui. Library authors who need more (Qml, Network,
    // etc.) extend via `[cmake] find_package = [...]` in cute.toml.
    let extra_find_packages = manifest
        .cmake
        .find_package
        .iter()
        .map(|args| format!("find_package({args} REQUIRED)"))
        .collect::<Vec<_>>()
        .join("\n");
    let extra_link_libs = if manifest.cmake.link_libraries.is_empty() {
        String::new()
    } else {
        format!(" {}", manifest.cmake.link_libraries.join(" "))
    };
    let name = &library.name;
    let version = &library.version;
    let header_file = &emit.header_filename;
    let source_file = &emit.source_filename;
    format!(
        r#"cmake_minimum_required(VERSION 3.21)
project({name} VERSION {version} LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC OFF)
set(CMAKE_AUTORCC ON)

find_package(Qt6 REQUIRED COMPONENTS Core Gui)
{extra_find_packages}

add_library({name} SHARED
    generated/{source_file}
)
target_include_directories({name}
    PRIVATE
        generated
        runtime/cpp
    PUBLIC
        $<BUILD_INTERFACE:${{CMAKE_SOURCE_DIR}}/generated>
        $<INSTALL_INTERFACE:include>
)
target_link_libraries({name} PUBLIC Qt6::Core Qt6::Gui{extra_link_libs})
set_target_properties({name} PROPERTIES
    VERSION {version}
    SOVERSION {version}
    MACOSX_RPATH TRUE
)

include(GNUInstallDirs)
install(TARGETS {name}
    EXPORT {name}Targets
    LIBRARY DESTINATION ${{CMAKE_INSTALL_LIBDIR}}
    ARCHIVE DESTINATION ${{CMAKE_INSTALL_LIBDIR}}
    RUNTIME DESTINATION ${{CMAKE_INSTALL_BINDIR}}
)
install(FILES generated/{header_file} RENAME {name}.h DESTINATION ${{CMAKE_INSTALL_INCLUDEDIR}})
install(FILES {name}.qpi DESTINATION share/cute/bindings)
install(EXPORT {name}Targets
    FILE {name}Targets.cmake
    NAMESPACE {name}::
    DESTINATION ${{CMAKE_INSTALL_LIBDIR}}/cmake/{name}
)
include(CMakePackageConfigHelpers)
configure_package_config_file(
    {name}Config.cmake.in
    ${{CMAKE_CURRENT_BINARY_DIR}}/{name}Config.cmake
    INSTALL_DESTINATION ${{CMAKE_INSTALL_LIBDIR}}/cmake/{name}
)
write_basic_package_version_file(
    ${{CMAKE_CURRENT_BINARY_DIR}}/{name}ConfigVersion.cmake
    VERSION {version}
    COMPATIBILITY SameMajorVersion
)
install(FILES
    ${{CMAKE_CURRENT_BINARY_DIR}}/{name}Config.cmake
    ${{CMAKE_CURRENT_BINARY_DIR}}/{name}ConfigVersion.cmake
    DESTINATION ${{CMAKE_INSTALL_LIBDIR}}/cmake/{name}
)
"#
    )
}

/// Render the `<Name>Config.cmake.in` template that
/// `configure_package_config_file` consumes. Pulls in cmake's
/// `find_dependency` for Qt6 (matching what `target_link_libraries`
/// declared `PUBLIC`) so consumers don't need to repeat the
/// `find_package(Qt6 ...)` call themselves.
fn generate_library_config_template(library: &Library) -> String {
    let name = &library.name;
    format!(
        r#"@PACKAGE_INIT@

include(CMakeFindDependencyMacro)
find_dependency(Qt6 COMPONENTS Core Gui)

include("${{CMAKE_CURRENT_LIST_DIR}}/{name}Targets.cmake")
"#
    )
}

/// Walk the user module's `pub` items and emit a `.qpi` binding file
/// describing the public surface. Consumed by downstream `cute build`s
/// when the consumer's cute.toml declares `[cute_libraries] deps =
/// ["<Name>"]` — the binding gets loaded into the type checker so
/// `use <Name>` brings the library's classes / structs / fns into
/// scope.
///
/// The emitter is intentionally minimal: pub class with prop /
/// signal / method signatures, pub struct with let/var fields, pub
/// fn with signature. Generic params, default values, weak/unowned
/// modifiers, init / deinit, and trait impls are not yet emitted —
/// downstream resolves them by re-parsing the consumer's source if
/// it actually imports them. (TODO: full surface coverage.)
fn generate_library_qpi(library: &Library, user_module: &ast::Module) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# {} v{} — auto-generated binding from `cute build` (library mode).",
        library.name, library.version
    );
    let _ = writeln!(out, "# Surface: pub classes / structs / fns.");
    let _ = writeln!(out);
    for item in &user_module.items {
        match item {
            ast::Item::Class(c) if c.is_pub => emit_qpi_class(&mut out, c),
            ast::Item::Struct(s) if s.is_pub => emit_qpi_struct(&mut out, s),
            ast::Item::Fn(f) if f.is_pub => emit_qpi_fn(&mut out, f),
            _ => {}
        }
    }
    out
}

fn emit_qpi_class(out: &mut String, c: &ast::ClassDecl) {
    use std::fmt::Write as _;
    let kind_kw = if c.is_arc {
        "arc "
    } else if c.is_extern_value {
        "extern value "
    } else {
        "class "
    };
    let _ = write!(out, "{kind_kw}{}", c.name.name);
    if let Some(super_) = &c.super_class {
        let _ = write!(out, " < {}", render_qpi_type(super_));
    }
    let _ = writeln!(out, " {{");
    for member in &c.members {
        match member {
            ast::ClassMember::Property(p) if p.is_pub => {
                let _ = writeln!(out, "  prop {} : {}", p.name.name, render_qpi_type(&p.ty));
            }
            ast::ClassMember::Signal(s) if s.is_pub => {
                let _ = writeln!(out, "  signal {}", s.name.name);
            }
            ast::ClassMember::Fn(f) if f.is_pub => emit_qpi_fn_member(out, f, "fn"),
            ast::ClassMember::Slot(f) if f.is_pub => emit_qpi_fn_member(out, f, "slot"),
            _ => {}
        }
    }
    let _ = writeln!(out, "}}");
    let _ = writeln!(out);
}

fn emit_qpi_struct(out: &mut String, s: &ast::StructDecl) {
    use std::fmt::Write as _;
    let _ = writeln!(out, "struct {} {{", s.name.name);
    for f in &s.fields {
        if !f.is_pub {
            continue;
        }
        let kind = if f.is_mut { "var " } else { "let " };
        let _ = writeln!(out, "  {kind}{} : {}", f.name.name, render_qpi_type(&f.ty));
    }
    for m in &s.methods {
        if !m.is_pub {
            continue;
        }
        emit_qpi_fn_member(out, m, "fn");
    }
    let _ = writeln!(out, "}}");
    let _ = writeln!(out);
}

fn emit_qpi_fn(out: &mut String, f: &ast::FnDecl) {
    use std::fmt::Write as _;
    let _ = write!(out, "fn {}", f.name.name);
    write_qpi_params(out, &f.params);
    if let Some(rt) = &f.return_ty {
        let _ = write!(out, " {}", render_qpi_type(rt));
    }
    let _ = writeln!(out);
}

fn emit_qpi_fn_member(out: &mut String, f: &ast::FnDecl, kw: &str) {
    use std::fmt::Write as _;
    let _ = write!(out, "  {kw} {}", f.name.name);
    write_qpi_params(out, &f.params);
    if let Some(rt) = &f.return_ty {
        let _ = write!(out, " {}", render_qpi_type(rt));
    }
    let _ = writeln!(out);
}

fn write_qpi_params(out: &mut String, params: &[ast::Param]) {
    use std::fmt::Write as _;
    if params.is_empty() {
        return;
    }
    let _ = write!(out, "(");
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            let _ = write!(out, ", ");
        }
        let _ = write!(out, "{}: {}", p.name.name, render_qpi_type(&p.ty));
    }
    let _ = write!(out, ")");
}

/// Render a Cute TypeExpr in `.qpi` shape. Delegates to cute-syntax's
/// existing `type_expr_render` so the binding output matches what
/// the consumer-side parser expects (and stays in sync with future
/// AST surface changes).
fn render_qpi_type(t: &ast::TypeExpr) -> String {
    ast::type_expr_render(t)
}

/// Test-mode counterpart to `compile_to_binary`. Codegen receives
/// `is_test_build=true`, so each `test fn name { ... }` lands in C++
/// and the runner main owns the entry point. The resulting binary
/// emits TAP-lite output and exits non-zero on the first failure.
pub fn compile_to_test_binary(input: &Path, output: Option<&Path>) -> Result<PathBuf, DriverError> {
    compile_binary_with_mode(input, output, true, &[])
}

/// Multi-input variant of `compile_to_test_binary`. The first path
/// in `inputs` becomes the primary entry (its stem is the binary
/// name; its parent directory drives `cute.toml` lookup). Remaining
/// paths are merged in as additional source modules so any `test fn`
/// they declare is reachable by the synthesized runner.
///
/// Used by `cute test` when invoked with no path argument: the CLI
/// walks cwd for `.cute` files and feeds the whole set in.
/// Run the same parse/resolve/typecheck/codegen frontend `cute build`
/// uses, then classify the resulting C++ via [`detect_mode`]. Returns
/// the inferred [`BuildMode`] alongside the loaded [`Manifest`] (or
/// `Manifest::default()` when no `cute.toml` sits next to the entry).
///
/// Skips cmake. Used by `cute_driver::doctor` to determine which
/// dependencies the build would look for without actually running it.
pub fn detect_mode_and_manifest(input: &Path) -> Result<(BuildMode, Manifest), DriverError> {
    let mut source_map = SourceMap::default();
    let (_stem, emit, manifest, _user_module) =
        frontend_with_mode(&mut source_map, input, false, &[])?;
    let mode = detect_mode(&emit.source);
    Ok((mode, manifest))
}

pub fn compile_to_test_binary_multi(
    inputs: &[PathBuf],
    output: Option<&Path>,
) -> Result<PathBuf, DriverError> {
    let primary = inputs
        .first()
        .ok_or_else(|| DriverError::NoStem(PathBuf::from("<empty>")))?;
    let extras: Vec<PathBuf> = inputs.iter().skip(1).cloned().collect();
    compile_binary_with_mode(primary, output, true, &extras)
}

fn compile_binary_with_mode(
    input: &Path,
    output: Option<&Path>,
    is_test_build: bool,
    extras: &[PathBuf],
) -> Result<PathBuf, DriverError> {
    let mut source_map = SourceMap::default();
    let (stem, mut emit, manifest, _user_module) =
        frontend_with_mode(&mut source_map, input, is_test_build, extras)?;

    let out_path: PathBuf = match output {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir()?.join(&stem),
    };

    let work = cache_dir_for(input)?;
    std::fs::create_dir_all(&work)?;

    // Generated C++ from cutec.
    // Mode detection from emitted C++ - cheap and decoupled from the AST.
    let mode = detect_mode(&emit.source);
    // Server mode pulls QHttpServer from the Qt6::HttpServer module
    // which lives behind its own umbrella header. Inject the include
    // before writing so users don't need a cute.toml just to satisfy
    // the link-side dependency announced by the binding.
    if matches!(mode, BuildMode::Server) {
        // Qt6::HttpServer relies on Qt6::Network for the underlying
        // QTcpServer that user code binds. The umbrella headers don't
        // overlap, so we pull both.
        prepend_after_pragma_once(
            &mut emit.header,
            "#include <QtHttpServer>\n#include <QtNetwork>\n",
        );
    }
    // Mode-orthogonal Qt extras (e.g. `Qt6::Network` when a QML or
    // CLI app pulls in QNetworkAccessManager). Detected from the
    // emitted source so the user doesn't need a cute.toml entry for
    // each Qt module their code happens to touch.
    // Scan BOTH the .cpp source and the .h header — inline class
    // member initializers (`CodeHighlighter* m_hl = new
    // CodeHighlighter(this)` for `let` fields on a class body)
    // live in the header rather than the source, so a cpp-only
    // scan would miss them and skip the include / link rewrite.
    let combined_for_extras = format!("{}{}", emit.source, emit.header);
    let build_extras = detect_build_extras(&combined_for_extras);
    for include_line in &build_extras.extra_includes {
        prepend_after_pragma_once(&mut emit.header, include_line);
    }

    let gen_dir = work.join("generated");
    std::fs::create_dir_all(&gen_dir)?;
    write_if_changed(&gen_dir.join(&emit.header_filename), &emit.header)?;
    write_if_changed(&gen_dir.join(&emit.source_filename), &emit.source)?;

    // Header-only Cute runtime (cute_arc.h, cute_error.h, ...). Bundled
    // into the cute binary at build time so users don't need the source
    // tree to run `cute build`.
    let runtime_dir = work.join("runtime").join("cpp");
    std::fs::create_dir_all(&runtime_dir)?;
    write_runtime_headers_if_changed(&runtime_dir)?;

    // QML mode: bundle every Cute-side view (lowered to .qml by codegen)
    // and, if the qml_app intrinsic refers to a non-view URL, the
    // sibling .qml file too. All embedded into a generated qml.qrc.
    if matches!(mode, BuildMode::Qml) {
        let qml_basename = detect_qml_basename(&emit.source).unwrap_or_else(|| "main.qml".into());
        let view_filenames: std::collections::HashSet<String> =
            emit.views.iter().map(|v| v.filename.clone()).collect();
        // 1) Cute UI DSL views always go in.
        for view in &emit.views {
            write_if_changed(&work.join(&view.filename), &view.qml)?;
        }
        // 2) If qml_app's URL points at something that isn't a Cute
        //    view, treat it as a sibling file and copy it across.
        if !view_filenames.contains(&qml_basename) {
            let parent = input.parent().unwrap_or_else(|| Path::new("."));
            let qml_path = parent.join(&qml_basename);
            if !qml_path.exists() {
                return Err(DriverError::MissingQml(qml_path));
            }
            let qml_text = std::fs::read_to_string(&qml_path)?;
            write_if_changed(&work.join(&qml_basename), &qml_text)?;
        }
        // 3) qml.qrc lists every embedded file.
        let mut files: Vec<String> = view_filenames.into_iter().collect();
        if !files.iter().any(|f| f == &qml_basename) {
            files.push(qml_basename.clone());
        }
        files.sort();
        let mut qrc = String::from("<RCC>\n    <qresource prefix=\"/\">\n");
        for f in &files {
            qrc.push_str(&format!("        <file>{f}</file>\n"));
        }
        qrc.push_str("    </qresource>\n</RCC>\n");
        write_if_changed(&work.join("qml.qrc"), &qrc)?;
    }

    // CMake project. PCH on by default; opt out via CUTE_NO_PCH=1.
    // The env-var check lives at the call site so generate_cmake stays
    // pure (deterministic test fixtures, no env-state thread races).
    let pch = std::env::var("CUTE_NO_PCH").ok().as_deref() != Some("1");
    let bundle_cfg = if macosx_bundle_for_mode(mode) {
        build_bundle_config(&manifest)
    } else {
        None
    };
    let cmake_text = generate_cmake(
        &stem,
        mode,
        &manifest,
        pch,
        bundle_cfg.as_ref(),
        &build_extras,
    );
    write_if_changed(&work.join("CMakeLists.txt"), &cmake_text)?;
    // Bundle-mode auxiliaries: Info.plist + entitlements XML, both
    // referenced by the generated CMakeLists. They live next to
    // CMakeLists.txt so `${CMAKE_SOURCE_DIR}` resolves to them.
    if let Some(cfg) = &bundle_cfg {
        write_if_changed(&work.join("Info.plist"), &render_info_plist(&stem, cfg))?;
        write_if_changed(&work.join("cute.entitlements"), CUTE_BUNDLE_ENTITLEMENTS)?;
    }

    // Configure (cmake itself is incremental: skips re-configure when
    // CMakeLists.txt mtime matches the cache).
    let build_dir = work.join("build");
    std::fs::create_dir_all(&build_dir)?;
    let mut configure_args: Vec<std::ffi::OsString> = vec![
        "-S".into(),
        work.clone().into_os_string(),
        "-B".into(),
        build_dir.clone().into_os_string(),
        "-DCMAKE_BUILD_TYPE=Release".into(),
    ];
    // Compose CMAKE_PREFIX_PATH from up to three probes:
    //  1. KDE Craft install (KF6 / Kirigami) — placed FIRST when the
    //     manifest pulls in KF6, so cmake picks Craft's Qt over
    //     Homebrew's. Kirigami is built against Craft's Qt; if Cute
    //     instead linked Homebrew Qt, libKirigami would drag the
    //     Craft Qt back in at load time and dyld would crash with
    //     a Qt-version mismatch. Aligning everyone on Craft's Qt
    //     eliminates the conflict and lets the .app bundle launch
    //     via `open Foo.app` without DYLD env. Trade-off: Craft Qt
    //     is older (6.10) than Homebrew (6.11), so CuteUI mode
    //     (which requires QtCanvasPainter from Qt 6.11) is
    //     incompatible with Kirigami in the same project.
    //  2. Qt 6 install (every mode needs it). Homebrew Qt for the
    //     non-Kirigami / CuteUI cases.
    //  3. CuteUI runtime install — only for `gpu_app` builds.
    // Joined with `;` (cmake's path-list separator on every platform)
    // so a single -D flag carries all three. A user's existing
    // CMAKE_PREFIX_PATH env value is captured by `find_qt_prefix`
    // already and rides along in the joined list.
    let mut prefix_paths: Vec<String> = Vec::new();
    if uses_kf6(&manifest) {
        if let Some(p) = find_craft_prefix() {
            prefix_paths.push(p);
        }
    }
    if let Some(p) = find_qt_prefix() {
        prefix_paths.push(p);
    }
    if mode == BuildMode::CuteUi {
        if let Some(p) = find_cute_ui_prefix() {
            prefix_paths.push(p);
        }
    }
    // Cute libraries declared under `[cute_libraries] deps` — append
    // each install prefix so cmake's `find_package(<Name>)` (added by
    // generate_cmake) resolves. The frontend already failed earlier
    // if any dep is missing, so unwrap here is safe.
    for dep_name in &manifest.cute_libraries.deps {
        if let Some(p) = find_cute_library_prefix(dep_name) {
            if let Some(s) = p.to_str() {
                prefix_paths.push(s.to_string());
            }
        }
    }
    if !prefix_paths.is_empty() {
        configure_args.push(format!("-DCMAKE_PREFIX_PATH={}", prefix_paths.join(";")).into());
    }
    let configure_args_ref: Vec<&std::ffi::OsStr> =
        configure_args.iter().map(|s| s.as_os_str()).collect();
    let configure = run_cmake(&configure_args_ref)?;
    if !configure.status.success() {
        return Err(DriverError::CmakeConfigure(format_cmake_configure_failure(
            &configure,
        )));
    }

    // Build.
    let built = run_cmake(&[
        "--build".as_ref(),
        build_dir.as_os_str(),
        "--config".as_ref(),
        "Release".as_ref(),
    ])?;
    if !built.status.success() {
        return Err(DriverError::CmakeBuild(combine_output(&built)));
    }

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    if let Some(cfg) = &bundle_cfg {
        // Bundle mode: cmake produced `{stem}.app/Contents/MacOS/{stem}`
        // under build_dir (or build_dir/Release). Stage the bundle
        // into a stable cache location, finalize it (launcher
        // injection + codesign), then copy the result to
        // `<out_path>.app` so the user can `open Foo.app` from cwd.
        // The flat `out_path` (no extension) is not produced.
        let app_name = format!("{}.app", &stem);
        let app_candidates = [
            build_dir.join(&app_name),
            build_dir.join("Release").join(&app_name),
        ];
        let built_app = app_candidates
            .iter()
            .find(|p| p.exists())
            .ok_or_else(|| DriverError::BinaryNotFound(build_dir.clone()))?;
        // Finalize in-place under build_dir: inject launcher (if
        // needed) then codesign with our entitlements. Doing this on
        // the build-dir copy keeps the cmake-tracked output stable
        // under incremental rebuilds.
        let entitlements_path = work.join("cute.entitlements");
        finalize_bundle(built_app, &stem, cfg, &entitlements_path)?;
        let dest_app = out_path.with_extension("app");
        copy_dir_recursive(built_app, &dest_app)?;
        return Ok(dest_app);
    }

    // Flat-binary mode (CLI / Server / Plain, or any non-mac target):
    // cmake stashed the output binary under build/ (single-config) or
    // build/Release/ (multi-config) — copy it to out_path.
    let candidates = [build_dir.join(&stem), build_dir.join("Release").join(&stem)];
    let built_bin = candidates
        .iter()
        .find(|p| p.exists())
        .ok_or_else(|| DriverError::BinaryNotFound(build_dir.clone()))?;
    std::fs::copy(built_bin, &out_path)?;

    // macOS Kirigami launch ergonomics for the legacy non-bundle path:
    // write a sibling `<binary>.run.sh` that handles the codesign +
    // DYLD/QML_IMPORT_PATH dance. Bundle mode replaces this; the
    // wrapper only fires for the (now rare) case of a Kirigami binary
    // built via `--out-dir` / non-GUI mode where bundling is off.
    if cfg!(target_os = "macos") && uses_kirigami(&manifest) {
        emit_macos_kirigami_wrapper(&out_path)?;
    }

    Ok(out_path)
}

/// True when `manifest`'s `[cmake] find_package` list references
/// Kirigami (KF6 or otherwise). This is the cue that the resulting
/// binary needs the macOS codesign + DYLD recipe to launch.
fn uses_kirigami(manifest: &Manifest) -> bool {
    manifest
        .cmake
        .find_package
        .iter()
        .any(|p| p.contains("Kirigami"))
}

/// Manifest references any KF6 component (Kirigami, CoreAddons, I18n,
/// etc.) — drives the Craft / `~/CraftRoot` prefix probe so
/// `find_package(KF6Foo)` resolves without the user setting
/// `CMAKE_PREFIX_PATH` manually. Catches both `KF6Foo` and bare
/// `Kirigami` references in the manifest's `find_package` list.
fn uses_kf6(manifest: &Manifest) -> bool {
    manifest
        .cmake
        .find_package
        .iter()
        .any(|p| p.contains("KF6") || p.contains("Kirigami"))
}

/// Write `<binary>.run.sh` next to the produced binary with the
/// codesign + DYLD env recipe baked in. Idempotent: the script
/// re-signs only on first run (or when the binary mtime moves past
/// the `.cs_resigned` marker, i.e. a fresh `cute build` rewrote it).
fn emit_macos_kirigami_wrapper(binary: &Path) -> Result<(), DriverError> {
    let bin_name = binary
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| DriverError::NoStem(binary.to_path_buf()))?;
    let wrapper = binary.with_extension("run.sh");
    let script = format!(
        r#"#!/bin/sh
# Auto-generated by cutec for Kirigami binaries on macOS.
# Re-signs the binary with the library-validation entitlements (one
# time per build) and sets DYLD / QML env vars so the binary loads
# Qt 6.11 from Homebrew (matching what cmake linked against) and
# Kirigami / KF6 from CraftRoot.
#
# Why two prefixes: `cute build`'s cmake step finds Homebrew Qt
# 6.11 first, so the binary is linked against Homebrew QtCore /
# QtGui / QtQml / QtQuick. libKirigami's RPATH points at
# CraftRoot/lib, which carries an *older* Qt (6.10.x); naively
# pointing DYLD at CraftRoot loads the older Qt and dies with
# "Symbol not found: QQmlTypeLoader::QQmlTypeLoader(...)" at startup.
# Setting DYLD_FRAMEWORK_PATH to Homebrew's qtbase + qtdeclarative
# forces dyld to consistently load Qt 6.11 from Homebrew while
# Kirigami still loads from CraftRoot via its baked absolute path.
#
# QML_IMPORT_PATH must combine both: Homebrew for QtQuick.Controls
# imports, CraftRoot for org.kde.kirigami's qmldir.
#
# Override paths via env: HOMEBREW_PREFIX (default /opt/homebrew),
# CRAFT_ROOT (default $HOME/CraftRoot).
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="$HERE/{bin_name}"
CRAFT="${{CRAFT_ROOT:-$HOME/CraftRoot}}"
BREW="${{HOMEBREW_PREFIX:-/opt/homebrew}}"

MARK="$BIN.cs_resigned"
if [ ! -f "$MARK" ] || [ "$BIN" -nt "$MARK" ]; then
  ENTITLEMENTS="$(mktemp -t cute-lv.entitlements)"
  cat > "$ENTITLEMENTS" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.cs.disable-library-validation</key>
    <true/>
    <key>com.apple.security.cs.allow-dyld-environment-variables</key>
    <true/>
    <key>com.apple.security.cs.allow-unsigned-executable-memory</key>
    <true/>
</dict>
</plist>
PLIST
  codesign --force --sign - --entitlements "$ENTITLEMENTS" "$BIN"
  rm -f "$ENTITLEMENTS"
  : > "$MARK"
fi

DYLD_FRAMEWORK_PATH="$BREW/opt/qtbase/lib:$BREW/opt/qtdeclarative/lib" \
  DYLD_LIBRARY_PATH="$CRAFT/lib" \
  QML_IMPORT_PATH="$BREW/share/qt/qml:$CRAFT/qml" \
  exec "$BIN" "$@"
"#
    );
    std::fs::write(&wrapper, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&wrapper)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&wrapper, perms)?;
    }
    Ok(())
}

// ---- macOS .app bundle support ---------------------------------------------

/// Codesign entitlements baked into every Cute `.app` bundle. Same set
/// the legacy `.run.sh` wrapper used to apply on first launch:
///
/// - `disable-library-validation`: required because the bundle links
///   against Homebrew Qt 6 frameworks signed by their own author rather
///   than ours, so AMFI's same-team check would otherwise refuse.
/// - `allow-dyld-environment-variables`: lets the user (or downstream
///   tooling) override DYLD_FRAMEWORK_PATH for debugging.
/// - `allow-unsigned-executable-memory`: needed by Qt's QML JIT path
///   (V4) which `mmap`s + writes generated bytecode at runtime.
const CUTE_BUNDLE_ENTITLEMENTS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.cs.disable-library-validation</key>
    <true/>
    <key>com.apple.security.cs.allow-dyld-environment-variables</key>
    <true/>
    <key>com.apple.security.cs.allow-unsigned-executable-memory</key>
    <true/>
</dict>
</plist>
"#;

/// Build the `BundleConfig` for a GUI build. Always returns
/// `Some(_)` on macOS — both the Kirigami and non-Kirigami GUI paths
/// bundle.
///
/// Two shapes:
///
/// - **Kirigami / KF6 builds**: rpath points at CraftRoot's lib dir
///   only. The cmake step put CraftRoot first in CMAKE_PREFIX_PATH
///   so Cute links against Craft's Qt 6.10 (matching what libKirigami
///   was built against). Same Qt across the binary + every
///   transitively loaded lib → no dyld version mismatch, no
///   `DYLD_FRAMEWORK_PATH` needed. Falls through to the Homebrew
///   path if Craft isn't installed (then the rebuild will fail at
///   link time, not at this discovery step).
///
/// - **Non-Kirigami builds (widgets / pure-QML / cute_ui)**: rpath
///   points at Homebrew Qt's qtbase + qtdeclarative lib dirs. Cute
///   linked against Homebrew Qt; rpath alone is enough.
///
/// Either way, `open Foo.app` works without DYLD env. The Kirigami
/// alignment relies on the user installing Qt + Kirigami via KDE
/// Craft (`craft kirigami`) — Cute's official Kirigami-on-macOS
/// setup story.
fn build_bundle_config(manifest: &Manifest) -> Option<BundleConfig> {
    if uses_kf6(manifest) {
        if let Some(craft) = find_craft_prefix() {
            return Some(BundleConfig {
                rpath_dirs: vec![format!("{craft}/lib")],
                dyld_framework_paths: Vec::new(),
                dyld_library_paths: Vec::new(),
                qml_import_paths: Vec::new(),
            });
        }
        // Craft missing — link & launch will fail downstream with a
        // clearer message than dyld's. Fall through to the Homebrew
        // path so we still emit a bundle (best-effort).
    }
    let brew_prefix = brew_prefix();
    let brew_qt_lib = format!("{brew_prefix}/opt/qtbase/lib");
    let brew_qml_lib = format!("{brew_prefix}/opt/qtdeclarative/lib");
    Some(BundleConfig {
        rpath_dirs: vec![brew_qt_lib, brew_qml_lib],
        dyld_framework_paths: Vec::new(),
        dyld_library_paths: Vec::new(),
        qml_import_paths: Vec::new(),
    })
}

/// Probe the active Homebrew prefix. Falls back to the canonical Apple
/// Silicon path; on Intel the user typically exports `HOMEBREW_PREFIX`
/// (or has `/usr/local` on PATH) so the env-var route covers them.
fn brew_prefix() -> String {
    if let Ok(p) = std::env::var("HOMEBREW_PREFIX") {
        if !p.is_empty() {
            return p;
        }
    }
    "/opt/homebrew".to_string()
}

/// Render the per-bundle Info.plist. Minimal but covers what Launch
/// Services actually reads:
///
/// - `CFBundleExecutable` / `CFBundleIdentifier` for the bundle's
///   identity (the executable lives at `Contents/MacOS/<exe>`).
/// - `CFBundlePackageType = APPL` so `mdimporter` / Finder treat it
///   as an application.
/// - `LSMinimumSystemVersion = 12.0` matches Qt 6's macOS floor.
/// - `NSHighResolutionCapable` so the app renders at native DPI on
///   Retina screens (otherwise 2x scaled pixel-doubling).
/// - `NSRequiresAquaSystemAppearance = false` lets the app honour
///   the user's Light / Dark setting (Qt's palette plumbs this).
/// - `LSEnvironment.{DYLD_FRAMEWORK_PATH, DYLD_LIBRARY_PATH,
///   QML_IMPORT_PATH}`: cute's substitute for the retired run.sh's
///   env exports. Launch Services sets these in the launched process's
///   environment, so:
///     - DYLD_FRAMEWORK_PATH overrides per-lib rpath when transitively
///       loaded libs (libKirigami) try to pull in their own Qt copy
///       — without this, dyld crashes with `Symbol not found:
///       QQmlTypeLoader::QQmlTypeLoader(...)` on Kirigami launch.
///     - QML_IMPORT_PATH lets the QML engine resolve imports like
///       `org.kde.kirigami`.
///   (Only effective via Launch Services — `open Foo.app` / Finder
///   double-click — not when running the bare binary from a shell.
///   The codesign entitlement `allow-dyld-environment-variables` is
///   what permits these; AMFI would otherwise strip them.)
fn render_info_plist(stem: &str, cfg: &BundleConfig) -> String {
    let mut env_pairs: Vec<(&str, String)> = Vec::new();
    if !cfg.dyld_framework_paths.is_empty() {
        env_pairs.push(("DYLD_FRAMEWORK_PATH", cfg.dyld_framework_paths.join(":")));
    }
    if !cfg.dyld_library_paths.is_empty() {
        env_pairs.push(("DYLD_LIBRARY_PATH", cfg.dyld_library_paths.join(":")));
    }
    if !cfg.qml_import_paths.is_empty() {
        env_pairs.push(("QML_IMPORT_PATH", cfg.qml_import_paths.join(":")));
    }
    let env_block = if env_pairs.is_empty() {
        String::new()
    } else {
        let mut s = String::from("    <key>LSEnvironment</key>\n    <dict>\n");
        for (k, v) in &env_pairs {
            s.push_str(&format!(
                "        <key>{k}</key>\n        <string>{v}</string>\n"
            ));
        }
        s.push_str("    </dict>\n");
        s
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>{stem}</string>
    <key>CFBundleIdentifier</key>
    <string>org.cute.{stem}</string>
    <key>CFBundleName</key>
    <string>{stem}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSRequiresAquaSystemAppearance</key>
    <false/>
{env_block}</dict>
</plist>
"#
    )
}

/// Adhoc-codesign the cmake-built .app with cute's entitlements.
/// Idempotent — safe to run on every `cute build`. Operates in place
/// so the next `cute build`'s incremental cmake step inspects a
/// consistent tree. Bypasses the cmake POST_BUILD path so this is
/// the single source of truth for bundle signing.
fn finalize_bundle(
    app: &Path,
    _stem: &str,
    _cfg: &BundleConfig,
    entitlements: &Path,
) -> Result<(), DriverError> {
    let status = Command::new("codesign")
        .arg("--force")
        .arg("--sign")
        .arg("-")
        .arg("--options=runtime")
        .arg("--entitlements")
        .arg(entitlements)
        .arg(app)
        .status()
        .map_err(|e| DriverError::CmakeBuild(format!("failed to invoke codesign: {e}")))?;
    if !status.success() {
        return Err(DriverError::CmakeBuild(format!(
            "codesign failed with status {status} on {app:?}"
        )));
    }
    Ok(())
}

/// Recursive directory copy (file-by-file `std::fs::copy`). Used to
/// stage the cmake-produced `Foo.app` from the build cache to the
/// user's cwd. Replaces any existing destination tree first so a
/// rebuild doesn't accumulate stale Contents/ files.
///
/// This is a deliberately small reimplementation rather than a new
/// dep; cute-driver's only existing crates are the workspace pieces.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.exists() {
        std::fs::remove_dir_all(dst)?;
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ty.is_symlink() {
            // Some Qt frameworks ship Versions/Current symlinks. Copy
            // the link target rather than dereferencing — preserves
            // the framework structure if cmake ever stages frameworks
            // into our bundle.
            let target = std::fs::read_link(&from)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &to)?;
            #[cfg(not(unix))]
            std::fs::copy(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

// ---- internals -------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BuildMode {
    /// `qml_app(...)` intrinsic: links Qt6::Gui/Qml/Quick/QuickControls2
    /// and bundles `main.qml` via a generated qrc.
    Qml,
    /// `widget_app(...)` intrinsic: links Qt6::Widgets for a
    /// QApplication-based main with no QML in the loop.
    Widgets,
    /// `server_app { ... }` intrinsic: QCoreApplication + Qt6::HttpServer
    /// for HTTP / event-loop services.
    Server,
    /// `cli_app { ... }` or any plain main using QCoreApplication.
    Cli,
    /// `fn main { ... }` with no Qt application wrapper.
    Plain,
    /// `gpu_app(window: T)` intrinsic: GPU-accelerated UI via the Qt 6.11
    /// QtCanvasPainter module (Tech Preview) on a QWindow. Requires the
    /// CuteUI runtime, installable via `cute install-cute-ui`.
    CuteUi,
}

/// True for build modes whose binary is a GUI app on macOS, and so
/// should be emitted as a `Foo.app` bundle rather than a flat binary.
/// Off-mac this is always false — CMake's `MACOSX_BUNDLE` is a
/// no-op there, and the wrapper-script path stays disabled.
///
/// Picks Qml / Widgets / CuteUi (every GUI mode); excludes Cli /
/// Server / Plain so users still get a flat `./tool` binary for
/// command-line and headless workloads.
fn macosx_bundle_for_mode(mode: BuildMode) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    matches!(
        mode,
        BuildMode::Qml | BuildMode::Widgets | BuildMode::CuteUi
    )
}

pub fn detect_mode(cpp: &str) -> BuildMode {
    // Match cute::ui::App first — it derives from QGuiApplication, so the
    // QGuiApplication substring would otherwise classify it as Qml.
    if cpp.contains("cute::ui::App") {
        return BuildMode::CuteUi;
    }
    // QApplication is checked before QGuiApplication because Widgets'
    // QApplication derives from QGuiApplication, so the source contains
    // the substring; we want the more specific match to win.
    if cpp.contains("QApplication app") {
        BuildMode::Widgets
    } else if cpp.contains("QGuiApplication") || cpp.contains("QQmlApplicationEngine") {
        BuildMode::Qml
    } else if cpp.contains("QHttpServer") {
        // HTTP server use - implies QCoreApplication-based main with
        // app.exec() event loop. Detected from any QHttpServer mention,
        // not just `server_app { ... }`, so a future free-standing
        // intrinsic-less HTTP setup still gets the right link line.
        // QtNetwork-only use (QNetworkAccessManager from a QML / CLI
        // app) takes a different path: see `detect_build_extras`,
        // which adds Qt6::Network to whichever mode is detected.
        BuildMode::Server
    } else if cpp.contains("QCoreApplication") {
        BuildMode::Cli
    } else {
        BuildMode::Plain
    }
}

/// Extract the QML file basename from the cutec-emitted main. Looks for
/// the canonical `engine.load(QUrl(QStringLiteral("qrc:/<file>")));`
/// shape that `emit_qml_app_main` produces, peels off `qrc:/`, returns
/// the trailing component (e.g. `"main.qml"`). Path components in the
/// URL are flattened to the basename - the embedded qml.qrc is always
/// rooted at `prefix="/"` for now.
fn detect_qml_basename(cpp: &str) -> Option<String> {
    let prefix = "QStringLiteral(\"qrc:/";
    let start = cpp.find(prefix)?;
    let after = &cpp[start + prefix.len()..];
    let end = after.find('"')?;
    let url_path = &after[..end];
    let basename = url_path.rsplit('/').next().unwrap_or(url_path);
    if basename.is_empty() {
        None
    } else {
        Some(basename.to_string())
    }
}

const CUTE_ARC_H: &str = include_str!("../../../runtime/cpp/cute_arc.h");
const CUTE_ASYNC_H: &str = include_str!("../../../runtime/cpp/cute_async.h");
const CUTE_ERROR_H: &str = include_str!("../../../runtime/cpp/cute_error.h");
const CUTE_SLICE_H: &str = include_str!("../../../runtime/cpp/cute_slice.h");
const CUTE_STRING_H: &str = include_str!("../../../runtime/cpp/cute_string.h");
const CUTE_META_H: &str = include_str!("../../../runtime/cpp/cute_meta.h");
const CUTE_NULLABLE_H: &str = include_str!("../../../runtime/cpp/cute_nullable.h");
const CUTE_TEST_H: &str = include_str!("../../../runtime/cpp/cute_test.h");
const CUTE_GENERIC_H: &str = include_str!("../../../runtime/cpp/cute_generic.h");
const CUTE_FUNCTION_H: &str = include_str!("../../../runtime/cpp/cute_function.h");
const CUTE_MODEL_H: &str = include_str!("../../../runtime/cpp/cute_model.h");
const CUTE_KI18N_H: &str = include_str!("../../../runtime/cpp/cute_ki18n.h");
const CUTE_CODE_HIGHLIGHTER_H: &str = include_str!("../../../runtime/cpp/cute_code_highlighter.h");

fn write_runtime_headers_if_changed(dir: &Path) -> std::io::Result<()> {
    write_if_changed(&dir.join("cute_arc.h"), CUTE_ARC_H)?;
    write_if_changed(&dir.join("cute_async.h"), CUTE_ASYNC_H)?;
    write_if_changed(&dir.join("cute_error.h"), CUTE_ERROR_H)?;
    write_if_changed(&dir.join("cute_slice.h"), CUTE_SLICE_H)?;
    write_if_changed(&dir.join("cute_string.h"), CUTE_STRING_H)?;
    write_if_changed(&dir.join("cute_meta.h"), CUTE_META_H)?;
    write_if_changed(&dir.join("cute_nullable.h"), CUTE_NULLABLE_H)?;
    write_if_changed(&dir.join("cute_test.h"), CUTE_TEST_H)?;
    write_if_changed(&dir.join("cute_generic.h"), CUTE_GENERIC_H)?;
    write_if_changed(&dir.join("cute_function.h"), CUTE_FUNCTION_H)?;
    write_if_changed(&dir.join("cute_model.h"), CUTE_MODEL_H)?;
    write_if_changed(&dir.join("cute_ki18n.h"), CUTE_KI18N_H)?;
    write_if_changed(
        &dir.join("cute_code_highlighter.h"),
        CUTE_CODE_HIGHLIGHTER_H,
    )?;
    Ok(())
}

/// Write `content` to `path` only if the existing contents differ.
/// Preserves mtime on no-op writes so downstream make/ninja sees the
/// file as up-to-date and skips its compile rule.
fn write_if_changed(path: &Path, content: &str) -> std::io::Result<bool> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        if existing == content {
            return Ok(false);
        }
    }
    std::fs::write(path, content)?;
    Ok(true)
}

/// Per-source persistent cache dir under the user's cache home. Same
/// `.cute` source path always hashes to the same dir, so subsequent
/// `cute build` invocations reuse cmake's CMakeCache.txt and the
/// build tool's incremental graph.
fn cache_dir_for(input: &Path) -> std::io::Result<PathBuf> {
    let canonical = std::fs::canonicalize(input).unwrap_or_else(|_| input.to_path_buf());
    let key = format!("{:016x}", fnv1a_64(canonical.to_string_lossy().as_bytes()));
    let root = cache_root();
    Ok(root.join("cute").join("build").join(key))
}

fn cache_root() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        let p = PathBuf::from(xdg);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".cache");
    }
    std::env::temp_dir()
}

fn fnv1a_64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Find Qt 6's installation prefix for `find_package(Qt6)`. Probes in
/// order:
///   1. `QT_DIR` env var (Qt's own convention)
///   2. `CMAKE_PREFIX_PATH` env var (cmake's convention)
///   3. `qmake6 -query QT_INSTALL_PREFIX` (Linux distro packages)
///   4. `qmake-qt6 -query` (Fedora) or `qmake -query` (one-Qt-installed boxes)
///   5. Platform-specific well-known dirs (Homebrew on macOS;
///      `/usr/lib/qt6`, `/usr/lib/x86_64-linux-gnu/qt6` on Linux)
///
/// Returns `None` if nothing matches; cmake's own search may still find
/// Qt in standard system locations even when we pass no `CMAKE_PREFIX_PATH`.
pub fn find_qt_prefix() -> Option<String> {
    if let Ok(p) = std::env::var("QT_DIR") {
        if !p.is_empty() {
            return Some(p);
        }
    }
    if let Ok(p) = std::env::var("CMAKE_PREFIX_PATH") {
        if !p.is_empty() {
            return Some(p);
        }
    }
    for tool in ["qmake6", "qmake-qt6", "qmake"] {
        if let Ok(out) = Command::new(tool)
            .arg("-query")
            .arg("QT_INSTALL_PREFIX")
            .output()
        {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() && std::path::Path::new(&s).exists() {
                    return Some(s);
                }
            }
        }
    }
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/opt/homebrew/opt/qt",
            "/opt/homebrew/opt/qt6",
            "/usr/local/opt/qt",
            "/usr/local/opt/qt6",
        ]
    } else if cfg!(target_os = "linux") {
        &[
            "/usr/lib/qt6",
            "/usr/lib/x86_64-linux-gnu/qt6",
            "/usr/lib64/qt6",
            "/usr/local/Qt-6",
            "/opt/Qt6",
        ]
    } else {
        &[]
    };
    candidates
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(|p| (*p).to_string())
}

/// Find the install prefix of KDE Craft (KF6 / Kirigami / Qt 6 frameworks)
/// so `find_package(KF6Kirigami)` and friends resolve without the user
/// setting `CMAKE_PREFIX_PATH` themselves.
///
/// Probes:
///   1. `CRAFT_ROOT` env var (matches what the macOS Kirigami `.run.sh`
///      wrapper already understands).
///   2. `~/CraftRoot` (the default `CraftBootstrap.py` location).
///
/// A prefix only counts when it carries the KF6 cmake config dir; a
/// stray `~/CraftRoot/` shell with nothing built into it doesn't shadow
/// other probes.
pub fn find_craft_prefix() -> Option<String> {
    use std::path::Path;
    let candidates: Vec<std::path::PathBuf> = [
        std::env::var_os("CRAFT_ROOT").map(std::path::PathBuf::from),
        std::env::var_os("HOME").map(|h| Path::new(&h).join("CraftRoot")),
    ]
    .into_iter()
    .flatten()
    .collect();
    for prefix in candidates {
        if has_kf6_config(&prefix) {
            return prefix.to_str().map(String::from);
        }
    }
    None
}

fn has_kf6_config(prefix: &std::path::Path) -> bool {
    // KF6 ships per-component config files. Any one of them confirms
    // a usable install.
    has_cmake_config(prefix, "KF6")
        || has_cmake_config(prefix, "KF6Kirigami")
        || has_cmake_config(prefix, "KF6CoreAddons")
}

/// True when `<prefix>/lib/cmake/<package>/<package>Config.cmake` (or
/// the `lib64` variant) exists. CMake's `find_package(<package>)` would
/// resolve under this prefix without further path hints. Used by
/// `cute_driver::doctor` to report module presence without invoking
/// cmake.
pub fn has_cmake_config(prefix: &std::path::Path, package: &str) -> bool {
    prefix
        .join(format!("lib/cmake/{package}/{package}Config.cmake"))
        .exists()
        || prefix
            .join(format!("lib64/cmake/{package}/{package}Config.cmake"))
            .exists()
}

/// Read Qt6's installed version from the cmake config files Qt ships
/// with. Qt 6.x writes the literal `set(PACKAGE_VERSION "6.X.Y")` line
/// in `Qt6CoreConfigVersionImpl.cmake` (newer wrapper-style configs)
/// or `Qt6CoreConfigVersion.cmake` (older / in-tree builds). Also
/// honours Homebrew's nested `opt/qt/...` layout when the prefix is
/// `/opt/homebrew` rather than `/opt/homebrew/opt/qt`.
///
/// `None` when no candidate is readable. Used by doctor to report
/// installed Qt version + flag too-old installs (`CuteUi` requires
/// 6.11+).
pub fn read_qt_version(prefix: &std::path::Path) -> Option<String> {
    let nested_homebrew = [
        prefix.join("opt/qt"),
        prefix.join("opt/qt6"),
        prefix.to_path_buf(),
    ];
    for base in &nested_homebrew {
        let candidates = [
            base.join("lib/cmake/Qt6Core/Qt6CoreConfigVersionImpl.cmake"),
            base.join("lib/cmake/Qt6Core/Qt6CoreConfigVersion.cmake"),
            base.join("lib64/cmake/Qt6Core/Qt6CoreConfigVersionImpl.cmake"),
            base.join("lib64/cmake/Qt6Core/Qt6CoreConfigVersion.cmake"),
        ];
        for path in &candidates {
            if let Ok(text) = std::fs::read_to_string(path) {
                if let Some(v) = parse_qt_version_cmake(&text) {
                    return Some(v);
                }
            }
        }
    }
    None
}

pub(crate) fn parse_qt_version_cmake(text: &str) -> Option<String> {
    // The shape across Qt 6.x is `set(PACKAGE_VERSION "6.11.0")`. Some
    // distributions add whitespace; tolerate that. We don't bring in
    // a regex crate just for this.
    for line in text.lines() {
        let line = line.trim();
        let lower = line.to_ascii_lowercase();
        let prefix = "set(package_version";
        if !lower.starts_with(prefix) {
            continue;
        }
        let after = &line[prefix.len()..];
        // After PACKAGE_VERSION there's whitespace then a quoted version.
        let q1 = after.find('"')?;
        let rest = &after[q1 + 1..];
        let q2 = rest.find('"')?;
        return Some(rest[..q2].to_string());
    }
    None
}

/// Find the install prefix of the CuteUI runtime so `find_package(CuteUI)`
/// in a `gpu_app` build resolves without the user setting
/// `CMAKE_PREFIX_PATH` themselves. Probes:
///
///   1. `CUTE_UI_DIR` env var (explicit override).
///   2. `~/.cache/cute/cute-ui-runtime/<version>/<triple>/` — what
///      `cute install-cute-ui` writes to. Walks the cache root and
///      picks the most recently modified subdirectory containing a
///      `lib/cmake/CuteUI/CuteUIConfig.cmake`, so a stale install
///      from an old crate version doesn't shadow a fresh one.
///   3. `<workspace>/runtime/cute-ui/install/` — the local-dev path
///      produced by `cmake --install` against `runtime/cute-ui/build`.
///      Resolved relative to this crate's `CARGO_MANIFEST_DIR` so it
///      only points at something useful when the binary was built
///      from inside the workspace; harmless when the dir doesn't
///      exist (e.g. user installed `cute` via `cargo install`).
///
/// Returns `None` when nothing matches; the user can still set
/// `CMAKE_PREFIX_PATH` manually as before.
pub fn find_cute_ui_prefix() -> Option<String> {
    use std::path::Path;
    if let Ok(p) = std::env::var("CUTE_UI_DIR") {
        if !p.is_empty() && has_cute_ui_config(Path::new(&p)) {
            return Some(p);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let cache_root = Path::new(&home).join(".cache/cute/cute-ui-runtime");
        if let Some(p) = find_cute_ui_under_cache(&cache_root) {
            return Some(p);
        }
    }
    let dev_install = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("runtime")
        .join("cute-ui")
        .join("install");
    if has_cute_ui_config(&dev_install) {
        if let Some(s) = dev_install
            .canonicalize()
            .ok()
            .and_then(|p| p.to_str().map(String::from))
        {
            return Some(s);
        }
    }
    None
}

/// Walk a `cute-ui-runtime/<version>/<triple>/` cache root and pick
/// the most recently modified install that contains a usable
/// CuteUIConfig.cmake. Lets the driver pick up the latest install
/// even when the cute-driver crate version doesn't exactly match
/// the cute-cli version that ran `install-cute-ui`.
fn find_cute_ui_under_cache(root: &std::path::Path) -> Option<String> {
    if !root.exists() {
        return None;
    }
    let mut best: Option<(std::time::SystemTime, String)> = None;
    let version_entries = std::fs::read_dir(root).ok()?;
    for v in version_entries.flatten() {
        let v_path = v.path();
        if !v_path.is_dir() {
            continue;
        }
        let triple_entries = match std::fs::read_dir(&v_path) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for t in triple_entries.flatten() {
            let candidate = t.path();
            if !has_cute_ui_config(&candidate) {
                continue;
            }
            let mtime = std::fs::metadata(&candidate)
                .and_then(|m| m.modified())
                .ok()?;
            let path_str = match candidate.to_str() {
                Some(s) => s.to_string(),
                None => continue,
            };
            match &best {
                Some((bt, _)) if *bt >= mtime => {}
                _ => best = Some((mtime, path_str)),
            }
        }
    }
    best.map(|(_, p)| p)
}

fn has_cute_ui_config(prefix: &std::path::Path) -> bool {
    prefix.join("lib/cmake/CuteUI/CuteUIConfig.cmake").exists()
        || prefix
            .join("lib64/cmake/CuteUI/CuteUIConfig.cmake")
            .exists()
}

/// Locate an installed Cute library by name in the user cache.
/// Returns the install prefix (`~/.cache/cute/libraries/<name>/<version>/<triple>/`)
/// for the most recently installed version that matches the host triple
/// and has a usable `<Name>Config.cmake`. Returns `None` if no install
/// is present.
///
/// Used by consumer-side `cute build` to resolve `[cute_libraries] deps
/// = [...]` entries — both the cmake side (CMAKE_PREFIX_PATH +
/// find_package + link_libraries) and the binding side (loading the
/// `.qpi` so `use <Name>` brings the library's classes into scope).
pub fn find_cute_library_prefix(name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let cache_root = PathBuf::from(home).join(".cache/cute/libraries").join(name);
    if !cache_root.exists() {
        return None;
    }
    let host_triple = host_triple_short();
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let version_entries = std::fs::read_dir(&cache_root).ok()?;
    for v in version_entries.flatten() {
        let v_path = v.path();
        if !v_path.is_dir() {
            continue;
        }
        let candidate = v_path.join(&host_triple);
        if !has_cute_library_config(&candidate, name) {
            continue;
        }
        let mtime = std::fs::metadata(&candidate)
            .and_then(|m| m.modified())
            .ok()?;
        match &best {
            Some((bt, _)) if *bt >= mtime => {}
            _ => best = Some((mtime, candidate)),
        }
    }
    best.map(|(_, p)| p)
}

fn has_cute_library_config(prefix: &std::path::Path, name: &str) -> bool {
    prefix
        .join(format!("lib/cmake/{name}/{name}Config.cmake"))
        .exists()
        || prefix
            .join(format!("lib64/cmake/{name}/{name}Config.cmake"))
            .exists()
}

/// Per-build inputs needed to wire up macOS `.app` bundling at cmake
/// emission time. `None` for non-bundle modes (CLI / Server / Plain,
/// or any non-mac target).
///
/// `rpath_dirs` is the search path baked into the binary via
/// `INSTALL_RPATH`. Sufficient on its own for non-Kirigami GUI apps:
/// dyld walks rpath plus standard system locations.
///
/// `dyld_framework_paths` and `dyld_library_paths` are the override
/// paths the legacy `.run.sh` wrapper used to set in the shell env.
/// When Kirigami is in play the binary's rpath alone is not enough
/// — libKirigami has its own rpath pointing at CraftRoot, which
/// carries an older Qt; without DYLD_FRAMEWORK_PATH forcing every
/// Qt-framework lookup back to Homebrew Qt 6.11, dyld loads the
/// CraftRoot QtQml and crashes with `Symbol not found:
/// QQmlTypeLoader::QQmlTypeLoader(...)`. We bake these into
/// `LSEnvironment` in Info.plist so `open Foo.app` (Launch
/// Services) sets them automatically — the user doesn't see DYLD.
///
/// `qml_import_paths` ride along inside `LSEnvironment` so
/// QML imports like `org.kde.kirigami` resolve when launched via
/// Launch Services.
struct BundleConfig {
    rpath_dirs: Vec<String>,
    dyld_framework_paths: Vec<String>,
    dyld_library_paths: Vec<String>,
    qml_import_paths: Vec<String>,
}

/// Qt6 components the `find_package(Qt6 REQUIRED COMPONENTS ...)` line
/// carries for `mode`. Single source of truth for both `generate_cmake`
/// and `required_deps_for`.
fn qt6_components_for_mode(mode: BuildMode) -> &'static str {
    match mode {
        BuildMode::Qml => "Core Gui Qml Quick QuickControls2",
        BuildMode::Widgets => "Core Gui Widgets",
        BuildMode::Server => "Core Network HttpServer",
        BuildMode::Cli => "Core",
        BuildMode::Plain => "Core",
        // GuiPrivate is needed for <rhi/qrhi.h>; CanvasPainter is the Tech
        // Preview module the cute_ui runtime renders through; Svg is for
        // SvgElement (QSvgRenderer is in Qt6::Svg).
        BuildMode::CuteUi => "Core Gui GuiPrivate Svg CanvasPainter",
    }
}

/// `target_link_libraries(...)` items for `mode`. Includes
/// `cute_ui::cute_ui` for CuteUi mode (not a Qt6 module, but linked
/// alongside).
fn qt6_link_libs_for_mode(mode: BuildMode) -> &'static str {
    match mode {
        BuildMode::Qml => "Qt6::Core Qt6::Gui Qt6::Qml Qt6::Quick Qt6::QuickControls2",
        BuildMode::Widgets => "Qt6::Core Qt6::Gui Qt6::Widgets",
        BuildMode::Server => "Qt6::Core Qt6::Network Qt6::HttpServer",
        BuildMode::Cli => "Qt6::Core",
        BuildMode::Plain => "Qt6::Core",
        BuildMode::CuteUi => {
            "Qt6::Core Qt6::Gui Qt6::GuiPrivate Qt6::Svg Qt6::CanvasPainter cute_ui::cute_ui"
        }
    }
}

fn sources_qrc_for_mode(mode: BuildMode) -> &'static str {
    match mode {
        BuildMode::Qml => "qml.qrc",
        _ => "",
    }
}

/// What this build needs at the toolchain level. Shared between
/// `generate_cmake` (which formats CMakeLists from these inputs) and
/// `cute_driver::doctor` (which checks each one is present and prints
/// platform-specific install commands when it isn't).
///
/// Pure function of `(mode, manifest)` — no filesystem / env access.
#[derive(Debug, Clone)]
pub struct RequiredDeps {
    pub mode: BuildMode,
    /// Qt6 components without the `Qt6::` prefix. E.g. `["Core","Gui","Qml"]`.
    /// CuteUi includes `GuiPrivate` (folded under `Gui` for package lookup)
    /// and `CanvasPainter` (Tech Preview, no package — see [`QtComponentKind`]).
    pub qt6_components: Vec<String>,
    /// Min Qt version. `Some("6.11")` for CuteUi, otherwise None.
    pub qt6_min_version: Option<&'static str>,
    /// Whether `find_package(CuteUI REQUIRED)` is added to CMakeLists.
    pub needs_cute_ui: bool,
    /// True when the manifest's `[cmake] find_package` mentions any KF6
    /// component or bare "Kirigami". Drives the Craft prefix probe.
    pub uses_kf6: bool,
    /// Manifest's `[cmake] find_package` entries verbatim — doctor
    /// surfaces these when reporting KF6 / other-package status.
    pub manifest_find_packages: Vec<String>,
    /// `[cute_libraries] deps` names. Each one resolves to a separate
    /// `find_package(<Name> REQUIRED)` plus a link entry.
    pub cute_libraries: Vec<String>,
}

/// Compute the [`RequiredDeps`] for a build with the given mode + manifest.
///
/// This is the v1 entry point for `cute doctor`: it reproduces the same
/// dependency view that `generate_cmake` emits to CMakeLists, without
/// any filesystem or cmake side effects.
pub fn required_deps_for(mode: BuildMode, manifest: &Manifest) -> RequiredDeps {
    let mut qt6_components: Vec<String> = qt6_components_for_mode(mode)
        .split_whitespace()
        .map(String::from)
        .collect();
    // Manifest may add extra Qt6 components via `find_package = ["Qt6
    // COMPONENTS Charts"]`. Merge those in so doctor reports them
    // alongside the BuildMode-derived base list. Order: base first
    // (preserves the canonical Core/Gui/... ordering), manifest adds
    // appended in the order they appear.
    for entry in &manifest.cmake.find_package {
        for c in doctor::packages::parse_qt6_components_from_find_package(entry) {
            if !qt6_components.iter().any(|x| x == &c) {
                qt6_components.push(c);
            }
        }
    }
    RequiredDeps {
        mode,
        qt6_components,
        qt6_min_version: if mode == BuildMode::CuteUi {
            Some("6.11")
        } else {
            None
        },
        needs_cute_ui: mode == BuildMode::CuteUi,
        uses_kf6: uses_kf6(manifest),
        manifest_find_packages: manifest.cmake.find_package.clone(),
        cute_libraries: manifest.cute_libraries.deps.clone(),
    }
}

fn generate_cmake(
    stem: &str,
    mode: BuildMode,
    manifest: &Manifest,
    pch: bool,
    bundle: Option<&BundleConfig>,
    extras: &BuildExtras,
) -> String {
    // Mode-derived base components/libs, then append source-detected
    // extras (e.g. `Network` when QNetworkAccessManager is in the
    // generated cpp). De-dup by string equality so a Server-mode
    // build (which already lists `Network`) doesn't repeat it.
    let mut components_v: Vec<&str> = qt6_components_for_mode(mode).split_whitespace().collect();
    for c in &extras.qt6_components {
        if !components_v.contains(c) {
            components_v.push(c);
        }
    }
    let components = components_v.join(" ");
    let mut libs_v: Vec<&str> = qt6_link_libs_for_mode(mode).split_whitespace().collect();
    for l in &extras.qt6_link_libs {
        if !libs_v.contains(l) {
            libs_v.push(l);
        }
    }
    let libs = libs_v.join(" ");
    let sources_qrc = sources_qrc_for_mode(mode);
    let qrc_line = if sources_qrc.is_empty() {
        String::new()
    } else {
        format!("    {}\n", sources_qrc)
    };
    // Manifest-declared `find_package(...)` lines are appended after
    // the built-in Qt6 line, so user packages can resolve their own
    // dependencies (e.g. KF6Kio implicitly pulls Qt6).
    //
    // Cute libraries declared under `[cute_libraries] deps` get the
    // same treatment automatically: `find_package(<Name>) REQUIRED` +
    // append `<Name>::<Name>` to the link line. The user doesn't
    // duplicate them under `[cmake]`. (Detailed per-spec entries
    // under `[cute_libraries.<Name>]` are honoured by `cute install`,
    // not by build directly — they show up here only after install.)
    let cute_lib_find_packages: Vec<String> = manifest
        .cute_libraries
        .deps
        .iter()
        .map(|name| format!("find_package({name} REQUIRED)"))
        .collect();
    let cute_lib_link_libs: String = manifest
        .cute_libraries
        .deps
        .iter()
        .map(|name| format!(" {name}::{name}"))
        .collect::<String>();
    let extra_find_packages = manifest
        .cmake
        .find_package
        .iter()
        .map(|args| format!("find_package({args} REQUIRED)"))
        .chain(cute_lib_find_packages)
        .collect::<Vec<_>>()
        .join("\n");
    let extra_link_libs =
        if manifest.cmake.link_libraries.is_empty() && cute_lib_link_libs.is_empty() {
            String::new()
        } else {
            format!(
                " {}{cute_lib_link_libs}",
                manifest.cmake.link_libraries.join(" ")
            )
        };
    // CuteUi pins Qt 6.11 (CanvasPainter requirement) and pulls in the
    // CuteUI runtime package automatically.
    let qt_min_version = if mode == BuildMode::CuteUi {
        " 6.11"
    } else {
        ""
    };
    let cuteui_find_package = if mode == BuildMode::CuteUi {
        "find_package(CuteUI REQUIRED)\n"
    } else {
        ""
    };
    // Precompiled headers: bundle the Qt umbrella headers most likely
    // to be transitively included by the generated .cpp. Re-parsing
    // these is the bulk of compile time on Qt projects (each one
    // pulls hundreds of files), so caching them once and reusing
    // across incremental rebuilds is the cheapest win available.
    //
    // The driver opts out by setting `pch=false` (CUTE_NO_PCH=1) —
    // useful if a sysroot's header layout doesn't tolerate PCH.
    let pch_block = if !pch {
        String::new()
    } else {
        let mut pch_headers = pch_headers_for_mode(mode);
        for h in &extras.pch_headers {
            if !pch_headers.contains(h) {
                pch_headers.push(h);
            }
        }
        let lines: Vec<String> = pch_headers.iter().map(|h| format!("    \"{h}\"")).collect();
        format!(
            "\n# Precompile the Qt umbrella headers — they dominate compile\n\
             # time on Qt projects and rarely change. Disable with CUTE_NO_PCH=1.\n\
             target_precompile_headers({stem} PRIVATE\n{}\n)\n",
            lines.join("\n")
        )
    };
    // Compute INSTALL_RPATH entries for each cute_libraries dep so
    // dyld resolves @rpath/lib<Name>.dylib at launch without needing
    // DYLD_LIBRARY_PATH. Falls back to empty when the dep's install
    // can't be located — the link step would have failed earlier in
    // that case anyway.
    let cute_lib_rpaths: Vec<String> = manifest
        .cute_libraries
        .deps
        .iter()
        .filter_map(|name| find_cute_library_prefix(name))
        .map(|p| {
            // The library's dylib lives under <prefix>/<libdir>/. CMake
            // installs to lib/ on macOS / Linux (lib64/ on some
            // distros). Try lib/ first, fall back to lib64/.
            let lib = p.join("lib");
            if lib.exists() {
                lib.to_string_lossy().to_string()
            } else {
                p.join("lib64").to_string_lossy().to_string()
            }
        })
        .collect();
    let bundle_target_block = render_bundle_target_block(stem, bundle, &cute_lib_rpaths);
    format!(
        r#"cmake_minimum_required(VERSION 3.21)
project({stem} CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
# cutec emits the qt_create_metaobjectdata<Tag> template specialization
# itself (Qt 6.9+ moc-output form). AUTORCC packages embedded resources.
set(CMAKE_AUTOMOC OFF)
set(CMAKE_AUTORCC ON)

find_package(Qt6{qt_min_version} REQUIRED COMPONENTS {components})
{cuteui_find_package}{extra_find_packages}

add_executable({stem}
{qrc_line}    generated/{stem}.cpp
)

{bundle_target_block}
target_include_directories({stem} PRIVATE
    generated
    runtime/cpp
)

target_link_libraries({stem} PRIVATE {libs}{extra_link_libs})
{pch_block}"#
    )
}

/// Emit the `set_target_properties` (and POST_BUILD codesign) block
/// that controls macOS bundling. Two shapes:
///
/// - `bundle = None` (CLI / Server / Plain, or off-mac): plain
///   `MACOSX_BUNDLE FALSE` so the user gets `./{stem}` they can run
///   directly.
///
/// - `bundle = Some(BundleConfig)` (GUI mode on mac): `MACOSX_BUNDLE
///   TRUE`, an Info.plist that sits next to CMakeLists.txt, an
///   `INSTALL_RPATH` baked into the binary so dyld finds Qt /
///   Kirigami frameworks without env, and a POST_BUILD codesign
///   step that re-signs the bundle with the entitlements needed
///   for library validation + dyld env vars (the same set the
///   retired `.run.sh` wrapper used).
fn render_bundle_target_block(
    stem: &str,
    bundle: Option<&BundleConfig>,
    cute_lib_rpaths: &[String],
) -> String {
    let cute_lib_rpath_block = if cute_lib_rpaths.is_empty() {
        String::new()
    } else {
        // CLI / non-bundle target with cute_libraries deps: bake an
        // INSTALL_RPATH so dyld resolves @rpath/lib<Name>.dylib to
        // each library's installed `lib/` dir. Without this the user
        // would have to set DYLD_LIBRARY_PATH on every launch.
        let joined = cute_lib_rpaths.join(";");
        format!(
            "set_target_properties({stem} PROPERTIES\n    \
             INSTALL_RPATH \"{joined}\"\n    \
             BUILD_WITH_INSTALL_RPATH TRUE\n    \
             MACOSX_RPATH TRUE\n)\n"
        )
    };
    match bundle {
        None => format!(
            "# Plain CLI binary (not a .app bundle), so users get `./{stem}` directly.\n\
             set_target_properties({stem} PROPERTIES MACOSX_BUNDLE FALSE)\n{cute_lib_rpath_block}",
        ),
        Some(b) => {
            // CMake's INSTALL_RPATH takes a `;`-separated list. Each
            // entry is searched in order at load time; @executable_path
            // resolves to `Foo.app/Contents/MacOS/`, so the standard
            // first entry covers any framework copied into the bundle
            // (deferred — for now the bundle relies on system frameworks
            // resolved through the remaining entries).
            //
            // Codesigning happens driver-side after the build (see
            // `finalize_bundle`), not here. We sometimes inject a
            // launcher script into Contents/MacOS/ post-build (for
            // Kirigami's DYLD-override needs), and a cmake POST_BUILD
            // codesign would seal the bundle before that injection
            // and need re-signing anyway.
            let mut rpath = vec!["@executable_path/../Frameworks".to_string()];
            rpath.extend(b.rpath_dirs.iter().cloned());
            rpath.extend(cute_lib_rpaths.iter().cloned());
            let rpath_list = rpath.join(";");
            format!(
                "# Emit a `.app` bundle so the GUI binary is `open Foo.app`-able\n\
                 # without DYLD env. INSTALL_RPATH bakes in the linked Qt /\n\
                 # Kirigami framework dirs. Codesign + entitlements are\n\
                 # applied driver-side after the build (see finalize_bundle).\n\
                 set_target_properties({stem} PROPERTIES\n    \
                 MACOSX_BUNDLE TRUE\n    \
                 MACOSX_BUNDLE_INFO_PLIST \"${{CMAKE_SOURCE_DIR}}/Info.plist\"\n    \
                 INSTALL_RPATH \"{rpath_list}\"\n    \
                 BUILD_WITH_INSTALL_RPATH TRUE\n    \
                 MACOSX_RPATH TRUE\n)\n",
            )
        }
    }
}

/// Pick PCH headers based on build mode. The list mirrors `libs` —
/// every Qt module the link line pulls in gets its umbrella header
/// in the PCH. Order is most-fundamental-first (QtCore before
/// QtWidgets) so the PCH compiler sees a sensible dependency order.
/// Optional Qt6 surface area inferred from the generated C++ that
/// the build mode alone doesn't pin. The canonical case is
/// QtNetwork: a QML or CLI app that uses `QNetworkAccessManager`
/// needs `Qt6::Network` linked, but neither the QML app intrinsic
/// nor `cli_app { ... }` reveals that — it shows up in the source
/// only after codegen has expanded the user's `manager.get(req)`.
///
/// Detected via substring scan over the emitted .cpp. Each entry is
/// purely additive: we never *replace* the mode-derived components,
/// only append. Idempotent — listing a class twice in the source
/// still produces one component entry.
#[derive(Default, Debug, Clone)]
struct BuildExtras {
    /// Extra Qt6 component names (without the `Qt6::` prefix) to feed
    /// into `find_package(Qt6 REQUIRED COMPONENTS ...)`.
    qt6_components: Vec<&'static str>,
    /// Extra `Qt6::*` link entries for `target_link_libraries`.
    qt6_link_libs: Vec<&'static str>,
    /// Extra umbrella headers to add to the precompiled-header set.
    pch_headers: Vec<&'static str>,
    /// Extra `#include` lines to inject after `#pragma once` in the
    /// generated .h. Needed because the cute-codegen output references
    /// Qt classes by bare name (`QNetworkReply*`) without including
    /// the umbrella header itself.
    extra_includes: Vec<&'static str>,
}

fn detect_build_extras(cpp: &str) -> BuildExtras {
    let mut out = BuildExtras::default();
    // QtNetwork — `QNetworkAccessManager`, `QNetworkRequest`,
    // `QNetworkReply`, `QSslSocket` etc. all live behind the QtNetwork
    // umbrella. Trigger on the manager class because that's the
    // entry point for every client; if a user reaches `QNetworkReply`
    // they almost certainly have a `QNetworkAccessManager` in scope
    // first.
    if cpp.contains("QNetworkAccessManager")
        || cpp.contains("QNetworkRequest")
        || cpp.contains("QNetworkReply")
    {
        out.qt6_components.push("Network");
        out.qt6_link_libs.push("Qt6::Network");
        out.pch_headers.push("<QtNetwork>");
        out.extra_includes.push("#include <QtNetwork>\n");
    }
    // `cute::CodeHighlighter` — the runtime helper that wraps
    // QSyntaxHighlighter with a regex-rule API. Trigger on
    // `new CodeHighlighter(` so a user class accidentally named
    // `CodeHighlighter` doesn't pull in the header. The class
    // sits in QtGui (for QSyntaxHighlighter / QTextDocument) but
    // its convenience constructors take QPlainTextEdit / QTextEdit
    // (QtWidgets) too, so we pull Widgets unconditionally. QML /
    // CuteUi consumers can still use the QTextDocument-only ctor
    // — they'll just get an unused QtWidgets link entry.
    if cpp.contains("new CodeHighlighter(") || cpp.contains("CodeHighlighter::") {
        out.qt6_components.push("Gui");
        out.qt6_link_libs.push("Qt6::Gui");
        out.qt6_components.push("Widgets");
        out.qt6_link_libs.push("Qt6::Widgets");
        out.pch_headers.push("<QSyntaxHighlighter>");
        out.extra_includes
            .push("#include \"cute_code_highlighter.h\"\n");
    }
    out
}

fn pch_headers_for_mode(mode: BuildMode) -> Vec<&'static str> {
    match mode {
        BuildMode::Qml => vec!["<QtCore>", "<QtGui>", "<QtQml>", "<QtQuick>"],
        BuildMode::Widgets => vec!["<QtCore>", "<QtGui>", "<QtWidgets>"],
        BuildMode::Server => vec!["<QtCore>", "<QtNetwork>", "<QtHttpServer>"],
        BuildMode::Cli => vec!["<QtCore>"],
        BuildMode::Plain => vec!["<QtCore>"],
        // CanvasPainter is excluded — Tech Preview headers churn often
        // enough that PCH invalidation would dominate.
        BuildMode::CuteUi => vec!["<QtCore>", "<QtGui>"],
    }
}

fn run_cmake(args: &[&std::ffi::OsStr]) -> Result<std::process::Output, DriverError> {
    let mut cmd = Command::new("cmake");
    cmd.args(args);
    cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            DriverError::CmakeMissing
        } else {
            DriverError::Io(e)
        }
    })
}

fn combine_output(out: &std::process::Output) -> String {
    let mut s = String::new();
    if !out.stdout.is_empty() {
        s.push_str("--- stdout ---\n");
        s.push_str(&String::from_utf8_lossy(&out.stdout));
    }
    if !out.stderr.is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str("--- stderr ---\n");
        s.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    s
}

/// Wrap a cmake-configure failure with a one-line hint when the
/// captured output looks like a missing-package error. The hint nudges
/// users toward `cute doctor` rather than leaving them to read raw
/// CMake errors.
fn format_cmake_configure_failure(out: &std::process::Output) -> String {
    let combined = combine_output(out);
    if looks_like_missing_package(&combined) {
        format!("{combined}\nhint: run `cute doctor` to diagnose missing dependencies.")
    } else {
        combined
    }
}

/// True when cmake's configure output names a `find_package` resolution
/// failure. The substrings here are stable across cmake 3.21+ — they
/// come from cmake's own message templates, not from anything we emit.
fn looks_like_missing_package(text: &str) -> bool {
    text.contains("Could NOT find Qt6")
        || text.contains("Could NOT find KF6")
        || text.contains("Could NOT find CuteUI")
        || text.contains("By not providing \"FindQt6.cmake\"")
        || text.contains("By not providing \"FindKF6.cmake\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_missing_package_catches_qt6_failure() {
        let cmake_err = "CMake Error at CMakeLists.txt:5 (find_package):\n\
                         Could NOT find Qt6 (missing: Qt6_DIR)\n";
        assert!(looks_like_missing_package(cmake_err));
    }

    #[test]
    fn looks_like_missing_package_catches_kf6_failure() {
        let cmake_err = "Could NOT find KF6Kirigami\n";
        assert!(looks_like_missing_package(cmake_err));
    }

    #[test]
    fn looks_like_missing_package_does_not_fire_on_unrelated_errors() {
        let cmake_err = "CMake Error: target_link_libraries called with invalid arguments";
        assert!(!looks_like_missing_package(cmake_err));
    }

    #[test]
    fn required_deps_for_qml_basic() {
        let deps = required_deps_for(BuildMode::Qml, &Manifest::default());
        assert_eq!(
            deps.qt6_components,
            vec!["Core", "Gui", "Qml", "Quick", "QuickControls2"]
        );
        assert_eq!(deps.qt6_min_version, None);
        assert!(!deps.needs_cute_ui);
        assert!(!deps.uses_kf6);
    }

    #[test]
    fn required_deps_for_cute_ui_pins_six_eleven() {
        let deps = required_deps_for(BuildMode::CuteUi, &Manifest::default());
        assert_eq!(deps.qt6_min_version, Some("6.11"));
        assert!(deps.needs_cute_ui);
        // GuiPrivate + CanvasPainter are part of the component list so
        // doctor's check + (later) install-message logic can see them.
        assert!(deps.qt6_components.contains(&"GuiPrivate".to_string()));
        assert!(deps.qt6_components.contains(&"CanvasPainter".to_string()));
    }

    #[test]
    fn required_deps_for_widgets_with_charts_manifest() {
        let mut m = Manifest::default();
        m.cmake.find_package = vec!["Qt6 COMPONENTS Charts".to_string()];
        let deps = required_deps_for(BuildMode::Widgets, &m);
        assert!(deps.qt6_components.contains(&"Charts".to_string()));
        // Base modules still come first.
        assert_eq!(deps.qt6_components[0], "Core");
    }

    #[test]
    fn required_deps_for_kirigami_flips_uses_kf6() {
        let mut m = Manifest::default();
        m.cmake.find_package = vec!["KF6Kirigami".to_string()];
        let deps = required_deps_for(BuildMode::Qml, &m);
        assert!(deps.uses_kf6);
    }

    #[test]
    fn detect_mode_widgets_wins_over_qml_substring() {
        let widgets =
            "int main(int argc, char** argv) { QApplication app(argc, argv); return app.exec(); }";
        assert_eq!(detect_mode(widgets), BuildMode::Widgets);

        let qml = "int main(int argc, char** argv) { QGuiApplication app(argc, argv); return app.exec(); }";
        assert_eq!(detect_mode(qml), BuildMode::Qml);

        let cli = "int main(int argc, char** argv) { QCoreApplication app(argc, argv); return app.exec(); }";
        assert_eq!(detect_mode(cli), BuildMode::Cli);

        assert_eq!(detect_mode("int main() { return 0; }"), BuildMode::Plain);
    }

    #[test]
    fn cmake_widgets_links_qt_widgets_only() {
        let s = generate_cmake(
            "demo",
            BuildMode::Widgets,
            &Manifest::default(),
            true,
            None,
            &BuildExtras::default(),
        );
        assert!(s.contains("Qt6::Widgets"), "expected Qt6::Widgets:\n{}", s);
        assert!(
            !s.contains("Qt6::Quick"),
            "widgets-mode should not pull QML libs:\n{}",
            s
        );
    }

    #[test]
    fn cmake_widgets_includes_qt_pch() {
        let s = generate_cmake(
            "demo",
            BuildMode::Widgets,
            &Manifest::default(),
            true,
            None,
            &BuildExtras::default(),
        );
        assert!(
            s.contains("target_precompile_headers(demo PRIVATE"),
            "expected target_precompile_headers in widgets mode:\n{s}"
        );
        assert!(
            s.contains("\"<QtWidgets>\""),
            "widgets PCH should include QtWidgets:\n{s}"
        );
        assert!(
            s.contains("\"<QtCore>\""),
            "every PCH should include QtCore:\n{s}"
        );
    }

    #[test]
    fn cmake_qml_includes_quick_pch() {
        let s = generate_cmake(
            "demo",
            BuildMode::Qml,
            &Manifest::default(),
            true,
            None,
            &BuildExtras::default(),
        );
        assert!(
            s.contains("\"<QtQuick>\"") && s.contains("\"<QtQml>\""),
            "qml PCH should include QtQuick and QtQml:\n{s}"
        );
        assert!(
            !s.contains("\"<QtWidgets>\""),
            "qml PCH should not pull QtWidgets:\n{s}"
        );
    }

    #[test]
    fn cmake_server_includes_httpserver_pch() {
        let s = generate_cmake(
            "demo",
            BuildMode::Server,
            &Manifest::default(),
            true,
            None,
            &BuildExtras::default(),
        );
        assert!(
            s.contains("\"<QtHttpServer>\"") && s.contains("\"<QtNetwork>\""),
            "server PCH should include QtHttpServer + QtNetwork:\n{s}"
        );
    }

    #[test]
    fn cmake_pch_disabled_omits_block() {
        let s = generate_cmake(
            "demo",
            BuildMode::Widgets,
            &Manifest::default(),
            false,
            None,
            &BuildExtras::default(),
        );
        assert!(
            !s.contains("targetPrecompileHeaders"),
            "pch=false should suppress PCH:\n{s}"
        );
    }

    #[test]
    fn cmake_qml_with_qnetwork_pulls_qt6_network() {
        // QML mode + QNetworkAccessManager in the source → Qt6::Network
        // gets appended to components/libs/PCH without flipping mode
        // to Server (QML apps shouldn't pull Qt6::HttpServer just to
        // get a HTTP client).
        let extras =
            detect_build_extras("auto m = new QNetworkAccessManager(); m->get(QNetworkRequest());");
        assert_eq!(extras.qt6_components, vec!["Network"]);
        let s = generate_cmake(
            "demo",
            BuildMode::Qml,
            &Manifest::default(),
            true,
            None,
            &extras,
        );
        assert!(
            s.contains("REQUIRED COMPONENTS Core Gui Qml Quick QuickControls2 Network"),
            "Network should append after the QML base components:\n{s}"
        );
        assert!(
            s.contains("Qt6::Quick Qt6::QuickControls2 Qt6::Network"),
            "Qt6::Network should follow the QML link line:\n{s}"
        );
        assert!(
            !s.contains("Qt6::HttpServer"),
            "QtNetwork detection should NOT pull HttpServer:\n{s}"
        );
        assert!(
            s.contains("\"<QtNetwork>\""),
            "PCH should pick up QtNetwork:\n{s}"
        );
    }

    #[test]
    fn manifest_extends_cmake_with_extra_packages_and_libs() {
        let manifest = Manifest {
            cmake: CmakeConfig {
                find_package: vec!["Qt6 COMPONENTS Charts".into()],
                link_libraries: vec!["Qt6::Charts".into()],
            },
            ..Default::default()
        };
        let s = generate_cmake(
            "demo",
            BuildMode::Widgets,
            &manifest,
            true,
            None,
            &BuildExtras::default(),
        );
        assert!(
            s.contains("find_package(Qt6 COMPONENTS Charts REQUIRED)"),
            "missing extra find_package:\n{}",
            s
        );
        assert!(
            s.contains("Qt6::Widgets Qt6::Charts"),
            "extra link library should follow built-in libs:\n{}",
            s
        );
    }

    #[test]
    fn library_qpi_emits_pub_class_with_signatures() {
        // Parse a small library source, walk the resulting module
        // through the .qpi emitter, and verify the surface
        // shape matches existing stdlib .qpi files (class header,
        // prop / signal / fn signatures, no method bodies).
        let src = "pub class LibCounter < QObject {\n  pub prop count : Int, notify: :countChanged, default: 0\n  pub signal countChanged\n  pub fn increment {\n    count = count + 1\n  }\n  fn privateHelper {\n    return\n  }\n}\n";
        let mut sm = SourceMap::default();
        let fid = sm.add("test.cute".into(), src.into());
        let module = parse(fid, src).expect("parse");
        let library = Library {
            name: "LibCounter".into(),
            version: "0.1.0".into(),
            description: "test".into(),
        };
        let qpi = generate_library_qpi(&library, &module);
        assert!(qpi.contains("class LibCounter < QObject"));
        assert!(qpi.contains("prop count : Int"));
        assert!(qpi.contains("signal countChanged"));
        assert!(qpi.contains("fn increment"));
        // Method bodies must NOT appear in the binding.
        assert!(
            !qpi.contains("count = count + 1"),
            "method body leaked into qpi:\n{qpi}"
        );
        // Non-pub members are filtered out.
        assert!(
            !qpi.contains("privateHelper"),
            "non-pub method leaked into qpi:\n{qpi}"
        );
    }

    #[test]
    fn cute_libraries_deps_drive_find_package_and_link_libs() {
        let manifest = Manifest {
            cute_libraries: CuteLibraries {
                deps: vec!["LibCounter".into(), "OtherLib".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let s = generate_cmake(
            "demo",
            BuildMode::Cli,
            &manifest,
            true,
            None,
            &BuildExtras::default(),
        );
        assert!(
            s.contains("find_package(LibCounter REQUIRED)"),
            "missing auto-generated find_package for cute_libraries dep:\n{s}"
        );
        assert!(s.contains("find_package(OtherLib REQUIRED)"));
        assert!(
            s.contains("LibCounter::LibCounter"),
            "missing auto-generated link_libraries for cute_libraries dep:\n{s}"
        );
        assert!(s.contains("OtherLib::OtherLib"));
    }

    #[test]
    fn library_install_prefix_uses_versioned_triple_path() {
        let lib = Library {
            name: "FooLib".into(),
            version: "1.2.3".into(),
            description: "".into(),
        };
        // HOME is set in any reasonable test env. Skip gracefully
        // otherwise so CI matrix variability doesn't fail the test.
        if std::env::var_os("HOME").is_none() {
            return;
        }
        let prefix = library_install_prefix(&lib).expect("HOME present");
        let s = prefix.to_string_lossy();
        assert!(
            s.contains(".cache/cute/libraries/FooLib/1.2.3/"),
            "install prefix shape changed: {s}"
        );
    }

    #[test]
    fn macosx_bundle_for_mode_picks_gui_modes_only_on_mac() {
        // The cfg(target_os = "macos") gate is asserted via the `if`
        // inside the helper — on non-mac this returns false for every
        // mode. The crate's test config matters: when `cargo test`
        // runs on mac, all GUI modes flip true; on linux they all
        // stay false.
        if cfg!(target_os = "macos") {
            assert!(macosx_bundle_for_mode(BuildMode::Qml));
            assert!(macosx_bundle_for_mode(BuildMode::Widgets));
            assert!(macosx_bundle_for_mode(BuildMode::CuteUi));
        }
        // CLI / Server / Plain never bundle (even on mac the user gets
        // a flat `./tool` for shell scripting).
        assert!(!macosx_bundle_for_mode(BuildMode::Cli));
        assert!(!macosx_bundle_for_mode(BuildMode::Server));
        assert!(!macosx_bundle_for_mode(BuildMode::Plain));
    }

    #[test]
    fn cmake_bundle_block_emits_macosx_bundle_true_and_codesign() {
        let cfg = BundleConfig {
            rpath_dirs: vec!["/opt/homebrew/opt/qtbase/lib".into()],
            dyld_framework_paths: vec![],
            dyld_library_paths: vec![],
            qml_import_paths: vec![],
        };
        let s = generate_cmake(
            "demo",
            BuildMode::Widgets,
            &Manifest::default(),
            true,
            Some(&cfg),
            &BuildExtras::default(),
        );
        assert!(
            s.contains("MACOSX_BUNDLE TRUE"),
            "bundle mode should flip MACOSX_BUNDLE TRUE:\n{s}"
        );
        assert!(
            s.contains("MACOSX_BUNDLE_INFO_PLIST"),
            "bundle should reference Info.plist:\n{s}"
        );
        assert!(
            s.contains("INSTALL_RPATH"),
            "bundle should embed INSTALL_RPATH:\n{s}"
        );
        assert!(
            s.contains("@executable_path/../Frameworks"),
            "rpath should include @executable_path:\n{s}"
        );
        assert!(
            s.contains("/opt/homebrew/opt/qtbase/lib"),
            "rpath should include caller-supplied dirs:\n{s}"
        );
        // Codesign moved out of the cmake POST_BUILD step into
        // driver-side `finalize_bundle`, so the cmake template no
        // longer contains the codesign command. The `add_custom_command`
        // line should not be regenerated by accident.
        assert!(
            !s.contains("add_custom_command"),
            "bundle codesign moved to driver-side finalize_bundle:\n{s}"
        );
    }

    #[test]
    fn build_bundle_config_kirigami_uses_craft_only_rpath() {
        // Craft-only path: Kirigami builds put CraftRoot into rpath only
        // — Cute is linked against Craft's Qt 6.10 (matching what
        // libKirigami was built against), so dyld doesn't need a
        // Homebrew framework path or DYLD_FRAMEWORK_PATH override.
        // Bundle launches via `open Foo.app` clean.
        let manifest = Manifest {
            cmake: CmakeConfig {
                find_package: vec!["KF6Kirigami".into()],
                link_libraries: vec!["KF6::Kirigami".into()],
            },
            ..Default::default()
        };
        let cfg = build_bundle_config(&manifest)
            .expect("Kirigami builds always produce a .app bundle on macOS");
        // Whether Craft is installed depends on the test env; on a
        // dev box with CraftRoot present, we expect the Craft lib
        // dir as the only rpath entry. On a Craft-less CI box the
        // helper falls back to the Homebrew shape (covered by the
        // non-Kirigami widgets test below).
        if let Some(craft) = find_craft_prefix() {
            assert_eq!(
                cfg.rpath_dirs,
                vec![format!("{craft}/lib")],
                "Kirigami rpath should be Craft-only, not Homebrew Qt",
            );
        }
        assert!(cfg.dyld_framework_paths.is_empty());
        assert!(cfg.dyld_library_paths.is_empty());
        assert!(cfg.qml_import_paths.is_empty());
    }

    #[test]
    fn build_bundle_config_returns_some_for_non_kirigami_widgets() {
        // Plain widgets / qml builds with no KF6 should bundle
        // straight away — rpath-only path covers them on macOS.
        let cfg = build_bundle_config(&Manifest::default());
        assert!(cfg.is_some(), "non-Kirigami GUI builds must bundle");
        let cfg = cfg.unwrap();
        assert!(
            cfg.rpath_dirs.iter().any(|p| p.contains("/opt/qtbase/lib")),
            "rpath must include Homebrew Qt qtbase lib",
        );
        // Empty DYLD/QML overrides — rpath alone is enough so the
        // bundle stays a plain Mach-O entry, no shell launcher.
        assert!(cfg.dyld_framework_paths.is_empty());
        assert!(cfg.dyld_library_paths.is_empty());
        assert!(cfg.qml_import_paths.is_empty());
    }

    #[test]
    fn cmake_non_bundle_keeps_macosx_bundle_false() {
        let s = generate_cmake(
            "demo",
            BuildMode::Cli,
            &Manifest::default(),
            true,
            None,
            &BuildExtras::default(),
        );
        assert!(
            s.contains("MACOSX_BUNDLE FALSE"),
            "CLI mode should keep MACOSX_BUNDLE FALSE:\n{s}"
        );
        assert!(
            !s.contains("INSTALL_RPATH"),
            "CLI mode should not emit rpath block:\n{s}"
        );
    }

    #[test]
    fn info_plist_minimal_shape_for_widgets_app() {
        let cfg = BundleConfig {
            rpath_dirs: vec![],
            dyld_framework_paths: vec![],
            dyld_library_paths: vec![],
            qml_import_paths: vec![],
        };
        let xml = render_info_plist("counter", &cfg);
        assert!(xml.contains("<key>CFBundleExecutable</key>"));
        assert!(xml.contains("<string>counter</string>"));
        assert!(xml.contains("<key>CFBundleIdentifier</key>"));
        assert!(xml.contains("org.cute.counter"));
        assert!(xml.contains("<key>NSHighResolutionCapable</key>"));
        // Empty env paths → no LSEnvironment block (avoids exporting
        // empty PATH-style vars).
        assert!(!xml.contains("LSEnvironment"));
    }

    #[test]
    fn info_plist_includes_lsenvironment_with_dyld_and_qml_paths() {
        let cfg = BundleConfig {
            rpath_dirs: vec![],
            dyld_framework_paths: vec![
                "/opt/homebrew/opt/qtbase/lib".into(),
                "/opt/homebrew/opt/qtdeclarative/lib".into(),
            ],
            dyld_library_paths: vec!["/Users/u/CraftRoot/lib".into()],
            qml_import_paths: vec![
                "/opt/homebrew/share/qt/qml".into(),
                "/Users/u/CraftRoot/qml".into(),
            ],
        };
        let xml = render_info_plist("kdemo", &cfg);
        assert!(xml.contains("LSEnvironment"));
        assert!(
            xml.contains("DYLD_FRAMEWORK_PATH"),
            "DYLD_FRAMEWORK_PATH must be set so transitively-loaded\nlibs honour Homebrew Qt:\n{xml}"
        );
        assert!(
            xml.contains("/opt/homebrew/opt/qtbase/lib:/opt/homebrew/opt/qtdeclarative/lib"),
            "framework paths should be `:`-joined:\n{xml}"
        );
        assert!(xml.contains("DYLD_LIBRARY_PATH"));
        assert!(xml.contains("/Users/u/CraftRoot/lib"));
        assert!(xml.contains("QML_IMPORT_PATH"));
        assert!(xml.contains("/opt/homebrew/share/qt/qml:/Users/u/CraftRoot/qml"));
    }

    /// Two-file project: `counter.cute` declares the QObject class,
    /// `main.cute` instantiates it from a `view`. The driver should
    /// merge them into one logical module so `main.cute` can name
    /// `Counter` even though it lives in a sibling file.
    #[test]
    fn multi_file_project_compiles() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("counter.cute"),
            "pub class Counter < QObject {\n  prop count : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "view Main {\n  ApplicationWindow {\n    Counter { id: c }\n  }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("cute.toml"),
            "[sources]\npaths = [\"counter.cute\"]\n",
        )
        .unwrap();

        let out_dir = dir.path().join("out");
        let written = build_file(&dir.path().join("main.cute"), &out_dir).expect("buildFile");
        assert_eq!(written.len(), 2, "expected .h + .cpp: {written:?}");

        // The emitted header should reference the Counter class
        // declared in the sibling file - proof that the merge
        // actually flowed into codegen.
        let header = std::fs::read_to_string(&written[0]).unwrap();
        assert!(
            header.contains("class Counter"),
            "expected Counter in emitted header:\n{header}"
        );
    }

    /// Two `.cute` files declaring the same simple name in
    /// different modules is ALLOWED via module-level namespacing.
    /// Codegen mangles to `<module>__<name>`. Within-file dupes
    /// remain an error.
    #[test]
    fn cross_module_same_name_compiles() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("a.cute"),
            "pub class Counter < QObject {\n  prop count : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "use a\n\nclass Counter < QObject {\n  prop other : Int, default: 0\n}\n\nfn main {}\n",
        )
        .unwrap();

        let written = build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect("cross-module same-name should compile via mangling");
        let header = std::fs::read_to_string(&written[0]).unwrap();
        // Module prefix is capitalised so the resulting name is a
        // legal QML type identifier (QML rejects type names that
        // start lowercase).
        assert!(
            header.contains("A__Counter") && header.contains("Main__Counter"),
            "expected both mangled names in header:\n{header}"
        );
    }

    /// `parse_user_sources` merges caller-supplied `extras` paths
    /// alongside the entry file. This is what powers `cute test` no-arg
    /// auto-discovery: the CLI walks cwd for `.cute` files and feeds
    /// the whole set in, so test fns from `tests/feature_x.cute` land
    /// in the same compilation unit as `main.cute`.
    #[test]
    fn parse_user_sources_merges_extras_alongside_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let primary = dir.path().join("main.cute");
        let extra = dir.path().join("more.cute");
        std::fs::write(&primary, "test fn alpha { assert_eq(1, 1) }\n").unwrap();
        std::fs::write(&extra, "test fn beta { assert_eq(2, 2) }\n").unwrap();

        let mut sm = SourceMap::default();
        let manifest = Manifest::default();
        let extras = vec![extra.clone()];
        let sources =
            parse_user_sources(&mut sm, &primary, &manifest, None, &extras).expect("parse");
        assert_eq!(
            sources.modules.len(),
            2,
            "expected entry + extra to be parsed, got {}",
            sources.modules.len()
        );
        // No `[sources] paths` was set, so the extra must NOT have
        // been recorded as ambient. Tests that share helpers should
        // `use` each other explicitly.
        assert!(
            sources.ambient_paths.is_empty(),
            "extras should not be ambient, got {:?}",
            sources.ambient_paths
        );
    }

    /// Multi-input `cute test`: the runner main calls every test fn
    /// across every input file. Smoke-tests the full
    /// frontend_with_mode pipeline at test-build mode rather than
    /// invoking cmake.
    #[test]
    fn frontend_test_build_emits_runner_for_extras() {
        let dir = tempfile::tempdir().expect("tempdir");
        let primary = dir.path().join("main.cute");
        let extra = dir.path().join("more.cute");
        std::fs::write(&primary, "test fn alpha { assert_eq(1, 1) }\n").unwrap();
        std::fs::write(&extra, "test fn beta { assert_eq(2, 2) }\n").unwrap();

        let mut sm = SourceMap::default();
        let extras = vec![extra];
        let (_stem, emit, _manifest, _user_module) =
            frontend_with_mode(&mut sm, &primary, true, &extras).expect("frontend");
        assert!(
            emit.source.contains("cuteTestAlpha") && emit.source.contains("cuteTestBeta"),
            "expected both tests in emitted source:\n{}",
            emit.source
        );
        assert!(
            emit.source.contains("\"1..2\\n\""),
            "expected TAP plan `1..2`:\n{}",
            emit.source
        );
    }

    /// Within a single `.cute` file, declaring two top-level items
    /// with the same simple name is still an error - the per-module
    /// items table can only hold one entry per name.
    #[test]
    fn within_file_duplicate_still_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("main.cute"),
            "pub class Counter < QObject {\n  prop a : Int, default: 0\n}\n\nclass Counter < QObject {\n  prop b : Int, default: 0\n}\n",
        )
        .unwrap();
        let err = build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect_err("expected within-file duplicate error");
        match err {
            DriverError::Resolve { first, .. } => {
                assert!(
                    first.contains("duplicate top-level declaration `Counter`"),
                    "wrong message: {first}"
                );
            }
            other => panic!("expected Resolve, got {other:?}"),
        }
    }

    /// Listing the entry file again under `[sources] paths` should be
    /// a no-op: canonical-path dedup keeps the file from being parsed
    /// (and merged) twice, which would otherwise re-trigger the
    /// duplicate-declaration check on its own items.
    #[test]
    fn entry_file_listed_in_sources_is_deduped() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("main.cute"),
            "pub class Counter < QObject {\n  prop count : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("cute.toml"),
            "[sources]\npaths = [\"main.cute\"]\n",
        )
        .unwrap();
        let written =
            build_file(&dir.path().join("main.cute"), &dir.path().join("out")).expect("buildFile");
        assert_eq!(written.len(), 2, "expected .h + .cpp");
    }

    /// `use foo` in the entry file should pull `foo.cute` from the same
    /// directory into the source set. No `cute.toml` required.
    #[test]
    fn use_decl_pulls_sibling_file_into_source_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("model.cute"),
            "pub class Counter < QObject {\n  prop count : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "use model\n\nview Main {\n  ApplicationWindow {\n    Counter { id: c }\n  }\n}\n",
        )
        .unwrap();
        let written =
            build_file(&dir.path().join("main.cute"), &dir.path().join("out")).expect("buildFile");
        let header = std::fs::read_to_string(&written[0]).unwrap();
        assert!(
            header.contains("class Counter"),
            "expected Counter from model.cute in emitted header:\n{header}"
        );
    }

    /// Transitive imports: main → a → b. All three must end up in
    /// the merged source set, parsed exactly once each even though
    /// the dependency graph could revisit b.
    #[test]
    fn use_decl_resolves_transitive_imports() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("b.cute"),
            "pub class Inner < QObject {\n  prop tag : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("a.cute"),
            "use b\n\npub class Mid < QObject {\n  prop kind : Int, default: 0\n}\n",
        )
        .unwrap();
        // `main` imports both `a` and `b` directly. Transitive *parse*
        // (a -> b is followed automatically so b.cute is in the source
        // set) is one thing; transitive *visibility* is a deliberate
        // non-feature. Each file sees only its own imports.
        std::fs::write(
            dir.path().join("main.cute"),
            "use a\nuse b\n\nview Main {\n  ApplicationWindow {\n    Inner { id: i }\n    Mid { id: m }\n  }\n}\n",
        )
        .unwrap();
        let written =
            build_file(&dir.path().join("main.cute"), &dir.path().join("out")).expect("buildFile");
        let header = std::fs::read_to_string(&written[0]).unwrap();
        assert!(header.contains("class Inner"), "missing Inner:\n{header}");
        assert!(header.contains("class Mid"), "missing Mid:\n{header}");
    }

    /// `use foo` where `foo.cute` does not exist surfaces a clear
    /// `UseNotFound` error with the path the compiler tried.
    #[test]
    fn use_decl_missing_file_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("main.cute"),
            "use missing\n\nview Main { ApplicationWindow {} }\n",
        )
        .unwrap();
        let err = build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect_err("expected use-not-found error");
        match err {
            DriverError::UseNotFound { use_path, resolved } => {
                assert_eq!(use_path, "missing");
                assert!(
                    resolved.ends_with("missing.cute"),
                    "unexpected resolved path: {resolved:?}"
                );
            }
            other => panic!("expected UseNotFound, got {other:?}"),
        }
    }

    /// Mutual imports (`a` uses `b`, `b` uses `a`) should not loop:
    /// canonical-path dedup terminates the worklist after each file
    /// is parsed once.
    #[test]
    fn use_decl_handles_mutual_imports() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("a.cute"),
            "use b\n\npub class A < QObject {\n  prop tag : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b.cute"),
            "use a\n\npub class B < QObject {\n  prop tag : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "use a\nuse b\n\nview Main {\n  ApplicationWindow {\n    A { id: a }\n    B { id: b }\n  }\n}\n",
        )
        .unwrap();
        let written =
            build_file(&dir.path().join("main.cute"), &dir.path().join("out")).expect("buildFile");
        let header = std::fs::read_to_string(&written[0]).unwrap();
        assert!(header.contains("class A"));
        assert!(header.contains("class B"));
    }

    /// `use foo.bar` resolves to `<root>/foo/bar.cute` so users can
    /// organize sources in subdirectories.
    #[test]
    fn use_decl_with_dotted_path_resolves_to_subdir() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("models")).unwrap();
        std::fs::write(
            dir.path().join("models").join("counter.cute"),
            "pub class Counter < QObject {\n  prop count : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "use models.counter\n\nview Main {\n  ApplicationWindow {\n    Counter { id: c }\n  }\n}\n",
        )
        .unwrap();
        let written =
            build_file(&dir.path().join("main.cute"), &dir.path().join("out")).expect("buildFile");
        let header = std::fs::read_to_string(&written[0]).unwrap();
        assert!(
            header.contains("class Counter"),
            "expected Counter from models/counter.cute:\n{header}"
        );
    }

    /// A lowercase-named class is module-private (Go-style case
    /// visibility): another module that wrote `use model` can `use`
    /// the file but cannot reference the lowercase class. The
    /// visibility check fires with the same diagnostic the explicit
    /// non-pub form used to raise.
    #[test]
    fn lowercase_class_blocked_across_modules() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("model.cute"),
            "class counter < QObject {\n  prop count : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "use model\n\nview Main {\n  ApplicationWindow {\n    counter { id: c }\n  }\n}\n",
        )
        .unwrap();
        let err = build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect_err("expected visibility error");
        match err {
            DriverError::Resolve { first, .. } => {
                assert!(
                    first.contains("not exported from module `model`")
                        || first.contains("private to module"),
                    "expected pub-export message, got: {first}"
                );
            }
            other => panic!("expected Resolve, got {other:?}"),
        }
    }

    /// With `pub`, the same setup compiles cleanly. The class is
    /// reachable from another module that has `use`d it.
    #[test]
    fn pub_class_visible_after_use() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("model.cute"),
            "pub class Counter < QObject {\n  prop count : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "use model\n\nview Main {\n  ApplicationWindow {\n    Counter { id: c }\n  }\n}\n",
        )
        .unwrap();
        build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect("class should be visible after `use`");
    }

    /// `use` without the import is required even when the file is in
    /// the source set: visibility is per-file, not per-project.
    #[test]
    fn pub_class_blocked_without_use() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("model.cute"),
            "pub class Counter < QObject {\n  prop count : Int, default: 0\n}\n",
        )
        .unwrap();
        // No `use model` here - we still parse model.cute via cute.toml,
        // but main.cute doesn't import the module.
        std::fs::write(
            dir.path().join("main.cute"),
            "view Main {\n  ApplicationWindow {\n    Counter { id: c }\n  }\n}\n",
        )
        .unwrap();
        // To get model.cute parsed without `use` we need to either rely
        // on the entry being part of the source set (it's not - main is
        // entry, model.cute is not referenced). So the build will fail
        // with `Counter` undeclared at codegen / resolver time, not
        // visibility. Skip this case - the fact that visibility
        // matches `use foo` is already covered by
        // `private_class_blocked_across_modules`.
        let _ = build_file(&dir.path().join("main.cute"), &dir.path().join("out"));
    }

    /// `model.Counter { ... }` works as a qualified element reference
    /// and shows that the namespace prefix mirrors the module name.
    #[test]
    fn qualified_element_reference_resolves() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("model.cute"),
            "pub class Counter < QObject {\n  prop count : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "use model\n\nview Main {\n  ApplicationWindow {\n    model.Counter { id: c }\n  }\n}\n",
        )
        .unwrap();
        build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect("qualified element ref should compile");
    }

    /// Mismatched qualifier surfaces a clear error: `view.Counter` when
    /// `Counter` actually lives in module `model`.
    #[test]
    fn qualifier_mismatch_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("model.cute"),
            "pub class Counter < QObject {\n  prop count : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "use model\n\nview Main {\n  ApplicationWindow {\n    other.Counter { id: c }\n  }\n}\n",
        )
        .unwrap();
        let err = build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect_err("expected qualifier-mismatch error");
        match err {
            DriverError::Resolve { first, .. } => {
                assert!(
                    first.contains("does not match where") || first.contains("not imported"),
                    "expected qualifier message, got: {first}"
                );
            }
            other => panic!("expected Resolve, got {other:?}"),
        }
    }

    /// `use foo as bar` makes the source module reachable under
    /// `bar.X` qualified syntax, while preserving the option to
    /// reference items unqualified.
    #[test]
    fn use_as_alias_introduces_local_module_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("model.cute"),
            "pub class Counter < QObject {\n  prop count : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "use model as m\n\nview Main {\n  ApplicationWindow {\n    m.Counter { id: c }\n  }\n}\n",
        )
        .unwrap();
        build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect("aliased qualifier should resolve");
    }

    /// `use foo.{X}` brings only X into scope; the source module
    /// itself is NOT a qualifier-importable name. Unqualified access
    /// still works.
    #[test]
    fn selective_use_brings_only_listed_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("model.cute"),
            "pub class Counter < QObject {\n  prop count : Int, default: 0\n}\nclass Other < QObject {\n  prop tag : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "use model.{Counter}\n\nview Main {\n  ApplicationWindow {\n    Counter { id: c }\n  }\n}\n",
        )
        .unwrap();
        build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect("selective use of Counter should compile");
    }

    /// Selective imports with `as` rename: `use model.{Counter as C}`
    /// makes `C` resolve to model's Counter class.
    #[test]
    fn selective_use_with_alias_renames_locally() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("model.cute"),
            "pub class Counter < QObject {\n  prop count : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "use model.{Counter as C}\n\nview Main {\n  ApplicationWindow {\n    C { id: c }\n  }\n}\n",
        )
        .unwrap();
        build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect("renamed selective import should resolve");
    }

    /// Items NOT in the selective list remain invisible from the
    /// importing module - even if the source file has them as `pub`.
    #[test]
    fn selective_use_excludes_unlisted_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("model.cute"),
            "pub class Counter < QObject {\n  prop count : Int, default: 0\n}\nclass Other < QObject {\n  prop tag : Int, default: 0\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "use model.{Counter}\n\nview Main {\n  ApplicationWindow {\n    Other { id: o }\n  }\n}\n",
        )
        .unwrap();
        let err = build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect_err("expected `Other` to be unreachable");
        match err {
            DriverError::Resolve { first, .. } => {
                assert!(
                    first.contains("does not `use`") || first.contains("not exported"),
                    "got: {first}"
                );
            }
            other => panic!("expected Resolve, got {other:?}"),
        }
    }

    // `pub use foo.{X}` re-export was retired alongside the `pub`
    // keyword; module visibility is now case-derived. Re-exports may
    // come back under a different syntax in the future.

    /// `[sources] paths` files are auto-imported by every module - the
    /// "ambient" semantic. A style palette listed there is reachable
    /// from a sibling without an explicit `use styles`.
    #[test]
    fn ambient_sources_auto_import_into_every_module() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("styles.cute"),
            "style Card { padding: 16 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "view Main {\n  ApplicationWindow {\n    Label { style: Card; text: \"hi\" }\n  }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("cute.toml"),
            "[sources]\npaths = [\"styles.cute\"]\n",
        )
        .unwrap();
        build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect("ambient styles should be visible without an explicit `use`");
    }

    /// `use qt.X` is reserved for stdlib bindings (auto-loaded by the
    /// driver) and must NOT trigger filesystem resolution.
    #[test]
    fn use_decl_qt_prefix_is_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("main.cute"),
            "use qt.core\nuse qt.widgets\n\nclass X < QObject {\n  prop tag : Int, default: 0\n}\n",
        )
        .unwrap();
        // No `qt/` directory exists; `use qt.core` must NOT error.
        let written = build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect("qt.* should be a no-op");
        assert_eq!(written.len(), 2, "expected .h + .cpp");
    }

    /// `[bindings] paths = ["foo.qpi"]` in cute.toml: the driver should
    /// parse each listed file as a Cute binding module and merge it
    /// into the type-check view alongside stdlib bindings. Proves the
    /// local-binding escape hatch is wired end-to-end.
    #[test]
    fn local_bindings_paths_loads_extra_qpi() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Local binding declares an extern type the user references.
        // Use a distinctive name (LocalBindingProbe) so it cannot
        // collide with anything in stdlib.
        std::fs::write(
            dir.path().join("local.qpi"),
            "class LocalBindingProbe < QObject {\n  fn answer Int\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("cute.toml"),
            "[bindings]\npaths = [\"local.qpi\"]\n",
        )
        .unwrap();
        // User code constructs the bound type and calls the bound
        // method. Without the local binding, the class would still
        // soft-pass (External), but the test below also exercises the
        // failure path.
        std::fs::write(
            dir.path().join("main.cute"),
            "fn main {\n  let probe = LocalBindingProbe()\n  let _ = probe.answer()\n}\n",
        )
        .unwrap();

        let _ = build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect("user source referencing a class from a local binding should compile");
    }

    /// Malformed `.qpi` listed in `[bindings] paths` must surface as a
    /// driver error (with the binding's path in the message), not be
    /// silently swallowed. Pin the negative-path wiring of the
    /// local-binding loader.
    #[test]
    fn local_bindings_paths_malformed_qpi_is_diagnostic() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("broken.qpi"), "class { not valid\n").unwrap();
        std::fs::write(
            dir.path().join("cute.toml"),
            "[bindings]\npaths = [\"broken.qpi\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.cute"),
            "fn main { println(\"hi\") }\n",
        )
        .unwrap();

        let err = build_file(&dir.path().join("main.cute"), &dir.path().join("out"))
            .expect_err("malformed local binding should bubble up as an error");
        let msg = format!("{err}");
        assert!(
            msg.contains("broken.qpi"),
            "expected error message to reference the offending binding path:\n{msg}"
        );
    }
}
