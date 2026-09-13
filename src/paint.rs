//! Colors: plain ANSI codes, on only when stdout is a terminal and `NO_COLOR` is unset.

use std::sync::atomic::{AtomicBool, Ordering};

static ON: AtomicBool = AtomicBool::new(false);

pub fn enable(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

pub fn is_on() -> bool {
    ON.load(Ordering::Relaxed)
}

pub fn wrap(code: &str, text: &str) -> String {
    match is_on() {
        true => format!("\x1b[{code}m{text}\x1b[0m"),
        false => text.to_string(),
    }
}

/// Always colored, for the input line (rustyline only calls this on a terminal).
pub fn ansi(code: &str, text: &str) -> String {
    format!("\x1b[{code}m{text}\x1b[0m")
}

pub fn bold(text: &str) -> String {
    wrap("1", text)
}
pub fn dim(text: &str) -> String {
    wrap("2", text)
}
pub fn red(text: &str) -> String {
    wrap("31", text)
}
pub fn green(text: &str) -> String {
    wrap("32", text)
}
pub fn yellow(text: &str) -> String {
    wrap("33", text)
}
pub fn blue(text: &str) -> String {
    wrap("34", text)
}
pub fn magenta(text: &str) -> String {
    wrap("35", text)
}
pub fn cyan(text: &str) -> String {
    wrap("36", text)
}
pub fn bold_green(text: &str) -> String {
    wrap("1;32", text)
}
pub fn bold_blue(text: &str) -> String {
    wrap("1;34", text)
}

/// Green at 75% and above, yellow from 40%, red below.
pub fn band(confidence: f32) -> &'static str {
    match confidence {
        c if c >= 0.75 => "32",
        c if c >= 0.40 => "33",
        _ => "31",
    }
}

/// "85%" in its band's color.
pub fn percent(confidence: f32) -> String {
    let text = format!("{}%", (confidence * 100.0).round() as i32);
    wrap(band(confidence), &text)
}

/// Colors every `NN%` inside a sentence the engine wrote, by its band.
pub fn percents_in(text: &str) -> String {
    if !is_on() {
        return text.to_string();
    }
    let mut out = String::new();
    let mut digits = String::new();
    for c in text.chars() {
        match c {
            '0'..='9' => digits.push(c),
            '%' if !digits.is_empty() => {
                let value: f32 = digits.parse().unwrap_or(0.0);
                out.push_str(&wrap(band(value / 100.0), &format!("{digits}%")));
                digits.clear();
            }
            _ => {
                out.push_str(&digits);
                digits.clear();
                out.push(c);
            }
        }
    }
    out.push_str(&digits);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_and_plain_output_when_off() {
        assert_eq!(band(0.9), "32");
        assert_eq!(band(0.75), "32");
        assert_eq!(band(0.5), "33");
        assert_eq!(band(0.1), "31");
        assert_eq!(ansi("1", "x"), "\x1b[1mx\x1b[0m");
    }

    #[test]
    fn percents_in_a_sentence_get_their_band() {
        enable(true);
        let colored = percents_in("90% (model 30%, subject 50%) in 2026");
        enable(false);
        assert_eq!(
            colored,
            "\x1b[32m90%\x1b[0m (model \x1b[31m30%\x1b[0m, subject \x1b[33m50%\x1b[0m) in 2026"
        );
        assert_eq!(percents_in("90% plain"), "90% plain");
    }
}
