# uperl-dist

A small CLI that drives the build lifecycle of an **already-unpacked** CPAN
distribution one step at a time, on top of
[`cpan-distribution-build`](https://github.com/uperl/rust-cpan-distribution-build).

Each subcommand runs one phase and exits with that phase's status:

| subcommand      | `ExtUtils::MakeMaker` | `Module::Build`      |
|-----------------|-----------------------|----------------------|
| `pre-configure` | list configure prereqs (nothing is run) | list configure prereqs |
| `configure`     | `perl Makefile.PL`    | `perl Build.PL`      |
| `build`         | `make`                | `perl Build`         |
| `test`          | `make test`           | `perl Build test`    |
| `install`       | `make install`        | `perl Build install` |

The build tool is picked from the scripts present in the distribution directory;
when it ships both `Makefile.PL` and `Build.PL`, `--prefer` decides (default
`mb`).

## Usage

```
uperl-dist [OPTIONS] <COMMAND>
```

Global options (accepted before or after the subcommand):

| option | meaning |
|--------|---------|
| `-C`, `--directory <DIR>` | distribution directory (default `.`) |
| `--perl <PATH>`           | interpreter to build with (default: first `perl` on `PATH`) |
| `--make <PATH>`           | `make` for EUMM distributions (default: first `make` on `PATH`) |
| `--install-base <DIR>`    | install prefix, like `local::lib` / `INSTALL_BASE` |
| `--lib <DIR>`             | directory to add to `PERL5LIB`; repeatable |
| `--prefer <eumm\|mb>`     | build tool when both configure scripts exist (default `mb`) |

`--install-base` and `--lib` affect the configure step's generated
`Makefile` / `Build` script, so pass them to `configure` (and, harmlessly, to the
later steps) — not to `install` alone.

### Example

```sh
tar xf Your-Dist-1.23.tar.gz
cd Your-Dist-1.23

uperl-dist pre-configure                 # what to install before configuring
uperl-dist --install-base ~/perl5 configure
uperl-dist --install-base ~/perl5 build
uperl-dist --install-base ~/perl5 test
uperl-dist --install-base ~/perl5 install
```

### Output

`pre-configure` prints the configure prerequisites (the distribution's
`configure` requires plus the build tool itself), one per line:

```
MODULE<TAB>VERSION-RANGE
```

`configure --show-prereqs` prints the resolved prerequisites — from `MYMETA`
when the configure step wrote one, otherwise from `META` — as:

```
PHASE<TAB>RELATIONSHIP<TAB>MODULE<TAB>VERSION-RANGE
```

`configure`, `build`, `test` and `install` pass `perl` / `make` output straight
through and exit with the child's status; a failing step is a non-zero exit, not
a panic.
