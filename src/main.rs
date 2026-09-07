//! `uperl-dist` — drive the build lifecycle of an unpacked CPAN distribution one
//! step at a time.
//!
//! Each subcommand maps to one phase of
//! [`cpan_distribution_build::Distribution`]:
//!
//! | subcommand      | EUMM                 | Module::Build          |
//! |-----------------|----------------------|------------------------|
//! | `pre-configure` | list configure deps  | list configure deps    |
//! | `configure`     | `perl Makefile.PL`   | `perl Build.PL`        |
//! | `build`         | `make`               | `perl Build`           |
//! | `test`          | `make test`          | `perl Build test`      |
//! | `install`       | `make install`       | `perl Build install`   |
//! | `clean`         | `make clean`         | `perl Build clean`     |
//! | `distclean`     | `make distclean`     | `perl Build distclean` |
//!
//! Every step but `pre-configure` lets the child's output through to this
//! process's stdout/stderr and exits with the child's status. `perl` and `make`
//! output is therefore live; a failing step is reported as a non-zero exit,
//! never as a panic.
//!
//! `pre-configure` and `configure` also print the prerequisites they compute:
//! by default as a `comfy-table` in the same house style as `uperl-metacpan`,
//! with an `installed` column giving each module's version on `dist.perl`'s
//! search path (`-` when it is not installed, `?` when it declares no version).
//!
//! `--json` replaces all of that with a single JSON object on stdout: the
//! child's captured, merged stdout+stderr under `output` (the empty string for
//! `pre-configure`, which runs nothing), the numeric `exit` code and a boolean
//! `success`, plus a `prereqs` object for `pre-configure` and `configure`. The
//! child's output is captured rather than streamed in this mode, so stdout stays
//! valid JSON. `pre-configure` always reports `exit` 0 and `success` true.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use comfy_table::{Attribute, Cell, ContentArrangement, Table, presets::UTF8_FULL};
use cpan_distribution_build::{
    BuildTool, Dependencies, Dependency, Distribution, ExecuteResult, Perl, PhaseDependencies,
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

    /// Emit a single JSON object on stdout instead of tables and live output:
    /// the captured command `output`, plus `prereqs` for `pre-configure` and
    /// `configure`.
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
    /// a `module` / `required` / `installed` table. Nothing is executed.
    PreConfigure,

    /// Run the configure step: `perl Makefile.PL` or `perl Build.PL`.
    ///
    /// Afterwards the resolved prerequisites (taken from `MYMETA` when the
    /// configure step wrote one, otherwise from `META`) are printed as a
    /// `phase` / `relationship` / `module` / `required` / `installed` table,
    /// unless `--no-prereqs` is given.
    Configure {
        /// Don't print the resolved prerequisites after configuring.
        #[arg(long)]
        no_prereqs: bool,

        /// Include `develop`-phase prerequisites in the table (skipped by
        /// default; `--json` always includes them).
        #[arg(long)]
        include_develop: bool,
    },

    /// Run the build step: `make` or `perl Build`.
    Build,

    /// Run the test suite: `make test` or `perl Build test`.
    Test,

    /// Install the built distribution: `make install` or `perl Build install`.
    Install,

    /// Remove build products: `make clean` or `perl Build clean`.
    Clean,

    /// Remove build products and the generated `Makefile` / `Build` script:
    /// `make distclean` or `perl Build distclean`.
    Distclean,
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
            let deps = dist.execute_pre_configure();
            if common.json {
                // `pre-configure` runs nothing: empty output, always successful.
                print_json(&json!({
                    "prereqs": pre_configure_prereqs_json(&deps),
                    "output": "",
                    "exit": 0,
                    "success": true,
                }))?;
            } else {
                print_pre_configure_table(&deps, &dist.perl);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Configure {
            no_prereqs,
            include_develop,
        } => {
            let (result, deps) = dist
                .execute_configure()
                .context("the configure step could not be started")?;
            let code = step_exit_code("configure", &result);
            if common.json {
                let mut obj = serde_json::Map::new();
                if !no_prereqs {
                    obj.insert("prereqs".to_string(), resolved_prereqs_json(&deps));
                }
                obj.insert(
                    "output".to_string(),
                    Value::String(captured_output(&result)),
                );
                obj.insert("exit".to_string(), json!(code));
                obj.insert("success".to_string(), json!(result.is_success));
                print_json(&Value::Object(obj))?;
            } else if !no_prereqs {
                print_resolved_prereqs_table(&deps, include_develop, &dist.perl);
            }
            Ok(ExitCode::from(code))
        }
        Command::Build => finish_step("build", &common, dist.execute_build()?),
        Command::Test => finish_step("test", &common, dist.execute_test()?),
        Command::Install => finish_step("install", &common, dist.execute_install()?),
        Command::Clean => finish_step("clean", &common, dist.execute_clean()?),
        Command::Distclean => finish_step("distclean", &common, dist.execute_distclean()?),
    }
}

/// Emit the JSON envelope for a bare build step when `--json` is set, then map
/// the [`ExecuteResult`] to a process exit code.
fn finish_step(step: &str, common: &CommonArgs, result: ExecuteResult) -> Result<ExitCode> {
    let code = step_exit_code(step, &result);
    if common.json {
        print_json(&json!({
            "output": captured_output(&result),
            "exit": code,
            "success": result.is_success,
        }))?;
    }
    Ok(ExitCode::from(code))
}

/// The child's captured, merged stdout+stderr as a lossy UTF-8 string, or `""`
/// when output was not captured (i.e. it went straight to the terminal).
fn captured_output(result: &ExecuteResult) -> String {
    result
        .output_lossy()
        .map(|text| text.into_owned())
        .unwrap_or_default()
}

/// Assemble the [`Perl`] wrapper from the shared options. Command output is
/// captured (rather than inherited) when `--json` is in effect, so it can be
/// folded into the JSON envelope.
fn build_perl(common: &CommonArgs) -> Result<Perl> {
    let mut perl = match &common.perl {
        Some(path) => Perl::with_perl(path),
        None => Perl::new().context("could not locate a `perl` interpreter on PATH")?,
    };

    perl = perl.with_capture_output(common.json);

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

/// The pre-configure prerequisites as `{ "configure": [ { "module", "version" },
/// ... ] }` — they are all configure-phase requirements.
fn pre_configure_prereqs_json(deps: &[Dependency]) -> Value {
    let rows: Vec<Value> = deps
        .iter()
        .map(|d| json!({ "module": d.module, "version": d.version }))
        .collect();
    json!({ "configure": rows })
}

/// Print the pre-configure prerequisites as a `module` / `required` /
/// `installed` table.
fn print_pre_configure_table(deps: &[Dependency], perl: &Perl) {
    let mut table = house_style_table();
    table.set_header(header_row(["module", "required", "installed"]));
    for dep in deps {
        table.add_row([
            Cell::new(&dep.module),
            Cell::new(&dep.version),
            Cell::new(installed_version(perl, &dep.module)),
        ]);
    }
    println!("{table}");
}

/// The resolved prerequisites as the full picture:
/// `{ "<phase>": [ { "relationship", "module", "version" }, ... ], ... }` with
/// every CPAN phase present as a key (empty phases map to `[]`).
fn resolved_prereqs_json(deps: &Dependencies) -> Value {
    let phases = [
        ("configure", &deps.configure),
        ("build", &deps.build),
        ("test", &deps.test),
        ("runtime", &deps.runtime),
        ("develop", &deps.develop),
    ];

    let mut prereqs = serde_json::Map::new();
    for (phase, group) in phases {
        prereqs.insert(phase.to_string(), Value::Array(phase_entries(group)));
    }
    Value::Object(prereqs)
}

/// Print the resolved prerequisites as a `phase` / `relationship` / `module` /
/// `required` / `installed` table that omits the `develop` phase unless
/// `include_develop` is set.
fn print_resolved_prereqs_table(deps: &Dependencies, include_develop: bool, perl: &Perl) {
    let mut table = house_style_table();
    table.set_header(header_row([
        "phase",
        "relationship",
        "module",
        "required",
        "installed",
    ]));
    for (phase, relationship, dep) in flatten_prereqs(deps, include_develop) {
        table.add_row([
            Cell::new(phase),
            Cell::new(relationship),
            Cell::new(&dep.module),
            Cell::new(&dep.version),
            Cell::new(installed_version(perl, &dep.module)),
        ]);
    }
    println!("{table}");
}

/// The version of `module` installed on `perl`'s module search path, for the
/// table's `installed` column: the `$VERSION` declared in its source, `"?"` when
/// it is installed but declares none, or `"-"` when it is not installed.
fn installed_version(perl: &Perl, module: &str) -> String {
    match perl.module(module) {
        Some(found) => found.version.unwrap_or_else(|| "?".to_string()),
        None => "-".to_string(),
    }
}

/// The `{ "relationship", "module", "version" }` entries of one phase, in a
/// stable relationship-then-module order.
fn phase_entries(group: &PhaseDependencies) -> Vec<Value> {
    let relationships = [
        ("requires", &group.requires),
        ("recommends", &group.recommends),
        ("suggests", &group.suggests),
        ("conflicts", &group.conflicts),
    ];

    let mut out = Vec::new();
    for (relationship, list) in relationships {
        for dep in list {
            out.push(json!({
                "relationship": relationship,
                "module": dep.module,
                "version": dep.version,
            }));
        }
    }
    out
}

/// Flatten [`Dependencies`] into `(phase, relationship, dependency)` triples in
/// a stable phase-then-relationship order. The `develop` phase is included only
/// when `include_develop` is set.
fn flatten_prereqs(
    deps: &Dependencies,
    include_develop: bool,
) -> Vec<(&'static str, &'static str, &Dependency)> {
    let mut phases = vec![
        ("configure", &deps.configure),
        ("build", &deps.build),
        ("test", &deps.test),
        ("runtime", &deps.runtime),
    ];
    if include_develop {
        phases.push(("develop", &deps.develop));
    }

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

/// The exit code this process should use for `step`'s [`ExecuteResult`],
/// reporting failures on stderr as a side effect. `0` on success; the child's
/// code (coerced into `1..=255`) on a non-zero exit; `1` when it was killed by a
/// signal.
fn step_exit_code(step: &str, result: &ExecuteResult) -> u8 {
    if result.is_success {
        return 0;
    }

    match result.code {
        Some(code) => {
            eprintln!("uperl-dist: the {step} step exited with status {code}");
            let byte = u8::try_from(code).unwrap_or(1);
            if byte == 0 { 1 } else { byte }
        }
        None => {
            eprintln!("uperl-dist: the {step} step was terminated by a signal");
            1
        }
    }
}
