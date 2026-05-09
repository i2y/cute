//! Per-distro package-name table + OS detection for `cute doctor`.
//!
//! Two tables, one for Qt6 components (`QT_PACKAGES`) and one for KF6
//! components (`KF6_PACKAGES`). Each row maps a component name (without
//! the `Qt6::` / `KF6` prefix) to per-distro install commands.
//!
//! macOS uses Homebrew's umbrella `qt` formula for Qt — every Qt6
//! component collapses to the same package, so doctor de-duplicates the
//! suggested command on mac. KF6 has no Homebrew presence; on macOS the
//! suggestion is the KDE Craft bootstrap.
//!
//! Detection of the host distro happens at most once per process via a
//! `OnceLock`. On non-Linux Unix we don't bother — `MacOS` and the
//! catch-all `Other` cover what we need today.

use std::sync::OnceLock;

/// Coarse classification of the host system. Used to look up package
/// names in the per-component tables; finer-grained distinctions
/// (e.g. Ubuntu LTS vs sid) are out of scope — doctor suggests, the
/// user's package manager has the final say on availability.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DistroId {
    /// macOS — paired with Homebrew suggestions for Qt6, Craft for KF6.
    MacOS,
    /// openSUSE Tumbleweed (or any distro reporting `ID=opensuse-*`).
    OpenSuse,
    /// Fedora rawhide or stable. RHEL / CentOS Stream collapse here too
    /// when their `ID_LIKE` carries `fedora`.
    Fedora,
    /// Debian sid / stable.
    Debian,
    /// Ubuntu (any release). Distinct from Debian because Ubuntu's
    /// LTS-vs-sid package availability differs and we want to surface
    /// `apt` over `dpkg` regardless.
    Ubuntu,
    /// Arch Linux + derivatives reporting `ID_LIKE=arch`.
    Arch,
    /// Linux but `os-release` didn't match any known distro. Doctor
    /// prints a generic "install Qt 6 dev packages from your repos"
    /// message instead of guessing wrong package names.
    UnknownLinux,
    /// Non-mac, non-Linux (Windows, BSD, etc.). Doctor falls back to
    /// the upstream Qt online installer URL.
    Other,
}

impl DistroId {
    /// Human-readable label for the distro name in doctor output.
    pub fn label(self) -> &'static str {
        match self {
            DistroId::MacOS => "macOS (Homebrew)",
            DistroId::OpenSuse => "openSUSE (zypper)",
            DistroId::Fedora => "Fedora (dnf)",
            DistroId::Debian => "Debian (apt)",
            DistroId::Ubuntu => "Ubuntu (apt)",
            DistroId::Arch => "Arch Linux (pacman)",
            DistroId::UnknownLinux => "Linux (unknown distribution)",
            DistroId::Other => "unknown OS",
        }
    }

    /// Prefix added in front of an aggregated install command. `sudo`
    /// for distros whose package manager needs it; empty for Homebrew
    /// (per-user) and the unknown / fallback cases.
    pub fn install_command_prefix(self) -> &'static str {
        match self {
            DistroId::MacOS => "",
            DistroId::OpenSuse => "sudo zypper install ",
            DistroId::Fedora => "sudo dnf install ",
            DistroId::Debian | DistroId::Ubuntu => "sudo apt install ",
            DistroId::Arch => "sudo pacman -S ",
            DistroId::UnknownLinux | DistroId::Other => "",
        }
    }

    /// Brew uses `brew install` rather than a sudo pkg manager. Returned
    /// separately so doctor can prefix the package-name list correctly.
    pub fn brew_install_command_prefix(self) -> &'static str {
        match self {
            DistroId::MacOS => "brew install ",
            _ => "",
        }
    }
}

/// Detect the host distro. Cached after the first call so doctor can
/// look the value up freely without re-parsing `/etc/os-release`.
pub fn detect_distro() -> DistroId {
    static CACHED: OnceLock<DistroId> = OnceLock::new();
    *CACHED.get_or_init(detect_distro_uncached)
}

fn detect_distro_uncached() -> DistroId {
    if cfg!(target_os = "macos") {
        return DistroId::MacOS;
    }
    if cfg!(target_os = "linux") {
        if let Ok(text) = std::fs::read_to_string("/etc/os-release") {
            return parse_os_release(&text);
        }
        return DistroId::UnknownLinux;
    }
    DistroId::Other
}

/// Parse the `ID=` and `ID_LIKE=` lines of `/etc/os-release` into a
/// `DistroId`. Quoting is optional (ID=arch, ID="arch", ID='arch' all
/// appear in the wild). Unknown values fall through to `UnknownLinux`.
pub fn parse_os_release(text: &str) -> DistroId {
    let mut id: Option<String> = None;
    let mut id_like: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("ID=") {
            id = Some(strip_quotes(rest).to_string());
        } else if let Some(rest) = line.strip_prefix("ID_LIKE=") {
            id_like = Some(strip_quotes(rest).to_string());
        }
    }
    let id_str = id.as_deref().unwrap_or("");
    let id_like_str = id_like.as_deref().unwrap_or("");
    classify(id_str, id_like_str)
}

fn classify(id: &str, id_like: &str) -> DistroId {
    let tokens = || id_like.split_whitespace().chain(std::iter::once(id));
    if id == "ubuntu" || tokens().any(|t| t == "ubuntu") {
        return DistroId::Ubuntu;
    }
    if id == "debian" || tokens().any(|t| t == "debian") {
        return DistroId::Debian;
    }
    if id.starts_with("opensuse") || tokens().any(|t| t.starts_with("opensuse") || t == "suse") {
        return DistroId::OpenSuse;
    }
    if id == "fedora" || tokens().any(|t| t == "fedora" || t == "rhel" || t == "centos") {
        return DistroId::Fedora;
    }
    if id == "arch" || tokens().any(|t| t == "arch") {
        return DistroId::Arch;
    }
    DistroId::UnknownLinux
}

fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix('"').unwrap_or(s);
    let s = s.strip_suffix('"').unwrap_or(s);
    let s = s.strip_prefix('\'').unwrap_or(s);
    let s = s.strip_suffix('\'').unwrap_or(s);
    s
}

/// Per-component package lookup table. The list-shape (rather than a
/// HashMap) is intentional — adding a new component / distro means
/// editing one row, no allocation, and the table doubles as
/// documentation when reading the file top-to-bottom.
type PackageRow = (&'static str, &'static [(DistroId, &'static [&'static str])]);

/// Macro to keep each Qt6 component's row readable.
macro_rules! row {
    ($comp:literal, $($d:expr => $p:expr),+ $(,)?) => {
        ($comp, &[$( ($d, &$p as &[&str]) ),+] as &[(DistroId, &[&str])])
    };
}

/// Qt6 component → per-distro package-name table.
///
/// **macOS**: every component collapses to `qt` (Homebrew umbrella
/// formula installs all Qt6 modules in one go). Doctor de-dupes so
/// the suggested command stays a single `brew install qt` line even
/// when multiple Qt6 components are missing.
///
/// **GuiPrivate**: ships inside `Qt6Gui`'s package — no separate
/// install command exists. Marked specially via `qt_component_kind`
/// below so doctor folds it under `Gui` rather than reporting a
/// missing package.
///
/// **CanvasPainter**: Tech Preview module shipping with Qt 6.11+.
/// Mainstream package managers don't carry it as of this writing —
/// doctor uses the [`QtComponentKind::TechPreview`] branch to point at
/// upstream Qt source build instead of suggesting a non-existent pkg.
pub const QT_PACKAGES: &[PackageRow] = &[
    row!(
        "Core",
        DistroId::MacOS => ["qt"],
        DistroId::OpenSuse => ["qt6-base-devel"],
        DistroId::Fedora => ["qt6-qtbase-devel"],
        DistroId::Debian => ["qt6-base-dev"],
        DistroId::Ubuntu => ["qt6-base-dev"],
        DistroId::Arch => ["qt6-base"],
    ),
    row!(
        "Gui",
        DistroId::MacOS => ["qt"],
        DistroId::OpenSuse => ["qt6-base-devel"],
        DistroId::Fedora => ["qt6-qtbase-devel"],
        DistroId::Debian => ["qt6-base-dev"],
        DistroId::Ubuntu => ["qt6-base-dev"],
        DistroId::Arch => ["qt6-base"],
    ),
    row!(
        "Widgets",
        DistroId::MacOS => ["qt"],
        DistroId::OpenSuse => ["qt6-base-devel"],
        DistroId::Fedora => ["qt6-qtbase-devel"],
        DistroId::Debian => ["qt6-base-dev"],
        DistroId::Ubuntu => ["qt6-base-dev"],
        DistroId::Arch => ["qt6-base"],
    ),
    row!(
        "Network",
        DistroId::MacOS => ["qt"],
        DistroId::OpenSuse => ["qt6-base-devel"],
        DistroId::Fedora => ["qt6-qtbase-devel"],
        DistroId::Debian => ["qt6-base-dev"],
        DistroId::Ubuntu => ["qt6-base-dev"],
        DistroId::Arch => ["qt6-base"],
    ),
    row!(
        "Qml",
        DistroId::MacOS => ["qt"],
        DistroId::OpenSuse => ["qt6-declarative-devel"],
        DistroId::Fedora => ["qt6-qtdeclarative-devel"],
        DistroId::Debian => ["qt6-declarative-dev"],
        DistroId::Ubuntu => ["qt6-declarative-dev"],
        DistroId::Arch => ["qt6-declarative"],
    ),
    row!(
        "Quick",
        DistroId::MacOS => ["qt"],
        DistroId::OpenSuse => ["qt6-declarative-devel"],
        DistroId::Fedora => ["qt6-qtdeclarative-devel"],
        DistroId::Debian => ["qt6-declarative-dev"],
        DistroId::Ubuntu => ["qt6-declarative-dev"],
        DistroId::Arch => ["qt6-declarative"],
    ),
    row!(
        "QuickControls2",
        DistroId::MacOS => ["qt"],
        DistroId::OpenSuse => ["qt6-declarative-devel"],
        DistroId::Fedora => ["qt6-qtdeclarative-devel"],
        DistroId::Debian => ["qt6-declarative-dev"],
        DistroId::Ubuntu => ["qt6-declarative-dev"],
        DistroId::Arch => ["qt6-declarative"],
    ),
    row!(
        "HttpServer",
        DistroId::MacOS => ["qt"],
        DistroId::OpenSuse => ["qt6-httpserver-devel"],
        DistroId::Fedora => ["qt6-qthttpserver-devel"],
        DistroId::Debian => ["qt6-httpserver-dev"],
        DistroId::Ubuntu => ["qt6-httpserver-dev"],
        DistroId::Arch => ["qt6-httpserver"],
    ),
    row!(
        "Charts",
        DistroId::MacOS => ["qt"],
        DistroId::OpenSuse => ["qt6-charts-devel"],
        DistroId::Fedora => ["qt6-qtcharts-devel"],
        DistroId::Debian => ["qt6-charts-dev"],
        DistroId::Ubuntu => ["qt6-charts-dev"],
        DistroId::Arch => ["qt6-charts"],
    ),
    row!(
        "Svg",
        DistroId::MacOS => ["qt"],
        DistroId::OpenSuse => ["qt6-svg-devel"],
        DistroId::Fedora => ["qt6-qtsvg-devel"],
        DistroId::Debian => ["qt6-svg-dev"],
        DistroId::Ubuntu => ["qt6-svg-dev"],
        DistroId::Arch => ["qt6-svg"],
    ),
    row!(
        "Multimedia",
        DistroId::MacOS => ["qt"],
        DistroId::OpenSuse => ["qt6-multimedia-devel"],
        DistroId::Fedora => ["qt6-qtmultimedia-devel"],
        DistroId::Debian => ["qt6-multimedia-dev"],
        DistroId::Ubuntu => ["qt6-multimedia-dev"],
        DistroId::Arch => ["qt6-multimedia"],
    ),
];

/// How doctor should handle a particular Qt6 component when reporting
/// status / install commands. Distinct from [`QT_PACKAGES`] because the
/// kind decision is per-component, not per-distro.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum QtComponentKind {
    /// Standard component — has a real package and a CMake config dir.
    Standard,
    /// Header-only / private umbrella that ships inside another
    /// component's package (e.g. `GuiPrivate` lives in `Qt6Gui`'s
    /// install). No separate package; doctor skips the missing-package
    /// suggestion and trusts the parent component's status.
    PrivateUmbrella { parent: &'static str },
    /// Tech Preview module not yet packaged by mainstream distros
    /// (e.g. `CanvasPainter` until it leaves Tech Preview). Doctor
    /// surfaces a "build Qt 6 from source / wait for upstream" message
    /// rather than a fake install command.
    TechPreview,
}

/// Classify a Qt6 component name. Default = `Standard`.
pub fn qt_component_kind(component: &str) -> QtComponentKind {
    match component {
        "GuiPrivate" => QtComponentKind::PrivateUmbrella { parent: "Gui" },
        "CanvasPainter" => QtComponentKind::TechPreview,
        _ => QtComponentKind::Standard,
    }
}

/// Look up the install package(s) for a Qt6 component on the given
/// distro. Returns `None` when no entry exists — doctor handles the
/// fallback message.
pub fn qt_packages_for(component: &str, distro: DistroId) -> Option<&'static [&'static str]> {
    let row = QT_PACKAGES.iter().find(|(name, _)| *name == component)?;
    row.1
        .iter()
        .find(|(d, _)| *d == distro)
        .map(|(_, pkgs)| *pkgs)
}

/// KF6 component → per-distro package table. macOS users go through
/// KDE Craft (no Homebrew formula exists for KF6); doctor surfaces
/// the Craft bootstrap recipe rather than an install line.
pub const KF6_PACKAGES: &[PackageRow] = &[
    row!(
        "Kirigami",
        DistroId::OpenSuse => ["kf6-kirigami-devel"],
        DistroId::Fedora => ["kf6-kirigami2-devel"],
        DistroId::Debian => ["libkf6kirigami-dev"],
        DistroId::Ubuntu => ["libkf6kirigami-dev"],
        DistroId::Arch => ["kirigami"],
    ),
    row!(
        "CoreAddons",
        DistroId::OpenSuse => ["kf6-kcoreaddons-devel"],
        DistroId::Fedora => ["kf6-kcoreaddons-devel"],
        DistroId::Debian => ["libkf6coreaddons-dev"],
        DistroId::Ubuntu => ["libkf6coreaddons-dev"],
        DistroId::Arch => ["kcoreaddons"],
    ),
    row!(
        "I18n",
        DistroId::OpenSuse => ["kf6-ki18n-devel"],
        DistroId::Fedora => ["kf6-ki18n-devel"],
        DistroId::Debian => ["libkf6i18n-dev"],
        DistroId::Ubuntu => ["libkf6i18n-dev"],
        DistroId::Arch => ["ki18n"],
    ),
];

pub fn kf6_packages_for(component: &str, distro: DistroId) -> Option<&'static [&'static str]> {
    let row = KF6_PACKAGES.iter().find(|(name, _)| *name == component)?;
    row.1
        .iter()
        .find(|(d, _)| *d == distro)
        .map(|(_, pkgs)| *pkgs)
}

/// CMake keywords that can appear after `COMPONENTS` and aren't
/// component names themselves. Used by both KF6 and Qt6 parsers.
const FIND_PACKAGE_KEYWORDS: &[&str] = &[
    "REQUIRED",
    "QUIET",
    "EXACT",
    "OPTIONAL_COMPONENTS",
    "MODULE",
    "CONFIG",
    "NO_MODULE",
    "GLOBAL",
    "NO_DEFAULT_PATH",
];

/// Generic helper: when `arg` has the form `<package_prefix>
/// COMPONENTS Foo Bar [REQUIRED ...]`, returns `["Foo","Bar"]`. When
/// it has the compact `<package_prefix>Foo` form (a single token like
/// `KF6Kirigami`), returns `["Foo"]`. Returns empty otherwise.
fn parse_components_with_prefix(arg: &str, package_prefix: &str) -> Vec<String> {
    let trimmed = arg.trim();
    let first = trimmed.split_whitespace().next().unwrap_or("");
    let mut out = Vec::new();
    if first == package_prefix {
        // "<prefix> COMPONENTS Foo Bar [REQUIRED ...]"
        let Some(idx) = trimmed.find("COMPONENTS") else {
            return out;
        };
        let after = &trimmed[idx + "COMPONENTS".len()..];
        for tok in after.split_whitespace() {
            if FIND_PACKAGE_KEYWORDS.contains(&tok) {
                continue;
            }
            out.push(tok.to_string());
        }
        return out;
    }
    if let Some(rest) = first.strip_prefix(package_prefix) {
        if !rest.is_empty() {
            out.push(rest.to_string());
        }
    }
    out
}

/// Parse a manifest `[cmake] find_package` entry like `"KF6Kirigami
/// REQUIRED"` or `"KF6 COMPONENTS Kirigami CoreAddons"` into a list of
/// component names. Returns an empty vec when the entry doesn't reference
/// KF6 at all (e.g. `"Qt6 COMPONENTS Charts"`).
pub fn parse_kf6_components_from_find_package(arg: &str) -> Vec<String> {
    parse_components_with_prefix(arg, "KF6")
}

/// Parse a manifest `[cmake] find_package` entry that references Qt6
/// add-on components, e.g. `"Qt6 COMPONENTS Charts Multimedia"` →
/// `["Charts","Multimedia"]`. Returns an empty vec when the entry isn't
/// a Qt6 find_package call.
pub fn parse_qt6_components_from_find_package(arg: &str) -> Vec<String> {
    parse_components_with_prefix(arg, "Qt6")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_os_release_tumbleweed() {
        let s = "NAME=\"openSUSE Tumbleweed\"\nID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n";
        assert_eq!(parse_os_release(s), DistroId::OpenSuse);
    }

    #[test]
    fn parse_os_release_fedora() {
        let s = "ID=fedora\nID_LIKE=\"rhel centos\"\n";
        assert_eq!(parse_os_release(s), DistroId::Fedora);
    }

    #[test]
    fn parse_os_release_ubuntu_via_id() {
        let s = "ID=ubuntu\nID_LIKE=debian\n";
        assert_eq!(parse_os_release(s), DistroId::Ubuntu);
    }

    #[test]
    fn parse_os_release_debian() {
        let s = "ID=debian\n";
        assert_eq!(parse_os_release(s), DistroId::Debian);
    }

    #[test]
    fn parse_os_release_arch() {
        let s = "ID=arch\n";
        assert_eq!(parse_os_release(s), DistroId::Arch);
    }

    #[test]
    fn parse_os_release_manjaro_via_id_like() {
        let s = "ID=manjaro\nID_LIKE=arch\n";
        assert_eq!(parse_os_release(s), DistroId::Arch);
    }

    #[test]
    fn parse_os_release_unknown() {
        let s = "ID=void\nID_LIKE=\n";
        assert_eq!(parse_os_release(s), DistroId::UnknownLinux);
    }

    #[test]
    fn qt_packages_for_charts_on_homebrew_collapses_to_umbrella() {
        let pkgs = qt_packages_for("Charts", DistroId::MacOS).unwrap();
        assert_eq!(pkgs, &["qt"]);
    }

    #[test]
    fn qt_packages_for_unknown_component_returns_none() {
        assert!(qt_packages_for("DoesNotExist", DistroId::Fedora).is_none());
    }

    #[test]
    fn kf6_packages_for_kirigami_on_arch() {
        let pkgs = kf6_packages_for("Kirigami", DistroId::Arch).unwrap();
        assert_eq!(pkgs, &["kirigami"]);
    }

    #[test]
    fn kf6_packages_for_kirigami_on_macos_is_none() {
        // No homebrew formula exists for KF6 — doctor falls back to the
        // Craft-bootstrap message instead of suggesting a fake package.
        assert!(kf6_packages_for("Kirigami", DistroId::MacOS).is_none());
    }

    #[test]
    fn parse_kf6_components_compact_form() {
        assert_eq!(
            parse_kf6_components_from_find_package("KF6Kirigami REQUIRED"),
            vec!["Kirigami".to_string()]
        );
    }

    #[test]
    fn parse_kf6_components_components_form() {
        assert_eq!(
            parse_kf6_components_from_find_package("KF6 COMPONENTS Kirigami CoreAddons REQUIRED"),
            vec!["Kirigami".to_string(), "CoreAddons".to_string()]
        );
    }

    #[test]
    fn parse_kf6_components_non_kf6_returns_empty() {
        assert!(parse_kf6_components_from_find_package("Qt6 COMPONENTS Charts").is_empty());
    }

    #[test]
    fn qt_component_kind_classifies_special_cases() {
        assert_eq!(qt_component_kind("Core"), QtComponentKind::Standard);
        assert!(matches!(
            qt_component_kind("GuiPrivate"),
            QtComponentKind::PrivateUmbrella { parent: "Gui" }
        ));
        assert_eq!(
            qt_component_kind("CanvasPainter"),
            QtComponentKind::TechPreview
        );
    }
}
