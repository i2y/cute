//! `cute-qpi-gen` — `.qpi` binding generator driven by a
//! `typesystem.toml` description.
//!
//! Two run modes:
//!
//! - **`--typesystem path.toml`** — production form. The toml file
//!   enumerates every class to bind, declares per-class allow /
//!   exclude / param-rename rules, and carries the libclang search
//!   paths so a single command reproduces the full `.qpi`. Emit
//!   order in the output mirrors the order of `[[classes]]` entries.
//!
//! - **`--header foo.h --class FooClass`** — ad-hoc / probing form.
//!   Useful when investigating one class before adding it to the
//!   typesystem; equivalent to a one-class typesystem with no
//!   include filter.
//!
//! Output goes to stdout. Pipe to a `.qpi` file or diff against the
//! handcrafted version to validate.

mod clang_walk;

use cute_qpi_gen::{emit, typesystem};

use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(
    name = "cute-qpi-gen",
    about = "Generate Cute .qpi binding files from C++ headers via libclang."
)]
struct Cli {
    /// Path to a `typesystem.toml` describing classes to bind. When
    /// set, `--header` / `--class` / `--include` are ignored.
    #[arg(long, conflicts_with_all = ["header", "class", "includes"])]
    typesystem: Option<PathBuf>,

    /// One-off mode: header to parse. Use `--class` to pick the
    /// class out of the translation unit.
    #[arg(long, requires = "class")]
    header: Option<PathBuf>,

    /// One-off mode: class name to extract from `--header`.
    #[arg(long = "class")]
    class: Option<String>,

    /// One-off mode: extra `-isystem` directories.
    #[arg(long = "include", short = 'I')]
    includes: Vec<PathBuf>,

    /// Force a specific C++ standard (default: c++17).
    #[arg(long, default_value = "c++17")]
    std: String,

    /// One-off mode: which Cute declaration form to emit. `value`
    /// is the default and produces an `extern value <Name>` block;
    /// `object` triggers Q_PROPERTY scraping and emits a
    /// `class <Name> < Super { prop ... signal ... fn ... }` block.
    /// Ignored when `--typesystem` is used (each entry there sets
    /// its own `kind`).
    #[arg(long, default_value = "value")]
    kind: String,

    /// Optional file-level header comment for the emitted `.qpi`.
    /// Each line gets a leading `# `. Useful when piping output to
    /// a stdlib file you want a documentation banner on.
    #[arg(long)]
    header_comment: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cute-qpi-gen: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<String, String> {
    let classes = if let Some(ts_path) = &cli.typesystem {
        let ts = typesystem::TypeSystem::load(ts_path)?;
        clang_walk::collect(&ts)?
    } else if let (Some(header), Some(class_name)) = (&cli.header, &cli.class) {
        let kind = match cli.kind.as_str() {
            "value" => typesystem::ClassKind::Value,
            "object" => typesystem::ClassKind::Object,
            "enum" => typesystem::ClassKind::Enum,
            "flags" => typesystem::ClassKind::Flags,
            other => {
                return Err(format!(
                    "--kind: expected `value` / `object` / `enum` / `flags`, got `{other}`"
                ));
            }
        };
        clang_walk::collect_one_off(
            header.clone(),
            class_name.clone(),
            cli.includes.clone(),
            cli.std.clone(),
            kind,
        )?
    } else {
        return Err(
            "either --typesystem PATH or (--header PATH --class NAME) is required".to_string(),
        );
    };
    Ok(emit::emit(cli.header_comment.as_deref(), &classes))
}
