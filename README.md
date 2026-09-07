# uperl-dist

A small CLI that drives the build lifecycle of an **already-unpacked** CPAN
distribution one step at a time, on top of
[`cpan-distribution-build`](https://github.com/uperl/rust-cpan-distribution-build).

Each subcommand runs one phase and exits with that phase's status:

| subcommand      | `ExtUtils::MakeMaker` | `Module::Build`        |
|-----------------|-----------------------|------------------------|
| `pre-configure` | list configure prereqs (nothing is run) | list configure prereqs |
| `configure`     | `perl Makefile.PL`    | `perl Build.PL`        |
| `build`         | `make`                | `perl Build`           |
| `test`          | `make test`           | `perl Build test`      |
| `install`       | `make install`        | `perl Build install`   |
| `clean`         | `make clean`          | `perl Build clean`     |
| `distclean`     | `make distclean`      | `perl Build distclean` |

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
| `-j`, `--json`            | print computed prerequisites as JSON instead of a table |

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

uperl-dist clean                         # remove build products
uperl-dist distclean                     # also remove the generated Makefile / Build
```

### Output

`pre-configure` prints the configure prerequisites (the distribution's
`configure` requires plus the build tool itself) as a table in the same style as
`uperl-metacpan`, with the required range next to the version found on the
interpreter's search path (`-` = not installed, `?` = installed but declares no
version):

```
┌─────────────────────┬─────────┬───────────┐
│ module              ┆ version ┆ installed │
╞═════════════════════╪═════════╪═══════════╡
│ ExtUtils::MakeMaker ┆ 0       ┆ 7.76      │
├╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┼╌╌╌╌╌╌╌╌╌┼╌╌╌╌╌╌╌╌╌╌╌┤
│ File::Which         ┆ 1.09    ┆ 1.27      │
└─────────────────────┴─────────┴───────────┘
```

`configure` prints the resolved prerequisites — from `MYMETA` when the configure
step wrote one, otherwise from `META` — as a `phase` / `relationship` / `module`
/ `version` / `installed` table. Pass `--no-prereqs` to suppress it. The table
omits the `develop` phase unless `--include-develop` is given; `--json` output
always includes it.

The `installed` column is table-only; `--json` output is unchanged.

Every subcommand except `pre-configure` passes `perl` / `make` output straight
through and exits with the child's status; a failing step is a non-zero exit,
not a panic.

### `--json`

`--json` (`-j`) works with every subcommand. It prints a single JSON object on
stdout and nothing else — the child's output is captured rather than streamed,
so stdout stays valid JSON. The object always has `output` (the child's merged
stdout+stderr; `""` for `pre-configure`, which runs nothing), `exit` (the
numeric exit code) and `success` (a boolean); `pre-configure` / `configure` also
add a `prereqs` object keyed by phase:

```json
{
  "prereqs": {
    "configure": [ { "relationship": "requires", "module": "ExtUtils::MakeMaker", "version": "0" } ],
    "build": [],
    "test": [ { "relationship": "requires", "module": "Test::More", "version": "0.88" } ],
    "runtime": [ { "relationship": "requires", "module": "perl", "version": "5.010" } ],
    "develop": []
  },
  "output": "Generating a Unix-style Makefile\n...",
  "exit": 0,
  "success": true
}
```

`configure` emits all five CPAN phases (empty ones as `[]`, `develop` always
included regardless of `--include-develop`); `pre-configure` emits only
`configure`, with `module` / `version` entries, and always reports `exit` 0 /
`success` true. `build`, `test`, `install`, `clean` and `distclean` emit just
`output` / `exit` / `success`. `--no-prereqs` drops the `prereqs` key from
`configure`'s object.
