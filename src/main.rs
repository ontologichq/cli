//! `ontologic`: the command line for an ontologic engine. It signs in, then every command calls
//! the engine over gRPC with TLS and renders what comes back; imports and questions print
//! stage by stage.

mod commands;
mod input;
mod paint;
mod render;

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;

use commands::{Cli, say};
use ontologic_kit::{DEFAULT_PORT, client, pb};

const HISTORY: &str = ".ontologic_history";

const USAGE: &str = "\
usage: ontologic -h <host:port> -u <user> -p <key> [--ca <pem>] [--no-color] [--help]

  -h, --host   the engine (default localhost:6969); https://, or http:// for an engine run with --no-tls
  -u, --user   your user name (or set ONTOLOGIC_USER)
  -p, --key    your key (or set ONTOLOGIC_KEY, which keeps it out of your shell history)
  --ca         trust this certificate (default: .ontologic/tls/engine.pem for localhost,
               .ontologic/tls/<host>.pem for other hosts, when the file exists)
  --no-color   plain output";

struct Args {
    host: String,
    user: String,
    key: String,
    ca: Option<PathBuf>,
    color: bool,
}

fn args() -> Result<Args, String> {
    let mut args = Args {
        host: format!("localhost:{DEFAULT_PORT}"),
        user: std::env::var("ONTOLOGIC_USER").unwrap_or_default(),
        key: std::env::var("ONTOLOGIC_KEY").unwrap_or_default(),
        ca: None,
        color: io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--host" => args.host = it.next().ok_or("--host needs a value")?,
            "-u" | "--user" => args.user = it.next().ok_or("--user needs a value")?,
            "-p" | "--key" => args.key = it.next().ok_or("--key needs a value")?,
            "--ca" => args.ca = Some(PathBuf::from(it.next().ok_or("--ca needs a value")?)),
            "--no-color" => args.color = false,
            "--help" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    if args.user.is_empty() || args.key.is_empty() {
        return Err(format!("sign in with -u <user> -p <key>\n{USAGE}"));
    }
    Ok(args)
}

fn interactive(cli: &mut Cli) -> rustyline::Result<()> {
    use rustyline::error::ReadlineError;
    let config = rustyline::Config::builder()
        .completion_type(rustyline::CompletionType::List)
        .build();
    let mut editor =
        rustyline::Editor::<input::Line, rustyline::history::DefaultHistory>::with_config(config)?;
    editor.set_helper(Some(input::Line::new(cli.names.clone(), paint::is_on())));
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
        client::connect(&args.host, args.ca.as_deref(), &args.user, &args.key)
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
        names: Default::default(),
        pending: None,
        last_ask: None,
    };
    say(&format!(
        "{} {}",
        paint::bold("ontologic"),
        paint::dim("\\h for help, \\q to quit")
    ));
    if !cli.welcome() {
        std::process::exit(1);
    }
    if io::stdin().is_terminal() {
        if let Err(e) = interactive(&mut cli) {
            eprintln!("{}", render::error(&e.to_string()));
        }
    } else {
        piped(&mut cli);
    }
}
