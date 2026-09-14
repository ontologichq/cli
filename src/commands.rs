//! What each command does: call the engine, then render the reply.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use brain_proto as pb;
use tokio::runtime::Runtime;
use tonic::{Code, Status};

use crate::connect::{Client, describe};
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
\\import fact <text>  add a fact: link things, find relations, re-link older sources
\\import blob <file>  add a file from this directory: an email (.eml) or text, part by part
\\ask <question>       answer from what the tenant has committed, with probabilities
\\ask --staged <q>     answer with the imports not committed yet included
\\commit               make the staged imports part of the tenant, so questions see them
\\rollback             drop the staged imports (and the re-links they made)
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
const TENANT_USAGE: &str = "usage: \\t create <name> | \\t checkout <name> | \\t get | \\t delete <name> | \\t export <file> | \\t import <file> | \\t users";
const USER_USAGE: &str =
    "usage: \\u me | \\u add <user> | \\u grant <user> <tenant> | \\u remove <user> <tenant>";

pub struct Cli {
    pub rt: Runtime,
    pub client: Client,
    pub host: String,
    pub current: Option<String>,
    pub session: pb::Cost,
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

    fn call<T>(&self, result: Result<tonic::Response<T>, Status>) -> Result<T, String> {
        result
            .map(tonic::Response::into_inner)
            .map_err(|status| describe(&status, &self.host))
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
        match self.call(self.rt.block_on(client.health(pb::Empty {}))) {
            Ok(h) if h.llm_error.is_empty() => say(&format!(
                "{} {} · llm {} · tagger {} · {signed_in}",
                paint::dim("engine:"),
                paint::bold(&self.host),
                paint::cyan(&h.llm),
                paint::cyan(&h.tagger)
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
            Err(e) => say(&render::error(&e)),
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
            Command::Ask(q) if q.text.is_empty() => {
                Err("usage: \\ask [--staged] [--facts] <question>".into())
            }
            Command::Ask(q) => self.ask(&q).map(|()| None),
            Command::Show(id) => self.show(id).map(Some),
            Command::Commit => self.commit(true).map(Some),
            Command::Rollback => self.commit(false).map(Some),
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
        Ok(render::version(&self.host, &health, env!("BRAIN_VERSION")))
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
                Ok(format!("tenant {}", paint::bold_blue(&t.name)))
            }
            "get" if arg.is_empty() => {
                let list = self.call(self.rt.block_on(client.list_tenants(pb::Empty {})))?;
                Ok(render::tenant_list(&list, self.current.as_deref()))
            }
            "users" if arg.is_empty() => {
                let name = self.tenant()?;
                let request = pb::TenantName { name: name.clone() };
                let list = self.call(self.rt.block_on(client.tenant_users(request)))?;
                Ok(render::tenant_users(&name, &list))
            }
            "delete" => {
                let t = self.call(self.rt.block_on(client.delete_tenant(named(arg)?)))?;
                if self.current.as_deref() == Some(t.name.as_str()) {
                    self.current = None;
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
        let request = pb::ImportRequest {
            tenant,
            text: text.to_string(),
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
                Event::Source(e) => say(&render::source_saved(&e)),
                Event::Code(e) => say(&render::code_pass(&e)),
                Event::Linked(e) => say(&render::linked_event(&e)),
                Event::Relinked(e) => say(&render::relinked(&e)),
                Event::Reasked(e) => say(&render::reasked(&e)),
                Event::People(e) => say(&render::people(&e)),
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
            tenant,
            question: question.to_string(),
            graph_only: q.facts,
            staged: q.staged,
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
                Event::Answer(e) => say(&render::ask_answer(&e)),
                Event::Finished(e) => {
                    if let Some(cost) = &e.cost {
                        add_cost(&mut self.session, cost);
                    }
                    say(&render::finished(&pb::Finished { waiting: 0, ..e }));
                }
            }
        }
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
            Some(View::Overview(o)) => render::overview(&o, &self.session),
            Some(View::Entity(e)) => render::entity_view(&e),
            Some(View::Source(s)) => render::source_view(&s),
            None => String::new(),
        })
    }

    pub fn prompt(&self) -> String {
        format!("{}> ", self.current.as_deref().unwrap_or("ontologic"))
    }

    /// Runs one line; false means quit.
    pub fn line(&mut self, line: &str) -> bool {
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
