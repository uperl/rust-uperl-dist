//! `uperl-dist` — drive the build lifecycle of an unpacked CPAN distribution one
//! step at a time.
//!
//! Each subcommand maps to one phase of
//! [`cpan_distribution_build::Distribution`]:
//!
//! | subcommand      | EUMM                | Module::Build        |
//! |-----------------|---------------------|----------------------|
//! | `pre-configure` | list configure deps | list configure deps  |
//! | `configure`     | `perl Makefile.PL`  | `perl Build.PL`      |
//! | `build`         | `make`              | `perl Build`         |
//! | `test`          | `make test`         | `perl Build test`    |
//! | `install`       | `make install`      | `perl Build install` |
//!
//! `configure`, `build`, `test` and `install` let the child's output through to
//! this process's stdout/stderr and exit with the child's status. `perl` and
//! `make` output is therefore live; a failing step is reported as a non-zero
//! exit, never as a panic.
//!
//! `pre-configure` and `configure` also print the prerequisites they compute:
//! by default as a `comfy-table` in the same house style as `uperl-metacpan`,
//! or as JSON with `--json`.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use comfy_table::{Attribute, Cell, ContentArrangement, Table, presets::UTF8_FULL};
use cpan_distribution_build::{
    BuildTool, Dependencies, Dependency, Distribution, ExecuteResult, Perl,
};
use serde_json::{Value, json};

/// Step-by-step build and install of an unpacked CPAN distribution.
#[derive(Debug, Parser)]
#[command(name = "uperl-dist", version, about, long_about = None)]
struct Cli {
    #[command(flatten)]
    common: CommonArgs,

    #[command(subcommand)]
    command: Command,
}

/// Options shared by every subcommand. They may be given before or after the
/// subcommand name.
#[derive(Debug, Args)]
struct CommonArgs {
    /// Directory holding the unpacked distribution (its `Makefile.PL` /
    /// `Build.PL` and `META.json`).
    #[arg(
        short = 'C',
        long = "directory",
        global = true,
        value_name = "DIR",
        default_value = "."
    )]
    directory: PathBuf,

    /// Perl interpreter to build with (default: the first `perl` on `PATH`).
    #[arg(long, global = true, value_name = "PATH")]
    perl: Option<PathBuf>,

    /// `make` to use for `ExtUtils::MakeMaker` distributions (default: the first
    /// `make` on `PATH`).
    #[arg(long, global = true, value_name = "PATH")]
    make: Option<PathBuf>,

    /// Install newly built modules under this prefix, the way `local::lib` /
    /// `INSTALL_BASE` would (default: the interpreter's own site directories).
    #[arg(long, global = true, value_name = "DIR")]
    install_base: Option<PathBuf>,

    /// Directory to add to `PERL5LIB` when running build steps; repeatable.
    #[arg(long = "lib", global = true, value_name = "DIR")]
    lib: Vec<PathBuf>,

    /// Which build tool to use when the distribution ships *both* `Build.PL` and
    /// `Makefile.PL` (ignored when only one is present).
    #[arg(long, global = true, value_name = "TOOL", default_value_t = Prefer::Mb)]
    prefer: Prefer,

    /// Print computed prerequisites as JSON instead of a table (`pre-configure`
    /// and `configure`).
    #[arg(long, short = 'j', global = true)]
    json: bool,
}

/// Build-tool preference for a dual-config distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Prefer {
    /// `ExtUtils::MakeMaker` (`Makefile.PL`).
    Eumm,
    /// `Module::Build` (`Build.PL`).
    Mb,
}

impl std::fmt::Display for Prefer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Prefer::Eumm => "eumm",
            Prefer::Mb => "mb",
        };
        f.write_str(s)
    }
}

impl From<Prefer> for BuildTool {
    fn from(prefer: Prefer) -> Self {
        match prefer {
            Prefer::Eumm => BuildTool::Eumm,
            Prefer::Mb => BuildTool::ModuleBuild,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the prerequisites that must be installed before `configure` can run
    /// (the distribution's `configure` requires, plus the build tool itself), as
    /// a `module` / `version` table. Nothing is executed.
    PreConfigure,

    /// Run the configure step: `perl Makefile.PL` or `perl Build.PL`.
    ///
    /// Afterwards the resolved prerequisites (taken from `MYMETA` when the
    /// configure step wrote one, otherwise from `META`) are printed as a
    /// `phase` / `relationship` / `module` / `version` table, unless
    /// `--no-prereqs` is given.
    Configure {
        /// Don't print the resolved prerequisites after configuring.
        #[arg(long)]
        no_prereqs: bool,
    },

    /// Run the build step: `make` or `perl Build`.
    Build,

    /// Run the test suite: `make test` or `perl Build test`.
    Test,

    /// Install the built distribution: `make install` or `perl Build install`.
    Install,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("uperl-dist: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let Cli { common, command } = cli;

    let perl = build_perl(&common)?;
    let mut dist = Distribution::with_preference(&common.directory, perl, common.prefer.into())
        .with_context(|| {
            format!(
                "failed to open a CPAN distribution in {}",
                common.directory.display()
            )
        })?;

    match command {
        Command::PreConfigure => {
            print_pre_configure_prereqs(&dist.execute_pre_configure(), common.json)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Configure { no_prereqs } => {
            let (result, deps) = dist
                .execute_configure()
                .context("the configure step could not be started")?;
            if !no_prereqs {
                print_resolved_prereqs(&deps, common.json)?;
            }
            Ok(exit_code_for("configure", &result))
        }
        Command::Build => Ok(exit_code_for("build", &dist.execute_build()?)),
        Command::Test => Ok(exit_code_for("test", &dist.execute_test()?)),
        Command::Install => Ok(exit_code_for("install", &dist.execute_install()?)),
    }
}

/// Assemble the [`Perl`] wrapper from the shared options.
fn build_perl(common: &CommonArgs) -> Result<Perl> {
    let mut perl = match &common.perl {
        Some(path) => Perl::with_perl(path),
        None => Perl::new().context("could not locate a `perl` interpreter on PATH")?,
    };

    if let Some(make) = &common.make {
        perl = perl.with_make(make);
    }
    if let Some(base) = &common.install_base {
        perl = perl.with_install_base(base);
    }
    if !common.lib.is_empty() {
        perl = perl.with_lib(common.lib.clone());
    }

    Ok(perl)
}

/// Print the pre-configure prerequisites: a `module` / `version` table, or a
/// JSON array of `{ "module", "version" }` objects when `as_json`.
fn print_pre_configure_prereqs(deps: &[Dependency], as_json: bool) -> Result<()> {
    if as_json {
        let rows: Vec<Value> = deps
            .iter()
            .map(|d| json!({ "module": d.module, "version": d.version }))
            .collect();
        return print_json(&Value::Array(rows));
    }

    let mut table = house_style_table();
    table.set_header(header_row(["module", "version"]));
    for dep in deps {
        table.add_row([Cell::new(&dep.module), Cell::new(&dep.version)]);
    }
    println!("{table}");
    Ok(())
}

/// Print the resolved prerequisites: a `phase` / `relationship` / `module` /
/// `version` table, or a JSON array of the same fields when `as_json`.
fn print_resolved_prereqs(deps: &Dependencies, as_json: bool) -> Result<()> {
    let rows = flatten_prereqs(deps);

    if as_json {
        let rows: Vec<Value> = rows
            .iter()
            .map(|(phase, relationship, dep)| {
                json!({
                    "phase": phase,
                    "relationship": relationship,
                    "module": dep.module,
                    "version": dep.version,
                })
            })
            .collect();
        return print_json(&Value::Array(rows));
    }

    let mut table = house_style_table();
    table.set_header(header_row(["phase", "relationship", "module", "version"]));
    for (phase, relationship, dep) in rows {
        table.add_row([
            Cell::new(phase),
            Cell::new(relationship),
            Cell::new(&dep.module),
            Cell::new(&dep.version),
        ]);
    }
    println!("{table}");
    Ok(())
}

/// Flatten [`Dependencies`] into `(phase, relationship, dependency)` triples in
/// a stable phase-then-relationship order.
fn flatten_prereqs(deps: &Dependencies) -> Vec<(&'static str, &'static str, &Dependency)> {
    let phases = [
        ("configure", &deps.configure),
        ("build", &deps.build),
        ("test", &deps.test),
        ("runtime", &deps.runtime),
        ("develop", &deps.develop),
    ];

    let mut out = Vec::new();
    for (phase, group) in phases {
        let relationships = [
            ("requires", &group.requires),
            ("recommends", &group.recommends),
            ("suggests", &group.suggests),
            ("conflicts", &group.conflicts),
        ];
        for (relationship, list) in relationships {
            for dep in list {
                out.push((phase, relationship, dep));
            }
        }
    }
    out
}

/// A fresh [`Table`] in the same house style `uperl-metacpan` uses: the
/// `UTF8_FULL` preset, dynamic column arrangement, and a fixed width when the
/// output is not a terminal (so piped output wraps rather than sprawls).
fn house_style_table() -> Table {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    if !std::io::stdout().is_terminal() {
        table.set_width(100);
    }
    table
}

/// Header cells, emphasised when stdout is a terminal.
fn header_row<'a>(cells: impl IntoIterator<Item = &'a str>) -> Vec<Cell> {
    let bold = std::io::stdout().is_terminal();
    cells
        .into_iter()
        .map(|c| {
            let cell = Cell::new(c);
            if bold {
                cell.add_attribute(Attribute::Bold)
            } else {
                cell
            }
        })
        .collect()
}

/// Print `value` as pretty JSON with a trailing newline.
fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Map an [`ExecuteResult`] to a process exit code, reporting failures on stderr.
fn exit_code_for(step: &str, result: &ExecuteResult) -> ExitCode {
    if result.is_success {
        return ExitCode::SUCCESS;
    }

    match result.code {
        Some(code) => {
            eprintln!("uperl-dist: the {step} step exited with status {code}");
            let byte = u8::try_from(code).unwrap_or(1);
            ExitCode::from(if byte == 0 { 1 } else { byte })
        }
        None => {
            eprintln!("uperl-dist: the {step} step was terminated by a signal");
            ExitCode::FAILURE
        }
    }
}
