# cli

`ontologic`, the command line for an ontologic engine. It signs in to an engine over gRPC with
TLS, and every command calls the engine and prints what comes back: tenants, imports and
questions stage by stage, what a tenant knows, and who can use it.

## Install

```bash
cargo install --locked --git https://github.com/ontologichq/cli --tag v0.1.0
```

This puts `ontologic` in `~/.cargo/bin`. From a checkout, `make install` does the same.

## Connect

```bash
export ONTOLOGIC_USER=maya ONTOLOGIC_KEY=<your key>
ontologic -h engine.example.com:6969
```

- `-h`, `--host`: the engine, `localhost:6969` when left out. A host without a port gets 6969.
  `https://` is assumed; `http://` reaches an engine run without TLS.
- `-u`, `--user` and `-p`, `--key`: who you are, or `ONTOLOGIC_USER` and `ONTOLOGIC_KEY`, which
  keep the key out of your shell history. An admin gives you a user and a key.
- `--ca <pem>`: the certificate to trust. Without it, the CLI looks in `.ontologic/tls` in the
  directory it runs from: `engine.pem` for `localhost`, `<host>.pem` for any other host
  (`.ontologic/tls/engine.example.com.pem`). When that file does not exist, the web's root
  certificates are trusted, as for an engine with a public certificate.
- `--no-color`: plain output. Colors are also off when stdout is not a terminal or `NO_COLOR` is
  set.

A wrong user or key ends the CLI at startup. An engine that cannot be reached is reported, and
every command says so until it can be reached again.

Commands can be piped in, one per line:

```bash
printf '\\t checkout acme\n\\ask who founded Lumenworks\n' | ontologic -h engine.example.com:6969
```

Typed commands keep a history in `.ontologic_history` in the current directory, and tab completes
commands, tenant names, ids and file names.

## Commands

`\h` prints the full list. Admins use every tenant, create and delete tenants, and manage users;
members use the tenants an admin granted them.

| Command | What it does |
| --- | --- |
| `\t create <name>`, `\t checkout <name>` | create a tenant and switch to it, or switch to one |
| `\t get` | list the tenants; `*` marks the one you are on |
| `\t delete <name>` | forget a tenant and everything in it |
| `\t users` | who can use the current tenant |
| `\t meta [name]` | a tenant's counts, files, dates, and cost per import and question |
| `\t export <file>`, `\t import <file>` | save the tenant to a `.ttl` file, or load one into an empty tenant |
| `\import fact <text>` | add a fact: link things, find relations, re-link older sources |
| `\import blob <file>` | add a file: an email (`.eml`) or text, part by part |
| `\commit`, `\rollback` | make the staged imports part of the tenant, or drop them |
| `\retract <doc...> [reason]`, `\restore <doc...>` | take documents (`d3`) out of every answer, count and link, or put them back |
| `\erase <doc...> [reason]` | admins: destroy documents for good, once you type the tenant's name to confirm |
| `\migrate <kind:version ...> [reason]` | admins: show what reading the tenant under other schema versions changes, then commit it when you type `yes` |
| `\acl <doc> <principal...>` | admins: who may read a document (`user:<name>` or a group) |
| `\principals <user> <group...>` | admins: the groups a member reads the current tenant's documents as |
| `\ask <question>` | answer from what the tenant has committed, with probabilities |
| `\ask --staged <q>`, `\ask --facts <q>` | include the imports not committed yet; answer from facts alone |
| `\s`, `\s <id>` | what the tenant knows and what it cost; everything about an entity (`e1`) or a source (`s1`) |
| `\u me` | your user, role and tenants |
| `\u add <user>` | admins: add a member and print their key, once |
| `\u grant <user> <tenant>`, `\u remove <user> <tenant>` | admins: let a member use a tenant, or stop them |
| `\v` | the engine's version and this CLI's |
| `\q` | quit (Ctrl-D works too) |

File names in commands are relative to the directory the CLI runs from.

A fact shows how sure the engine is of it and, from an engine that ranks its facts, its rung: what
it may be used for (`proposed`, `supported`, `accepted`, `computable` or `identity-trusted`). An
answer that is a number the engine computed shows its floor and its ceiling, and how far it can be
claimed. An answer a check doubts is shown with `unsure` and why; one whose options a check dropped
says `withheld` and why.

## Develop

The engine's API and the client come from [kit](https://github.com/ontologichq/kit). The tests
run the real binary against kit's fake engine, so they need no engine.

```bash
make check   # formatting, clippy with warnings as errors, tests
make run P=<key> HOST=localhost:6969 U=admin
```

To change kit and the CLI together, point Cargo at a local kit checkout in a gitignored
`.cargo/config.toml`:

```toml
[patch."https://github.com/ontologichq/kit"]
ontologic-kit = { path = "../kit" }
```

Licensed under the Apache License, Version 2.0.
