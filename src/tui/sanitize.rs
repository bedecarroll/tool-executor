use ansi_to_tui::IntoText;
use ratatui::text::Text;
use std::iter::Peekable;
use std::str::Chars;

#[derive(Debug, Clone)]
pub(crate) struct SanitizedText {
    pub lines: Vec<String>,
    pub styled: Option<Text<'static>>,
}

pub(crate) fn sanitize_ansi(input: &str) -> SanitizedText {
    let mut sanitized = String::with_capacity(input.len());
    let mut iter = input.chars().peekable();

    while let Some(ch) = iter.next() {
        if ch == '\u{1b}' {
            match iter.peek().copied() {
                Some(']' | 'P' | '_' | '^' | 'X') => {
                    iter.next();
                    skip_control_string(&mut iter);
                }
                Some('\u{9c}') => {
                    iter.next();
                }
                _ => sanitized.push(ch),
            }
        } else if ch != '\u{9c}' {
            sanitized.push(ch);
        }
    }

    let lines = sanitized
        .lines()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let styled = if sanitized.is_empty() {
        None
    } else {
        sanitized.as_str().into_text().ok()
    };

    SanitizedText { lines, styled }
}

fn skip_control_string(iter: &mut Peekable<Chars<'_>>) {
    while let Some(ch) = iter.next() {
        match ch {
            '\x07' | '\u{9c}' => break,
            '\u{1b}' => {
                if matches!(iter.peek().copied(), Some('\\')) {
                    iter.next();
                    break;
                }
            }
            _ => {}
        }
    }
}
