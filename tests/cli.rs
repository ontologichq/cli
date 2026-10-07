//! The real `ontologic` binary against kit's fake engine: commands are piped into it, what it
//! prints comes back, and the fake engine remembers every call it got. No engine, model or
//! network is needed.
//!
//! Stdout is a pipe, so the CLI prints no colors and assertions match plain text.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ontologic_kit::fake::{Call, FakeEngine, Rpc};
use ontologic_kit::{SIGN_IN_FAILED, pb};
use tonic::Status;

use pb::ask_event::Event as Ask;
use pb::import_event::Event as Import;
use pb::show_reply::View;

/// The fake engines sign in this user with this key.
const USER: &str = "maya";
const KEY: &str = "invented-key";

/// A fresh, empty working directory for one test: the CLI looks for certificates there and
/// reads and writes files there.
fn workdir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn user(name: &str, role: &str, tenants: &[&str]) -> pb::User {
    pb::User {
        name: name.into(),
        role: role.into(),
        tenants: tenants.iter().map(|t| t.to_string()).collect(),
        principals: Vec::new(),
    }
}

fn health() -> pb::HealthReply {
    pb::HealthReply {
        llm: "fake-model (reasoning low)".into(),
        llm_error: String::new(),
        tagger: "fake-tagger".into(),
        version: "0abc123, built 2026-09-14 05:12 UTC".into(),
        embedder: "fake-embedder".into(),
    }
}

/// A fake engine that signs in `USER` with `KEY` as `role` and answers what the CLI asks when it
/// starts: `Me`, `Health`, and `ListTenants` with no tenants. Tests add the replies they need.
fn engine_as(role: &str, tenants: &[&str]) -> FakeEngine {
    let engine = FakeEngine::start();
    engine.sign_in(USER, KEY);
    engine.reply(Rpc::Me, user(USER, role, tenants));
    engine.reply(Rpc::Health, health());
    engine.reply(Rpc::ListTenants, pb::TenantList::default());
    engine
}

fn engine() -> FakeEngine {
    engine_as("admin", &[])
}

/// The engine's certificate, written into `dir` to pass with `--ca`.
fn trust(engine: &FakeEngine, dir: &Path) -> String {
    let ca = dir.join("engine-ca.pem");
    std::fs::write(&ca, engine.ca_pem()).unwrap();
    ca.to_str().unwrap().to_string()
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Runs `ontologic` in `dir` with `args` and `input` on stdin until the input ends. The sign-in
/// variables are removed from the environment, then `env` is set.
fn run(dir: &Path, args: &[&str], env: &[(&str, &str)], input: &str) -> Run {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ontologic"));
    command
        .current_dir(dir)
        .args(args)
        .env_remove("ONTOLOGIC_USER")
        .env_remove("ONTOLOGIC_KEY")
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn().unwrap();
    // A CLI that exits at startup may close stdin before it is written; its output says why.
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    let output = child.wait_with_output().unwrap();
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

/// Runs `ontologic` signed in as `USER`, trusting `engine`; expects a clean exit and returns
/// what it printed.
fn ontologic(engine: &FakeEngine, dir: &Path, input: &str) -> String {
    let ca = trust(engine, dir);
    let args = ["-h", engine.host(), "-u", USER, "-p", KEY, "--ca", &ca];
    let run = run(dir, &args, &[], input);
    assert_eq!(
        run.code, 0,
        "ontologic exited with {}:\n{}{}",
        run.code, run.stdout, run.stderr
    );
    run.stdout
}

#[track_caller]
fn assert_has(output: &str, needle: &str) {
    assert!(
        output.contains(needle),
        "expected {needle:?} in the output:\n{output}"
    );
}

#[track_caller]
fn assert_lacks(output: &str, needle: &str) {
    assert!(
        !output.contains(needle),
        "did not expect {needle:?} in the output:\n{output}"
    );
}

fn calls_to(engine: &FakeEngine, rpc: Rpc) -> Vec<Call> {
    engine
        .calls()
        .into_iter()
        .filter(|c| c.rpc == rpc)
        .collect()
}

fn rpcs(engine: &FakeEngine) -> Vec<Rpc> {
    engine.calls().iter().map(|c| c.rpc).collect()
}

/// The tenant names of every call to `rpc`, in order.
fn tenants_named(engine: &FakeEngine, rpc: Rpc) -> Vec<String> {
    calls_to(engine, rpc)
        .iter()
        .map(|c| c.request::<pb::TenantName>().name)
        .collect()
}

fn tenant(name: &str, entities: u32, sources: u32, relations: u32) -> pb::TenantSummary {
    pb::TenantSummary {
        name: name.into(),
        entities,
        sources,
        relations,
        ..Default::default()
    }
}

fn guess(id: &str, name: &str, confidence: f32) -> pb::Guess {
    pb::Guess {
        entity_id: id.into(),
        entity_name: name.into(),
        confidence,
    }
}

fn decision(subject: &str, accepted: bool, detail: &str) -> pb::Decision {
    pb::Decision {
        subject: subject.into(),
        accepted,
        detail: detail.into(),
    }
}

/// A fact from `subject` to the entity `object_id`, or to a value when `object_id` is empty.
fn fact(
    id: &str,
    subject: &str,
    predicate: &str,
    object_id: &str,
    object: &str,
    confidence: f32,
) -> pb::Relation {
    pb::Relation {
        id: id.into(),
        subject_name: subject.into(),
        predicate: predicate.into(),
        object_id: object_id.into(),
        object_name: object.into(),
        confidence,
        ..Default::default()
    }
}

fn link(text: &str, guesses: Vec<pb::Guess>) -> pb::Segment {
    pb::Segment {
        text: text.into(),
        link: true,
        guesses,
    }
}

fn plain(text: &str) -> pb::Segment {
    pb::Segment {
        text: text.into(),
        link: false,
        guesses: Vec::new(),
    }
}

fn candidate(text: &str, kind: &str) -> pb::Candidate {
    pb::Candidate {
        text: text.into(),
        kind: kind.into(),
        probability: 0.8,
    }
}

fn cite(id: &str, label: &str, stored: Option<f32>) -> pb::Citation {
    pb::Citation {
        id: id.into(),
        label: label.into(),
        stored,
    }
}

fn import_event(event: Import) -> Result<pb::ImportEvent, Status> {
    Ok(pb::ImportEvent { event: Some(event) })
}

fn ask_event(event: Ask) -> Result<pb::AskEvent, Status> {
    Ok(pb::AskEvent { event: Some(event) })
}

#[test]
fn the_banner_names_the_engine_its_models_and_who_signed_in() {
    let engine = engine();
    let dir = workdir("banner");
    let out = ontologic(&engine, &dir, "\\h\n\\v\nhello\n\\q\n\\v\n");
    assert!(
        out.starts_with(&format!(
            "ontologic \\h for help, \\q to quit\nengine: {} · llm fake-model (reasoning low) · \
             tagger fake-tagger · embedder fake-embedder · signed in as maya (admin)\n",
            engine.host()
        )),
        "{out}"
    );
    assert_has(
        &out,
        "ontologic> \\t create <name>      create a tenant and switch to it\n",
    );
    assert_has(
        &out,
        "\\q                    quit (Ctrl-D works too)\nUp and down arrows walk the history.\n",
    );
    assert_has(&out, "ontologic> ── version ──");
    assert_has(
        &out,
        &format!(
            " engine   0abc123, built 2026-09-14 05:12 UTC ({})\n",
            engine.host()
        ),
    );
    // The CLI's own build, stamped when it was compiled.
    let cli = out.lines().find(|l| l.starts_with(" cli      ")).unwrap();
    assert!(cli.contains(", built 20"), "{cli}");
    assert_has(
        &out,
        " models   llm fake-model (reasoning low) · tagger fake-tagger · embedder fake-embedder\n",
    );
    assert_has(&out, "ontologic> error: unknown command hello, see \\h\n");
    // \q ends the session, so the second \v never asks the engine.
    assert_eq!(
        rpcs(&engine),
        [Rpc::Me, Rpc::Health, Rpc::ListTenants, Rpc::Health]
    );
}

#[test]
fn an_engine_without_a_model_warns_that_imports_and_questions_will_fail() {
    let engine = FakeEngine::start();
    engine.sign_in(USER, KEY);
    engine.reply(Rpc::Me, user(USER, "member", &["acme"]));
    engine.reply(
        Rpc::Health,
        pb::HealthReply {
            llm: String::new(),
            llm_error: "no model is configured".into(),
            ..health()
        },
    );
    let dir = workdir("no-model");
    let out = ontologic(&engine, &dir, "");
    assert_has(
        &out,
        &format!(
            "engine: {} · signed in as maya (member)\nwarning: the engine has no LLM configured \
             (no model is configured); \\import and \\ask will fail\n",
            engine.host()
        ),
    );
}

#[test]
fn a_wrong_key_ends_the_cli_at_startup_with_the_engines_message() {
    let engine = engine();
    let dir = workdir("wrong-key");
    let ca = trust(&engine, &dir);
    let args = [
        "-h",
        engine.host(),
        "-u",
        USER,
        "-p",
        "not-the-key",
        "--ca",
        &ca,
    ];
    let run = run(&dir, &args, &[], "\\t get\n");
    assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
    assert_has(&run.stdout, &format!("error: {SIGN_IN_FAILED}\n"));
    assert_lacks(&run.stdout, "engine:");
    assert_lacks(&run.stdout, "ontologic> ");
    assert_eq!(rpcs(&engine), [Rpc::Me], "nothing is called after that");
}

#[test]
fn signing_in_takes_a_user_and_key_from_flags_or_the_environment() {
    let engine = engine();
    let dir = workdir("sign-in");
    let ca = trust(&engine, &dir);

    let missing = run(
        &dir,
        &["-h", engine.host(), "-u", USER, "--ca", &ca],
        &[],
        "",
    );
    assert_eq!(missing.code, 2);
    assert_has(
        &missing.stderr,
        "sign in with -u <user> -p <key>\nusage: ontologic -h <host:port> -u <user> -p <key>",
    );
    assert_has(
        &missing.stderr,
        "  --ca         trust this certificate (default: .ontologic/tls/engine.pem for localhost,\n\
         \x20              .ontologic/tls/<host>.pem for other hosts, when the file exists)\n",
    );
    let unknown = run(&dir, &["--colour"], &[], "");
    assert_eq!(unknown.code, 2);
    assert_has(
        &unknown.stderr,
        "unknown argument --colour\nusage: ontologic",
    );
    assert!(engine.calls().is_empty(), "{:?}", rpcs(&engine));

    let signed_in = [("ONTOLOGIC_USER", USER), ("ONTOLOGIC_KEY", KEY)];
    let from_env = run(&dir, &["-h", engine.host(), "--ca", &ca], &signed_in, "");
    assert_eq!(from_env.code, 0, "{}{}", from_env.stdout, from_env.stderr);
    assert_has(&from_env.stdout, "signed in as maya (admin)\n");
}

#[test]
fn every_call_signs_in_with_the_user_and_key() {
    let engine = FakeEngine::start();
    engine.sign_in("priya_raman", "another-invented-key");
    engine.reply(Rpc::Me, user("priya_raman", "member", &["acme"]));
    engine.reply(Rpc::Health, health());
    engine.reply(Rpc::ListTenants, pb::TenantList::default());
    engine.reply(Rpc::GetTenant, tenant("acme", 0, 0, 0));
    engine.reply(Rpc::Show, pb::ShowReply::default());
    engine.stream(
        Rpc::Ask,
        vec![ask_event(Ask::Finished(pb::Finished::default()))],
    );
    engine.reply(Rpc::Commit, tenant("acme", 0, 0, 0));
    let dir = workdir("every-call");
    let ca = trust(&engine, &dir);
    let args = [
        "-h",
        engine.host(),
        "-u",
        "priya_raman",
        "-p",
        "another-invented-key",
        "--ca",
        &ca,
    ];
    let input = "\\t checkout acme\n\\t get\n\\ask who is here\n\\commit\n\\u me\n\\v\n";
    let run = run(&dir, &args, &[], input);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert_lacks(&run.stdout, "error:");
    assert_eq!(
        rpcs(&engine),
        [
            Rpc::Me,
            Rpc::Health,
            Rpc::ListTenants,
            Rpc::GetTenant,
            Rpc::Show,
            Rpc::ListTenants,
            Rpc::Ask,
            Rpc::Commit,
            Rpc::Me,
            Rpc::Health
        ]
    );
    for call in engine.calls() {
        assert_eq!(
            (call.user.as_str(), call.key.as_str()),
            ("priya_raman", "another-invented-key"),
            "{:?}",
            call.rpc
        );
    }
}

#[test]
fn an_engine_whose_certificate_is_not_trusted_is_refused() {
    let engine = engine();
    let other = FakeEngine::start();
    let dir = workdir("wrong-certificate");
    let ca = trust(&other, &dir);
    let args = ["-h", engine.host(), "-u", USER, "-p", KEY, "--ca", &ca];
    let run = run(&dir, &args, &[], "\\t get\n");
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    let refused = format!(
        "error: cannot reach engine at {} (is the engine running?): invalid peer certificate",
        engine.host()
    );
    assert_has(
        &run.stdout,
        &format!("ontologic \\h for help, \\q to quit\n{refused}"),
    );
    assert_has(&run.stdout, &format!("ontologic> {refused}"));
    assert_lacks(&run.stdout, "signed in as");
    assert!(engine.calls().is_empty(), "no call gets past the handshake");
}

#[test]
fn a_certificate_in_the_trust_directory_is_used_without_ca() {
    let engine = engine();
    let dir = workdir("trust-directory");
    std::fs::create_dir_all(dir.join(".ontologic/tls")).unwrap();
    std::fs::write(dir.join(".ontologic/tls/engine.pem"), engine.ca_pem()).unwrap();
    let args = ["-h", engine.host(), "-u", USER, "-p", KEY];
    let trusted = run(&dir, &args, &[], "");
    assert_eq!(trusted.code, 0, "{}{}", trusted.stdout, trusted.stderr);
    assert_has(
        &trusted.stdout,
        &format!("engine: {} · llm fake-model", engine.host()),
    );

    // Without the file the web's roots are trusted, and they never signed the engine's.
    std::fs::remove_dir_all(dir.join(".ontologic")).unwrap();
    let untrusted = run(&dir, &args, &[], "");
    assert_has(
        &untrusted.stdout,
        &format!(
            "error: cannot reach engine at {} (is the engine running?): invalid peer certificate",
            engine.host()
        ),
    );
}

#[test]
fn an_engine_that_is_not_running_is_described_on_every_command() {
    let dir = workdir("not-running");
    let args = ["-h", "127.0.0.1:1", "-u", USER, "-p", KEY];
    let run = run(&dir, &args, &[], "\\t get\n");
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    let unreachable = "error: cannot reach engine at 127.0.0.1:1 (is the engine running?): ";
    assert!(
        run.stdout.starts_with(&format!(
            "ontologic \\h for help, \\q to quit\n{unreachable}"
        )),
        "{}",
        run.stdout
    );
    assert_has(&run.stdout, &format!("ontologic> {unreachable}"));
    assert_has(&run.stdout, "refused");
    assert_lacks(&run.stdout, "engine: ");
}

#[test]
fn tenants_are_created_listed_checked_out_and_deleted() {
    let engine = engine();
    engine.reply(Rpc::ListTenants, pb::TenantList::default());
    engine.reply(
        Rpc::ListTenants,
        pb::TenantList {
            tenants: vec![
                pb::TenantSummary {
                    cost: Some(pb::Cost::default()),
                    ..tenant("acme", 0, 0, 0)
                },
                pb::TenantSummary {
                    cost: Some(pb::Cost {
                        calls: 3,
                        dollars: Some(0.0042),
                        ..Default::default()
                    }),
                    staged: Some(pb::Staged {
                        sources: 2,
                        entities: 1,
                        relations: 1,
                    }),
                    ..tenant("beta", 2, 1, 1)
                },
            ],
        },
    );
    engine.reply(Rpc::CreateTenant, tenant("acme", 0, 0, 0));
    engine.reply(Rpc::GetTenant, tenant("beta", 2, 1, 1));
    engine.reply(
        Rpc::Show,
        pb::ShowReply {
            view: Some(View::Overview(pb::Overview {
                summary: Some(tenant("beta", 2, 1, 1)),
                source_ids: vec!["s1".into()],
                ..Default::default()
            })),
        },
    );
    engine.reply(
        Rpc::TenantUsers,
        pb::UserList {
            users: vec![
                user("maya", "admin", &[]),
                user("priya_raman", "member", &["beta"]),
            ],
        },
    );
    engine.reply(Rpc::DeleteTenant, tenant("acme", 0, 0, 0));
    let dir = workdir("tenants");
    let out = ontologic(
        &engine,
        &dir,
        "\\t get\n\\t create acme\n\\t get\n\\t checkout beta\n\\t get\n\\t users\n\
         \\t delete acme\n\\t wat\n",
    );
    assert_has(&out, "ontologic> no tenants (\\t create <name>)\n");
    assert_has(&out, "ontologic> tenant acme created\n");
    assert_has(
        &out,
        "acme> * acme  0 entities · 0 sources · 0 relations · 0 calls\n\
         \x20 beta  2 entities · 1 source · 1 relation · 3 calls · $0.0042 · 2 imports staged\n",
    );
    assert_has(&out, "acme> tenant beta\n");
    assert_has(
        &out,
        "beta>   acme  0 entities · 0 sources · 0 relations · 0 calls\n* beta  2 entities",
    );
    assert_has(&out, "beta> ── users of beta ──");
    assert_has(&out, "\n maya         admin\n priya_raman  member\n");
    assert_has(
        &out,
        "beta> deleted acme: 0 entities · 0 sources · 0 relations\n",
    );
    assert_has(
        &out,
        "beta> error: usage: \\t create <name> | \\t checkout <name> | \\t get",
    );
    assert_eq!(tenants_named(&engine, Rpc::CreateTenant), ["acme"]);
    assert_eq!(tenants_named(&engine, Rpc::GetTenant), ["beta"]);
    assert_eq!(tenants_named(&engine, Rpc::TenantUsers), ["beta"]);
    assert_eq!(tenants_named(&engine, Rpc::DeleteTenant), ["acme"]);
    // A checkout reads the tenant's overview for tab completion and prints none of it.
    let shown: Vec<pb::ShowRequest> = calls_to(&engine, Rpc::Show)
        .iter()
        .map(Call::request)
        .collect();
    assert_eq!(
        shown,
        [pb::ShowRequest {
            tenant: "beta".into(),
            id: String::new()
        }]
    );
}

#[test]
fn an_import_prints_each_stage_as_the_engine_sends_it() {
    let engine = engine();
    engine.reply(Rpc::CreateTenant, tenant("acme", 0, 0, 0));
    let text = "Maya Chen joined Lumenworks in Toronto with Tomas.";
    engine.stream(
        Rpc::Import,
        vec![
            import_event(Import::Source(pb::SourceSaved {
                id: "s3".into(),
                text: text.into(),
                kind: "fact".into(),
                name: String::new(),
                parts: 1,
            })),
            import_event(Import::Code(pb::CodePass {
                matches: vec![
                    pb::CodeMatch {
                        text: "Maya Chen".into(),
                        guesses: vec![guess("e1", "Maya Chen", 1.0)],
                    },
                    pb::CodeMatch {
                        text: "Lumenworks".into(),
                        guesses: vec![
                            guess("e2", "Lumenworks", 0.5),
                            guess("e3", "Lumenworks Labs", 0.5),
                        ],
                    },
                ],
                part: 0,
                text: String::new(),
            })),
            import_event(Import::Linked(pb::Linked {
                llm_error: String::new(),
                mentions: vec![
                    decision("Lumenworks", true, "e2 Lumenworks, 70%"),
                    decision("Toronto", true, "new place"),
                    decision("Tomas", false, "two people are called Tomas"),
                ],
                relations: vec![
                    decision("Maya Chen worksAt Lumenworks", true, "90%"),
                    decision(
                        "Lumenworks livesIn Toronto",
                        false,
                        "livesIn connects a person to a place",
                    ),
                ],
                new_entities: vec![pb::Entity {
                    id: "e4".into(),
                    name: "Toronto".into(),
                    description: "a city".into(),
                    aliases: vec!["Toronto".into()],
                    kind: "place".into(),
                }],
                new_aliases: vec![pb::Alias {
                    entity_id: "e1".into(),
                    entity_name: "Maya Chen".into(),
                    alias: "Maya".into(),
                }],
                new_relations: vec![
                    pb::Relation {
                        new_type: true,
                        ..fact("r1", "Maya Chen", "worksAt", "e2", "Lumenworks", 0.9)
                    },
                    pb::Relation {
                        object_kind: "percent".into(),
                        condition: "fully diluted".into(),
                        rung: "supported".into(),
                        ..fact("r2", "Maya Chen", "holdsShare", "", "40%", 0.85)
                    },
                ],
                segments: vec![
                    link("Maya Chen", vec![guess("e1", "Maya Chen", 1.0)]),
                    plain(" joined "),
                    link(
                        "Lumenworks",
                        vec![
                            guess("e2", "Lumenworks", 0.7),
                            guess("e3", "Lumenworks Labs", 0.3),
                        ],
                    ),
                    plain(" in "),
                    link("Toronto", vec![guess("e4", "Toronto", 1.0)]),
                    plain(" with "),
                    link("Tomas", Vec::new()),
                    plain("."),
                ],
                checklist: Some(pb::Checklist {
                    found: vec![
                        candidate("Maya Chen", "person"),
                        candidate("Lumenworks", "organization"),
                        candidate("Toronto", "place"),
                    ],
                    follow_up: vec!["Toronto".into()],
                    undecided: vec!["Tomas".into()],
                    error: String::new(),
                }),
                more_evidence: vec![pb::MoreEvidence {
                    relation: Some(pb::Relation {
                        rung: "supported".into(),
                        ..fact("r3", "Maya Chen", "livesIn", "e4", "Toronto", 0.8)
                    }),
                    evidence: Some(pb::Evidence {
                        source_id: "s3".into(),
                        start: 0,
                        end: 50,
                        text: text.into(),
                        confidence: 0.8,
                    }),
                }],
                part: 0,
                new_types: vec![pb::RelationType {
                    name: "worksAt".into(),
                    definition: "the subject works for the object".into(),
                    subject_role: "employee".into(),
                    object_role: "employer".into(),
                    inverse: "employs".into(),
                    single_valued: true,
                }],
                timing: Some(pb::Timing {
                    tagger_ms: 1200,
                    model_ms: 6400,
                    code_ms: 3,
                    ..Default::default()
                }),
            })),
            import_event(Import::Relinked(pb::Relinked {
                mentions: vec![pb::Mention {
                    source_id: "s1".into(),
                    text: "Maya".into(),
                    start: 0,
                    end: 4,
                    guesses: vec![guess("e1", "Maya Chen", 1.0)],
                    by: "code".into(),
                    source_text: "Maya founded Lumenworks.".into(),
                }],
            })),
            import_event(Import::Reasked(pb::Reasked {
                source_id: "s1".into(),
                llm_error: String::new(),
                relations: vec![decision("Maya Chen founded Lumenworks", true, "85%")],
                new_relations: vec![fact("r4", "Maya Chen", "founded", "e2", "Lumenworks", 0.85)],
                more_evidence: Vec::new(),
                new_types: vec![pb::RelationType {
                    name: "founded".into(),
                    definition: "the subject started the object".into(),
                    ..Default::default()
                }],
            })),
            import_event(Import::Reasked(pb::Reasked {
                source_id: "s2".into(),
                llm_error: "the model timed out".into(),
                ..Default::default()
            })),
            import_event(Import::Finished(pb::Finished {
                cost: Some(pb::Cost {
                    calls: 3,
                    input_tokens: 1200,
                    output_tokens: 300,
                    reasoning_tokens: 40,
                    unreported_calls: 0,
                    seconds: 7.8,
                    dollars: Some(0.0012),
                    cached_tokens: 200,
                }),
                waiting: 2,
                timing: Some(pb::Timing {
                    tagger_ms: 1200,
                    model_ms: 6400,
                    code_ms: 3,
                    relink_ms: 20,
                    reask_ms: 900,
                    save_ms: 5,
                    context_ms: 0,
                    total_ms: 8600,
                }),
                ..Default::default()
            })),
        ],
    );
    let dir = workdir("import");
    let out = ontologic(
        &engine,
        &dir,
        &format!("\\t create acme\n\\import fact {text}\n"),
    );
    assert_has(&out, "acme> ── import s3 ──");
    assert_has(
        &out,
        "\n source   Maya Chen joined Lumenworks in Toronto with Tomas.\n\
         \x20code     \"Maya Chen\" → e1 Maya Chen 100%\n\
         \x20         \"Lumenworks\" → e2 Lumenworks 50%, e3 Lumenworks Labs 50%\n\
         \x20llm      ✓ \"Lumenworks\" → e2 Lumenworks, 70%\n\
         \x20         ✓ \"Toronto\" → new place\n\
         \x20         ✗ \"Tomas\" → two people are called Tomas\n\
         \x20rel      ✓ Maya Chen worksAt Lumenworks → 90%\n\
         \x20         ✗ Lumenworks livesIn Toronto → livesIn connects a person to a place\n\
         \x20check    3 accepted · 2 rejected\n\
         \x20tagger   3 candidates · 1 asked again · 1 left out: \"Tomas\"\n\
         \x20time     tagger 1.2 s · model 6.4 s · code 3 ms\n\
         \x20new      e4 Toronto (place: a city)\n\
         \x20alias    \"Maya\" → e1 Maya Chen\n\
         \x20type     worksAt  the subject works for the object \
         (employee -> employer; inverse employs; one at a time)\n\
         \x20new      r1 Maya Chen worksAt Lumenworks 90% (new relation type)\n\
         \x20new      r2 Maya Chen holdsShare \"40%\" percent (if fully diluted) 85% supported\n\
         \x20more     r3 Maya Chen livesIn Toronto + s3 80% \
         \"Maya Chen joined Lumenworks in Toronto with Tomas.\" (fact now 80% supported)\n\
         \x20linked   :e1(Maya Chen) joined [Lumenworks? e2 70%, e3 30%] in :e4(Toronto) \
         with [Tomas?].\n\
         \x20relink   s1 \"Maya\" → e1 Maya Chen 100%\n\
         \x20reask    s1 ✓ Maya Chen founded Lumenworks → 85%\n\
         \x20         s1 type founded  the subject started the object\n\
         \x20         s1 new r4 Maya Chen founded Lumenworks 85%\n\
         \x20reask    s2 ✗ error: the model timed out (will retry next import)\n\
         \x20waiting  2 sources still waiting for a relation pass\n\
         \x20cost     3 calls · 1200 in (200 cached) + 300 out (40 reasoning) · 7.8 s · $0.0012\n\
         \x20time     tagger 1.2 s · model 6.4 s · code 3 ms · relink 20 ms · reask 900 ms · \
         save 5 ms · total 8.6 s\nacme> \n",
    );
    let imported: Vec<pb::ImportRequest> = calls_to(&engine, Rpc::Import)
        .iter()
        .map(Call::request)
        .collect();
    assert_eq!(
        imported,
        [pb::ImportRequest {
            tenant: "acme".into(),
            text: text.into(),
            key: String::new()
        }]
    );
}

#[test]
fn a_blob_is_sent_by_its_file_name_and_prints_its_people_and_parts() {
    let engine = engine();
    engine.reply(Rpc::CreateTenant, tenant("acme", 0, 0, 0));
    let dir = workdir("blob");
    std::fs::create_dir_all(dir.join("mail")).unwrap();
    let content = "An invented thread, read only by the fake engine.\n";
    std::fs::write(dir.join("mail/launch.eml"), content).unwrap();
    engine.stream(
        Rpc::ImportBlob,
        vec![
            import_event(Import::Source(pb::SourceSaved {
                id: "s4".into(),
                text: "x".repeat(1234),
                kind: "email".into(),
                name: "launch.eml".into(),
                parts: 2,
            })),
            import_event(Import::People(pb::People {
                new_entities: vec![pb::Entity {
                    id: "e5".into(),
                    name: "Priya Raman".into(),
                    description: String::new(),
                    aliases: vec![
                        "Priya Raman".into(),
                        "Priya".into(),
                        "priya@lumenworks.example".into(),
                    ],
                    kind: "person".into(),
                }],
                new_aliases: vec![pb::Alias {
                    entity_id: "e1".into(),
                    entity_name: "Maya Chen".into(),
                    alias: "maya@lumenworks.example".into(),
                }],
                known: vec![pb::Entity {
                    id: "e1".into(),
                    name: "Maya Chen".into(),
                    kind: "person".into(),
                    ..Default::default()
                }],
                messages: 2,
                new_relations: vec![pb::Relation {
                    object_kind: "contact".into(),
                    ..fact(
                        "r5",
                        "Priya Raman",
                        "hasEmail",
                        "",
                        "priya@lumenworks.example",
                        1.0,
                    )
                }],
                new_types: vec![pb::RelationType {
                    name: "hasEmail".into(),
                    definition: "the subject can be reached at the object".into(),
                    subject_role: "person".into(),
                    object_role: "address".into(),
                    ..Default::default()
                }],
                new_messages: vec![pb::Entity {
                    id: "e6".into(),
                    name: "the launch message from Priya Raman".into(),
                    kind: "event".into(),
                    ..Default::default()
                }],
            })),
            import_event(Import::Code(pb::CodePass {
                matches: Vec::new(),
                part: 1,
                text: "Priya, the launch moves to Friday and the Toronto office opens in May."
                    .into(),
            })),
            import_event(Import::Linked(pb::Linked {
                llm_error: "the model timed out".into(),
                part: 1,
                segments: vec![plain("Priya, the launch moves to Friday.")],
                timing: Some(pb::Timing {
                    model_ms: 180_000,
                    ..Default::default()
                }),
                ..Default::default()
            })),
            import_event(Import::Code(pb::CodePass {
                matches: vec![pb::CodeMatch {
                    text: "Maya".into(),
                    guesses: vec![guess("e1", "Maya Chen", 1.0)],
                }],
                part: 2,
                text: "Maya: see you there.".into(),
            })),
            import_event(Import::Linked(pb::Linked {
                part: 2,
                segments: vec![
                    link("Maya", vec![guess("e1", "Maya Chen", 1.0)]),
                    plain(": see you there."),
                ],
                checklist: Some(pb::Checklist {
                    error: "the tagger is not loaded".into(),
                    ..Default::default()
                }),
                ..Default::default()
            })),
            import_event(Import::Finished(pb::Finished {
                cost: Some(pb::Cost {
                    calls: 1,
                    ..Default::default()
                }),
                waiting: 0,
                timing: None,
                ..Default::default()
            })),
        ],
    );
    let out = ontologic(
        &engine,
        &dir,
        "\\t create acme\n\\import blob mail/launch.eml\n",
    );
    assert_has(&out, "acme> ── import s4 launch.eml ──");
    assert_has(
        &out,
        "\n source   email · 2 parts · 1234 bytes\n\
         \x20people   e5 Priya Raman Priya, priya@lumenworks.example new\n\
         \x20         e1 Maya Chen\n\
         \x20         \"maya@lumenworks.example\" → e1 Maya Chen\n\
         \x20         r5 Priya Raman hasEmail \"priya@lumenworks.example\" contact 100%\n\
         \x20type     hasEmail  the subject can be reached at the object (person -> address)\n\
         \x20messages 2\n\
         \x20         e6 the launch message from Priya Raman new\n\
         \x20part     1 \"Priya, the launch moves to Friday and the Toronto office ope…\"\n\
         \x20code     no known names\n\
         \x20llm      ✗ error: the model timed out (keeping the code links)\n\
         \x20time     model 180.0 s\n\
         \x20linked   Priya, the launch moves to Friday.\n\
         \x20part     2 \"Maya: see you there.\"\n\
         \x20code     \"Maya\" → e1 Maya Chen 100%\n\
         \x20llm      nothing to add\n\
         \x20rel      none\n\
         \x20check    0 accepted · 0 rejected\n\
         \x20tagger   ✗ error: the tagger is not loaded\n\
         \x20linked   :e1(Maya Chen): see you there.\n\
         \x20cost     1 call · 0 in + 0 out (0 reasoning) · 0.0 s\nacme> \n",
    );
    // The engine gets the file's name without its directory, and its bytes.
    let sent: Vec<pb::BlobRequest> = calls_to(&engine, Rpc::ImportBlob)
        .iter()
        .map(Call::request)
        .collect();
    assert_eq!(
        sent,
        [pb::BlobRequest {
            tenant: "acme".into(),
            name: "launch.eml".into(),
            content: content.as_bytes().to_vec(),
            key: String::new()
        }]
    );
}

#[test]
fn a_question_prints_what_it_found_its_options_and_what_they_rest_on() {
    let engine = engine();
    engine.reply(Rpc::CreateTenant, tenant("acme", 0, 0, 0));
    engine.stream(
        Rpc::Ask,
        vec![
            ask_event(Ask::Index(pb::AskIndex {
                matches: vec![pb::CodeMatch {
                    text: "Lumenworks".into(),
                    guesses: vec![guess("e2", "Lumenworks", 1.0)],
                }],
                entities: 3,
                relations: 2,
                sources: 1,
            })),
            ask_event(Ask::Answer(pb::AskAnswer {
                error: String::new(),
                options: vec![
                    pb::AnswerOption {
                        answer: "Maya Chen".into(),
                        entity_id: "e1".into(),
                        entity_name: "Maya Chen".into(),
                        probability: 0.8,
                        based_on: vec![
                            cite("r4", "Maya Chen founded Lumenworks", Some(0.85)),
                            cite("s1", "Maya founded Lumenworks.", None),
                        ],
                    },
                    pb::AnswerOption {
                        answer: "Priya Raman".into(),
                        entity_id: "e5".into(),
                        entity_name: "Priya Raman".into(),
                        probability: 0.15,
                        based_on: Vec::new(),
                    },
                ],
                something_else: 0.05,
                notes: vec!["dropped r9, which is not in the context".into()],
                exclusive: true,
                count: None,
                // An engine before 0.3.0 sends no status.
                status: String::new(),
                status_reason: String::new(),
            })),
            ask_event(Ask::Finished(pb::Finished {
                cost: Some(pb::Cost {
                    calls: 1,
                    input_tokens: 900,
                    output_tokens: 120,
                    reasoning_tokens: 0,
                    unreported_calls: 1,
                    seconds: 2.5,
                    dollars: None,
                    cached_tokens: 0,
                }),
                // A question never waits for a relation pass, whatever the engine says.
                waiting: 3,
                timing: Some(pb::Timing {
                    context_ms: 40,
                    model_ms: 2400,
                    total_ms: 2500,
                    ..Default::default()
                }),
                // The tenant keeps no question log.
                ask_id: String::new(),
            })),
        ],
    );
    engine.stream(
        Rpc::Ask,
        vec![
            ask_event(Ask::Index(pb::AskIndex {
                matches: Vec::new(),
                entities: 0,
                relations: 1,
                sources: 0,
            })),
            ask_event(Ask::Answer(pb::AskAnswer {
                options: vec![
                    pb::AnswerOption {
                        answer: "Toronto".into(),
                        entity_id: "e4".into(),
                        entity_name: "Toronto".into(),
                        probability: 0.6,
                        based_on: vec![cite("r3", "Lumenworks basedIn Toronto", Some(0.7))],
                    },
                    pb::AnswerOption {
                        answer: "Lisbon".into(),
                        probability: 0.3,
                        ..Default::default()
                    },
                ],
                something_else: 0.1,
                exclusive: false,
                ..Default::default()
            })),
        ],
    );
    engine.stream(
        Rpc::Ask,
        vec![ask_event(Ask::Answer(pb::AskAnswer {
            something_else: 1.0,
            exclusive: true,
            ..Default::default()
        }))],
    );
    let dir = workdir("ask");
    let out = ontologic(
        &engine,
        &dir,
        "\\ask who is here\n\\t create acme\n\\ask who founded Lumenworks\n\
         \\ask --facts --staged where is Lumenworks\n\\ask \"what does Lumenworks sell\"\n\\ask\n",
    );
    assert_has(
        &out,
        "ontologic> error: no tenant: run \\t create <name> first\n",
    );
    assert_has(&out, "acme> ── ask ──");
    assert_has(
        &out,
        "\n question who founded Lumenworks\n\
         \x20index    \"Lumenworks\" → e2 Lumenworks\n\
         \x20evidence 3 entities · 2 relations · 1 source\n\
         \x20answer   Maya Chen (e1) 80%  from r4 Maya Chen founded Lumenworks (stored 85%), s1\n\
         \x20         Priya Raman (e5) 15%  no evidence named\n\
         \x20         something else 5%\n\
         \x20check    dropped r9, which is not in the context\n\
         \x20basis    cites s1\n\
         \x20cost     1 call · 900 in + 120 out (0 reasoning) · 2.5 s (1 call reported no usage)\n\
         \x20time     context 40 ms · model 2.4 s · total 2.5 s\nacme> ",
    );
    assert_lacks(&out, "waiting for a relation pass");
    assert_has(
        &out,
        "\n question where is Lumenworks\n\
         \x20index    no known names in the question; searching everything\n\
         \x20evidence 0 entities · 1 relation · 0 sources\n\
         \x20answer   Toronto (e4) 60%  from r3 Lumenworks basedIn Toronto (stored 70%)\n\
         \x20         Lisbon 30%  no evidence named\n\
         \x20         something else 10% · the answers can all be right, each has its own chance\n\
         \x20basis    facts only (--facts)\nacme> ",
    );
    assert_has(
        &out,
        "acme>  answer   I don't know (something else 100%)\nacme> ",
    );
    assert_has(
        &out,
        "acme> error: usage: \\ask [--staged] [--facts] <question>\n",
    );
    let asked: Vec<pb::AskRequest> = calls_to(&engine, Rpc::Ask)
        .iter()
        .map(Call::request)
        .collect();
    let question = |question: &str, graph_only: bool, staged: bool| pb::AskRequest {
        tenant: "acme".into(),
        question: question.into(),
        graph_only,
        staged,
        include_set_members: false,
    };
    assert_eq!(
        asked,
        [
            question("who founded Lumenworks", false, false),
            question("where is Lumenworks", true, true),
            question("what does Lumenworks sell", false, false),
        ]
    );
}

#[test]
fn commit_and_rollback_say_what_they_moved() {
    let engine = engine();
    engine.reply(Rpc::CreateTenant, tenant("acme", 0, 0, 0));
    engine.reply(
        Rpc::Commit,
        pb::TenantSummary {
            staged: Some(pb::Staged {
                sources: 2,
                entities: 3,
                relations: 2,
            }),
            ..tenant("acme", 5, 3, 4)
        },
    );
    engine.reply(
        Rpc::Rollback,
        pb::TenantSummary {
            staged: Some(pb::Staged {
                sources: 1,
                entities: 1,
                relations: 0,
            }),
            ..tenant("acme", 5, 3, 4)
        },
    );
    let dir = workdir("commit");
    let out = ontologic(
        &engine,
        &dir,
        "\\commit\n\\t create acme\n\\commit\n\\rollback\n",
    );
    assert_has(
        &out,
        "ontologic> error: no tenant: run \\t create <name> first\n",
    );
    assert_has(
        &out,
        "acme> committed 2 imports · 3 entities · 2 relations: acme now has 5 entities · \
         3 sources · 4 relations\n",
    );
    assert_has(
        &out,
        "acme> rolled back 1 import · 1 entity · 0 relations: acme is back to 5 entities · \
         3 sources · 4 relations\n",
    );
    assert_eq!(tenants_named(&engine, Rpc::Commit), ["acme"]);
    assert_eq!(tenants_named(&engine, Rpc::Rollback), ["acme"]);
}

#[test]
fn an_error_from_the_engine_prints_as_the_engines_message() {
    let engine = engine();
    engine.reply(Rpc::CreateTenant, tenant("acme", 0, 0, 0));
    engine.fail(
        Rpc::Commit,
        Status::failed_precondition("an import is running in tenant acme"),
    );
    engine.stream(
        Rpc::Import,
        vec![
            import_event(Import::Source(pb::SourceSaved {
                id: "s1".into(),
                text: "Tomas moved to Lisbon.".into(),
                kind: "fact".into(),
                parts: 1,
                ..Default::default()
            })),
            Err(Status::internal("the staged tenant could not be saved")),
        ],
    );
    engine.fail(Rpc::GetTenant, Status::not_found("no tenant nope"));
    engine.fail(
        Rpc::TenantMeta,
        Status::permission_denied("no access to tenant beta"),
    );
    engine.fail(
        Rpc::ImportBlob,
        Status::invalid_argument("the file is not UTF-8 text"),
    );
    let dir = workdir("errors");
    std::fs::write(dir.join("notes.bin"), [0xff, 0xfe, 0x00]).unwrap();
    let out = ontologic(
        &engine,
        &dir,
        "\\t create acme\n\\commit\n\\import fact Tomas moved to Lisbon.\n\\t checkout nope\n\
         \\t meta beta\n\\import blob notes.bin\n\\import blob missing.eml\n",
    );
    assert_has(&out, "acme> error: an import is running in tenant acme\n");
    assert_has(
        &out,
        "\n source   Tomas moved to Lisbon.\nerror: the staged tenant could not be saved\nacme> ",
    );
    assert_has(&out, "acme> error: no tenant nope (\\t create nope)\n");
    assert_has(&out, "acme> error: no access to tenant beta\n");
    assert_has(&out, "acme> error: notes.bin: the file is not UTF-8 text\n");
    assert_has(&out, "acme> error: missing.eml: ");
    // A file the CLI cannot read is never sent, and a failed checkout keeps the tenant.
    assert_eq!(calls_to(&engine, Rpc::ImportBlob).len(), 1);
    assert!(out.ends_with("acme> \n"), "{out}");
}

#[test]
fn show_prints_the_tenant_an_entity_and_a_source() {
    let engine = engine();
    engine.reply(Rpc::CreateTenant, tenant("acme", 0, 0, 0));
    engine.stream(
        Rpc::Import,
        vec![import_event(Import::Finished(pb::Finished {
            cost: Some(pb::Cost {
                calls: 3,
                input_tokens: 1200,
                output_tokens: 300,
                reasoning_tokens: 40,
                seconds: 7.8,
                dollars: Some(0.0012),
                ..Default::default()
            }),
            ..Default::default()
        }))],
    );
    engine.reply(
        Rpc::Show,
        pb::ShowReply {
            view: Some(View::Overview(pb::Overview {
                summary: Some(pb::TenantSummary {
                    pending_reask: 1,
                    cost: Some(pb::Cost {
                        calls: 4,
                        input_tokens: 2000,
                        output_tokens: 400,
                        reasoning_tokens: 50,
                        seconds: 9.5,
                        dollars: Some(0.0021),
                        ..Default::default()
                    }),
                    staged: Some(pb::Staged {
                        sources: 1,
                        entities: 1,
                        relations: 1,
                    }),
                    ..tenant("acme", 2, 1, 1)
                }),
                unplaced: 1,
                unsettled: 0,
                entities: vec![
                    pb::EntityLine {
                        id: "e1".into(),
                        name: "Maya Chen".into(),
                        other_names: vec!["Maya".into()],
                        mentions: 3,
                        relations: 1,
                    },
                    pb::EntityLine {
                        id: "e2".into(),
                        name: "Lumenworks".into(),
                        other_names: Vec::new(),
                        mentions: 1,
                        relations: 0,
                    },
                ],
                recent_calls: vec![pb::CallLine {
                    what: "import s1".into(),
                    seconds: 2.4,
                    input_tokens: 800,
                    output_tokens: 150,
                }],
                source_ids: vec!["s1".into()],
                relation_types: vec![
                    pb::RelationType {
                        name: "worksAt".into(),
                        definition: "the subject works for the object".into(),
                        subject_role: "employee".into(),
                        object_role: "employer".into(),
                        inverse: "employs".into(),
                        single_valued: true,
                    },
                    pb::RelationType {
                        name: "knows".into(),
                        ..Default::default()
                    },
                ],
            })),
        },
    );
    let long = "Maya Chen, Priya Raman and Tomas moved the Lumenworks office to Toronto in May.";
    engine.reply(
        Rpc::Show,
        pb::ShowReply {
            view: Some(View::Entity(pb::EntityView {
                entity: Some(pb::Entity {
                    id: "e1".into(),
                    name: "Maya Chen".into(),
                    description: "founder of Lumenworks".into(),
                    aliases: vec!["Maya Chen".into(), "Maya".into()],
                    kind: "person".into(),
                }),
                relations: vec![pb::Relation {
                    evidence: vec![
                        pb::Evidence {
                            source_id: "s1".into(),
                            start: 0,
                            end: 30,
                            text: "Maya Chen works at Lumenworks.".into(),
                            confidence: 0.9,
                        },
                        pb::Evidence {
                            source_id: "s2".into(),
                            start: 0,
                            end: 79,
                            text: long.into(),
                            confidence: 0.6,
                        },
                    ],
                    rung: "accepted".into(),
                    ..fact("r1", "Maya Chen", "worksAt", "e2", "Lumenworks", 0.9)
                }],
                mentions: vec![pb::Mention {
                    source_id: "s1".into(),
                    text: "Maya Chen".into(),
                    start: 0,
                    end: 9,
                    guesses: vec![guess("e1", "Maya Chen", 1.0)],
                    by: "code".into(),
                    source_text: "Maya Chen works at Lumenworks.".into(),
                }],
            })),
        },
    );
    engine.reply(
        Rpc::Show,
        pb::ShowReply {
            view: Some(View::Source(pb::SourceView {
                id: "s1".into(),
                text: "Maya Chen works at Lumenworks.".into(),
                mentions: vec![pb::Mention {
                    source_id: "s1".into(),
                    text: "Lumenworks".into(),
                    start: 19,
                    end: 29,
                    guesses: vec![
                        guess("e2", "Lumenworks", 0.6),
                        guess("e3", "Lumenworks Labs", 0.3),
                    ],
                    by: "llm".into(),
                    source_text: "Maya Chen works at Lumenworks.".into(),
                }],
                relations: Vec::new(),
                segments: vec![
                    link("Maya Chen", vec![guess("e1", "Maya Chen", 1.0)]),
                    plain(" works at "),
                    link(
                        "Lumenworks",
                        vec![
                            guess("e2", "Lumenworks", 0.6),
                            guess("e3", "Lumenworks Labs", 0.3),
                        ],
                    ),
                    plain("."),
                ],
            })),
        },
    );
    let dir = workdir("show");
    let out = ontologic(
        &engine,
        &dir,
        "\\t create acme\n\\import fact Maya Chen works at Lumenworks.\n\\s\n\\s e1\n\\s s1\n",
    );
    assert_has(&out, "acme> ── tenant acme ──");
    assert_has(
        &out,
        "\n summary  2 entities · 1 source · 1 relation · 1 unplaced · 0 unsettled · 1 waiting\n\
         \x20staged   1 import · 1 entity · 1 relation waiting (\\commit or \\rollback)\n\
         \x20entities e1 Maya Chen  3 mentions · 1 relation also \"Maya\"\n\
         \x20         e2 Lumenworks  1 mention · 0 relations\n\
         \x20types    worksAt  the subject works for the object \
         (employee -> employer; inverse employs; one at a time)\n\
         \x20         knows\n\
         \x20cost     tenant 4 calls · 2000 in + 400 out (50 reasoning) · 9.5 s · $0.0021\n\
         \x20session  this CLI 3 calls · 1200 in + 300 out (40 reasoning) · 7.8 s · $0.0012\n\
         \x20calls    import s1  2.4 s  800 in + 150 out\nacme> ",
    );
    assert_has(&out, "acme> ── e1 Maya Chen ──");
    assert_has(
        &out,
        "\n names    Maya Chen, Maya\n\
         \x20kind     person\n\
         \x20about    founder of Lumenworks\n\
         \x20rel      r1 Maya Chen worksAt Lumenworks 90% accepted\n\
         \x20           s1 90% \"Maya Chen works at Lumenworks.\"\n\
         \x20           s2 60% \"Maya Chen, Priya Raman and Tomas moved the Lumenworks office…\"\n\
         \x20mentions s1 \"Maya Chen\" 0..9 → e1 Maya Chen 100% (by code) \
         Maya Chen works at Lumenworks.\nacme> ",
    );
    assert_has(&out, "acme> ── s1 ──");
    assert_has(
        &out,
        "\n text     Maya Chen works at Lumenworks.\n\
         \x20mentions s1 \"Lumenworks\" 19..29 → e2 Lumenworks 60%, e3 Lumenworks Labs 30% (by llm)\n\
         \x20rel      none\n\
         \x20linked   :e1(Maya Chen) works at [Lumenworks? e2 60%, e3 30%].\nacme> ",
    );
    let shown: Vec<String> = calls_to(&engine, Rpc::Show)
        .iter()
        .map(|c| c.request::<pb::ShowRequest>().id)
        .collect();
    assert_eq!(shown, ["", "e1", "s1"]);
}

#[test]
fn meta_prints_counts_files_dates_and_what_imports_and_questions_cost() {
    let engine = engine();
    engine.reply(Rpc::CreateTenant, tenant("acme", 0, 0, 0));
    engine.reply(
        Rpc::TenantMeta,
        pb::Meta {
            tenant: "acme".into(),
            created: "2026-09-01T10:00:00Z".into(),
            updated: "2026-09-14T16:30:00Z".into(),
            file_bytes: 41_200,
            staged_bytes: 0,
            source_bytes: 2_500_000,
            entities: vec![
                pb::KindCount {
                    kind: "person".into(),
                    count: 2,
                },
                pb::KindCount {
                    kind: "organization".into(),
                    count: 1,
                },
                pb::KindCount {
                    kind: "place".into(),
                    count: 0,
                },
            ],
            sources: vec![
                pb::KindCount {
                    kind: "fact".into(),
                    count: 1,
                },
                pb::KindCount {
                    kind: "email".into(),
                    count: 2,
                },
            ],
            facts: 7,
            value_facts: 2,
            relation_types: 4,
            mentions: 12,
            unplaced: 1,
            unsettled: 2,
            pending_reask: 1,
            staged: Some(pb::Staged {
                sources: 1,
                entities: 2,
                relations: 3,
            }),
            imports: 3,
            import_cost: Some(pb::Cost {
                calls: 9,
                input_tokens: 9000,
                output_tokens: 1500,
                reasoning_tokens: 300,
                seconds: 30.0,
                dollars: Some(0.009),
                ..Default::default()
            }),
            questions: 0,
            question_cost: None,
            run: Some(pb::Cost {
                calls: 12,
                input_tokens: 12_000,
                output_tokens: 2000,
                reasoning_tokens: 400,
                seconds: 40.0,
                dollars: Some(0.012),
                ..Default::default()
            }),
            version: "0abc123, built 2026-09-14 05:12 UTC".into(),
            llm: "fake-model (reasoning low)".into(),
            tagger: String::new(),
            embedder: "fake-embedder".into(),
            import_timing: Some(pb::Timing {
                tagger_ms: 3000,
                model_ms: 24_000,
                code_ms: 30,
                save_ms: 90,
                total_ms: 30_000,
                ..Default::default()
            }),
            question_timing: None,
        },
    );
    let dir = workdir("meta");
    let out = ontologic(&engine, &dir, "\\t meta\n\\t create acme\n\\t meta\n");
    assert_has(
        &out,
        "ontologic> error: no tenant: run \\t create <name> first\n",
    );
    assert_has(&out, "acme> ── meta acme ──");
    assert_has(
        &out,
        "\n tenant   created 2026-09-01T10:00:00Z · updated 2026-09-14T16:30:00Z\n\
         \x20files    tenant 41.2 KB · staged 0 B · source text 2.5 MB\n\
         \x20entities 3: 2 person · 1 organization\n\
         \x20sources  3: 1 fact · 2 email\n\
         \x20facts    7 · 2 end in a value · 4 relation types\n\
         \x20links    12 mentions · 1 unplaced · 2 unsettled · 1 part waiting for a relation pass\n\
         \x20staged   1 import · 2 entities · 3 relations waiting (\\commit or \\rollback)\n\
         \x20imports  3 imports · $0.0030 and 10.0 s per import on average\n\
         \x20         total 9 calls · 9000 in + 1500 out (300 reasoning) · 30.0 s · $0.0090\n\
         \x20asks     no questions yet\n\
         \x20time     per import tagger 1.0 s · model 8.0 s · code 10 ms · save 30 ms · \
         total 10.0 s\n\
         \x20run      12 calls · 12000 in + 2000 out (400 reasoning) · 40.0 s · $0.0120 \
         (since the engine started)\n\
         \x20wrote    0abc123, built 2026-09-14 05:12 UTC · llm fake-model (reasoning low) · \
         tagger unknown · embedder fake-embedder\nacme> ",
    );
    assert_eq!(tenants_named(&engine, Rpc::TenantMeta), ["acme"]);
}

#[test]
fn u_me_shows_the_signed_in_user_their_role_and_tenants() {
    for (role, tenants, shown) in [
        ("admin", &[][..], " role     admin\n tenants  all\n"),
        (
            "member",
            &["acme", "beta"][..],
            " role     member\n tenants  acme, beta\n",
        ),
        (
            "member",
            &[][..],
            " role     member\n tenants  none yet (an admin can \\u grant you)\n",
        ),
    ] {
        let engine = engine_as(role, tenants);
        let dir = workdir(&format!("me-{role}-{}", tenants.len()));
        let out = ontologic(&engine, &dir, "\\u me\n\\u me too\n");
        assert_has(&out, "ontologic> ── me ──");
        assert_has(&out, &format!("\n user     maya\n{shown}"));
        assert_has(
            &out,
            "ontologic> error: usage: \\u me | \\u add <user> | \\u grant <user> <tenant> | \
             \\u remove <user> <tenant>\n",
        );
        assert_eq!(
            rpcs(&engine),
            [Rpc::Me, Rpc::Health, Rpc::ListTenants, Rpc::Me]
        );
    }
}

#[test]
fn admins_add_members_and_grant_and_remove_tenants() {
    let engine = engine();
    engine.reply(
        Rpc::AddUser,
        pb::NewUser {
            user: Some(user("tomas", "member", &[])),
            key: "7e3f0c9a1b2d".into(),
        },
    );
    engine.reply(Rpc::Grant, user("tomas", "member", &["acme"]));
    engine.reply(Rpc::Revoke, user("tomas", "member", &[]));
    let dir = workdir("users");
    let out = ontologic(
        &engine,
        &dir,
        "\\u add tomas\n\\u grant tomas acme\n\\u remove tomas acme\n\\u grant tomas\n",
    );
    assert_has(
        &out,
        "ontologic> user tomas added (member)\nkey  7e3f0c9a1b2d\n     \
         shown once: hand it to tomas, who signs in with -u tomas -p <key>\n",
    );
    assert_has(&out, "ontologic> tomas can now use acme\n");
    assert_has(&out, "ontologic> tomas can no longer use acme\n");
    assert_has(&out, "ontologic> error: usage: \\u me | \\u add <user>");
    let added: Vec<pb::UserName> = calls_to(&engine, Rpc::AddUser)
        .iter()
        .map(Call::request)
        .collect();
    assert_eq!(
        added,
        [pb::UserName {
            name: "tomas".into()
        }]
    );
    let access = pb::Access {
        user: "tomas".into(),
        tenant: "acme".into(),
    };
    let granted: Vec<pb::Access> = calls_to(&engine, Rpc::Grant)
        .iter()
        .map(Call::request)
        .collect();
    let revoked: Vec<pb::Access> = calls_to(&engine, Rpc::Revoke)
        .iter()
        .map(Call::request)
        .collect();
    assert_eq!(granted, std::slice::from_ref(&access));
    assert_eq!(revoked, [access]);
}

#[test]
fn turtle_files_are_written_to_and_read_from_the_cli_directory() {
    let engine = engine();
    engine.reply(Rpc::CreateTenant, tenant("acme", 0, 0, 0));
    engine.reply(Rpc::CreateTenant, tenant("copy", 0, 0, 0));
    let turtle = "@prefix ex: <https://example.com/> .\nex:maya ex:worksAt ex:lumenworks .\n";
    engine.reply(
        Rpc::ExportTurtle,
        pb::TurtleFile {
            tenant: "acme".into(),
            content: turtle.into(),
            summary: Some(pb::TenantSummary {
                staged: Some(pb::Staged {
                    sources: 1,
                    entities: 0,
                    relations: 0,
                }),
                ..tenant("acme", 3, 1, 2)
            }),
        },
    );
    engine.reply(
        Rpc::ImportTurtle,
        pb::TurtleImported {
            summary: Some(tenant("copy", 3, 1, 2)),
            warning: "the file is from tenant acme".into(),
        },
    );
    engine.fail(
        Rpc::ImportTurtle,
        Status::failed_precondition("tenant not empty: \\t create a new tenant first"),
    );
    engine.fail(
        Rpc::ImportTurtle,
        Status::invalid_argument("line 2: expected a dot"),
    );
    let dir = workdir("turtle");
    let out = ontologic(
        &engine,
        &dir,
        "\\t create acme\n\\t export out/acme.ttl\n\\t create copy\n\\t import out/acme.ttl\n\
         \\t import out/acme.ttl\n\\t import out/acme.ttl\n\\t import missing.ttl\n\\t export\n",
    );
    assert_has(
        &out,
        "acme> wrote out/acme.ttl (3 entities · 1 source · 2 relations); staged imports are not \
         exported until \\commit\n",
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("out/acme.ttl")).unwrap(),
        turtle
    );
    assert_has(
        &out,
        "copy> warning: the file is from tenant acme\n\
         imported 3 entities · 1 source · 2 relations from out/acme.ttl\n",
    );
    assert_has(
        &out,
        "copy> error: tenant not empty: \\t create a new tenant first\n",
    );
    assert_has(&out, "copy> error: out/acme.ttl: line 2: expected a dot\n");
    assert_has(&out, "copy> error: missing.ttl: ");
    assert_has(&out, "copy> error: usage: \\t export <file>\n");
    assert_eq!(tenants_named(&engine, Rpc::ExportTurtle), ["acme"]);
    let loaded: Vec<pb::TurtleFile> = calls_to(&engine, Rpc::ImportTurtle)
        .iter()
        .map(Call::request)
        .collect();
    let sent = pb::TurtleFile {
        tenant: "copy".into(),
        content: turtle.into(),
        summary: None,
    };
    assert_eq!(loaded, [sent.clone(), sent.clone(), sent]);
}

#[test]
fn a_computed_count_shows_its_floor_and_its_ceiling() {
    let engine = engine();
    engine.reply(Rpc::CreateTenant, tenant("acme", 0, 0, 0));
    let counted = |answer: &str, operation: &str, lower, upper, status: &str| {
        vec![ask_event(Ask::Answer(pb::AskAnswer {
            options: vec![pb::AnswerOption {
                answer: answer.into(),
                probability: 0.9,
                ..Default::default()
            }],
            something_else: 0.1,
            exclusive: true,
            count: Some(pb::CountRange {
                operation: operation.into(),
                lower,
                upper,
                status: status.into(),
            }),
            ..Default::default()
        }))]
    };
    engine.stream(
        Rpc::Ask,
        counted("3 or 4 postmortems", "count", 3, Some(4), "bounded"),
    );
    engine.stream(
        Rpc::Ask,
        counted("8 customers", "distinct customer", 8, Some(8), "exact"),
    );
    engine.stream(
        Rpc::Ask,
        counted("at least 5", "count", 5, None, "over the records held"),
    );
    let dir = workdir("count");
    let out = ontologic(
        &engine,
        &dir,
        "\\t create acme\n\\ask how many postmortems in May\n\\ask how many customers\n\
         \\ask how many tickets\n",
    );
    assert_has(
        &out,
        "acme>  answer   3 or 4 postmortems 90%  no evidence named\n\
         \x20         something else 10%\n\
         \x20computed count 3 to 4 (bounded)\nacme> ",
    );
    assert_has(&out, " computed distinct customer 8 (exact)\n");
    assert_has(&out, " computed count at least 5 (over the records held)\n");
}

#[test]
fn a_set_page_the_cli_never_asks_for_prints_only_its_count() {
    let engine = engine();
    engine.reply(Rpc::CreateTenant, tenant("acme", 0, 0, 0));
    let member = |id: &str, label: &str| pb::SetMember {
        id: id.into(),
        label: label.into(),
        support: Vec::new(),
    };
    engine.stream(
        Rpc::Ask,
        vec![
            ask_event(Ask::SetPage(pb::SetPage {
                members: vec![member("r1", "Acme"), member("r2", "Lumenworks")],
                member_count: 3,
                last: false,
                ..Default::default()
            })),
            ask_event(Ask::SetPage(pb::SetPage {
                offset: 2,
                members: vec![member("r3", "Maya Chen")],
                member_count: 3,
                last: true,
                ..Default::default()
            })),
            ask_event(Ask::SetPage(pb::SetPage {
                error: "the tenant changed while its members were read".into(),
                ..Default::default()
            })),
        ],
    );
    let dir = workdir("set-page");
    let out = ontologic(&engine, &dir, "\\t create acme\n\\ask which customers\n");
    assert_has(
        &out,
        "acme>  set      2 members of 3 · more pages follow\n\
         \x20set      1 member of 3 · last page\n\
         \x20set      ✗ error: the tenant changed while its members were read\nacme> ",
    );
    assert_lacks(&out, "Lumenworks");
    let asked: Vec<pb::AskRequest> = calls_to(&engine, Rpc::Ask)
        .iter()
        .map(Call::request)
        .collect();
    assert!(!asked[0].include_set_members);
}

#[test]
fn documents_are_retracted_restored_and_erased_once_the_tenant_is_named() {
    let engine = engine();
    engine.reply(Rpc::CreateTenant, tenant("acme", 0, 0, 0));
    let counts = |entities, sources, relations| {
        Some(pb::Counts {
            entities,
            relations,
            sources,
        })
    };
    let changed = |documents: &[&str], before, after| pb::LifecycleReply {
        documents: documents.iter().map(|d| d.to_string()).collect(),
        before,
        after,
    };
    engine.reply(
        Rpc::Retract,
        changed(&["d1", "d2"], counts(5, 3, 4), counts(3, 1, 2)),
    );
    engine.reply(
        Rpc::Restore,
        changed(&["d1"], counts(3, 1, 2), counts(4, 2, 3)),
    );
    engine.reply(
        Rpc::Erase,
        changed(&["d2"], counts(4, 2, 3), counts(2, 1, 1)),
    );
    let dir = workdir("documents");
    let out = ontologic(
        &engine,
        &dir,
        "\\retract d1\n\\t create acme\n\\retract d1 d2 \"an old copy\"\n\\restore d1\n\
         \\erase d2 asked to\nacme-2\n\\erase d2 asked to\nacme\n\\retract why\n\\erase\n",
    );
    assert_has(
        &out,
        "ontologic> error: no tenant: run \\t create <name> first\n",
    );
    assert_has(
        &out,
        "acme> retracted d1, d2: 5 entities · 3 sources · 4 relations → 3 entities · 1 source · \
         2 relations\n",
    );
    assert_has(
        &out,
        "acme> restored d1: 3 entities · 1 source · 2 relations → 4 entities · 2 sources · \
         3 relations\n",
    );
    let asked = "acme> warning: erasing destroys d2 for good; only a tombstone with each version's \
                 hash stays\ntype the tenant's name, acme, to erase; anything else cancels\n";
    assert_has(&out, &format!("{asked}confirm> nothing erased\n"));
    assert_has(
        &out,
        &format!(
            "{asked}confirm> erased d2: 4 entities · 2 sources · 3 relations → 2 entities · \
             1 source · 1 relation\nacme> "
        ),
    );
    assert_has(&out, "acme> error: usage: \\retract <doc...> [reason]\n");
    assert_has(&out, "acme> error: usage: \\erase <doc...> [reason]\n");
    let sent = |rpc| -> Vec<pb::DocumentsRequest> {
        calls_to(&engine, rpc).iter().map(Call::request).collect()
    };
    let request = |documents: &[&str], reason: &str| pb::DocumentsRequest {
        tenant: "acme".into(),
        documents: documents.iter().map(|d| d.to_string()).collect(),
        reason: reason.into(),
    };
    assert_eq!(sent(Rpc::Retract), [request(&["d1", "d2"], "an old copy")]);
    assert_eq!(sent(Rpc::Restore), [request(&["d1"], "")]);
    assert_eq!(
        sent(Rpc::Erase),
        [request(&["d2"], "asked to")],
        "erased once, and only after the tenant's name"
    );
}

#[test]
fn a_migration_shows_what_it_changes_and_commits_only_on_yes() {
    let engine = engine();
    engine.reply(Rpc::CreateTenant, tenant("acme", 0, 0, 0));
    let diff = pb::MigrationDiff {
        facts_added: 3,
        facts_removed: 1,
        promoted: 2,
        support_changed: 4,
        facts_unchanged: 120,
        entities_renamed: 1,
        by_type: vec![pb::TypeChange {
            relation_type: "hasCustomer".into(),
            added: 3,
            removed: 1,
            promoted: 2,
            demoted: 0,
        }],
        ..Default::default()
    };
    let migrated = |committed| pb::MigrateReply {
        committed,
        diff: Some(diff.clone()),
    };
    engine.reply(Rpc::Migrate, migrated(false));
    engine.reply(Rpc::Migrate, migrated(false));
    engine.reply(Rpc::Migrate, migrated(true));
    let dir = workdir("migrate");
    let out = ontologic(
        &engine,
        &dir,
        "\\t create acme\n\\migrate ticket:13 postmortem:5 new rules\nno\n\\migrate ticket:13\n\
         yes\n\\migrate ticket\n",
    );
    let shown = "── migrate acme ──";
    let staged = " pins     ticket:13 · postmortem:5\n\
                  \x20facts    3 added · 1 removed · 2 promoted · 0 demoted · 4 support changed · \
                  120 unchanged\n\
                  \x20entities 0 added · 0 removed · 1 renamed · 0 identity changed\n\
                  \x20types    hasCustomer 3 added · 1 removed · 2 promoted\n\
                  \x20calls    0 model calls\n\
                  \x20state    staged, nothing written\n\
                  type yes to commit it; anything else cancels\nconfirm> nothing committed\n";
    assert_has(&out, &format!("acme> {shown}"));
    assert_has(&out, staged);
    assert_has(&out, &format!("confirm> {shown}"));
    assert_has(&out, " pins     ticket:13\n facts    3 added · 1 removed");
    assert_has(&out, " calls    0 model calls\n state    committed\nacme> ");
    assert_has(
        &out,
        "acme> error: usage: \\migrate <kind:version ...> [reason]\n",
    );
    let sent: Vec<pb::MigrateRequest> = calls_to(&engine, Rpc::Migrate)
        .iter()
        .map(Call::request)
        .collect();
    let request = |pins: &[(&str, u32)], reason: &str, commit| pb::MigrateRequest {
        tenant: "acme".into(),
        kind: pb::MigrationKind::Reprojection as i32,
        pins: pins
            .iter()
            .map(|(kind, version)| pb::SchemaPin {
                kind: kind.to_string(),
                version: *version,
            })
            .collect(),
        reason: reason.into(),
        commit,
    };
    assert_eq!(
        sent,
        [
            request(&[("ticket", 13), ("postmortem", 5)], "new rules", false),
            request(&[("ticket", 13)], "", false),
            request(&[("ticket", 13)], "", true),
        ]
    );
}

#[test]
fn admins_say_who_reads_a_document_and_what_a_member_reads_as() {
    let engine = engine();
    engine.reply(Rpc::CreateTenant, tenant("acme", 0, 0, 0));
    engine.reply(Rpc::SetAcl, pb::Empty {});
    let groups = vec!["group:sales".to_string(), "group:eng".to_string()];
    engine.reply(
        Rpc::SetPrincipals,
        pb::User {
            principals: vec![pb::TenantGroups {
                tenant: "acme".into(),
                groups: groups.clone(),
            }],
            ..user("tomas", "member", &["acme"])
        },
    );
    let dir = workdir("access");
    let out = ontologic(
        &engine,
        &dir,
        "\\t create acme\n\\acl d2 group:finance user:maya\n\\acl d2\n\
         \\principals tomas group:sales group:eng\n\\principals\n",
    );
    assert_has(
        &out,
        "acme> d2 is read by group:finance, user:maya from now on, and by every admin\n",
    );
    assert_has(
        &out,
        "acme> error: usage: \\acl <doc> <principal...> (user:<name> or a group)\n",
    );
    assert_has(
        &out,
        "acme> tomas reads acme as user:tomas, group:sales, group:eng\n",
    );
    assert_has(&out, "acme> error: usage: \\principals <user> <group...>\n");
    let acl: Vec<pb::AclRequest> = calls_to(&engine, Rpc::SetAcl)
        .iter()
        .map(Call::request)
        .collect();
    assert_eq!(
        acl,
        [pb::AclRequest {
            tenant: "acme".into(),
            document: "d2".into(),
            principals: vec!["group:finance".into(), "user:maya".into()],
        }]
    );
    let given: Vec<pb::PrincipalsRequest> = calls_to(&engine, Rpc::SetPrincipals)
        .iter()
        .map(Call::request)
        .collect();
    assert_eq!(
        given,
        [pb::PrincipalsRequest {
            user: "tomas".into(),
            tenant: "acme".into(),
            groups,
        }]
    );
}
