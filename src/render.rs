//! How the engine's answers look in a terminal: a header rule per command, a label column,
//! and colors that carry meaning (green sure, yellow weighed, red rejected or unplaced).

use brain_proto as pb;

use crate::paint::{
    band, blue, bold, bold_blue, bold_green, cyan, dim, green, magenta, percent, percents_in, red,
    yellow,
};

const WIDTH: usize = 64;

pub fn header(title: &str) -> String {
    let rule = WIDTH.saturating_sub(title.chars().count() + 4);
    format!("{} {} {}", dim("──"), bold(title), dim(&"─".repeat(rule)))
}

/// ` label    first line`, continuation lines under the text.
pub fn row(label: &str, lines: &[String]) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let head = match i {
                0 => format!("{label:<8}"),
                _ => " ".repeat(8),
            };
            format!(" {} {line}", dim(&head))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub fn error(message: &str) -> String {
    format!("{} {message}", red("error:"))
}

pub fn warning(message: &str) -> String {
    format!("{} {message}", yellow("warning:"))
}

fn entity_ref(id: &str, name: &str) -> String {
    match name.is_empty() {
        true => blue(id),
        false => format!("{} {}", blue(id), bold(name)),
    }
}

fn guess_list(guesses: &[pb::Guess]) -> String {
    match guesses.is_empty() {
        true => red("not placed"),
        false => guesses
            .iter()
            .map(|g| {
                format!(
                    "{} {}",
                    entity_ref(&g.entity_id, &g.entity_name),
                    percent(g.confidence)
                )
            })
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// The sentence with each link shown by how sure the brain is.
pub fn linked(segments: &[pb::Segment]) -> String {
    segments
        .iter()
        .map(|s| match (s.link, s.guesses.as_slice()) {
            (false, _) => s.text.clone(),
            (true, []) => red(&format!("[{}?]", s.text)),
            (true, [top, ..]) if top.confidence >= 0.75 => crate::paint::wrap(
                band(top.confidence),
                &format!(":{}({})", top.entity_id, top.entity_name),
            ),
            (true, guesses) => {
                let options: Vec<String> = guesses
                    .iter()
                    .take(3)
                    .map(|g| format!("{} {}%", g.entity_id, (g.confidence * 100.0).round() as i32))
                    .collect();
                yellow(&format!("[{}? {}]", s.text, options.join(", ")))
            }
        })
        .collect()
}

fn decision(d: &pb::Decision, quote: bool) -> String {
    let subject = match quote {
        true => format!("\"{}\"", d.subject),
        false => d.subject.clone(),
    };
    match d.accepted {
        true => format!("{} {subject} → {}", green("✓"), percents_in(&d.detail)),
        false => format!("{} {subject} → {}", red("✗"), red(&d.detail)),
    }
}

/// An entity's name in bold, or a value in quotes with its kind: `"$8,000" money`.
fn object(r: &pb::Relation) -> String {
    match r.object_id.is_empty() {
        true => format!(
            "{} {}",
            cyan(&format!("\"{}\"", r.object_name)),
            dim(&r.object_kind)
        ),
        false => bold(&r.object_name),
    }
}

fn relation(r: &pb::Relation) -> String {
    let fresh = match r.new_type {
        true => dim(" (new relation type)"),
        false => String::new(),
    };
    format!(
        "{} {} {} {} {}{fresh}",
        dim(&r.id),
        bold(&r.subject_name),
        magenta(&r.predicate),
        object(r),
        percent(r.confidence)
    )
}

/// `s5 90% "Tomas still lives in Toronto"`, the words cut to 60 characters.
fn evidence_line(e: &pb::Evidence) -> String {
    let mut words: String = e.text.chars().take(60).collect();
    if e.text.chars().count() > 60 {
        words.push('…');
    }
    format!(
        "{} {} {}",
        dim(&e.source_id),
        percent(e.confidence),
        dim(&format!("\"{words}\""))
    )
}

/// A fact that gained evidence: the evidence and what the fact is now.
fn more_evidence(m: &pb::MoreEvidence) -> String {
    let (Some(r), Some(e)) = (&m.relation, &m.evidence) else {
        return String::new();
    };
    format!(
        "{} {} {} {} {} {} {}",
        dim(&r.id),
        bold(&r.subject_name),
        magenta(&r.predicate),
        object(r),
        dim("+"),
        evidence_line(e),
        dim(&format!("(fact now {})", percent_plain(r.confidence)))
    )
}

fn percent_plain(value: f32) -> String {
    format!("{}%", (value * 100.0).round() as i32)
}

pub fn cost(cost: &pb::Cost) -> String {
    let mut parts = vec![
        count(cost.calls, "call", "calls"),
        format!(
            "{} in + {} out ({} reasoning)",
            cost.input_tokens, cost.output_tokens, cost.reasoning_tokens
        ),
        format!("{:.1} s", cost.seconds),
    ];
    if let Some(dollars) = cost.dollars {
        parts.push(match dollars > 0.0 && dollars < 0.00005 {
            true => "<$0.0001".to_string(),
            false => format!("${dollars:.4}"),
        });
    }
    let mut line = dim(&parts.join(" · "));
    if cost.unreported_calls > 0 {
        let calls = count(cost.unreported_calls, "call", "calls");
        line.push_str(&yellow(&format!(" ({calls} reported no usage)")));
    }
    line
}

pub fn summary(s: &pb::TenantSummary) -> String {
    format!(
        "{} · {} · {}",
        count(s.entities.into(), "entity", "entities"),
        count(s.sources.into(), "source", "sources"),
        count(s.relations.into(), "relation", "relations")
    )
}

// ---------------------------------------------------------------------------------------
// Import, event by event

pub fn source_saved(e: &pb::SourceSaved) -> String {
    if e.kind.is_empty() || e.kind == "fact" {
        return format!(
            "{}\n{}",
            header(&format!("import {}", e.id)),
            row("source", std::slice::from_ref(&e.text))
        );
    }
    format!(
        "{}\n{}",
        header(&format!("import {} {}", e.id, e.name)),
        row(
            "source",
            &[format!(
                "{} · {} · {} bytes",
                e.kind,
                count(u64::from(e.parts), "part", "parts"),
                e.text.len()
            )]
        )
    )
}

/// `\\v`: the engine's build and the CLI's.
pub fn version(host: &str, health: &pb::HealthReply, cli: &str) -> String {
    let engine = match health.version.is_empty() {
        true => dim("older than \\v"),
        false => health.version.clone(),
    };
    [
        header("version"),
        row(
            "engine",
            &[format!("{engine} {}", dim(&format!("({host})")))],
        ),
        row("cli", &[cli.to_string()]),
    ]
    .join("\n")
}

/// The people an email's headers name, linked before any model call.
pub fn people(e: &pb::People) -> String {
    let mut lines: Vec<String> = e
        .new_entities
        .iter()
        .map(|p| {
            format!(
                "{} {} {}",
                entity_ref(&p.id, &p.name),
                dim(&p
                    .aliases
                    .iter()
                    .filter(|a| **a != p.name)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")),
                green("new")
            )
        })
        .collect();
    lines.extend(e.known.iter().map(|p| entity_ref(&p.id, &p.name)));
    for alias in &e.new_aliases {
        lines.push(format!(
            "\"{}\" → {}",
            cyan(&alias.alias),
            entity_ref(&alias.entity_id, &alias.entity_name)
        ));
    }
    lines.extend(e.new_relations.iter().map(relation));
    if lines.is_empty() {
        lines.push(dim("no names in the headers"));
    }
    let head = row("people", &lines);
    format!("{head}\n{}", row("messages", &[e.messages.to_string()]))
}

pub fn code_pass(e: &pb::CodePass) -> String {
    let mut out = String::new();
    if e.part > 0 {
        let mut words: String = e.text.chars().take(60).collect();
        if e.text.chars().count() > 60 {
            words.push('…');
        }
        out = row(
            "part",
            &[format!(
                "{} {}",
                bold(&e.part.to_string()),
                dim(&format!("\"{words}\""))
            )],
        ) + "\n";
    }
    let lines: Vec<String> = match e.matches.is_empty() {
        true => vec![dim("no known names")],
        false => e
            .matches
            .iter()
            .map(|m| format!("\"{}\" → {}", m.text, guess_list(&m.guesses)))
            .collect(),
    };
    out + &row("code", &lines)
}

/// `4 candidates · 1 asked again · 1 left out: "Brightline"`.
fn checklist(p: &pb::Checklist) -> String {
    if !p.error.is_empty() {
        return red(&format!("✗ error: {}", p.error));
    }
    let quoted = |list: &[String]| {
        list.iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut parts = vec![count(p.found.len() as u64, "candidate", "candidates")];
    if !p.follow_up.is_empty() {
        parts.push(format!("{} asked again", p.follow_up.len()));
    }
    parts.push(match p.undecided.is_empty() {
        true => green("all decided"),
        false => yellow(&format!(
            "{} left out: {}",
            p.undecided.len(),
            quoted(&p.undecided)
        )),
    });
    parts.join(" · ")
}

pub fn linked_event(e: &pb::Linked) -> String {
    let mut out = Vec::new();
    let llm: Vec<String> = match (e.llm_error.is_empty(), e.mentions.is_empty()) {
        (false, _) => vec![red(&format!(
            "✗ error: {} (keeping the code links)",
            e.llm_error
        ))],
        (true, true) => vec![dim("nothing to add")],
        (true, false) => e.mentions.iter().map(|d| decision(d, true)).collect(),
    };
    out.push(row("llm", &llm));
    if e.llm_error.is_empty() {
        let rel: Vec<String> = match e.relations.is_empty() {
            true => vec![dim("none")],
            false => e.relations.iter().map(|d| decision(d, false)).collect(),
        };
        out.push(row("rel", &rel));
        let decided = e.mentions.iter().chain(&e.relations);
        let rejected = decided.clone().filter(|d| !d.accepted).count() as u64;
        let accepted = decided.count() as u64 - rejected;
        out.push(row(
            "check",
            &[format!(
                "{} accepted · {} rejected",
                green(&accepted.to_string()),
                match rejected {
                    0 => "0".to_string(),
                    n => red(&n.to_string()),
                }
            )],
        ));
        if let Some(p) = &e.checklist {
            out.push(row("tagger", &[checklist(p)]));
        }
    }
    for entity in &e.new_entities {
        out.push(row(
            "new",
            &[format!(
                "{} {}",
                entity_ref(&entity.id, &entity.name),
                dim(&format!("({}: {})", entity.kind, entity.description))
            )],
        ));
    }
    for alias in &e.new_aliases {
        out.push(row(
            "alias",
            &[format!(
                "\"{}\" → {}",
                cyan(&alias.alias),
                entity_ref(&alias.entity_id, &alias.entity_name)
            )],
        ));
    }
    for r in &e.new_relations {
        out.push(row("new", &[relation(r)]));
    }
    for m in &e.more_evidence {
        out.push(row("more", &[more_evidence(m)]));
    }
    out.push(row("linked", &[linked(&e.segments)]));
    out.join("\n")
}

pub fn relinked(e: &pb::Relinked) -> String {
    let lines: Vec<String> = e
        .mentions
        .iter()
        .map(|m| {
            format!(
                "{} \"{}\" → {}",
                dim(&m.source_id),
                m.text,
                guess_list(&m.guesses)
            )
        })
        .collect();
    row("relink", &lines)
}

pub fn reasked(e: &pb::Reasked) -> String {
    let id = dim(&e.source_id);
    let mut lines = Vec::new();
    if !e.llm_error.is_empty() {
        lines.push(format!(
            "{id} {}",
            red(&format!(
                "✗ error: {} (will retry next import)",
                e.llm_error
            ))
        ));
    } else if e.relations.is_empty() {
        lines.push(format!("{id} {}", dim("no relations")));
    }
    lines.extend(
        e.relations
            .iter()
            .map(|d| format!("{id} {}", decision(d, false))),
    );
    lines.extend(
        e.new_relations
            .iter()
            .map(|r| format!("{id} new {}", relation(r))),
    );
    lines.extend(
        e.more_evidence
            .iter()
            .map(|m| format!("{id} more {}", more_evidence(m))),
    );
    row("reask", &lines)
}

pub fn finished(e: &pb::Finished) -> String {
    let mut out = Vec::new();
    if e.waiting > 0 {
        out.push(row(
            "waiting",
            &[yellow(&format!(
                "{} still waiting for a relation pass",
                count(e.waiting.into(), "source", "sources")
            ))],
        ));
    }
    if let Some(c) = &e.cost {
        out.push(row("cost", &[cost(c)]));
    }
    out.join("\n")
}

// ---------------------------------------------------------------------------------------
// Ask

pub fn ask_index(question: &str, e: &pb::AskIndex) -> String {
    let index: Vec<String> = match e.matches.is_empty() {
        true => vec![dim("no known names in the question; searching everything")],
        false => e
            .matches
            .iter()
            .map(|m| {
                let names: Vec<String> = m
                    .guesses
                    .iter()
                    .map(|g| entity_ref(&g.entity_id, &g.entity_name))
                    .collect();
                format!("\"{}\" → {}", m.text, names.join(", "))
            })
            .collect(),
    };
    [
        header("ask"),
        row("question", &[bold(question)]),
        row("index", &index),
        row(
            "evidence",
            &[format!(
                "{} · {} · {}",
                count(e.entities.into(), "entity", "entities"),
                count(e.relations.into(), "relation", "relations"),
                count(e.sources.into(), "source", "sources")
            )],
        ),
    ]
    .join("\n")
}

pub fn ask_answer(e: &pb::AskAnswer) -> String {
    if !e.error.is_empty() {
        return row("answer", &[red(&format!("✗ error: {}", e.error))]);
    }
    let mut lines = Vec::new();
    for (i, option) in e.options.iter().enumerate() {
        let who = match option.entity_id.is_empty() {
            true => String::new(),
            false => format!(" ({})", blue(&option.entity_id)),
        };
        let answer = match i {
            0 => bold_green(&option.answer),
            _ => option.answer.clone(),
        };
        let from: Vec<String> = option
            .based_on
            .iter()
            .map(|ev| match (ev.stored, ev.id.starts_with('r')) {
                (Some(stored), true) => {
                    format!("{} {} (stored {})", dim(&ev.id), ev.label, percent(stored))
                }
                _ => dim(&ev.id),
            })
            .collect();
        let from = match from.is_empty() {
            true => dim("no evidence named"),
            false => format!("{} {}", dim("from"), from.join(", ")),
        };
        lines.push(format!(
            "{answer}{who} {}  {from}",
            percent(option.probability)
        ));
    }
    match e.options.is_empty() {
        true => lines.push(format!(
            "{} {}",
            yellow("I don't know"),
            dim(&format!(
                "(something else {}%)",
                (e.something_else * 100.0).round() as i32
            ))
        )),
        false => lines.push(dim(&format!(
            "something else {}%{}",
            (e.something_else * 100.0).round() as i32,
            match e.exclusive {
                true => "",
                false => " · the answers can all be right, each has its own chance",
            }
        ))),
    }
    let mut out = vec![row("answer", &lines)];
    if !e.notes.is_empty() {
        let notes: Vec<String> = e.notes.iter().map(|n| yellow(n)).collect();
        out.push(row("check", &notes));
    }
    out.join("\n")
}

// ---------------------------------------------------------------------------------------
// Tenants and views

pub fn tenant_list(list: &pb::TenantList, current: Option<&str>) -> String {
    if list.tenants.is_empty() {
        return dim("no tenants (\\t create <name>)");
    }
    list.tenants
        .iter()
        .map(|t| {
            let here = current == Some(t.name.as_str());
            let marker = match here {
                true => bold_green("*"),
                false => " ".to_string(),
            };
            let name = match here {
                true => bold_blue(&t.name),
                false => t.name.clone(),
            };
            let mut line = format!("{marker} {name}  {}", summary(t));
            if let Some(c) = &t.cost {
                line.push_str(&dim(&format!(" · {}", count(c.calls, "call", "calls"))));
                if let Some(dollars) = c.dollars {
                    line.push_str(&dim(&format!(" · ${dollars:.4}")));
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn overview(o: &pb::Overview, session: &pb::Cost) -> String {
    let s = o.summary.clone().unwrap_or_default();
    let mut out = vec![header(&format!("tenant {}", s.name))];
    out.push(row(
        "summary",
        &[format!(
            "{} · {} · {} · {} waiting",
            summary(&s),
            count(o.unplaced.into(), "unplaced", "unplaced"),
            count(o.unsettled.into(), "unsettled", "unsettled"),
            s.pending_reask
        )],
    ));
    let entities: Vec<String> = match o.entities.is_empty() {
        true => vec![dim("none")],
        false => o
            .entities
            .iter()
            .map(|e| {
                let mut line = format!(
                    "{}  {}",
                    entity_ref(&e.id, &e.name),
                    dim(&format!(
                        "{} · {}",
                        count(e.mentions.into(), "mention", "mentions"),
                        count(e.relations.into(), "relation", "relations")
                    ))
                );
                if !e.other_names.is_empty() {
                    let names: Vec<String> = e
                        .other_names
                        .iter()
                        .map(|n| cyan(&format!("\"{n}\"")))
                        .collect();
                    line.push_str(&format!(" {} {}", dim("also"), names.join(", ")));
                }
                line
            })
            .collect(),
    };
    out.push(row("entities", &entities));
    let types = match o.types.is_empty() {
        true => dim("none"),
        false => o
            .types
            .iter()
            .map(|t| magenta(t))
            .collect::<Vec<_>>()
            .join(", "),
    };
    out.push(row("types", &[types]));
    if let Some(c) = &s.cost {
        out.push(row("cost", &[format!("{} {}", dim("tenant"), cost(c))]));
    }
    out.push(row(
        "session",
        &[format!("{} {}", dim("this CLI"), cost(session))],
    ));
    let calls: Vec<String> = o
        .recent_calls
        .iter()
        .map(|c| {
            dim(&format!(
                "{}  {:.1} s  {} in + {} out",
                c.what, c.seconds, c.input_tokens, c.output_tokens
            ))
        })
        .collect();
    if !calls.is_empty() {
        out.push(row("calls", &calls));
    }
    out.join("\n")
}

fn mention_line(m: &pb::Mention, with_source_text: bool) -> String {
    let mut line = format!(
        "{} \"{}\" {} → {} {}",
        dim(&m.source_id),
        m.text,
        dim(&format!("{}..{}", m.start, m.end)),
        guess_list(&m.guesses),
        dim(&format!("(by {})", m.by))
    );
    if with_source_text {
        line.push_str(&format!(" {}", dim(&m.source_text)));
    }
    line
}

pub fn entity_view(v: &pb::EntityView) -> String {
    let e = v.entity.clone().unwrap_or_default();
    let mut out = vec![header(&format!("{} {}", e.id, e.name))];
    out.push(row(
        "names",
        &[e.aliases
            .iter()
            .map(|a| cyan(a))
            .collect::<Vec<_>>()
            .join(", ")],
    ));
    out.push(row("kind", std::slice::from_ref(&e.kind)));
    out.push(row("about", std::slice::from_ref(&e.description)));
    let relations: Vec<String> = match v.relations.is_empty() {
        true => vec![dim("none")],
        // Each fact, then one line per piece of evidence behind it.
        false => v
            .relations
            .iter()
            .flat_map(|r| {
                std::iter::once(relation(r))
                    .chain(r.evidence.iter().map(|e| format!("  {}", evidence_line(e))))
            })
            .collect(),
    };
    out.push(row("rel", &relations));
    let mentions: Vec<String> = match v.mentions.is_empty() {
        true => vec![dim("none")],
        false => v.mentions.iter().map(|m| mention_line(m, true)).collect(),
    };
    out.push(row("mentions", &mentions));
    out.join("\n")
}

pub fn source_view(v: &pb::SourceView) -> String {
    let mut out = vec![header(&v.id)];
    out.push(row("text", std::slice::from_ref(&v.text)));
    let mentions: Vec<String> = match v.mentions.is_empty() {
        true => vec![dim("none")],
        false => v.mentions.iter().map(|m| mention_line(m, false)).collect(),
    };
    out.push(row("mentions", &mentions));
    let relations: Vec<String> = match v.relations.is_empty() {
        true => vec![dim("none")],
        false => v.relations.iter().map(relation).collect(),
    };
    out.push(row("rel", &relations));
    out.push(row("linked", &[linked(&v.segments)]));
    out.join("\n")
}

// ---------------------------------------------------------------------------------------
// Users

fn role(role: &str) -> String {
    match role {
        "admin" => magenta(role),
        _ => cyan(role),
    }
}

/// `\u add`: the new member, and their key this one time.
pub fn new_user(added: &pb::NewUser) -> String {
    let name = added.user.as_ref().map_or("", |u| u.name.as_str());
    format!(
        "user {} added ({})\n{} {}\n     {}",
        bold(name),
        role("member"),
        dim("key "),
        bold_green(&added.key),
        dim(&format!(
            "shown once: hand it to {name}, who signs in with -u {name} -p <key>"
        ))
    )
}

/// `\u me`.
pub fn me(user: &pb::User) -> String {
    let tenants = match (user.role.as_str(), user.tenants.is_empty()) {
        ("admin", _) => "all".to_string(),
        (_, true) => dim("none yet (an admin can \\u grant you)"),
        _ => user
            .tenants
            .iter()
            .map(|t| blue(t))
            .collect::<Vec<_>>()
            .join(", "),
    };
    [
        header("me"),
        row("user", &[bold(&user.name)]),
        row("role", &[role(&user.role)]),
        row("tenants", &[tenants]),
    ]
    .join("\n")
}

/// `\t users`: everyone who can use the tenant.
pub fn tenant_users(tenant: &str, list: &pb::UserList) -> String {
    let width = list.users.iter().map(|u| u.name.len()).max().unwrap_or(0);
    let mut lines = vec![header(&format!("users of {tenant}"))];
    for user in &list.users {
        let name = format!("{:<width$}", user.name);
        lines.push(format!(" {}  {}", bold(&name), role(&user.role)));
    }
    lines.join("\n")
}

/// `\u grant` and `\u remove`.
pub fn access(user: &pb::User, tenant: &str, granted: bool) -> String {
    let now = match granted {
        true => green("can now use"),
        false => yellow("can no longer use"),
    };
    format!("{} {now} {}", bold(&user.name), blue(tenant))
}
