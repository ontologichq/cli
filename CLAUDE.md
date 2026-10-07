# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`ontologic-cli`, the `ontologic` command line for an ontologic engine: it signs in, calls the
engine over gRPC with TLS through `ontologic-kit`, and renders each reply, imports and questions
stage by stage.

This repository is public: no server addresses, keys, real people's or customer names, engine
prompts, model answer formats or email content go in it, in code, tests, comments or commit
messages. Tests and docs use invented names (Maya Chen, Priya Raman, Tomas, Acme, Lumenworks).

## Layout

- `src/main.rs`: flags (`-h`, `-u` or `ONTOLOGIC_USER`, `-p` or `ONTOLOGIC_KEY`, `--ca`,
  `--no-color`), connecting, and two loops: interactive (rustyline, history in
  `.ontologic_history`) and piped (stdin is not a terminal; a prompt is printed before each line).
- `src/commands.rs`: `Cli` and what each command does (call the engine, render the reply),
  `HELP`, the startup banner (`Me`, `Health`, `ListTenants`; a wrong key exits with 1), and the
  session's cost.
- `src/input.rs`: a line parsed into a `Command`, colors while typing, and tab completion from
  `Names`, the tenants and ids earlier replies named, so a tab never waits on the network.
- `src/render.rs`: how replies look: a header rule per command, a label column, one function per
  reply or event.
- `src/paint.rs`: ANSI colors, only when stdout is a terminal and `NO_COLOR` is unset;
  percentages green from 75%, yellow from 40%, red below.
- `build.rs`, `build/version.rs`: `ONTOLOGIC_VERSION` (the commit, `-dirty`, the build time),
  which `\v` prints. Setting it in the environment names the commit of a copy without `.git`.

The API (`ontologic_kit::pb`), the client (`connect`, `SignIn`, `describe`, `TRUST_DIR`) and the
fake engine come from [kit](https://github.com/ontologichq/kit), pinned in Cargo.toml by tag, or by
revision until kit's next release is tagged (now kit's `sprint-27/ask-log`, to be v0.3.0). To
change kit and the CLI together, patch it to a sibling checkout in a gitignored
`.cargo/config.toml` (`[patch."https://github.com/ontologichq/kit"] ontologic-kit = { path =
"../kit" }`), and bump the tag once kit is released. Code a second repository needs moves into kit.

## Tests

- Unit tests sit next to the code: `input.rs` (parsing, completion, highlighting) and `paint.rs`.
- `tests/cli.rs` runs the real binary (`CARGO_BIN_EXE_ontologic`) against
  `ontologic_kit::fake::FakeEngine`, a TLS server on 127.0.0.1 with a fresh certificate. A test
  scripts the replies (`reply`, `fail`, `stream`; used in order, the last one repeats), pipes
  commands in with `-h <fake host> -u <user> -p <key> --ca <its certificate>` from a fresh
  directory under `target/tmp`, asserts on substrings of what the CLI printed (stdout is a pipe,
  so no colors), and checks what the engine got with `calls()`. At startup the CLI calls `Me`,
  `Health` and `ListTenants`, which take the first scripted reply of each.
- What the CLI prints is its interface: a change to the rendering changes the expected text in
  `tests/cli.rs` in the same commit.

## Commands

```bash
make check                      # fmt --check, clippy --all-targets -D warnings, tests
make fmt
cargo test --test cli <name>    # one end-to-end test
cargo test input::tests         # one module's unit tests
make install                    # cargo install --path . --locked
make run P=<key>                # the CLI on HOST (localhost:6969) as U (admin)
```

## Conventions

- Branches `<issue>/<slug>`, Conventional Commits, no direct commits to `main`.
- No AI co-author trailers in commits and no generated-by footers in pull requests.
- No em dashes in docs, comments or commit messages.
