//! `cutec` - Cute language compiler driver CLI.

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(
    name = "cutec",
    version,
    about = "Cute language compiler",
    long_about = "Compiles .cute sources into C++ that integrates with Qt 6 via CMake. \
                  Replaces moc - QMetaObject data is generated directly by cutec."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Lex a Cute source file and print the token stream.
    Lex {
        /// Path to a `.cute` file.
        file: PathBuf,
    },
    /// Parse a Cute source file and emit something for inspection.
    Parse {
        /// Path to a `.cute` file.
        file: PathBuf,

        /// What to emit: `ast` (default) prints a pretty-tree of the AST.
        #[arg(long, default_value = "ast")]
        emit: EmitKind,
    },
    /// Compile a Cute source file. Without `--out-dir`, drives an
    /// internal cmake build and produces a native binary in the current
    /// directory. With `--out-dir`, writes the generated `.h` + `.cpp`
    /// pair for downstream cmake integration. When no `<file>` is
    /// given, picks `<cwd>.cute` (matches `cute init`'s naming),
    /// then `main.cute`, then the single `.cute` file in cwd.
    Build {
        /// Path to a `.cute` file. Optional; auto-detected from cwd
        /// if omitted.
        file: Option<PathBuf>,

        /// Directory to write generated `.h` / `.cpp` into. Skips the
        /// final binary build when set.
        #[arg(long)]
        out_dir: Option<PathBuf>,

        /// Output binary path (default: `<stem>` in the current dir).
        /// Ignored when `--out-dir` is set.
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// Run parse + resolve + type-check on a Cute source file
    /// without invoking codegen, cmake, or the linker. Renders every
    /// diagnostic and exits non-zero when any errors were found.
    /// Suitable for CI / pre-commit / fast iteration. When no
    /// `<file>` is given, picks the same entry `cute build` would.
    Check {
        /// Path to a `.cute` file. Optional; auto-detected from cwd
        /// if omitted.
        file: Option<PathBuf>,
    },
    /// Build a Cute source in test mode and run the resulting test
    /// binary. The runner emits TAP-lite output (`ok N - <name>` /
    /// `not ok N - <name>: <msg>`) and exits 0 only when every
    /// `test fn` passes. When no `<file>` is given, picks the same
    /// entry `cute build` would.
    Test {
        /// Path to a `.cute` file. Optional; auto-detected from cwd
        /// if omitted.
        file: Option<PathBuf>,
    },
    /// Format Cute source file(s) in-place. Comments above items /
    /// class members / statements are preserved; mid-expression
    /// comments are dropped. When no `<file>` is given, recursively
    /// formats every `.cute` file under cwd (skipping `target/`,
    /// `.git/`, and similar build/vcs dirs).
    Fmt {
        /// Path to a `.cute` file. Optional; defaults to recursive.
        file: Option<PathBuf>,

        /// Don't modify the file; instead exit 0 if all files are
        /// already formatted, 1 if any aren't. Suitable for CI checks.
        #[arg(long)]
        check: bool,
    },
    /// Scaffold a new Cute project under `<name>/`. Generates a
    /// `cute.toml`, an entry `<name>.cute`, and a `.gitignore`.
    /// The flag selects which app intrinsic the entry uses; templates
    /// compile end-to-end with `cute build` once you cd in.
    Init {
        /// Project name. Becomes the directory under cwd and the
        /// stem of the entry `.cute` file.
        name: String,

        /// QtQuick / Material GUI (default).
        #[arg(long, group = "kind")]
        qml: bool,

        /// QtWidgets / OS-native GUI.
        #[arg(long, group = "kind")]
        widget: bool,

        /// HTTP server / event-loop service (server_app intrinsic).
        #[arg(long, group = "kind")]
        server: bool,

        /// CLI tool with QCommandLineParser-style argv lifting.
        #[arg(long, group = "kind")]
        cli: bool,

        /// Kirigami app (KDE Frameworks 6, mobile-ready).
        #[arg(long, group = "kind")]
        kirigami: bool,

        /// GPU-accelerated UI via the Qt 6.11 Canvas Painter module
        /// (`gpu_app` intrinsic). No QML / JS / QtWidgets. Requires
        /// Qt 6.11+ and the CuteUI runtime — run `cute install-cute-ui`.
        #[arg(long, group = "kind")]
        gpu: bool,

        /// Also write `.vscode/settings.json` + `.vscode/extensions.json`
        /// wiring `cute-lsp` for diagnostics, hover, go-to-definition,
        /// completion, and format-on-save.
        #[arg(long)]
        vscode: bool,
    },
    /// Build + install a Cute library to the user cache so other
    /// Cute projects can depend on it via `[cute_libraries] deps`.
    ///
    /// Three forms:
    ///   `cute install <local-path>`  — directory containing a `cute.toml`
    ///                                  with `[library]`. Builds in place.
    ///   `cute install <git-url>[@rev]` — clone (rev = tag/branch/commit
    ///                                    or default branch HEAD), then build.
    ///   `cute install`                — read `[cute_libraries.<Name>]`
    ///                                   specs from cwd's `cute.toml` and
    ///                                   install each (git or path).
    Install {
        /// Local path or `<git-url>[@rev]`. Omit to install every
        /// `[cute_libraries.<Name>]` declared in cwd's cute.toml.
        target: Option<String>,
    },

    /// Install the CuteUI runtime so that `cute build` of `gpu_app`
    /// projects can `find_package(CuteUI)`. Currently builds the bundled
    /// `runtime/cute-ui/` source tree and installs it to a local prefix
    /// (default `~/.cache/cute/cute-ui-runtime/<version>/<triple>/`).
    InstallCuteUi {
        /// Path to the cute-ui CMake source root. Defaults to
        /// `runtime/cute-ui` next to the cute binary's repo.
        #[arg(long)]
        source: Option<PathBuf>,

        /// Override the install prefix.
        #[arg(long)]
        prefix: Option<PathBuf>,
    },

    /// Build a Cute source, run the resulting binary, then watch for
    /// `.cute` / `cute.toml` edits in the source's directory and
    /// auto-rebuild + relaunch on change. Edit/run loop tightener for
    /// development; not in-process hot reload (the binary restarts each
    /// edit, so app state isn't preserved). Ctrl-C exits.
    Watch {
        /// Path to a `.cute` file. Optional; auto-detected from cwd
        /// (same rules as `cute build`).
        file: Option<PathBuf>,
    },

    /// Diagnose Qt 6 / KF 6 / CuteUI dependencies for a Cute build.
    /// Without `<file>`, picks the same entry as `cute build`. Reports
    /// which modules are present / missing and prints the platform-
    /// specific install command for anything missing. Exits 0 when
    /// everything needed is present, 1 otherwise.
    Doctor {
        /// Path to a `.cute` file. Optional; auto-detected from cwd
        /// (same rules as `cute build`).
        file: Option<PathBuf>,
    },

    /// Install Cute-aware AI agent skills into your home directory
    /// so Claude Code / Cursor / Codex become fluent in Cute syntax
    /// + gotchas without per-project setup. With no flag, installs
    /// to every detected target.
    InstallSkills {
        /// Install for Claude Code (`~/.claude/skills/cute/SKILL.md`).
        #[arg(long)]
        claude: bool,

        /// Install for Cursor (`~/.cursor/rules/cute.mdc`).
        #[arg(long)]
        cursor: bool,

        /// Install for Codex / generic AGENTS.md tools
        /// (`~/.codex/AGENTS.md`).
        #[arg(long)]
        codex: bool,

        /// Overwrite existing skill file(s) without prompting.
        #[arg(long)]
        force: bool,
    },

    /// Install the bundled VS Code extension (`vscode-cute`) into
    /// `~/.vscode/extensions/i2y.cute-language-<version>/`. Reload
    /// VS Code afterwards (Cmd/Ctrl+Shift+P → "Reload Window") to
    /// activate syntax highlighting + the language configuration.
    InstallVscode {
        /// Overwrite an existing extension dir without prompting.
        #[arg(long)]
        force: bool,
    },

    /// Install the bundled Kate / KSyntaxHighlighting syntax
    /// definition (`cute.xml`) into the per-user search path —
    /// `$XDG_DATA_HOME/org.kde.syntax-highlighting/syntax/` on
    /// Linux, `~/Library/Application Support/...` on macOS.
    /// Picks up automatically on Kate / KWrite / KDevelop restart.
    InstallKate {
        /// Overwrite an existing `cute.xml` without prompting.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Clone, Debug, clap::ValueEnum)]
enum EmitKind {
    Ast,
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match cli.command {
        Cmd::Lex { file } => {
            let source = std::fs::read_to_string(&file)?;
            let mut source_map = cute_syntax::SourceMap::default();
            let file_id = source_map.add(file.to_string_lossy().into_owned(), source);
            let src = source_map.source(file_id);
            let tokens = cute_syntax::lex(file_id, src)?;
            for tok in &tokens {
                println!("{:?}", tok);
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Parse { file, emit } => {
            let mut source_map = cute_syntax::SourceMap::default();
            let module = cute_driver::parse_file(&mut source_map, &file)?;
            match emit {
                EmitKind::Ast => println!("{}", cute_syntax::ast::pretty(&module)),
                EmitKind::Json => println!("{:#?}", module),
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Build {
            file,
            out_dir,
            output,
        } => {
            let file = resolve_entry(file)?;
            if let Some(dir) = out_dir {
                let written = cute_driver::build_file(&file, &dir)?;
                for path in written {
                    println!("{}", path.display());
                }
            } else {
                let bin = cute_driver::compile_to_binary(&file, output.as_deref())?;
                println!("{}", bin.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Check { file } => {
            let file = resolve_entry(file)?;
            let errors = cute_driver::check_file(&file)?;
            if errors == 0 {
                Ok(ExitCode::SUCCESS)
            } else {
                eprintln!(
                    "{errors} error{plural}",
                    plural = if errors == 1 { "" } else { "s" }
                );
                Ok(ExitCode::FAILURE)
            }
        }
        Cmd::Test { file } => {
            // Two modes:
            //   * `cute test <path>` — exact file (legacy / focused
            //     run; mirrors `cute build <path>`).
            //   * `cute test`        — auto-walk cwd for every
            //     `.cute` source, so a project laid out as
            //     `src/foo.cute` + `tests/feature_x.cute` gets all
            //     of its `test fn`s in one binary without a manifest
            //     line.
            // The driver uses inputs[0] as the binary stem; we pick a
            // canonical entry (cwd-name.cute / main.cute / single
            // file) so the cache dir matches what `cute build` would
            // pick. If no canonical entry exists, fall back to the
            // first file alphabetically — the runner main is
            // synthesized regardless of which file we pick.
            let bin = match file {
                Some(p) => cute_driver::compile_to_test_binary(&p, None)?,
                None => {
                    let cwd = std::env::current_dir()?;
                    let inputs = collect_test_inputs(&cwd)?;
                    if inputs.is_empty() {
                        eprintln!("no `.cute` files found under {}", cwd.display());
                        return Ok(ExitCode::FAILURE);
                    }
                    cute_driver::compile_to_test_binary_multi(&inputs, None)?
                }
            };
            // Forward the runner's exit code so CI / shell scripts can
            // gate on it. Output streams through unmodified so users
            // see TAP lines as they're produced.
            let status = std::process::Command::new(&bin).status()?;
            Ok(match status.code() {
                Some(0) => ExitCode::SUCCESS,
                Some(c) if (0..=255).contains(&c) => ExitCode::from(c as u8),
                _ => ExitCode::FAILURE,
            })
        }
        Cmd::Fmt { file, check } => {
            let files = match file {
                Some(p) => vec![p],
                None => find_all_cute_files(&std::env::current_dir()?)?,
            };
            if files.is_empty() {
                eprintln!("no `.cute` files found under cwd");
                return Ok(ExitCode::FAILURE);
            }
            let mut all_clean = true;
            for path in &files {
                let source = std::fs::read_to_string(path)?;
                let mut sm = cute_syntax::SourceMap::default();
                let fid = sm.add(path.to_string_lossy().into_owned(), source.clone());
                let formatted = cute_syntax::format_source(fid, &source)?;
                if check {
                    if formatted != source {
                        eprintln!("{} is not formatted", path.display());
                        all_clean = false;
                    }
                } else if formatted != source {
                    std::fs::write(path, &formatted)?;
                    println!("formatted {}", path.display());
                }
            }
            Ok(if all_clean {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Cmd::Init {
            name,
            qml,
            widget,
            server,
            cli,
            kirigami,
            gpu,
            vscode,
        } => init_project(
            &name,
            ProjectKind::pick(qml, widget, server, cli, kirigami, gpu),
            vscode,
        ),
        Cmd::InstallSkills {
            claude,
            cursor,
            codex,
            force,
        } => install_skills(claude, cursor, codex, force),
        Cmd::InstallVscode { force } => install_vscode(force),
        Cmd::InstallKate { force } => install_kate(force),
        Cmd::InstallCuteUi { source, prefix } => install_cute_ui(source, prefix),
        Cmd::Install { target } => cute_install(target),
        Cmd::Watch { file } => cute_watch(file),
        Cmd::Doctor { file } => {
            let report = cute_driver::doctor::run(file.as_deref())?;
            print!("{report}");
            Ok(if report.is_healthy() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
    }
}

/// `cute watch [foo.cute]` — auto-rebuild + relaunch loop. Polls the
/// source file's directory for `.cute` / `cute.toml` mtime changes;
/// on edit, kills the running child, rebuilds, and respawns. Ctrl-C
/// terminates the loop and the child.
fn cute_watch(file: Option<PathBuf>) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use std::process::{Child, Command};
    use std::time::{Duration, SystemTime};

    let entry = resolve_entry(file)?;
    let entry = std::fs::canonicalize(&entry).unwrap_or(entry);
    let dir = entry
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });

    fn collect_max_mtime(dir: &std::path::Path) -> SystemTime {
        let mut max = SystemTime::UNIX_EPOCH;
        let Ok(rd) = std::fs::read_dir(dir) else {
            return max;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let watched = name.ends_with(".cute") || name == "cute.toml";
            if !watched {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(&path) {
                if let Ok(mt) = meta.modified() {
                    if mt > max {
                        max = mt;
                    }
                }
            }
        }
        max
    }

    fn build_and_spawn(entry: &std::path::Path) -> Result<Child, Box<dyn std::error::Error>> {
        let bin = cute_driver::compile_to_binary(entry, None)?;
        Ok(Command::new(&bin).spawn()?)
    }

    println!(">> watching {} (Ctrl-C to stop)", dir.display());
    let mut last_mtime = collect_max_mtime(&dir);
    let mut child: Option<Child> = match build_and_spawn(&entry) {
        Ok(c) => {
            println!(">> launched {}", entry.display());
            Some(c)
        }
        Err(e) => {
            eprintln!(">> initial build failed: {e}");
            None
        }
    };

    loop {
        std::thread::sleep(Duration::from_millis(400));
        // Reap a self-exited child so we don't try to kill an orphan.
        if let Some(c) = child.as_mut() {
            if let Ok(Some(_)) = c.try_wait() {
                child = None;
            }
        }
        let cur = collect_max_mtime(&dir);
        if cur > last_mtime {
            last_mtime = cur;
            println!(">> change detected; rebuilding...");
            if let Some(mut c) = child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            match build_and_spawn(&entry) {
                Ok(c) => {
                    println!(">> relaunched");
                    child = Some(c);
                }
                Err(e) => {
                    eprintln!(">> build failed: {e}");
                }
            }
        }
    }
}

/// Builds and installs the bundled `runtime/cute-ui/` source tree.
/// `cute install [target]` — build + install a Cute library into
/// the user cache (`~/.cache/cute/libraries/<Name>/<version>/<triple>/`).
///
/// Three forms:
///   - `target = Some("/path/to/dir")` — local path containing
///     cute.toml + a Cute source. Builds in place.
///   - `target = Some("https://...git[@rev]")` — git clone (rev =
///     tag/branch/commit hash, defaults to default branch HEAD)
///     into a scratch dir, then build.
///   - `target = None` — read cwd's cute.toml, walk every
///     `[cute_libraries.<Name>]` spec, install each (git or path).
fn cute_install(target: Option<String>) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match target {
        Some(s) if looks_like_git_url(&s) => install_from_git(&s),
        Some(s) => install_from_local_path(&PathBuf::from(s)),
        None => install_from_consumer_manifest(),
    }
}

/// Heuristic for "looks like a git URL". Conservative on purpose:
/// matches https/http/ssh/git protocols + scp-style `user@host:path`,
/// or a `.git` suffix. Anything else is treated as a local path.
fn looks_like_git_url(s: &str) -> bool {
    s.starts_with("https://")
        || s.starts_with("http://")
        || s.starts_with("ssh://")
        || s.starts_with("git://")
        || s.starts_with("git@")
        || s.ends_with(".git")
        || (s.contains(':') && !s.starts_with('/') && !s.starts_with('.'))
}

/// Resolve an entry `.cute` file inside `dir` using the same search
/// order `resolve_entry` applies to cwd. Used by both local-path and
/// git-clone installs to find the source to compile.
fn resolve_entry_in_dir(dir: &std::path::Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(base) = dir.file_name().and_then(|s| s.to_str()) {
        let candidate = dir.join(format!("{base}.cute"));
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    let main = dir.join("main.cute");
    if main.exists() {
        return Ok(main);
    }
    let cutes: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("cute"))
        .collect();
    if cutes.len() == 1 {
        return Ok(cutes.into_iter().next().unwrap());
    }
    Err(format!(
        "no entry `.cute` file in {}; expected `<dir-basename>.cute`, `main.cute`, or a single `.cute` file",
        dir.display()
    )
    .into())
}

fn install_from_local_path(path: &std::path::Path) -> Result<ExitCode, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Err(format!("path {} does not exist", path.display()).into());
    }
    let entry = if path.is_file() {
        path.to_path_buf()
    } else {
        resolve_entry_in_dir(path)?
    };
    eprintln!("==> Building library from {}", entry.display());
    let prefix = cute_driver::compile_to_library(&entry)?;
    eprintln!("Installed to {}", prefix.display());
    Ok(ExitCode::SUCCESS)
}

fn install_from_git(spec: &str) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use std::process::Command;

    // Split off optional `@rev` suffix. Only the LAST `@` counts to
    // tolerate URLs containing `@` (scp-style `user@host:path`).
    let (url, rev) = match spec.rsplit_once('@') {
        // Treat `user@host:...` as URL-only when the rsplit ate the
        // user portion (the right side then contains `:` or `/`).
        Some((_, right)) if right.contains(':') || right.contains('/') => (spec, None),
        Some((u, r)) => (u, Some(r)),
        None => (spec, None),
    };
    // Stable scratch dir per URL (so a repeated install doesn't
    // accumulate clones). Clear any existing checkout first to avoid
    // dirty-tree issues with `git checkout`.
    let scratch_root = scratch_dir();
    let dir_name = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("install")
        .trim_end_matches(".git")
        .to_string();
    let work = scratch_root.join(&dir_name);
    if work.exists() {
        std::fs::remove_dir_all(&work)?;
    }
    std::fs::create_dir_all(&scratch_root)?;
    eprintln!("==> Cloning {url} into {}", work.display());
    let clone = Command::new("git")
        .arg("clone")
        .arg("--depth=1")
        .args(rev.map(|_| Vec::<&str>::new()).unwrap_or_default()) // keep depth=1 by default
        .arg(url)
        .arg(&work)
        .status()?;
    if !clone.success() {
        return Err(format!("git clone of {url} failed").into());
    }
    if let Some(r) = rev {
        // Need full history for arbitrary refs. Re-fetch unshallowed.
        eprintln!("==> Fetching full history to resolve rev `{r}`");
        let unshallow = Command::new("git")
            .current_dir(&work)
            .args(["fetch", "--unshallow", "--tags"])
            .status();
        let _ = unshallow; // best-effort; remote may already be full
        let checkout = Command::new("git")
            .current_dir(&work)
            .args(["checkout", r])
            .status()?;
        if !checkout.success() {
            return Err(format!("git checkout {r} failed").into());
        }
    }
    install_from_local_path(&work)
}

fn scratch_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".cache/cute/install-scratch")
}

fn install_from_consumer_manifest() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    let cute_toml = cwd.join("cute.toml");
    if !cute_toml.exists() {
        return Err(format!(
            "no cute.toml in {}; pass a path or git URL explicitly",
            cwd.display()
        )
        .into());
    }
    // Reuse cute-driver's manifest loader so cute-cli doesn't pull
    // toml in directly. `try_load` walks the source's parent dir for
    // `cute.toml`; pass any file inside cwd as the probe.
    let (manifest, _dir) = cute_driver::Manifest::try_load(&cute_toml)?
        .ok_or_else(|| format!("missing cute.toml at {}", cute_toml.display()))?;
    if manifest.cute_libraries.specs.is_empty() {
        eprintln!(
            "no [cute_libraries.<Name>] specs in {}; nothing to install",
            cute_toml.display()
        );
        return Ok(ExitCode::SUCCESS);
    }
    let manifest_dir = cute_toml.parent().unwrap_or(&cwd);
    for (name, spec) in &manifest.cute_libraries.specs {
        eprintln!("==> Installing {name}");
        if let Some(git) = &spec.git {
            let target: String = match &spec.rev {
                Some(r) => format!("{git}@{r}"),
                None => git.clone(),
            };
            install_from_git(&target)?;
        } else if let Some(p) = &spec.path {
            install_from_local_path(&manifest_dir.join(p))?;
        } else {
            eprintln!("  spec for `{name}` has neither `git` nor `path`; skipping");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn install_cute_ui(
    source: Option<PathBuf>,
    prefix: Option<PathBuf>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use std::process::Command;

    let source = source
        .or_else(|| {
            std::env::current_exe().ok().and_then(|p| {
                p.parent()?
                    .parent()?
                    .parent()
                    .map(|p| p.join("runtime/cute-ui"))
            })
        })
        .filter(|p| p.exists())
        .ok_or("could not locate runtime/cute-ui source. Pass --source <path> explicitly.")?;

    let prefix = prefix
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let triple = host_triple();
                PathBuf::from(h)
                    .join(".cache/cute/cute-ui-runtime")
                    .join(env!("CARGO_PKG_VERSION"))
                    .join(triple)
            })
        })
        .ok_or("could not determine install prefix; pass --prefix")?;

    let build_dir = source.join("build");
    eprintln!("==> Configuring cute-ui at {}", source.display());
    let mut configure = Command::new("cmake");
    configure
        .arg("-S")
        .arg(&source)
        .arg("-B")
        .arg(&build_dir)
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DCUTE_UI_BUILD_EXAMPLES=OFF");
    // Reuse the driver's Qt-prefix probe so a Homebrew-only macOS box without
    // QT_DIR / CMAKE_PREFIX_PATH still finds Qt 6.11 + CanvasPainter.
    if let Some(prefix) = cute_driver::find_qt_prefix() {
        configure.arg(format!("-DCMAKE_PREFIX_PATH={prefix}"));
    }
    if !configure.status()?.success() {
        return Err("cmake configure failed".into());
    }
    eprintln!("==> Building cute-ui");
    let build = Command::new("cmake")
        .args(["--build", build_dir.to_str().unwrap(), "-j"])
        .status()?;
    if !build.success() {
        return Err("cmake build failed".into());
    }
    eprintln!("==> Installing cute-ui to {}", prefix.display());
    let install = Command::new("cmake")
        .arg("--install")
        .arg(&build_dir)
        .arg("--prefix")
        .arg(&prefix)
        .status()?;
    if !install.success() {
        return Err("cmake install failed".into());
    }
    eprintln!();
    eprintln!("CuteUI installed to {}", prefix.display());
    eprintln!();
    eprintln!("Add it to CMAKE_PREFIX_PATH so cute build can find it:");
    eprintln!(
        "    export CMAKE_PREFIX_PATH={}:$CMAKE_PREFIX_PATH",
        prefix.display()
    );
    Ok(ExitCode::SUCCESS)
}

/// Best-effort host triple of the form `<arch>-<os>` for cache naming.
fn host_triple() -> String {
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    format!("{arch}-{os}")
}

/// Pick the `.cute` entry file when the user didn't pass one.
/// Search order, all relative to cwd:
///   1. `<basename(cwd)>.cute` — matches what `cute init` creates
///   2. `main.cute` — Rust / Go convention
///   3. The single `.cute` file in cwd, if exactly one exists
///
/// Returns the explicit path unchanged when one is given.
fn resolve_entry(file: Option<PathBuf>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(p) = file {
        return Ok(p);
    }
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
    Err(format!(
        "no entry `.cute` file found in {}; expected `<dir>.cute`, `main.cute`, or a single `.cute` file",
        cwd.display()
    )
    .into())
}

/// Recursively collect every `.cute` file under `root`, skipping
/// build / VCS / IDE directories that wouldn't have user-authored
/// sources.
fn find_all_cute_files(root: &std::path::Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut out = Vec::new();
    visit_cute(root, &mut out)?;
    out.sort();
    Ok(out)
}

/// Collect inputs for `cute test` no-arg form, ordered so the
/// canonical entry file (cwd-name.cute / main.cute / single file)
/// comes first. The driver uses inputs[0] as the binary stem and
/// for `cute.toml` lookup, so picking the same entry that
/// `cute build` would gives users a single binary cache dir
/// shared across modes.
fn collect_test_inputs(cwd: &std::path::Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut all = find_all_cute_files(cwd)?;
    if all.is_empty() {
        return Ok(all);
    }
    // Promote the canonical entry (if present) to position 0 by
    // partitioning the vec in-place. Mirrors `resolve_entry`'s
    // priority list.
    let canonical = canonical_entry(cwd, &all);
    if let Some(idx) = canonical.and_then(|p| all.iter().position(|x| x == &p)) {
        all.swap(0, idx);
    }
    Ok(all)
}

fn canonical_entry(cwd: &std::path::Path, files: &[PathBuf]) -> Option<PathBuf> {
    if let Some(base) = cwd.file_name().and_then(|s| s.to_str()) {
        let candidate = cwd.join(format!("{base}.cute"));
        if files.iter().any(|p| p == &candidate) {
            return Some(candidate);
        }
    }
    let main = cwd.join("main.cute");
    if files.iter().any(|p| p == &main) {
        return Some(main);
    }
    None
}

fn visit_cute(dir: &std::path::Path, out: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            // Skip standard build / VCS / IDE / cache directories.
            // These never carry user-authored Cute sources.
            if matches!(
                name,
                "target" | ".git" | ".claude" | ".vscode" | ".idea" | "node_modules" | ".cache"
            ) {
                continue;
            }
        }
        if path.is_dir() {
            visit_cute(&path, out)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("cute") {
            out.push(path);
        }
    }
    Ok(())
}

#[derive(Copy, Clone)]
enum ProjectKind {
    Qml,
    Widget,
    Server,
    Cli,
    Kirigami,
    Gpu,
}

impl ProjectKind {
    fn pick(qml: bool, widget: bool, server: bool, cli: bool, kirigami: bool, gpu: bool) -> Self {
        if widget {
            ProjectKind::Widget
        } else if server {
            ProjectKind::Server
        } else if cli {
            ProjectKind::Cli
        } else if kirigami {
            ProjectKind::Kirigami
        } else if gpu {
            ProjectKind::Gpu
        } else {
            // Default (no flag, or `--qml`): a QtQuick / Material app.
            let _ = qml;
            ProjectKind::Qml
        }
    }

    fn cute_template(self) -> &'static str {
        match self {
            ProjectKind::Qml => include_str!("../templates/qml.cute.tmpl"),
            ProjectKind::Widget => include_str!("../templates/widget.cute.tmpl"),
            ProjectKind::Server => include_str!("../templates/server.cute.tmpl"),
            ProjectKind::Cli => include_str!("../templates/cli.cute.tmpl"),
            ProjectKind::Kirigami => include_str!("../templates/kirigami.cute.tmpl"),
            ProjectKind::Gpu => include_str!("../templates/gpu.cute.tmpl"),
        }
    }

    fn cute_toml_template(self) -> &'static str {
        match self {
            ProjectKind::Kirigami => include_str!("../templates/cute.toml.kirigami"),
            ProjectKind::Gpu => include_str!("../templates/cute.toml.gpu"),
            _ => include_str!("../templates/cute.toml.empty"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            ProjectKind::Qml => "qml_app (QtQuick)",
            ProjectKind::Widget => "widget_app (QtWidgets)",
            ProjectKind::Server => "server_app (HTTP / event loop)",
            ProjectKind::Cli => "cli_app",
            ProjectKind::Kirigami => "Kirigami (KDE Frameworks 6)",
            ProjectKind::Gpu => "gpu_app (Qt 6.11 Canvas Painter)",
        }
    }
}

fn init_project(
    name: &str,
    kind: ProjectKind,
    vscode: bool,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    if !is_valid_project_name(name) {
        eprintln!(
            "error: `{name}` is not a valid project name (use letters, digits, `_`, starting with letter or `_`)"
        );
        return Ok(ExitCode::FAILURE);
    }
    let dir = std::path::Path::new(name);
    if dir.exists() {
        eprintln!("error: `{}` already exists; aborting", dir.display());
        return Ok(ExitCode::FAILURE);
    }
    std::fs::create_dir(dir)?;

    let cute_text = kind.cute_template().replace("{NAME}", name);
    std::fs::write(dir.join(format!("{name}.cute")), cute_text)?;

    let toml_text = kind.cute_toml_template().to_string();
    std::fs::write(dir.join("cute.toml"), toml_text)?;

    let gitignore = include_str!("../templates/gitignore").replace("{NAME}", name);
    std::fs::write(dir.join(".gitignore"), gitignore)?;

    if vscode {
        let vscode_dir = dir.join(".vscode");
        std::fs::create_dir(&vscode_dir)?;
        std::fs::write(
            vscode_dir.join("settings.json"),
            include_str!("../templates/vscode_settings.json"),
        )?;
        std::fs::write(
            vscode_dir.join("extensions.json"),
            include_str!("../templates/vscode_extensions.json"),
        )?;
    }

    println!("created {}/", dir.display());
    println!("  template: {}", kind.label());
    if vscode {
        println!("  vscode:   .vscode/settings.json + extensions.json wired to cute-lsp");
    }
    println!("  next:");
    println!("    cd {name}");
    println!("    cute build {name}.cute");
    println!("    ./{name}");
    Ok(ExitCode::SUCCESS)
}

fn install_skills(
    claude: bool,
    cursor: bool,
    codex: bool,
    force: bool,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        eprintln!("error: $HOME is not set; can't locate per-user agent dirs");
        return Ok(ExitCode::FAILURE);
    };

    // No flag = install to all detected targets. Otherwise honor
    // exactly the flags that were passed.
    let any_flag = claude || cursor || codex;
    let do_claude = !any_flag || claude;
    let do_cursor = !any_flag || cursor;
    let do_codex = !any_flag || codex;

    let body = include_str!("../templates/cute_agent_body.md");

    let mut written: Vec<std::path::PathBuf> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    if do_claude {
        let dir = home.join(".claude").join("skills").join("cute");
        let path = dir.join("SKILL.md");
        let content = format!(
            "---\nname: cute-language\ndescription: |\n  Authoritative guide to the Cute programming language (https://github.com/i2y/cute) — a general-purpose language designed for the Qt 6 / KDE Frameworks ecosystem. Trigger when the user opens, edits, or asks about `.cute` files, `cute.toml`, `cute build` / `cute check` / `cute fmt` / `cute init` / `cute-lsp`, `.qpi` binding files, the `view` / `widget` / `prop` / `signal` / `slot` / `extern value` keywords, or Vala-style Qt programming. Skip when the user means the English word \"cute\" or a different project.\ntype: reference\n---\n{body}"
        );
        match write_skill(&dir, &path, &content, force) {
            Ok(()) => written.push(path),
            Err(e) => failures.push(format!("claude: {e}")),
        }
    }

    if do_cursor {
        let dir = home.join(".cursor").join("rules");
        let path = dir.join("cute.mdc");
        // Cursor's rule frontmatter shape (modern .mdc format).
        let content = format!(
            "---\ndescription: Cute language reference — Qt 6 / KDE Frameworks-targeted language by i2y. See body for hard requirements and common AI mistakes.\nglobs: \"**/*.cute,**/*.qpi,**/cute.toml\"\nalwaysApply: false\n---\n{body}"
        );
        match write_skill(&dir, &path, &content, force) {
            Ok(()) => written.push(path),
            Err(e) => failures.push(format!("cursor: {e}")),
        }
    }

    if do_codex {
        let dir = home.join(".codex");
        let path = dir.join("AGENTS.md");
        // AGENTS.md is plain markdown, no frontmatter — read by
        // Codex CLI, aider, cline, and other agentic tools that
        // follow the agents.md spec.
        match write_skill(&dir, &path, body, force) {
            Ok(()) => written.push(path),
            Err(e) => failures.push(format!("codex: {e}")),
        }
    }

    if !written.is_empty() {
        println!("installed Cute agent skill:");
        for p in &written {
            println!("  {}", p.display());
        }
        println!();
        println!("These trigger on `.cute` file edits or questions about Cute.");
        println!("Claude Code picks the skill up on the next session — no restart needed.");
    }
    if !failures.is_empty() {
        for f in &failures {
            eprintln!("error: {f}");
        }
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

fn install_vscode(force: bool) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        eprintln!("error: $HOME is not set; can't locate ~/.vscode/extensions");
        return Ok(ExitCode::FAILURE);
    };

    // Bundled at compile time so the binary is self-sufficient
    // — `cute install-vscode` works the same whether the user
    // built from a clone or installed from a packaged release.
    let package_json = include_str!("../../../extensions/vscode-cute/package.json");
    let readme = include_str!("../../../extensions/vscode-cute/README.md");
    let lang_config = include_str!("../../../extensions/vscode-cute/language-configuration.json");
    let tm_grammar = include_str!("../../../extensions/vscode-cute/syntaxes/cute.tmLanguage.json");

    // Read the version out of package.json so the installed dir
    // name follows the Marketplace convention
    // (`<publisher>.<name>-<version>`).
    let version = parse_extension_version(package_json);
    let dir_name = format!("i2y.cute-language-{version}");
    let target = home.join(".vscode").join("extensions").join(&dir_name);

    if target.exists() {
        if !force {
            eprintln!(
                "error: {} already exists; pass --force to overwrite",
                target.display()
            );
            return Ok(ExitCode::FAILURE);
        }
        std::fs::remove_dir_all(&target)?;
    }

    let syntaxes = target.join("syntaxes");
    std::fs::create_dir_all(&syntaxes)?;
    std::fs::write(target.join("package.json"), package_json)?;
    std::fs::write(target.join("README.md"), readme)?;
    std::fs::write(target.join("language-configuration.json"), lang_config)?;
    std::fs::write(syntaxes.join("cute.tmLanguage.json"), tm_grammar)?;

    println!("installed VS Code extension:");
    println!("  {}", target.display());
    println!();
    println!("Reload VS Code (Cmd/Ctrl+Shift+P → \"Reload Window\") to activate.");
    println!("Open a `.cute` file to verify syntax highlighting and the language config.");
    Ok(ExitCode::SUCCESS)
}

fn install_kate(force: bool) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        eprintln!("error: $HOME is not set");
        return Ok(ExitCode::FAILURE);
    };

    let cute_xml = include_str!("../../../extensions/kate-cute/cute.xml");

    // KSyntaxHighlighting reads syntax from Qt's
    // `QStandardPaths::GenericDataLocation` joined with
    // `org.kde.syntax-highlighting/syntax/`:
    //   - Linux:   $XDG_DATA_HOME (default ~/.local/share)
    //   - macOS:   ~/Library/Application Support
    let data_home = if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        std::path::PathBuf::from(xdg)
    } else {
        home.join(".local").join("share")
    };
    let target_dir = data_home.join("org.kde.syntax-highlighting").join("syntax");
    let target = target_dir.join("cute.xml");

    if target.exists() && !force {
        eprintln!(
            "error: {} already exists; pass --force to overwrite",
            target.display()
        );
        return Ok(ExitCode::FAILURE);
    }

    std::fs::create_dir_all(&target_dir)?;
    std::fs::write(&target, cute_xml)?;

    println!("installed Kate syntax definition:");
    println!("  {}", target.display());
    println!();
    println!(
        "Restart Kate (or Settings → Configure Kate → Open / Save → Modes &\nFiletypes, re-select Cute) to pick up the syntax. .cute / .qpi files\nauto-detect by extension."
    );
    Ok(ExitCode::SUCCESS)
}

/// Tiny ad-hoc `"version"` extraction from package.json — pulling
/// `serde_json` just for one field would be overkill, and the
/// shape is fixed (we author the file ourselves).
fn parse_extension_version(package_json: &str) -> String {
    for line in package_json.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("\"version\"") {
            return rest
                .trim_start_matches(':')
                .trim()
                .trim_end_matches(',')
                .trim_matches('"')
                .to_string();
        }
    }
    "0.0.0".to_string()
}

fn write_skill(
    dir: &std::path::Path,
    path: &std::path::Path,
    content: &str,
    force: bool,
) -> std::io::Result<()> {
    if path.exists() && !force {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "{} already exists; pass --force to overwrite",
                path.display()
            ),
        ));
    }
    std::fs::create_dir_all(dir)?;
    std::fs::write(path, content)?;
    Ok(())
}

fn is_valid_project_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
