//! What each command does: call the engine, then render the reply.

use std::cell::RefCell;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::rc::Rc;

use ontologic_kit::client::{Client, describe};
use ontologic_kit::pb;
use tokio::runtime::Runtime;
use tonic::{Code, Status};

use crate::input::{self, Command};
use crate::{paint, render};

pub const HELP: &str = "\
\\t create <name>      create a tenant and switch to it
\\t checkout <name>    switch to a tenant
\\t get                list the tenants; * marks the one you are on
\\t delete <name>      forget a tenant and everything in it
\\t export <file>      save the tenant to a .ttl file in this directory
\\t import <file>      load a .ttl file into the current (empty) tenant
\\t users              who can use the current tenant
\\t meta [name]        a tenant's counts, files, dates, and cost per import and question
\\t asklog <days>      admins: keep the question log <days> days; 0 stops and deletes it
\\import fact <text>  add a fact: link things, find relations, re-link older sources
\\import blob <file>  add a file from this directory: an email (.eml) or text, part by part
\\ask <question>       answer from what the tenant has committed, with probabilities
\\ask --staged <q>     answer with the imports not committed yet included
\\ask --facts <q>      answer from facts alone, with no source text: what the graph holds
\\good [note]          the last logged answer was right
\\partly [note]        the last logged answer was partly right, and why
\\bad [note]           the last logged answer was wrong, and why
\\commit               make the staged imports part of the tenant, so questions see them
\\rollback             drop the staged imports (and the re-links they made)
\\retract <d> [why]    take documents (d3 ...) out of every answer, count and link
\\restore <d>          put retracted documents back
\\erase <d> [why]      admins: destroy documents for good, once you type the tenant's name
\\migrate <kind:v>     admins: the diff other schema versions make, then commit it or not
\\acl <d> <who...>     admins: who may read a document (user:<name> or a group)
\\principals <u> <g>   admins: the groups member <u> reads this tenant as
\\s                    what the tenant knows, and what it cost
\\s <id>               everything about an entity (e1) or a source (s1)
\\u me                 your user, role and tenants
\\u add <user>         admins: add a member and print their key once
\\u grant <user> <t>   admins: let a member use tenant <t>
\\u remove <user> <t>  admins: take that access away
\\v                    the engine's version and this CLI's
\\h                    this help
\\q                    quit (Ctrl-D works too)
Up and down arrows walk the history.";

const IMPORT_USAGE: &str = "usage: \\import fact <sentence> | \\import blob <file>";
const TENANT_USAGE: &str = "usage: \\t create <name> | \\t checkout <name> | \\t get | \\t delete <name> | \\t export <file> | \\t import <file> | \\t users | \\t meta [name] | \\t asklog <days>";
const ASK_LOG_USAGE: &str = "usage: \\t asklog <days>, 0 to 365; 0 stops the log and deletes it";
/// What an engine older than kit 0.3.0 means when it answers `Feedback` or `SetAskLog` with an
/// UNIMPLEMENTED that carries no message.
const NO_ASK_LOG: &str = "this engine keeps no question log (it is older than kit 0.3.0)";
/// The longest note `\\good`, `\\partly` and `\\bad` send, in bytes; the engine refuses longer.
const NOTE_BYTES: usize = 1024;
const USER_USAGE: &str =
    "usage: \\u me | \\u add <user> | \\u grant <user> <tenant> | \\u remove <user> <tenant>";

pub struct Cli {
    pub rt: Runtime,
    pub client: Client,
    pub host: String,
    pub current: Option<String>,
    pub session: pb::Cost,
    /// What tab completes; filled from replies.
    pub names: Rc<RefCell<input::Names>>,
    /// A command that goes ahead only if the next line says so.
    pub pending: Option<Pending>,
    /// The tenant and id of the last answer the engine logged, what `\\good`, `\\partly` and
    /// `\\bad` judge; each `\\ask` clears it, a refused one too.
    pub last_ask: Option<(String, String)>,
}

/// What the next line confirms; any other line cancels it.
pub enum Pending {
    /// These documents erased, once the tenant's name is typed.
    Erase(pb::DocumentsRequest),
    /// This migration committed, once `yes` is typed.
    Migrate(pb::MigrateRequest),
}

fn add_cost(total: &mut pb::Cost, cost: &pb::Cost) {
    total.calls += cost.calls;
    total.input_tokens += cost.input_tokens;
    total.output_tokens += cost.output_tokens;
    total.reasoning_tokens += cost.reasoning_tokens;
    total.unreported_calls += cost.unreported_calls;
    total.seconds += cost.seconds;
    if let Some(dollars) = cost.dollars {
        total.dollars = Some(total.dollars.unwrap_or(0.0) + dollars);
    }
}

pub fn say(text: &str) {
    println!("{text}");
    let _ = io::stdout().flush();
}

impl Cli {
    fn tenant(&self) -> Result<String, String> {
        self.current
            .clone()
            .ok_or_else(|| "no tenant: run \\t create <name> first".to_string())
    }

    /// Ids a reply named, for completion after `\\s`.
    fn remember_ids<'a>(&self, ids: impl IntoIterator<Item = &'a String>) {
        let known = &mut self.names.borrow_mut().ids;
        for id in ids {
            if !known.contains(id) {
                known.push(id.clone());
            }
        }
    }

    /// The current tenant's entity and source ids, from its overview; nothing on an error.
    fn load_ids(&self) {
        let Some(tenant) = self.current.clone() else {
            return;
        };
        let mut client = self.client.clone();
        let request = pb::ShowRequest {
            tenant,
            id: String::new(),
        };
        self.names.borrow_mut().ids.clear();
        if let Ok(reply) = self.rt.block_on(client.show(request))
            && let Some(pb::show_reply::View::Overview(o)) = reply.into_inner().view
        {
            self.remember_ids(o.entities.iter().map(|e| &e.id).chain(&o.source_ids));
        }
    }

    fn call<T>(&self, result: Result<tonic::Response<T>, Status>) -> Result<T, String> {
        result
            .map(tonic::Response::into_inner)
            .map_err(|status| describe(&status, &self.host))
    }

    /// `call` for `Feedback` and `SetAskLog`, which an engine older than kit 0.3.0 does not have.
    fn call_ask_log<T>(&self, result: Result<tonic::Response<T>, Status>) -> Result<T, String> {
        match result {
            Err(status) if status.code() == Code::Unimplemented => Err(NO_ASK_LOG.into()),
            result => self.call(result),
        }
    }

    /// Prints the engine, its model and who is signed in. Returns false when the user or key
    /// is wrong, which ends the CLI; an engine that cannot be reached is only reported.
    pub fn welcome(&mut self) -> bool {
        let mut client = self.client.clone();
        let me = match self.rt.block_on(client.me(pb::Empty {})) {
            Ok(me) => me.into_inner(),
            Err(status) if status.code() == Code::Unauthenticated => {
                say(&render::error(status.message()));
                return false;
            }
            Err(status) => {
                say(&render::error(&describe(&status, &self.host)));
                return true;
            }
        };
        let signed_in = format!("signed in as {} ({})", paint::bold(&me.name), me.role);
        let health = self.call(self.rt.block_on(client.health(pb::Empty {})));
        match &health {
            Ok(h) if h.llm_error.is_empty() => say(&format!(
                "{} {} · llm {} · tagger {} · embedder {} · {signed_in}",
                paint::dim("engine:"),
                paint::bold(&self.host),
                paint::cyan(&h.llm),
                paint::cyan(&h.tagger),
                paint::cyan(&h.embedder)
            )),
            Ok(h) => say(&format!(
                "{} {} · {signed_in}\n{}",
                paint::dim("engine:"),
                paint::bold(&self.host),
                render::warning(&format!(
                    "the engine has no LLM configured ({}); \\import and \\ask will fail",
                    h.llm_error
                ))
            )),
            Err(e) => say(&render::error(e)),
        }
        // What the engine says is wrong now (kit 0.4.0): a provider refusing its key or out of
        // credit, a tenant's index missing vectors. An older engine says nothing.
        if let Ok(h) = &health {
            for line in &h.degraded {
                say(&render::warning(&format!("the engine reports {line}")));
            }
        }
        if let Ok(list) = self.rt.block_on(client.list_tenants(pb::Empty {})) {
            self.names.borrow_mut().tenants = list
                .into_inner()
                .tenants
                .into_iter()
                .map(|t| t.name)
                .collect();
        }
        true
    }

    fn run(&mut self, command: Command) -> Result<Option<String>, String> {
        match command {
            Command::Tenant(sub, arg) => self.tenant_command(sub, arg).map(Some),
            Command::User(sub, rest) => self.user_command(sub, rest).map(Some),
            Command::Import("fact", text) if !text.is_empty() => {
                self.import_fact(text).map(|()| None)
            }
            Command::Import("blob", file) if !file.is_empty() => {
                self.import_blob(file).map(|()| None)
            }
            Command::Import(..) => Err(IMPORT_USAGE.into()),
            Command::Ask(q) => {
                // Any question, a refused one too, leaves the answer before it unjudged.
                self.last_ask = None;
                match q.text.is_empty() {
                    true => Err("usage: \\ask [--staged] [--facts] <question>".into()),
                    false => self.ask(&q).map(|()| None),
                }
            }
            Command::Feedback(verdict, note) => self.feedback(verdict, note).map(Some),
            Command::Show(id) => self.show(id).map(Some),
            Command::Commit => self.commit(true).map(Some),
            Command::Rollback => self.commit(false).map(Some),
            Command::Documents(verb, documents) => self.documents(verb, &documents).map(Some),
            Command::Migrate(migration) => self.migrate(&migration).map(Some),
            Command::Acl(doc, principals) => self.acl(doc, &principals).map(Some),
            Command::Principals(user, groups) => self.principals(user, &groups).map(Some),
            Command::Version => self.version().map(Some),
            Command::Help => Ok(Some(HELP.to_string())),
            Command::Quit => unreachable!("handled by the loop"),
            Command::Unknown(name) => Err(format!("unknown command {name}, see \\h")),
        }
    }

    /// `\\v`: which build of the engine answers, and which build of the CLI asks.
    fn version(&self) -> Result<String, String> {
        let mut client = self.client.clone();
        let health = self.call(self.rt.block_on(client.health(pb::Empty {})))?;
        Ok(render::version(
            &self.host,
            &health,
            env!("ONTOLOGIC_VERSION"),
        ))
    }

    fn tenant_command(&mut self, sub: &str, arg: &str) -> Result<String, String> {
        let mut client = self.client.clone();
        let named = |arg: &str| match arg.is_empty() {
            true => Err(format!("usage: \\t {sub} <name>")),
            false => Ok(pb::TenantName {
                name: arg.to_string(),
            }),
        };
        match sub {
            "create" => {
                let t = self.call(self.rt.block_on(client.create_tenant(named(arg)?)))?;
                self.current = Some(t.name.clone());
                let mut names = self.names.borrow_mut();
                names.tenants.push(t.name.clone());
                names.ids.clear();
                Ok(format!("tenant {} created", paint::bold_blue(&t.name)))
            }
            "checkout" => {
                let t = self
                    .call(self.rt.block_on(client.get_tenant(named(arg)?)))
                    .map_err(|e| match e.starts_with("no tenant") {
                        true => format!("{e} (\\t create {arg})"),
                        false => e,
                    })?;
                self.current = Some(t.name.clone());
                self.load_ids();
                Ok(format!("tenant {}", paint::bold_blue(&t.name)))
            }
            "get" if arg.is_empty() => {
                let list = self.call(self.rt.block_on(client.list_tenants(pb::Empty {})))?;
                self.names.borrow_mut().tenants =
                    list.tenants.iter().map(|t| t.name.clone()).collect();
                Ok(render::tenant_list(&list, self.current.as_deref()))
            }
            "users" if arg.is_empty() => {
                let name = self.tenant()?;
                let request = pb::TenantName { name: name.clone() };
                let list = self.call(self.rt.block_on(client.tenant_users(request)))?;
                Ok(render::tenant_users(&name, &list))
            }
            "meta" => {
                let name = match arg.is_empty() {
                    true => self.tenant()?,
                    false => arg.to_string(),
                };
                let meta = self.call(self.rt.block_on(client.tenant_meta(named(&name)?)))?;
                Ok(render::meta(&meta))
            }
            "asklog" => {
                let days = arg
                    .parse::<u32>()
                    .ok()
                    .filter(|days| *days <= 365)
                    .ok_or_else(|| ASK_LOG_USAGE.to_string())?;
                let tenant = self.tenant()?;
                let request = pb::AskLogRequest {
                    tenant: tenant.clone(),
                    keep_days: days,
                };
                self.call_ask_log(self.rt.block_on(client.set_ask_log(request)))?;
                Ok(render::ask_log(&tenant, days))
            }
            "delete" => {
                let t = self.call(self.rt.block_on(client.delete_tenant(named(arg)?)))?;
                self.names
                    .borrow_mut()
                    .tenants
                    .retain(|name| *name != t.name);
                if self.current.as_deref() == Some(t.name.as_str()) {
                    self.current = None;
                    self.names.borrow_mut().ids.clear();
                }
                Ok(format!("deleted {}: {}", t.name, render::summary(&t)))
            }
            "export" => {
                if arg.is_empty() {
                    return Err("usage: \\t export <file>".into());
                }
                let name = self.tenant()?;
                let file = self.call(
                    self.rt
                        .block_on(client.export_turtle(pb::TenantName { name })),
                )?;
                let path = Path::new(arg);
                if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                }
                fs::write(path, &file.content).map_err(|e| format!("{arg}: {e}"))?;
                let staged = file
                    .summary
                    .as_ref()
                    .and_then(|s| s.staged.as_ref())
                    .is_some_and(|s| s.sources > 0);
                let summary = file
                    .summary
                    .map(|s| render::summary(&s))
                    .unwrap_or_default();
                let note = match staged {
                    true => "; staged imports are not exported until \\commit",
                    false => "",
                };
                Ok(format!("wrote {} ({summary}){note}", paint::bold(arg)))
            }
            "import" => {
                if arg.is_empty() {
                    return Err("usage: \\t import <file>".into());
                }
                let name = self.tenant()?;
                let content = fs::read_to_string(arg).map_err(|e| format!("{arg}: {e}"))?;
                let request = pb::TurtleFile {
                    tenant: name,
                    content,
                    summary: None,
                };
                let imported = self
                    .call(self.rt.block_on(client.import_turtle(request)))
                    .map_err(|e| match e.starts_with("tenant not empty") {
                        true => e,
                        false => format!("{arg}: {e}"),
                    })?;
                let mut out = String::new();
                if !imported.warning.is_empty() {
                    out.push_str(&render::warning(&imported.warning));
                    out.push('\n');
                }
                let summary = imported
                    .summary
                    .map(|s| render::summary(&s))
                    .unwrap_or_default();
                out.push_str(&format!("imported {summary} from {}", paint::bold(arg)));
                Ok(out)
            }
            _ => Err(TENANT_USAGE.into()),
        }
    }

    fn import_fact(&mut self, text: &str) -> Result<(), String> {
        let tenant = self.tenant()?;
        let mut client = self.client.clone();
        // No key: every import from the CLI is a document of its own.
        let request = pb::ImportRequest {
            tenant,
            text: text.to_string(),
            key: String::new(),
        };
        let stream = self.call(self.rt.block_on(client.import(request)))?;
        self.follow_import(stream)
    }

    /// Sends a file from the CLI's directory; the engine reads it by its name.
    fn import_blob(&mut self, file: &str) -> Result<(), String> {
        let tenant = self.tenant()?;
        let content = fs::read(file).map_err(|e| format!("{file}: {e}"))?;
        let name = Path::new(file)
            .file_name()
            .map_or(file.to_string(), |n| n.to_string_lossy().to_string());
        let mut client = self.client.clone();
        let request = pb::BlobRequest {
            tenant,
            name,
            content,
            key: String::new(),
        };
        let stream = self
            .call(self.rt.block_on(client.import_blob(request)))
            .map_err(|e| format!("{file}: {e}"))?;
        self.follow_import(stream)
    }

    /// Prints each stage of an import as it arrives.
    fn follow_import(
        &mut self,
        mut stream: tonic::Streaming<pb::ImportEvent>,
    ) -> Result<(), String> {
        use pb::import_event::Event;
        loop {
            let next = self.rt.block_on(stream.message());
            let event = match next {
                Ok(Some(pb::ImportEvent { event: Some(event) })) => event,
                Ok(Some(_)) => continue,
                Ok(None) => return Ok(()),
                Err(status) => return Err(describe(&status, &self.host)),
            };
            match event {
                Event::Source(e) => {
                    self.remember_ids([&e.id]);
                    say(&render::source_saved(&e))
                }
                Event::Code(e) => say(&render::code_pass(&e)),
                Event::Linked(e) => {
                    self.remember_ids(e.new_entities.iter().map(|n| &n.id));
                    say(&render::linked_event(&e))
                }
                Event::Relinked(e) => say(&render::relinked(&e)),
                Event::Reasked(e) => say(&render::reasked(&e)),
                Event::People(e) => {
                    self.remember_ids(e.new_entities.iter().map(|n| &n.id));
                    say(&render::people(&e))
                }
                Event::Finished(e) => {
                    if let Some(cost) = &e.cost {
                        add_cost(&mut self.session, cost);
                    }
                    say(&render::finished(&e));
                }
            }
        }
    }

    /// `\\commit` (true) or `\\rollback` (false).
    fn commit(&mut self, commit: bool) -> Result<String, String> {
        let name = pb::TenantName {
            name: self.tenant()?,
        };
        let mut client = self.client.clone();
        let reply = match commit {
            true => self.rt.block_on(client.commit(name)),
            false => self.rt.block_on(client.rollback(name)),
        };
        let summary = self.call(reply)?;
        Ok(render::committed(&summary, commit))
    }

    fn ask(&mut self, q: &input::Question) -> Result<(), String> {
        use pb::ask_event::Event;
        let question = q.text;
        let tenant = self.tenant()?;
        let mut client = self.client.clone();
        let request = pb::AskRequest {
            tenant: tenant.clone(),
            question: question.to_string(),
            graph_only: q.facts,
            staged: q.staged,
            include_set_members: false,
        };
        let mut stream = self.call(self.rt.block_on(client.ask(request)))?;
        loop {
            let event = match self.rt.block_on(stream.message()) {
                Ok(Some(pb::AskEvent { event: Some(event) })) => event,
                Ok(Some(_)) => continue,
                Ok(None) => return Ok(()),
                Err(status) => return Err(describe(&status, &self.host)),
            };
            match event {
                Event::Index(e) => say(&render::ask_index(question, &e)),
                Event::Answer(e) => say(&render::ask_answer(&e, q.facts)),
                // The CLI never asks for a set's members, so an engine sends no pages.
                Event::SetPage(e) => say(&render::set_page(&e)),
                Event::Finished(e) => {
                    if let Some(cost) = &e.cost {
                        add_cost(&mut self.session, cost);
                    }
                    if !e.ask_id.is_empty() {
                        self.last_ask = Some((tenant.clone(), e.ask_id.clone()));
                    }
                    say(&render::finished(&pb::Finished { waiting: 0, ..e }));
                }
            }
        }
    }

    /// `\\good`, `\\partly` and `\\bad`: whether the last answer the engine logged in this
    /// tenant was right, with a note on why.
    fn feedback(&mut self, verdict: &str, note: &str) -> Result<String, String> {
        let Some((tenant, ask_id)) = self
            .last_ask
            .clone()
            .filter(|(tenant, _)| self.current.as_ref() == Some(tenant))
        else {
            return Err("no logged answer to give feedback on".into());
        };
        if note.len() > NOTE_BYTES {
            return Err(format!(
                "a note is at most {NOTE_BYTES} bytes, and this one is {}; nothing was sent",
                note.len()
            ));
        }
        let request = pb::FeedbackRequest {
            tenant,
            ask_id: ask_id.clone(),
            verdict: verdict.to_string(),
            note: note.to_string(),
        };
        let mut client = self.client.clone();
        self.call_ask_log(self.rt.block_on(client.feedback(request)))?;
        Ok(render::feedback(&ask_id, verdict, !note.is_empty()))
    }

    fn user_command(&mut self, sub: &str, rest: &str) -> Result<String, String> {
        let mut client = self.client.clone();
        let (user, tenant) = input::split_word(rest);
        let access = || pb::Access {
            user: user.to_string(),
            tenant: tenant.to_string(),
        };
        match sub {
            "me" if rest.is_empty() => {
                let me = self.call(self.rt.block_on(client.me(pb::Empty {})))?;
                Ok(render::me(&me))
            }
            "add" if !user.is_empty() && tenant.is_empty() => {
                let request = pb::UserName {
                    name: user.to_string(),
                };
                let added = self.call(self.rt.block_on(client.add_user(request)))?;
                Ok(render::new_user(&added))
            }
            "grant" if !user.is_empty() && !tenant.is_empty() => {
                let granted = self.call(self.rt.block_on(client.grant(access())))?;
                Ok(render::access(&granted, tenant, true))
            }
            "remove" if !user.is_empty() && !tenant.is_empty() => {
                let revoked = self.call(self.rt.block_on(client.revoke(access())))?;
                Ok(render::access(&revoked, tenant, false))
            }
            _ => Err(USER_USAGE.into()),
        }
    }

    /// `\\retract` and `\\restore` at once; `\\erase` asks for the tenant's name first.
    fn documents(&mut self, verb: &str, documents: &input::Documents) -> Result<String, String> {
        if documents.ids.is_empty() {
            return Err(format!("usage: \\{verb} <doc...> [reason]"));
        }
        let request = pb::DocumentsRequest {
            tenant: self.tenant()?,
            documents: documents.ids.iter().map(|id| id.to_string()).collect(),
            reason: documents.reason.to_string(),
        };
        let mut client = self.client.clone();
        let reply = match verb {
            "retract" => self.rt.block_on(client.retract(request)),
            "restore" => self.rt.block_on(client.restore(request)),
            _ => {
                let question = render::erase_question(&request);
                self.pending = Some(Pending::Erase(request));
                return Ok(question);
            }
        };
        let done = match verb {
            "retract" => "retracted",
            _ => "restored",
        };
        Ok(render::lifecycle(done, &self.call(reply)?))
    }

    /// `\\migrate`: the migration staged and its diff shown, then committed if the next line is
    /// `yes`.
    fn migrate(&mut self, migration: &input::Migration) -> Result<String, String> {
        if migration.pins.is_empty() {
            return Err("usage: \\migrate <kind:version ...> [reason]".into());
        }
        let mut request = pb::MigrateRequest {
            tenant: self.tenant()?,
            kind: pb::MigrationKind::Reprojection as i32,
            pins: migration
                .pins
                .iter()
                .map(|(kind, version)| pb::SchemaPin {
                    kind: kind.to_string(),
                    version: *version,
                })
                .collect(),
            reason: migration.reason.to_string(),
            commit: false,
        };
        let mut client = self.client.clone();
        let staged = self.call(self.rt.block_on(client.migrate(request.clone())))?;
        request.commit = true;
        let shown = render::migration(&request, &staged);
        self.pending = Some(Pending::Migrate(request));
        Ok(format!(
            "{shown}\ntype yes to commit it; anything else cancels"
        ))
    }

    /// The line after `\\erase` or a staged `\\migrate`: the tenant's name erases, `yes`
    /// commits, and anything else leaves the tenant as it is.
    fn confirm(&mut self, pending: Pending, line: &str) -> Result<String, String> {
        let mut client = self.client.clone();
        match pending {
            Pending::Erase(request) if line == request.tenant => {
                let erased = self.call(self.rt.block_on(client.erase(request)))?;
                Ok(render::lifecycle("erased", &erased))
            }
            Pending::Migrate(request) if line == "yes" => {
                let committed = self.call(self.rt.block_on(client.migrate(request.clone())))?;
                Ok(render::migration(&request, &committed))
            }
            Pending::Erase(_) => Ok("nothing erased".into()),
            Pending::Migrate(_) => Ok("nothing committed".into()),
        }
    }

    /// `\\acl`: who may read a document from now on.
    fn acl(&mut self, doc: &str, principals: &[&str]) -> Result<String, String> {
        if doc.is_empty() || principals.is_empty() {
            return Err("usage: \\acl <doc> <principal...> (user:<name> or a group)".into());
        }
        let request = pb::AclRequest {
            tenant: self.tenant()?,
            document: doc.to_string(),
            principals: principals.iter().map(|p| p.to_string()).collect(),
        };
        let mut client = self.client.clone();
        self.call(self.rt.block_on(client.set_acl(request)))?;
        Ok(render::acl(doc, principals))
    }

    /// `\\principals`: the groups a member reads the current tenant's documents as; none leaves
    /// them reading as themselves.
    fn principals(&mut self, user: &str, groups: &[&str]) -> Result<String, String> {
        if user.is_empty() {
            return Err("usage: \\principals <user> <group...>".into());
        }
        let tenant = self.tenant()?;
        let request = pb::PrincipalsRequest {
            user: user.to_string(),
            tenant: tenant.clone(),
            groups: groups.iter().map(|g| g.to_string()).collect(),
        };
        let mut client = self.client.clone();
        let member = self.call(self.rt.block_on(client.set_principals(request)))?;
        Ok(render::principals(&member, &tenant))
    }

    fn show(&mut self, id: &str) -> Result<String, String> {
        use pb::show_reply::View;
        let tenant = self.tenant()?;
        let mut client = self.client.clone();
        let request = pb::ShowRequest {
            tenant,
            id: id.to_string(),
        };
        let reply = self.call(self.rt.block_on(client.show(request)))?;
        Ok(match reply.view {
            Some(View::Overview(o)) => {
                self.names.borrow_mut().ids.clear();
                self.remember_ids(o.entities.iter().map(|e| &e.id).chain(&o.source_ids));
                render::overview(&o, &self.session)
            }
            Some(View::Entity(e)) => render::entity_view(&e),
            Some(View::Source(s)) => render::source_view(&s),
            None => String::new(),
        })
    }

    pub fn prompt(&self) -> String {
        match self.pending {
            Some(_) => "confirm> ".to_string(),
            None => format!("{}> ", self.current.as_deref().unwrap_or("ontologic")),
        }
    }

    /// Runs one line; false means quit.
    pub fn line(&mut self, line: &str) -> bool {
        if let Some(pending) = self.pending.take() {
            match self.confirm(pending, line.trim()) {
                Ok(text) => say(&text),
                Err(e) => say(&render::error(&e)),
            }
            return true;
        }
        let Some(command) = input::parse(line) else {
            return true;
        };
        if matches!(command, Command::Quit) {
            return false;
        }
        match self.run(command) {
            Ok(Some(text)) => say(&text),
            Ok(None) => {}
            Err(e) => say(&render::error(&e)),
        }
        true
    }
}
