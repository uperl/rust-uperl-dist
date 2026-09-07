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

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use cpan_distribution_build::{
    BuildTool, Dependencies, Dependency, Distribution, ExecuteResult, Perl,
};

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
    /// (the distribution's `configure` requires, plus the build tool itself),
    /// one `MODULE<TAB>VERSION-RANGE` per line. Nothing is executed.
    PreConfigure,

    /// Run the configure step: `perl Makefile.PL` or `perl Build.PL`.
    Configure {
        /// After configuring, print the resolved prerequisites (taken from
        /// `MYMETA` when the configure step wrote one) as
        /// `PHASE<TAB>RELATIONSHIP<TAB>MODULE<TAB>VERSION-RANGE` lines.
        #[arg(long)]
        show_prereqs: bool,
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
            print_dependency_list(&dist.execute_pre_configure());
            Ok(ExitCode::SUCCESS)
        }
        Command::Configure { show_prereqs } => {
            let (result, deps) = dist
                .execute_configure()
                .context("the configure step could not be started")?;
            if show_prereqs {
                print_resolved_prereqs(&deps);
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

/// Print a flat dependency list as `MODULE<TAB>VERSION-RANGE` lines.
fn print_dependency_list(deps: &[Dependency]) {
    for dep in deps {
        println!("{}\t{}", dep.module, dep.version);
    }
}

/// Print every non-empty prerequisite group as
/// `PHASE<TAB>RELATIONSHIP<TAB>MODULE<TAB>VERSION-RANGE` lines.
fn print_resolved_prereqs(deps: &Dependencies) {
    let phases = [
        ("configure", &deps.configure),
        ("build", &deps.build),
        ("test", &deps.test),
        ("runtime", &deps.runtime),
        ("develop", &deps.develop),
    ];

    for (phase, group) in phases {
        let relationships = [
            ("requires", &group.requires),
            ("recommends", &group.recommends),
            ("suggests", &group.suggests),
            ("conflicts", &group.conflicts),
        ];
        for (relationship, list) in relationships {
            for dep in list {
                println!("{phase}\t{relationship}\t{}\t{}", dep.module, dep.version);
            }
        }
    }
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
