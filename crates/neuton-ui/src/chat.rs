//! The chat log and its input line.

use neuton_net::Span;
use std::time::{Duration, Instant};

/// How long a message stays on screen when chat is closed, as in the game.
const FADE_AFTER: Duration = Duration::from_secs(10);
/// Lines kept in the scrollback.
const HISTORY: usize = 100;

pub struct Chat {
    lines: Vec<(Instant, Vec<Span>)>,
    /// The line being typed, if any. `None` means chat is closed.
    input: Option<String>,
    /// Previously sent messages, newest last, for the up arrow.
    sent: Vec<String>,
    /// Position in `sent` while browsing it.
    browsing: Option<usize>,
}

impl Default for Chat {
    fn default() -> Self {
        Self { lines: Vec::new(), input: None, sent: Vec::new(), browsing: None }
    }
}

impl Chat {
    pub fn push(&mut self, spans: Vec<Span>) {
        self.lines.push((Instant::now(), spans));
        if self.lines.len() > HISTORY {
            self.lines.drain(..self.lines.len() - HISTORY);
        }
    }

    /// Adds a line of the client's own text, for errors and notices.
    pub fn note(&mut self, text: impl Into<String>) {
        self.push(vec![Span {
            text: text.into(),
            color: Some([0xAA, 0xAA, 0xAA]),
            bold: false,
            italic: true,
            underlined: false,
            strikethrough: false,
            obfuscated: false,
        }]);
    }

    pub fn is_open(&self) -> bool {
        self.input.is_some()
    }

    pub fn input(&self) -> Option<&str> {
        self.input.as_deref()
    }

    pub fn input_mut(&mut self) -> Option<&mut String> {
        self.input.as_mut()
    }

    /// Opens the input line, optionally pre-filled with a slash.
    pub fn open(&mut self, prefill: &str) {
        self.input = Some(prefill.to_string());
        self.browsing = None;
    }

    pub fn close(&mut self) {
        self.input = None;
        self.browsing = None;
    }

    /// Takes the typed line, closing the input.
    ///
    /// Returns `None` for an empty line, so pressing enter on nothing just
    /// closes chat rather than sending a blank message the server will reject.
    pub fn submit(&mut self) -> Option<String> {
        let text = self.input.take()?;
        self.browsing = None;
        let text = text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        if self.sent.last() != Some(&text) {
            self.sent.push(text.clone());
            if self.sent.len() > 50 {
                self.sent.remove(0);
            }
        }
        Some(text)
    }

    /// Steps back through previously sent lines.
    pub fn history_back(&mut self) {
        if self.sent.is_empty() || self.input.is_none() {
            return;
        }
        let next = match self.browsing {
            None => self.sent.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.browsing = Some(next);
        self.input = Some(self.sent[next].clone());
    }

    /// Steps forward again, ending on an empty line.
    pub fn history_forward(&mut self) {
        let Some(i) = self.browsing else { return };
        if i + 1 >= self.sent.len() {
            self.browsing = None;
            self.input = Some(String::new());
        } else {
            self.browsing = Some(i + 1);
            self.input = Some(self.sent[i + 1].clone());
        }
    }

    /// Lines to draw: everything while open, only the recent ones while closed.
    pub fn visible(&self) -> impl Iterator<Item = &Vec<Span>> {
        let open = self.is_open();
        let now = Instant::now();
        self.lines
            .iter()
            .filter(move |(at, _)| open || now.duration_since(*at) < FADE_AFTER)
            .map(|(_, spans)| spans)
            .rev()
            .take(if open { 20 } else { 10 })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str) -> Vec<Span> {
        vec![Span {
            text: text.into(),
            color: None,
            bold: false,
            italic: false,
            underlined: false,
            strikethrough: false,
            obfuscated: false,
        }]
    }

    #[test]
    fn opening_and_submitting_round_trips() {
        let mut c = Chat::default();
        assert!(!c.is_open());
        c.open("");
        assert!(c.is_open());
        *c.input_mut().unwrap() = "hello".into();
        assert_eq!(c.submit().as_deref(), Some("hello"));
        assert!(!c.is_open(), "submitting closes the input");
    }

    #[test]
    fn an_empty_line_sends_nothing() {
        let mut c = Chat::default();
        c.open("");
        assert_eq!(c.submit(), None);
        c.open("   ");
        assert_eq!(c.submit(), None);
        assert!(!c.is_open());
    }

    #[test]
    fn a_slash_prefill_survives() {
        let mut c = Chat::default();
        c.open("/");
        assert_eq!(c.input(), Some("/"));
    }

    #[test]
    fn history_walks_back_and_forward() {
        let mut c = Chat::default();
        for text in ["one", "two", "three"] {
            c.open("");
            *c.input_mut().unwrap() = text.into();
            c.submit();
        }
        c.open("");
        c.history_back();
        assert_eq!(c.input(), Some("three"));
        c.history_back();
        assert_eq!(c.input(), Some("two"));
        c.history_forward();
        assert_eq!(c.input(), Some("three"));
        // Forward past the end returns to an empty line.
        c.history_forward();
        assert_eq!(c.input(), Some(""));
    }

    #[test]
    fn repeats_are_not_stored_twice() {
        let mut c = Chat::default();
        for _ in 0..3 {
            c.open("");
            *c.input_mut().unwrap() = "same".into();
            c.submit();
        }
        c.open("");
        c.history_back();
        assert_eq!(c.input(), Some("same"));
        c.history_back();
        assert_eq!(c.input(), Some("same"), "only one entry to walk back to");
    }

    #[test]
    fn closed_chat_shows_only_recent_lines() {
        let mut c = Chat::default();
        c.push(span("old"));
        // Backdate it past the fade.
        c.lines[0].0 = Instant::now() - FADE_AFTER - Duration::from_secs(1);
        c.push(span("new"));

        let visible: Vec<String> =
            c.visible().map(|s| s.iter().map(|x| x.text.clone()).collect()).collect();
        assert_eq!(visible, vec!["new".to_string()]);

        // Opening chat brings the whole scrollback back.
        c.open("");
        assert_eq!(c.visible().count(), 2);
    }

    #[test]
    fn scrollback_is_bounded() {
        let mut c = Chat::default();
        for i in 0..HISTORY + 50 {
            c.push(span(&i.to_string()));
        }
        assert_eq!(c.len(), HISTORY);
    }
}
