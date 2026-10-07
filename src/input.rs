//! Commands as typed: parsing, and highlighting and completing the line while it is being
//! typed.

use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::highlight::{CmdKind, Highlighter};
use rustyline::{Context, Helper, Hinter, Validator};

use crate::paint::ansi;

pub enum Command<'a> {
    /// `\t <subcommand> [arg]`
    Tenant(&'a str, &'a str),
    /// `\u <subcommand> [user [tenant]]`
    User(&'a str, &'a str),
    /// `\\import fact <sentence>` or `\\import blob <file>`
    Import(&'a str, &'a str),
    /// `\ask [--staged] [--facts] <question>`
    Ask(Question<'a>),
    /// `\good`, `\partly` or `\bad`, as the verdict sent (right, partly or wrong), and a note
    Feedback(&'a str, &'a str),
    Show(&'a str),
    Commit,
    Rollback,
    /// `\retract`, `\restore` or `\erase`, named without the backslash, and its documents
    Documents(&'a str, Documents<'a>),
    /// `\migrate <kind:version ...> [reason]`
    Migrate(Migration<'a>),
    /// `\acl <doc> <principal...>`
    Acl(&'a str, Vec<&'a str>),
    /// `\principals <user> <group...>`
    Principals(&'a str, Vec<&'a str>),
    Version,
    Help,
    Quit,
    Unknown(&'a str),
}

/// A question and how to answer it: from the staged copy, and from facts alone.
#[derive(Debug, PartialEq)]
pub struct Question<'a> {
    pub text: &'a str,
    pub staged: bool,
    pub facts: bool,
}

/// Documents by id (`d3`), and why.
#[derive(Debug, PartialEq)]
pub struct Documents<'a> {
    pub ids: Vec<&'a str>,
    pub reason: &'a str,
}

/// Schema versions to read the tenant under (`ticket:12`), and why.
#[derive(Debug, PartialEq)]
pub struct Migration<'a> {
    pub pins: Vec<(&'a str, u32)>,
    pub reason: &'a str,
}

pub const COMMANDS: &[&str] = &[
    "\\t",
    "\\u",
    "\\import",
    "\\ask",
    "\\good",
    "\\partly",
    "\\bad",
    "\\s",
    "\\commit",
    "\\rollback",
    "\\retract",
    "\\restore",
    "\\erase",
    "\\migrate",
    "\\acl",
    "\\principals",
    "\\v",
    "\\h",
    "\\q",
];
pub const TENANT_SUBCOMMANDS: &[&str] = &[
    "create", "checkout", "get", "delete", "import", "export", "users", "meta", "asklog",
];
pub const USER_SUBCOMMANDS: &[&str] = &["me", "add", "grant", "remove"];
pub const IMPORT_SUBCOMMANDS: &[&str] = &["fact", "blob"];

/// "import Priya lives here" -> ("import", "Priya lives here").
pub fn split_word(text: &str) -> (&str, &str) {
    let (word, rest) = text.split_at(text.find(char::is_whitespace).unwrap_or(text.len()));
    (word, rest.trim())
}

/// `"Priya lives here"` -> `Priya lives here`; unquoted text is left alone.
pub fn unquote(text: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = text.strip_prefix(quote).and_then(|t| t.strip_suffix(quote)) {
            return inner.trim();
        }
    }
    text
}

/// `--staged --facts who is CEO`: the flags in front, then the question.
fn question(mut rest: &str) -> Question<'_> {
    let (mut staged, mut facts) = (false, false);
    loop {
        match split_word(rest) {
            ("--staged", tail) => (staged, rest) = (true, tail),
            ("--facts", tail) => (facts, rest) = (true, tail),
            _ => break,
        }
    }
    Question {
        text: unquote(rest),
        staged,
        facts,
    }
}

/// `d3 d4 an old copy`: the document ids in front, then why.
fn documents(mut rest: &str) -> Documents<'_> {
    let mut ids = Vec::new();
    loop {
        let (word, tail) = split_word(rest);
        let digits = word.strip_prefix('d').unwrap_or_default();
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        ids.push(word);
        rest = tail;
    }
    Documents {
        ids,
        reason: unquote(rest),
    }
}

/// `ticket:12 postmortem:5 new rules`: the `kind:version` pins in front, then why.
fn migration(mut rest: &str) -> Migration<'_> {
    let mut pins = Vec::new();
    loop {
        let (word, tail) = split_word(rest);
        let Some((kind, Ok(version))) = word.rsplit_once(':').map(|(k, v)| (k, v.parse())) else {
            break;
        };
        if kind.is_empty() {
            break;
        }
        pins.push((kind, version));
        rest = tail;
    }
    Migration {
        pins,
        reason: unquote(rest),
    }
}

/// A line is `\command` plus optional text; the text is the rest of the line, trimmed.
pub fn parse(line: &str) -> Option<Command<'_>> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let (command, rest) = split_word(line);
    Some(match command {
        "\\t" => {
            let (sub, arg) = split_word(rest);
            Command::Tenant(sub, unquote(arg))
        }
        "\\u" => {
            let (sub, rest) = split_word(rest);
            Command::User(sub, rest)
        }
        "\\import" => {
            let (sub, rest) = split_word(rest);
            Command::Import(sub, unquote(rest))
        }
        "\\ask" => Command::Ask(question(rest)),
        "\\good" => Command::Feedback("right", unquote(rest)),
        "\\partly" => Command::Feedback("partly", unquote(rest)),
        "\\bad" => Command::Feedback("wrong", unquote(rest)),
        "\\s" => Command::Show(rest),
        "\\commit" => Command::Commit,
        "\\rollback" => Command::Rollback,
        "\\retract" | "\\restore" | "\\erase" => Command::Documents(&command[1..], documents(rest)),
        "\\migrate" => Command::Migrate(migration(rest)),
        "\\acl" => {
            let (doc, principals) = split_word(rest);
            Command::Acl(doc, principals.split_whitespace().collect())
        }
        "\\principals" => {
            let (user, groups) = split_word(rest);
            Command::Principals(user, groups.split_whitespace().collect())
        }
        "\\v" => Command::Version,
        "\\h" => Command::Help,
        "\\q" => Command::Quit,
        other => Command::Unknown(other),
    })
}

/// Whether a typed line may be written to the history file. Questions and feedback stay in the
/// session, and so does a line that is not a command (a question typed without `\ask`, or the
/// answer to a confirmation).
pub fn kept_on_disk(line: &str) -> bool {
    !matches!(
        parse(line),
        None | Some(Command::Ask(_) | Command::Feedback(..) | Command::Unknown(_))
    )
}

const COMMAND: &str = "1;36";
const SUBCOMMAND: &str = "1;35";
const TEXT: &str = "32";
const UNKNOWN: &str = "31";
const TYPING: &str = "36";

/// The line with the command, subcommand and text in their own colors. Only ANSI codes are
/// added, so the display width stays the same.
pub fn highlight_line(line: &str) -> String {
    let lead = line.len() - line.trim_start().len();
    let (spaces, rest) = line.split_at(lead);
    if !rest.starts_with('\\') {
        return line.to_string();
    }
    let command_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let (command, after) = rest.split_at(command_end);
    let command_color = match () {
        _ if COMMANDS.contains(&command) => COMMAND,
        _ if after.is_empty() && COMMANDS.iter().any(|c| c.starts_with(command)) => TYPING,
        _ => UNKNOWN,
    };
    let mut out = format!("{spaces}{}", ansi(command_color, command));
    if after.is_empty() {
        return out;
    }
    let gap = after.len() - after.trim_start().len();
    let (between, body) = after.split_at(gap);
    out.push_str(between);
    if body.is_empty() {
        return out;
    }
    match command {
        "\\t" | "\\u" | "\\import" => {
            let subcommands = match command {
                "\\t" => TENANT_SUBCOMMANDS,
                "\\u" => USER_SUBCOMMANDS,
                _ => IMPORT_SUBCOMMANDS,
            };
            let word_end = body.find(char::is_whitespace).unwrap_or(body.len());
            let (word, tail) = body.split_at(word_end);
            let word_color = match subcommands.contains(&word) {
                true => SUBCOMMAND,
                false => TYPING,
            };
            out.push_str(&ansi(word_color, word));
            let gap = tail.len() - tail.trim_start().len();
            let (between, text) = tail.split_at(gap);
            out.push_str(between);
            if !text.is_empty() {
                out.push_str(&ansi(TEXT, text));
            }
        }
        "\\ask" | "\\good" | "\\partly" | "\\bad" | "\\s" | "\\retract" | "\\restore"
        | "\\erase" | "\\migrate" | "\\acl" | "\\principals" => out.push_str(&ansi(TEXT, body)),
        _ => out.push_str(body),
    }
    out
}

/// Names the CLI has seen in the engine's replies, kept for tab completion, so a tab never
/// waits on the network.
#[derive(Default)]
pub struct Names {
    pub tenants: Vec<String>,
    /// Entity and source ids of the current tenant.
    pub ids: Vec<String>,
}

/// What a tab completes at the cursor.
#[derive(Debug, PartialEq)]
enum Offer {
    /// Where the word being completed starts, and the words that fit it.
    Words(usize, Vec<String>),
    Files,
}

/// The offer for the line up to the cursor: commands, then subcommands (both with a space
/// after), tenant names where a tenant belongs, ids after `\s`, flags on a question, and file
/// names where a file belongs.
fn offer(before: &str, names: &Names) -> Offer {
    let partial = match before.ends_with(char::is_whitespace) {
        true => "",
        false => before.split_whitespace().last().unwrap_or(""),
    };
    let start = before.len() - partial.len();
    let fitting = |options: &[&str], after: &str| -> Vec<String> {
        options
            .iter()
            .filter(|o| o.starts_with(partial))
            .map(|o| format!("{o}{after}"))
            .collect()
    };
    let known = |options: &[String]| -> Vec<String> {
        options
            .iter()
            .filter(|o| o.starts_with(partial))
            .cloned()
            .collect()
    };
    let words: Vec<&str> = before[..start].split_whitespace().collect();
    let offered = match words.as_slice() {
        [] => fitting(COMMANDS, " "),
        ["\\t"] => fitting(TENANT_SUBCOMMANDS, " "),
        ["\\u"] => fitting(USER_SUBCOMMANDS, " "),
        ["\\import"] => fitting(IMPORT_SUBCOMMANDS, " "),
        ["\\t", "checkout" | "delete" | "meta"] | ["\\u", "grant" | "remove", _] => {
            known(&names.tenants)
        }
        ["\\t", "import" | "export"] | ["\\import", "blob"] => return Offer::Files,
        ["\\s"] => known(&names.ids),
        ["\\ask", flags @ ..]
            if partial.starts_with('-') && flags.iter().all(|f| f.starts_with("--")) =>
        {
            fitting(&["--staged", "--facts"], " ")
        }
        _ => Vec::new(),
    };
    Offer::Words(start, offered)
}

/// The line editor's helper: colors while typing (when color is on) and tab completion.
#[derive(Helper, Hinter, Validator)]
pub struct Line {
    names: Rc<RefCell<Names>>,
    color: bool,
    files: FilenameCompleter,
}

impl Line {
    pub fn new(names: Rc<RefCell<Names>>, color: bool) -> Self {
        Line {
            names,
            color,
            files: FilenameCompleter::new(),
        }
    }
}

impl Completer for Line {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        match offer(&line[..pos], &self.names.borrow()) {
            Offer::Files => self.files.complete_path(line, pos),
            Offer::Words(start, words) => Ok((
                start,
                words
                    .into_iter()
                    .map(|word| Pair {
                        display: word.trim_end().to_string(),
                        replacement: word,
                    })
                    .collect(),
            )),
        }
    }
}

impl Highlighter for Line {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        match self.color {
            true => Cow::Owned(highlight_line(line)),
            false => Cow::Borrowed(line),
        }
    }

    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> Cow<'b, str> {
        match (self.color, prompt.strip_suffix("> ")) {
            (true, Some(name)) => Cow::Owned(format!("{}{}", ansi("1;34", name), ansi("2", "> "))),
            _ => Cow::Borrowed(prompt),
        }
    }

    fn highlight_char(&self, _line: &str, _pos: usize, _kind: CmdKind) -> bool {
        self.color
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names {
        Names {
            tenants: vec!["acme".into(), "acme-2".into(), "beta".into()],
            ids: vec!["e1".into(), "e12".into(), "s1".into()],
        }
    }

    fn words(before: &str) -> Vec<String> {
        match offer(before, &names()) {
            Offer::Words(start, words) => {
                assert!(before[start..].chars().all(|c| !c.is_whitespace()));
                words
            }
            Offer::Files => panic!("{before:?} offers files"),
        }
    }

    #[test]
    fn completes_commands_and_subcommands() {
        assert_eq!(words("\\co"), ["\\commit "]);
        assert_eq!(words("").len(), COMMANDS.len());
        assert_eq!(
            offer("\\t c", &names()),
            Offer::Words(3, vec!["create ".into(), "checkout ".into()])
        );
        assert_eq!(words("\\u g"), ["grant "]);
        assert_eq!(words("\\import "), ["fact ", "blob "]);
        assert_eq!(words("\\re"), ["\\retract ", "\\restore "]);
        assert_eq!(words("\\p"), ["\\partly ", "\\principals "]);
        assert_eq!(words("\\t a"), ["asklog "]);
        assert!(words("\\q ").is_empty());
    }

    #[test]
    fn completes_tenant_names_where_a_tenant_belongs() {
        assert_eq!(words("\\t checkout ac"), ["acme", "acme-2"]);
        assert_eq!(words("\\t meta b"), ["beta"]);
        assert_eq!(words("\\t delete "), ["acme", "acme-2", "beta"]);
        assert_eq!(words("\\u grant tomas a"), ["acme", "acme-2"]);
        assert!(words("\\u grant a").is_empty(), "a user name comes first");
        assert!(
            words("\\t create a").is_empty(),
            "a new tenant has a new name"
        );
        assert!(words("\\t users ").is_empty());
    }

    #[test]
    fn completes_ids_after_show() {
        assert_eq!(words("\\s e1"), ["e1", "e12"]);
        assert_eq!(words("\\s s"), ["s1"]);
        assert!(words("\\s e1 ").is_empty());
    }

    #[test]
    fn completes_file_names_after_import_and_export() {
        // Tests run in the crate directory, which has a Cargo.toml.
        let line = Line::new(Rc::new(RefCell::new(names())), false);
        let history = rustyline::history::DefaultHistory::new();
        for typed in [
            "\\t export Cargo.to",
            "\\t import Cargo.to",
            "\\import blob Cargo.to",
        ] {
            let (start, found) = line
                .complete(typed, typed.len(), &Context::new(&history))
                .unwrap();
            assert_eq!(start, typed.len() - "Cargo.to".len());
            let found: Vec<&str> = found.iter().map(|p| p.replacement.as_str()).collect();
            assert_eq!(found, ["Cargo.toml"]);
        }
        assert_eq!(
            offer("\\import fact Cargo", &names()),
            Offer::Words(13, Vec::new())
        );
    }

    #[test]
    fn completes_the_flags_on_a_question() {
        assert_eq!(words("\\ask --s"), ["--staged "]);
        assert_eq!(words("\\ask --facts --"), ["--staged ", "--facts "]);
        assert!(words("\\ask who is -").is_empty());
        assert!(words("\\ask who").is_empty());
    }

    #[test]
    fn parses_commands_and_trimmed_text() {
        assert!(matches!(
            parse("  \\import fact  \"Priya went to Acme.\"  "),
            Some(Command::Import("fact", "Priya went to Acme."))
        ));
        let asked = |line| match parse(line) {
            Some(Command::Ask(q)) => Some(q),
            _ => None,
        };
        assert_eq!(
            asked("\\ask who is Priya"),
            Some(Question {
                text: "who is Priya",
                staged: false,
                facts: false
            })
        );
        assert_eq!(
            asked("\\ask --facts --staged  \"who is CEO\""),
            Some(Question {
                text: "who is CEO",
                staged: true,
                facts: true
            })
        );
        assert_eq!(
            asked("\\ask --factsy"),
            Some(Question {
                text: "--factsy",
                staged: false,
                facts: false
            })
        );
        assert!(matches!(parse("\\commit"), Some(Command::Commit)));
        assert!(matches!(parse("\\rollback"), Some(Command::Rollback)));
        assert!(matches!(
            parse("\\t export my file.ttl"),
            Some(Command::Tenant("export", "my file.ttl"))
        ));
        assert!(matches!(parse("\\t get"), Some(Command::Tenant("get", ""))));
        assert!(matches!(
            parse("\\u grant  priya_raman acme "),
            Some(Command::User("grant", "priya_raman acme"))
        ));
        assert!(matches!(parse("\\s e1"), Some(Command::Show("e1"))));
        assert!(matches!(parse("\\q"), Some(Command::Quit)));
        assert!(matches!(
            parse("\\tacme"),
            Some(Command::Unknown("\\tacme"))
        ));
        assert!(matches!(parse("hello"), Some(Command::Unknown("hello"))));
        assert!(parse("   ").is_none());
        assert_eq!(unquote("'x'"), "x");
        assert_eq!(unquote("Priya's company"), "Priya's company");
    }

    #[test]
    fn parses_feedback_as_its_verdict_and_note() {
        let feedback = |line| match parse(line) {
            Some(Command::Feedback(verdict, note)) => Some((verdict, note)),
            _ => None,
        };
        assert_eq!(feedback("\\good"), Some(("right", "")));
        assert_eq!(
            feedback("  \\partly  \"the date is wrong\" "),
            Some(("partly", "the date is wrong"))
        );
        assert_eq!(
            feedback("\\bad it names Tomas, not Maya"),
            Some(("wrong", "it names Tomas, not Maya"))
        );
        assert!(feedback("\\goods").is_none());
        assert!(matches!(
            parse("\\t asklog 30"),
            Some(Command::Tenant("asklog", "30"))
        ));
    }

    #[test]
    fn questions_feedback_and_other_text_are_not_kept_on_disk() {
        for line in [
            "\\ask who founded Lumenworks",
            "  \\ask --staged where is Acme",
            "\\good",
            "\\partly it left out Priya",
            "\\bad wrong city",
            "who founded Lumenworks",
            "acme",
            " ",
        ] {
            assert!(!kept_on_disk(line), "{line:?}");
        }
        for line in ["\\t create acme", "\\s e1", "\\import blob mail.eml", "\\q"] {
            assert!(kept_on_disk(line), "{line:?}");
        }
    }

    #[test]
    fn parses_documents_pins_and_principals() {
        let documents = |line| match parse(line) {
            Some(Command::Documents(verb, documents)) => Some((verb, documents)),
            _ => None,
        };
        assert_eq!(
            documents("\\retract d3 d12 \"an old copy\""),
            Some((
                "retract",
                Documents {
                    ids: vec!["d3", "d12"],
                    reason: "an old copy"
                }
            ))
        );
        assert_eq!(
            documents("\\erase d3"),
            Some((
                "erase",
                Documents {
                    ids: vec!["d3"],
                    reason: ""
                }
            ))
        );
        assert_eq!(
            documents("\\restore duplicate d3"),
            Some((
                "restore",
                Documents {
                    ids: Vec::new(),
                    reason: "duplicate d3"
                }
            )),
            "the documents come first"
        );
        let migration = |line| match parse(line) {
            Some(Command::Migrate(migration)) => Some(migration),
            _ => None,
        };
        assert_eq!(
            migration("\\migrate ticket:13 registry-system:2 rules: new"),
            Some(Migration {
                pins: vec![("ticket", 13), ("registry-system", 2)],
                reason: "rules: new"
            })
        );
        assert_eq!(
            migration("\\migrate ticket :4 x:y"),
            Some(Migration {
                pins: Vec::new(),
                reason: "ticket :4 x:y"
            })
        );
        assert!(matches!(
            parse("\\acl d2  group:hr user:maya "),
            Some(Command::Acl("d2", principals)) if principals == ["group:hr", "user:maya"]
        ));
        assert!(matches!(
            parse("\\principals tomas"),
            Some(Command::Principals("tomas", groups)) if groups.is_empty()
        ));
    }

    #[test]
    fn a_typed_command_gets_its_own_colors_and_keeps_its_width() {
        let strip = |s: &str| {
            let mut out = String::new();
            let mut in_code = false;
            for c in s.chars() {
                match (in_code, c) {
                    (false, '\x1b') => in_code = true,
                    (true, 'm') => in_code = false,
                    (true, _) => {}
                    (false, c) => out.push(c),
                }
            }
            out
        };
        let ask = highlight_line("\\ask where does Priya live");
        assert_eq!(
            ask,
            format!(
                "{} {}",
                ansi(COMMAND, "\\ask"),
                ansi(TEXT, "where does Priya live")
            )
        );
        let tenant = highlight_line("  \\t  create   acme ");
        assert_eq!(
            tenant,
            format!(
                "  {}  {}   {}",
                ansi(COMMAND, "\\t"),
                ansi(SUBCOMMAND, "create"),
                ansi(TEXT, "acme ")
            )
        );
        assert_eq!(
            highlight_line("\\u grant tomas acme"),
            format!(
                "{} {} {}",
                ansi(COMMAND, "\\u"),
                ansi(SUBCOMMAND, "grant"),
                ansi(TEXT, "tomas acme")
            )
        );
        assert_eq!(
            highlight_line("\\retract d3 stale"),
            format!("{} {}", ansi(COMMAND, "\\retract"), ansi(TEXT, "d3 stale"))
        );
        assert_eq!(
            highlight_line("\\bad wrong year"),
            format!("{} {}", ansi(COMMAND, "\\bad"), ansi(TEXT, "wrong year"))
        );
        assert_eq!(highlight_line("\\as"), ansi(TYPING, "\\as"));
        assert_eq!(
            highlight_line("\\x hi"),
            format!("{} hi", ansi(UNKNOWN, "\\x"))
        );
        assert_eq!(highlight_line("plain words"), "plain words");
        for line in [
            "\\ask where does Priya live",
            "  \\t  create   acme ",
            "\\x hi",
            "\\s e1",
        ] {
            assert_eq!(strip(&highlight_line(line)), line);
        }
    }
}
