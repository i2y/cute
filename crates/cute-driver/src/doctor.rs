//! `cute doctor` — preflight diagnostic for the toolchain dependencies
//! a `cute build` would look for.
//!
//! Pipeline:
//!   1. Resolve the project entry (same logic the build path uses).
//!   2. If `cute.toml` declares `[library]`, exit early with a v1.1
//!      "library mode is deferred" notice — app-mode-only for v1.
//!   3. Otherwise run the frontend (parse + resolve + typecheck +
//!      codegen) and `detect_mode` the result, mirroring `cute build`.
//!   4. Compute [`crate::RequiredDeps`] from `(mode, manifest)` — a
//!      pure function, no fs / cmake side effects.
//!   5. Probe the host for each dependency: Qt6 prefix + version, KF6
//!      via Craft, CuteUI runtime via the install cache, [cute_libraries]
//!      deps via the user cache.
//!   6. For anything missing, look up the platform-specific install
//!      command via [`packages`] and emit a copy-pasteable line.
//!
//! Doctor never invokes cmake. Presence checks read CMake-config files
//! directly; version probing reads `Qt6CoreConfigVersion.cmake`. Both
//! are O(file-stat) so the command stays under a second even for
//! CuteUI projects with several deps.

pub mod packages;

use std::fmt;
use std::path::{Path, PathBuf};

use crate::{
    BuildMode, DriverError, Manifest, RequiredDeps, detect_mode_and_manifest, find_craft_prefix,
    find_cute_library_prefix, find_cute_ui_prefix, find_qt_prefix, has_cmake_config,
    read_qt_version, required_deps_for,
};

use packages::{
    DistroId, QtComponentKind, detect_distro, kf6_packages_for,
    parse_kf6_components_from_find_package, qt_component_kind, qt_packages_for,
};

/// What `cute doctor` produces. Carries enough structure for both
/// the `Display` impl (CLI output) and `is_healthy` (exit code).
#[derive(Debug)]
pub struct DoctorReport {
    pub project_path: PathBuf,
    pub library_mode_skipped: bool,
    pub distro: DistroId,
    /// `None` when `library_mode_skipped` is true (we don't compute deps
    /// for library projects in v1).
    pub mode_and_deps: Option<ModeAndDeps>,
    pub qt: Option<QtToolchainStatus>,
    pub kf6: Option<Kf6ToolchainStatus>,
    pub cute_ui: Option<CuteUiStatus>,
    pub cute_libraries: Vec<CuteLibraryStatus>,
}

#[derive(Debug)]
pub struct ModeAndDeps {
    pub mode: BuildMode,
    pub deps: RequiredDeps,
}

#[derive(Debug)]
pub struct QtToolchainStatus {
    pub prefix: Option<String>,
    pub version: Option<String>,
    /// `Some(min)` when the build mode pins a min Qt version (CuteUi → "6.11");
    /// doctor uses this to flag too-old installs.
    pub required_min_version: Option<&'static str>,
    pub modules: Vec<QtModuleStatus>,
}

#[derive(Debug)]
pub struct QtModuleStatus {
    pub component: String,
    pub kind: QtComponentKind,
    pub present: bool,
}

#[derive(Debug)]
pub struct Kf6ToolchainStatus {
    pub prefix: Option<String>,
    pub modules: Vec<Kf6ModuleStatus>,
}

#[derive(Debug)]
pub struct Kf6ModuleStatus {
    pub component: String,
    pub present: bool,
}

#[derive(Debug)]
pub struct CuteUiStatus {
    pub prefix: Option<String>,
    pub qt_min: &'static str,
}

#[derive(Debug)]
pub struct CuteLibraryStatus {
    pub name: String,
    pub prefix: Option<PathBuf>,
}

impl DoctorReport {
    /// True when every required dependency is present at a usable
    /// version. Drives the doctor exit code.
    pub fn is_healthy(&self) -> bool {
        if self.library_mode_skipped {
            return true;
        }
        let qt_ok = self.qt.as_ref().is_some_and(|q| qt_status_is_ok(q));
        let kf6_ok = self.kf6.as_ref().is_none_or(kf6_status_is_ok);
        let cute_ui_ok = self.cute_ui.as_ref().is_none_or(|s| s.prefix.is_some());
        let libs_ok = self.cute_libraries.iter().all(|l| l.prefix.is_some());
        qt_ok && kf6_ok && cute_ui_ok && libs_ok
    }
}

fn qt_status_is_ok(q: &QtToolchainStatus) -> bool {
    if q.prefix.is_none() {
        return false;
    }
    if let (Some(req), Some(have)) = (q.required_min_version, q.version.as_deref()) {
        if !version_at_least(have, req) {
            return false;
        }
    }
    q.modules.iter().all(qt_module_is_ok)
}

fn qt_module_is_ok(m: &QtModuleStatus) -> bool {
    match m.kind {
        // Private umbrellas ride along with their parent — we don't
        // judge them individually.
        QtComponentKind::PrivateUmbrella { .. } => true,
        // Tech-Preview modules are best-effort: present means we found
        // the cmake config; missing surfaces a build-from-source note,
        // not a failure (since no package manager carries them yet).
        QtComponentKind::TechPreview => m.present,
        QtComponentKind::Standard => m.present,
    }
}

fn kf6_status_is_ok(k: &Kf6ToolchainStatus) -> bool {
    k.prefix.is_some() && k.modules.iter().all(|m| m.present)
}

/// Compare two dotted version strings. Returns true when `have >= req`.
/// Tolerates differing component counts (`6.11` vs `6.11.0`) by treating
/// missing components as zero.
fn version_at_least(have: &str, req: &str) -> bool {
    let h: Vec<u32> = have.split('.').filter_map(|p| p.parse().ok()).collect();
    let r: Vec<u32> = req.split('.').filter_map(|p| p.parse().ok()).collect();
    let n = h.len().max(r.len());
    for i in 0..n {
        let hi = h.get(i).copied().unwrap_or(0);
        let ri = r.get(i).copied().unwrap_or(0);
        if hi != ri {
            return hi > ri;
        }
    }
    true
}

/// Run the doctor pipeline and produce a report. `file` is the entry
/// `.cute` source; `None` means auto-detect via the same rules
/// `cute build` uses (cwd basename, `main.cute`, single `.cute` in cwd).
pub fn run(file: Option<&Path>) -> Result<DoctorReport, DriverError> {
    let entry = match file {
        Some(p) => p.to_path_buf(),
        None => resolve_default_entry()?,
    };

    // Library-mode bail-out (v1 scope). We can detect this from the
    // manifest alone — no need to run codegen. Saves time and keeps
    // the v1.1 deferred message clean.
    if let Some((m, _dir)) = Manifest::try_load(&entry)? {
        if m.library.is_some() {
            return Ok(DoctorReport {
                project_path: entry,
                library_mode_skipped: true,
                distro: detect_distro(),
                mode_and_deps: None,
                qt: None,
                kf6: None,
                cute_ui: None,
                cute_libraries: Vec::new(),
            });
        }
    }

    let (mode, manifest) = detect_mode_and_manifest(&entry)?;
    let deps = required_deps_for(mode, &manifest);

    let qt = Some(collect_qt_status(&deps));
    let kf6 = if deps.uses_kf6 {
        Some(collect_kf6_status(&deps))
    } else {
        None
    };
    let cute_ui = if deps.needs_cute_ui {
        Some(CuteUiStatus {
            prefix: find_cute_ui_prefix(),
            qt_min: deps.qt6_min_version.unwrap_or("6.11"),
        })
    } else {
        None
    };
    let cute_libraries = deps
        .cute_libraries
        .iter()
        .map(|name| CuteLibraryStatus {
            name: name.clone(),
            prefix: find_cute_library_prefix(name),
        })
        .collect();

    Ok(DoctorReport {
        project_path: entry,
        library_mode_skipped: false,
        distro: detect_distro(),
        mode_and_deps: Some(ModeAndDeps { mode, deps }),
        qt,
        kf6,
        cute_ui,
        cute_libraries,
    })
}

fn collect_qt_status(deps: &RequiredDeps) -> QtToolchainStatus {
    let prefix = find_qt_prefix();
    let prefix_path = prefix.as_deref().map(Path::new);
    let version = prefix_path.and_then(read_qt_version);
    let modules = deps
        .qt6_components
        .iter()
        .map(|c| {
            let kind = qt_component_kind(c);
            // PrivateUmbrella and other folded variants don't have
            // their own `Qt6<C>Config.cmake`, so we treat the parent's
            // presence as the answer. Standard + TechPreview hit the
            // filesystem.
            let present = match (kind, prefix_path) {
                (_, None) => false,
                (QtComponentKind::PrivateUmbrella { parent }, Some(p)) => {
                    has_cmake_config(p, &format!("Qt6{parent}"))
                }
                (_, Some(p)) => has_cmake_config(p, &format!("Qt6{c}")),
            };
            QtModuleStatus {
                component: c.clone(),
                kind,
                present,
            }
        })
        .collect();
    QtToolchainStatus {
        prefix,
        version,
        required_min_version: deps.qt6_min_version,
        modules,
    }
}

fn collect_kf6_status(deps: &RequiredDeps) -> Kf6ToolchainStatus {
    let prefix = find_craft_prefix();
    // Pull the KF6 components the manifest names. Anything we can't
    // parse falls through to the prefix-presence question alone.
    let mut components: Vec<String> = deps
        .manifest_find_packages
        .iter()
        .flat_map(|s| parse_kf6_components_from_find_package(s))
        .collect();
    components.sort();
    components.dedup();
    let modules = components
        .into_iter()
        .map(|c| {
            let present = match prefix.as_deref() {
                Some(p) => has_cmake_config(Path::new(p), &format!("KF6{c}")),
                None => false,
            };
            Kf6ModuleStatus {
                component: c,
                present,
            }
        })
        .collect();
    Kf6ToolchainStatus { prefix, modules }
}

/// Same fallback rules `cute build` uses when called without an explicit
/// file: prefer `<cwd-basename>.cute`, then `main.cute`, then the lone
/// `.cute` file in cwd. Doctor reuses these so a no-arg run analyses
/// what `cute build` would.
fn resolve_default_entry() -> Result<PathBuf, DriverError> {
    let cwd = std::env::current_dir()?;
    if let Some(base) = cwd.file_name().and_then(|s| s.to_str()) {
        let candidate = cwd.join(format!("{base}.cute"));
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    let main = cwd.join("main.cute");
    if main.exists() {
        return Ok(main);
    }
    let cutes: Vec<PathBuf> = std::fs::read_dir(&cwd)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("cute"))
        .collect();
    if cutes.len() == 1 {
        return Ok(cutes.into_iter().next().unwrap());
    }
    Err(DriverError::Manifest(format!(
        "no entry `.cute` file found in {}; expected `<dir>.cute`, `main.cute`, or a single `.cute` file (or pass an explicit path: `cute doctor <file>`)",
        cwd.display()
    )))
}

// ---- Display impl ---------------------------------------------------------

impl fmt::Display for DoctorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "==> Cute doctor")?;
        writeln!(f)?;

        if self.library_mode_skipped {
            writeln!(
                f,
                "Project: {} ([library] mode)",
                self.project_path.display()
            )?;
            writeln!(f)?;
            writeln!(
                f,
                "Library-mode doctoring is deferred to v1.1. Run `cute build`"
            )?;
            writeln!(f, "directly to surface any missing dependencies for now.")?;
            return Ok(());
        }

        if let Some(md) = &self.mode_and_deps {
            writeln!(
                f,
                "Project: {} (BuildMode::{:?})",
                self.project_path.display(),
                md.mode
            )?;
            writeln!(f)?;
        }

        // Qt toolchain
        if let Some(q) = &self.qt {
            render_qt_section(f, q)?;
        }

        // KF6 (only when the manifest pulls it in).
        if let Some(k) = &self.kf6 {
            render_kf6_section(f, k, self.distro)?;
        }

        // CuteUI runtime.
        if let Some(c) = &self.cute_ui {
            render_cute_ui_section(f, c)?;
        }

        // Cute libraries.
        if !self.cute_libraries.is_empty() {
            render_cute_libraries_section(f, &self.cute_libraries)?;
        }

        writeln!(f, "Detected distro: {}", self.distro.label())?;
        writeln!(f)?;

        if self.is_healthy() {
            writeln!(f, "All dependencies present. `cute build` should succeed.")?;
        } else {
            render_install_block(f, self)?;
        }

        Ok(())
    }
}

fn render_qt_section(f: &mut fmt::Formatter<'_>, q: &QtToolchainStatus) -> fmt::Result {
    writeln!(f, "Qt 6 toolchain")?;
    match (&q.prefix, &q.version) {
        (Some(p), Some(v)) => {
            // Version-sufficiency check: warn when CuteUi pins 6.11+ but
            // the install is older. Doctor still itemizes modules in case
            // a partial upgrade is possible.
            match q.required_min_version {
                Some(req) if !version_at_least(v, req) => {
                    writeln!(f, "  ! Found Qt {v} at {p} — required: {req} or newer.")?;
                }
                _ => writeln!(f, "  + Found at {p} ({v})")?,
            }
        }
        (Some(p), None) => writeln!(f, "  + Found at {p} (version unknown)")?,
        (None, _) => writeln!(f, "  - Qt 6 not found on this system.")?,
    }
    writeln!(f)?;

    writeln!(f, "Required Qt 6 modules ({})", q.modules.len())?;
    for m in &q.modules {
        match m.kind {
            QtComponentKind::PrivateUmbrella { parent } => {
                writeln!(f, "  + Qt6::{} (ships with Qt6::{})", m.component, parent)?
            }
            QtComponentKind::TechPreview => {
                if m.present {
                    writeln!(f, "  + Qt6::{} (Tech Preview)", m.component)?;
                } else {
                    writeln!(
                        f,
                        "  ! Qt6::{} (Tech Preview) — not yet packaged by mainstream distros.",
                        m.component
                    )?;
                    writeln!(
                        f,
                        "      Build Qt 6.11+ from source with `-feature-qtcanvaspainter`,"
                    )?;
                    writeln!(f, "      or wait for upstream packaging.")?;
                }
            }
            QtComponentKind::Standard => {
                if m.present {
                    writeln!(f, "  + Qt6::{}", m.component)?;
                } else {
                    writeln!(f, "  - Qt6::{} — missing", m.component)?;
                }
            }
        }
    }
    writeln!(f)?;
    Ok(())
}

fn render_kf6_section(
    f: &mut fmt::Formatter<'_>,
    k: &Kf6ToolchainStatus,
    distro: DistroId,
) -> fmt::Result {
    writeln!(f, "KF 6 / Kirigami toolchain")?;
    match &k.prefix {
        Some(p) => writeln!(f, "  + Found at {p}")?,
        None => match distro {
            DistroId::MacOS => {
                writeln!(f, "  - KF 6 not found at $CRAFT_ROOT or ~/CraftRoot.")?;
                writeln!(
                    f,
                    "    Install via KDE Craft (https://community.kde.org/Craft):"
                )?;
                writeln!(
                    f,
                    "      python3 -m venv ~/CraftRoot && source ~/CraftRoot/bin/activate"
                )?;
                writeln!(
                    f,
                    "      curl -O https://invent.kde.org/packaging/craft/-/raw/master/setup/CraftBootstrap.py"
                )?;
                writeln!(f, "      python3 CraftBootstrap.py")?;
                writeln!(f, "      craft kirigami")?;
            }
            _ => writeln!(f, "  - KF 6 not found.")?,
        },
    }
    if !k.modules.is_empty() {
        for m in &k.modules {
            if m.present {
                writeln!(f, "    + KF6{}", m.component)?;
            } else {
                writeln!(f, "    - KF6{} — missing", m.component)?;
            }
        }
    }
    writeln!(f)?;
    Ok(())
}

fn render_cute_ui_section(f: &mut fmt::Formatter<'_>, c: &CuteUiStatus) -> fmt::Result {
    writeln!(f, "CuteUI runtime")?;
    match &c.prefix {
        Some(p) => writeln!(f, "  + Installed at {p}")?,
        None => {
            writeln!(f, "  - CuteUI runtime not installed.")?;
            writeln!(f, "    Install with: cute install-cute-ui")?;
            writeln!(
                f,
                "    (Requires Qt {} or newer for QtCanvasPainter.)",
                c.qt_min
            )?;
        }
    }
    writeln!(f)?;
    Ok(())
}

fn render_cute_libraries_section(
    f: &mut fmt::Formatter<'_>,
    libs: &[CuteLibraryStatus],
) -> fmt::Result {
    writeln!(f, "Cute libraries ({})", libs.len())?;
    for l in libs {
        match &l.prefix {
            Some(p) => writeln!(f, "  + {} at {}", l.name, p.display())?,
            None => writeln!(
                f,
                "  - {} — missing. Install with: cute install <path-or-git-url>",
                l.name
            )?,
        }
    }
    writeln!(f)?;
    Ok(())
}

fn render_install_block(f: &mut fmt::Formatter<'_>, r: &DoctorReport) -> fmt::Result {
    writeln!(f, "To install missing dependencies, run:")?;
    writeln!(f)?;

    let lines = build_install_command_lines(r);
    if lines.is_empty() {
        writeln!(
            f,
            "    (no automatic install command available — see notes above)"
        )?;
    } else {
        for line in lines {
            writeln!(f, "    {line}")?;
        }
    }
    writeln!(f)?;
    writeln!(f, "Then run `cute build` again.")?;
    Ok(())
}

/// Aggregate the missing-package commands into a small set of
/// copy-pasteable shell lines. macOS collapses Qt6 components onto a
/// single `brew install qt`. Linux distros emit one
/// `sudo <pm> install <pkg> <pkg>...` line. CuteUI / Cute libraries get
/// their own `cute install-cute-ui` / `cute install` lines because they
/// don't go through the OS package manager.
fn build_install_command_lines(r: &DoctorReport) -> Vec<String> {
    let mut lines = Vec::new();
    let distro = r.distro;

    // Collect Qt6 packages.
    let mut qt_pkgs: Vec<&'static str> = Vec::new();
    if let Some(q) = &r.qt {
        if q.prefix.is_none() {
            // Whole Qt6 missing — name every required component (the
            // table will collapse to `qt` on macOS, individual
            // qt6-*-dev packages on Linux).
            for m in &q.modules {
                if let QtComponentKind::Standard = m.kind {
                    if let Some(pkgs) = qt_packages_for(&m.component, distro) {
                        qt_pkgs.extend(pkgs.iter().copied());
                    }
                }
            }
        } else {
            // Some modules missing — only those.
            for m in &q.modules {
                if !m.present
                    && matches!(m.kind, QtComponentKind::Standard)
                    && let Some(pkgs) = qt_packages_for(&m.component, distro)
                {
                    qt_pkgs.extend(pkgs.iter().copied());
                }
            }
            // Version too old: still flag, but doctor's main job is
            // package lookup; for upgrade we'd add `brew upgrade qt`
            // / equivalent here. macOS only has `brew upgrade qt` as
            // a clean line; Linux distros vary too much to suggest a
            // shape. Keep this minimal for v1.
            if let (Some(req), Some(have)) = (q.required_min_version, q.version.as_deref()) {
                if !version_at_least(have, req) && distro == DistroId::MacOS {
                    lines.push("brew upgrade qt".to_string());
                }
            }
        }
    }
    qt_pkgs.sort();
    qt_pkgs.dedup();

    if !qt_pkgs.is_empty() {
        match distro {
            DistroId::MacOS => {
                lines.push(format!(
                    "{}{}",
                    distro.brew_install_command_prefix(),
                    qt_pkgs.join(" ")
                ));
            }
            DistroId::UnknownLinux | DistroId::Other => {
                lines.push("# Install Qt 6 development packages from your distro:".to_string());
                lines.push(format!("#   needed: {}", qt_pkgs.join(" ")));
            }
            _ => {
                lines.push(format!(
                    "{}{}",
                    distro.install_command_prefix(),
                    qt_pkgs.join(" ")
                ));
            }
        }
    }

    // KF6 packages.
    if let Some(k) = &r.kf6 {
        let mut kf6_pkgs: Vec<&'static str> = Vec::new();
        if k.prefix.is_none() {
            // No prefix at all. On macOS we already advised Craft
            // bootstrap above; for Linux we can suggest packages
            // when we know any.
            for m in &k.modules {
                if let Some(pkgs) = kf6_packages_for(&m.component, distro) {
                    kf6_pkgs.extend(pkgs.iter().copied());
                }
            }
            if kf6_pkgs.is_empty() && k.modules.is_empty() {
                // We only know "manifest references KF6 / Kirigami"
                // but no specific component — try the canonical Kirigami pkg.
                if let Some(pkgs) = kf6_packages_for("Kirigami", distro) {
                    kf6_pkgs.extend(pkgs.iter().copied());
                }
            }
        } else {
            for m in &k.modules {
                if !m.present
                    && let Some(pkgs) = kf6_packages_for(&m.component, distro)
                {
                    kf6_pkgs.extend(pkgs.iter().copied());
                }
            }
        }
        kf6_pkgs.sort();
        kf6_pkgs.dedup();

        if !kf6_pkgs.is_empty() {
            match distro {
                DistroId::MacOS | DistroId::UnknownLinux | DistroId::Other => {
                    // Already handled in the section's prose; skip
                    // duplicating here.
                }
                _ => {
                    lines.push(format!(
                        "{}{}",
                        distro.install_command_prefix(),
                        kf6_pkgs.join(" ")
                    ));
                }
            }
        }
    }

    // CuteUI runtime.
    if let Some(c) = &r.cute_ui {
        if c.prefix.is_none() {
            lines.push("cute install-cute-ui".to_string());
        }
    }

    // Cute libraries — emit one line per missing dep, since they may
    // come from different sources (path / git url).
    for l in &r.cute_libraries {
        if l.prefix.is_none() {
            lines.push(format!("cute install <path-or-git-url-for-{}>", l.name));
        }
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_at_least_basic() {
        assert!(version_at_least("6.11.0", "6.11"));
        assert!(version_at_least("6.11.0", "6.11.0"));
        assert!(version_at_least("6.11.1", "6.11.0"));
        assert!(version_at_least("6.12.0", "6.11"));
        assert!(!version_at_least("6.10.2", "6.11"));
        assert!(!version_at_least("6.10.0", "6.11.0"));
    }

    #[test]
    fn version_at_least_handles_short_components() {
        // "6.11" should compare equal to "6.11.0" for the major+minor
        // fields, treating missing patch as zero.
        assert!(version_at_least("6.11", "6.11.0"));
        assert!(version_at_least("6.11.0", "6.11"));
    }

    #[test]
    fn parse_qt_version_cmake_extracts_dotted() {
        let s = r#"
# CMake-format file
set(PACKAGE_VERSION "6.11.0")
"#;
        assert_eq!(crate::parse_qt_version_cmake(s), Some("6.11.0".to_string()));
    }

    #[test]
    fn parse_qt_version_cmake_returns_none_on_missing() {
        assert_eq!(crate::parse_qt_version_cmake("no version here"), None);
    }
}
