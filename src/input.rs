//! Commands as typed: parsing, and highlighting the line while it is being typed.

use std::borrow::Cow;

use rustyline::highlight::{CmdKind, Highlighter};
use rustyline::{Completer, Helper, Hinter, Validator};

use crate::paint::ansi;

pub enum Command<'a> {
    /// `\t <subcommand> [arg]`
    Tenant(&'a str, &'a str),
    Import(&'a str),
    Ask(&'a str),
    Show(&'a str),
    Help,
    Quit,
    Unknown(&'a str),
}

pub const COMMANDS: &[&str] = &["\\t", "\\import", "\\ask", "\\s", "\\h", "\\q"];
pub const TENANT_SUBCOMMANDS: &[&str] =
    &["create", "checkout", "get", "delete", "import", "export"];

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
        "\\import" => Command::Import(unquote(rest)),
        "\\ask" => Command::Ask(unquote(rest)),
        "\\s" => Command::Show(rest),
        "\\h" => Command::Help,
        "\\q" => Command::Quit,
        other => Command::Unknown(other),
    })
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
        "\\t" => {
            let word_end = body.find(char::is_whitespace).unwrap_or(body.len());
            let (word, tail) = body.split_at(word_end);
            let word_color = match TENANT_SUBCOMMANDS.contains(&word) {
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
        "\\import" | "\\ask" | "\\s" => out.push_str(&ansi(TEXT, body)),
        _ => out.push_str(body),
    }
    out
}

#[derive(Completer, Helper, Hinter, Validator)]
pub struct Colors;

impl Highlighter for Colors {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        Cow::Owned(highlight_line(line))
    }

    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> Cow<'b, str> {
        match prompt.strip_suffix("> ") {
            Some(name) => Cow::Owned(format!("{}{}", ansi("1;34", name), ansi("2", "> "))),
            None => Cow::Borrowed(prompt),
        }
    }

    fn highlight_char(&self, _line: &str, _pos: usize, _kind: CmdKind) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands_and_trimmed_text() {
        assert!(matches!(
            parse("  \\import  \"Priya went to Acme.\"  "),
            Some(Command::Import("Priya went to Acme."))
        ));
        assert!(matches!(
            parse("\\ask who is Priya"),
            Some(Command::Ask("who is Priya"))
        ));
        assert!(matches!(
            parse("\\t export my file.ttl"),
            Some(Command::Tenant("export", "my file.ttl"))
        ));
        assert!(matches!(parse("\\t get"), Some(Command::Tenant("get", ""))));
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
