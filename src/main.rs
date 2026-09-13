//! `brain`: the command line for a brain engine. Every command calls the engine over gRPC
//! with TLS and renders what comes back; imports and questions print stage by stage.

mod input;
mod paint;
mod render;

use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use brain_proto as pb;
use brain_proto::brain_client::BrainClient;
use input::Command;
use tokio::runtime::Runtime;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};
use tonic::{Code, Status};

const DEFAULT_PORT: u16 = 6969;
const HISTORY: &str = ".brain_history";
const LOCAL_CA: &str = ".brain/tls/engine.pem";

const USAGE: &str = "\
usage: brain [-h|--host <host[:port]>] [--ca <pem>] [--no-color] [--help]

  -h, --host   the engine (default localhost:6969); https://, or http:// for an engine run with --no-tls
  --ca         trust this certificate (default for localhost: .brain/tls/engine.pem)
  --no-color   plain output";

const HELP: &str = "\
\\t create <name>      create a tenant and switch to it
\\t checkout <name>    switch to a tenant
\\t get                list the tenants; * marks the one you are on
\\t delete <name>      forget a tenant and everything in it
\\t export <file>      save the tenant to a .ttl file in this directory
\\t import <file>      load a .ttl file into the current (empty) tenant
\\import <sentence>    add a fact: link names, find relations, re-link older sources
\\ask <question>       answer from what the tenant knows, with probabilities
\\s                    what the tenant knows, and what it cost
\\s <id>               everything about an entity (e1) or a source (s1)
\\h                    this help
\\q                    quit (Ctrl-D works too)
Up and down arrows walk the history.";

const TENANT_USAGE: &str = "usage: \\t create <name> | \\t checkout <name> | \\t get | \\t delete <name> | \\t export <file> | \\t import <file>";

struct Args {
    host: String,
    ca: Option<PathBuf>,
    color: bool,
}

fn args() -> Result<Args, String> {
    let mut args = Args {
        host: format!("localhost:{DEFAULT_PORT}"),
        ca: None,
        color: io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--host" => args.host = it.next().ok_or("--host needs a value")?,
            "--ca" => args.ca = Some(PathBuf::from(it.next().ok_or("--ca needs a value")?)),
            "--no-color" => args.color = false,
            "--help" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    Ok(args)
}

/// `localhost` -> (`https://localhost:6969`, `localhost`); an `http://` host stays plain.
fn endpoint_url(host: &str) -> (String, String, bool) {
    let (scheme, rest, tls) = match host {
        h if h.starts_with("http://") => ("http", &h["http://".len()..], false),
        h if h.starts_with("https://") => ("https", &h["https://".len()..], true),
        h => ("https", h, true),
    };
    let rest = rest.trim_end_matches('/');
    let has_port = match rest.rsplit_once(':') {
        Some((_, port)) => port.parse::<u16>().is_ok() && !rest.ends_with(']'),
        None => false,
    };
    let authority = match has_port {
        true => rest.to_string(),
        false => format!("{rest}:{DEFAULT_PORT}"),
    };
    let name = authority
        .rsplit_once(':')
        .map_or(authority.as_str(), |(name, _)| name)
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    (format!("{scheme}://{authority}"), name, tls)
}

fn connect(args: &Args) -> Result<(BrainClient<Channel>, String), String> {
    let (url, name, tls) = endpoint_url(&args.host);
    let mut endpoint = Endpoint::from_shared(url.clone())
        .map_err(|e| format!("bad host {}: {e}", args.host))?
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(600));
    if tls {
        let mut config = ClientTlsConfig::new().domain_name(name.clone());
        let local = matches!(name.as_str(), "localhost" | "127.0.0.1" | "::1");
        let ca = args.ca.clone().or_else(|| {
            local
                .then(|| PathBuf::from(LOCAL_CA))
                .filter(|p| p.exists())
        });
        config = match ca {
            Some(path) => {
                let pem = fs::read_to_string(&path)
                    .map_err(|e| format!("reading {}: {e}", path.display()))?;
                config.ca_certificate(Certificate::from_pem(pem))
            }
            None => config.with_webpki_roots(),
        };
        endpoint = endpoint
            .tls_config(config)
            .map_err(|e| format!("TLS setup for {url}: {e}"))?;
    }
    let host = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_string();
    Ok((BrainClient::new(endpoint.connect_lazy()), host))
}

struct Cli {
    rt: Runtime,
    client: BrainClient<Channel>,
    host: String,
    current: Option<String>,
    session: pb::Cost,
}

/// What went wrong, in words: unreachable engines and TLS failures get a hint.
fn describe(status: &Status, host: &str) -> String {
    if status.code() != Code::Unavailable {
        return status.message().to_string();
    }
    // The innermost cause says what actually failed: refused, timed out, bad certificate.
    let mut detail = status.message().to_string();
    let mut source = std::error::Error::source(status);
    while let Some(e) = source {
        detail = e.to_string();
        source = e.source();
    }
    format!("cannot reach engine at {host} (start it with make run): {detail}")
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

fn say(text: &str) {
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

    fn health(&mut self) {
        let mut client = self.client.clone();
        match self.call(self.rt.block_on(client.health(pb::Empty {}))) {
            Ok(h) if h.llm_error.is_empty() => say(&format!(
                "{} {} · llm {}",
                paint::dim("engine:"),
                paint::bold(&self.host),
                paint::cyan(&h.llm)
            )),
            Ok(h) => say(&render::warning(&format!(
                "engine at {} has no LLM configured ({}); \\import and \\ask will fail",
                self.host, h.llm_error
            ))),
            Err(e) => say(&render::error(&e)),
        }
    }

    fn run(&mut self, command: Command) -> Result<Option<String>, String> {
        match command {
            Command::Tenant(sub, arg) => self.tenant_command(sub, arg).map(Some),
            Command::Import("") => Err("usage: \\import <sentence>".into()),
            Command::Import(text) => self.import(text).map(|()| None),
            Command::Ask("") => Err("usage: \\ask <question>".into()),
            Command::Ask(text) => self.ask(text).map(|()| None),
            Command::Show(id) => self.show(id).map(Some),
            Command::Help => Ok(Some(HELP.to_string())),
            Command::Quit => unreachable!("handled by the loop"),
            Command::Unknown(name) => Err(format!("unknown command {name}, see \\h")),
        }
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
                let summary = file
                    .summary
                    .map(|s| render::summary(&s))
                    .unwrap_or_default();
                Ok(format!("wrote {} ({summary})", paint::bold(arg)))
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

    fn import(&mut self, text: &str) -> Result<(), String> {
        use pb::import_event::Event;
        let tenant = self.tenant()?;
        let mut client = self.client.clone();
        let request = pb::ImportRequest {
            tenant,
            text: text.to_string(),
        };
        let mut stream = self.call(self.rt.block_on(client.import(request)))?;
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
                Event::Finished(e) => {
                    if let Some(cost) = &e.cost {
                        add_cost(&mut self.session, cost);
                    }
                    say(&render::finished(&e));
                }
            }
        }
    }

    fn ask(&mut self, question: &str) -> Result<(), String> {
        use pb::ask_event::Event;
        let tenant = self.tenant()?;
        let mut client = self.client.clone();
        let request = pb::AskRequest {
            tenant,
            question: question.to_string(),
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

    fn prompt(&self) -> String {
        format!("{}> ", self.current.as_deref().unwrap_or("brain"))
    }

    /// Runs one line; false means quit.
    fn line(&mut self, line: &str) -> bool {
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

fn interactive(cli: &mut Cli) -> rustyline::Result<()> {
    use rustyline::error::ReadlineError;
    let mut editor = rustyline::Editor::<input::Colors, rustyline::history::DefaultHistory>::new()?;
    if paint::is_on() {
        editor.set_helper(Some(input::Colors));
    }
    let _ = editor.load_history(HISTORY);
    loop {
        match editor.readline(&cli.prompt()) {
            Ok(line) => {
                if !line.trim().is_empty() {
                    let _ = editor.add_history_entry(line.as_str());
                    let _ = editor.save_history(HISTORY);
                }
                if !cli.line(&line) {
                    return Ok(());
                }
            }
            Err(ReadlineError::Eof | ReadlineError::Interrupted) => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

fn piped(cli: &mut Cli) {
    let stdin = io::stdin();
    loop {
        print!("{}", cli.prompt());
        let _ = io::stdout().flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => {
                println!();
                return;
            }
            Ok(_) => {
                if !cli.line(&line) {
                    return;
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                return;
            }
        }
    }
}

fn main() {
    let args = match args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    paint::enable(args.color);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("start the runtime");
    let connected = {
        let _guard = rt.enter();
        connect(&args)
    };
    let (client, host) = match connected {
        Ok(connected) => connected,
        Err(e) => {
            eprintln!("{}", render::error(&e));
            std::process::exit(1);
        }
    };
    let mut cli = Cli {
        rt,
        client,
        host,
        current: None,
        session: pb::Cost::default(),
    };
    say(&format!(
        "{} {}",
        paint::bold("brain"),
        paint::dim("\\h for help, \\q to quit")
    ));
    cli.health();
    if io::stdin().is_terminal() {
        if let Err(e) = interactive(&mut cli) {
            eprintln!("{}", render::error(&e.to_string()));
        }
    } else {
        piped(&mut cli);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_get_a_scheme_and_the_default_port() {
        assert_eq!(
            endpoint_url("localhost"),
            ("https://localhost:6969".into(), "localhost".into(), true)
        );
        assert_eq!(
            endpoint_url("brain.example.com:443"),
            (
                "https://brain.example.com:443".into(),
                "brain.example.com".into(),
                true
            )
        );
        assert_eq!(
            endpoint_url("http://127.0.0.1:7000/"),
            ("http://127.0.0.1:7000".into(), "127.0.0.1".into(), false)
        );
        assert_eq!(
            endpoint_url("https://brain.example.com"),
            (
                "https://brain.example.com:6969".into(),
                "brain.example.com".into(),
                true
            )
        );
    }
}
