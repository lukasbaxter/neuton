//! Minecraft text components, as NBT.
//!
//! Chat and disconnect reasons arrive as a component tree rather than a string:
//! a node carries styling, optional children that inherit it, and possibly a
//! translation key instead of literal text. Flattening it to styled runs is what
//! turns that into something drawable.

use crate::status::Span;
use neuton_nbt::Value;

/// Flattens a component into styled runs.
pub fn flatten(value: &Value<'_>) -> Vec<Span> {
    let mut out = Vec::new();
    walk(value, &base(), &mut out);
    out.retain(|s| !s.text.is_empty());
    out
}

/// The plain text of a component, styling discarded.
pub fn to_text(value: &Value<'_>) -> String {
    flatten(value).into_iter().map(|s| s.text).collect()
}

fn base() -> Span {
    Span {
        text: String::new(),
        color: None,
        bold: false,
        italic: false,
        underlined: false,
        strikethrough: false,
        obfuscated: false,
    }
}

fn walk(value: &Value<'_>, inherited: &Span, out: &mut Vec<Span>) {
    match value {
        // A bare string is a literal, and may still carry legacy codes.
        Value::String(s) => crate::status::push_legacy_public(s, inherited, out),
        Value::List(items) => {
            for item in items {
                walk(item, inherited, out);
            }
        }
        Value::Compound(_) => {
            let mut style = inherited.clone();
            style.text.clear();
            if let Some(c) = value.get("color").and_then(|c| c.as_str())
                && let Some(rgb) = crate::status::color_of_public(c)
            {
                style.color = Some(rgb);
            }
            for (key, flag) in [
                ("bold", 0usize),
                ("italic", 1),
                ("underlined", 2),
                ("strikethrough", 3),
                ("obfuscated", 4),
            ] {
                if let Some(b) = value.get(key).and_then(|v| v.as_bool()) {
                    match flag {
                        0 => style.bold = b,
                        1 => style.italic = b,
                        2 => style.underlined = b,
                        3 => style.strikethrough = b,
                        _ => style.obfuscated = b,
                    }
                }
            }

            if let Some(text) = value.get("text").and_then(|t| t.as_str()) {
                crate::status::push_legacy_public(text, &style, out);
            }

            // A translated component names a key the client is meant to look up
            // in its language file. Until those are loaded, the arguments carry
            // most of the meaning: "%s joined the game" is mostly the name.
            if let Some(key) = value.get("translate").and_then(|t| t.as_str()) {
                let args: Vec<Vec<Span>> = value
                    .get("with")
                    .and_then(|w| w.as_list())
                    .map(|items| items.iter().map(flatten).collect())
                    .unwrap_or_default();
                expand_translation(key, &args, &style, out);
            }

            if let Some(extra) = value.get("extra") {
                walk(extra, &style, out);
            }
        }
        _ => {}
    }
}

/// Renders a translated component without a language file.
///
/// The handful of keys that carry real text in chat are spelled out; anything
/// else falls back to its arguments joined together, which is almost always the
/// part a player cares about.
fn expand_translation(key: &str, args: &[Vec<Span>], style: &Span, out: &mut Vec<Span>) {
    let literal = |text: &str, out: &mut Vec<Span>| {
        let mut span = style.clone();
        span.text = text.to_string();
        out.push(span);
    };
    let arg = |i: usize, out: &mut Vec<Span>| {
        if let Some(spans) = args.get(i) {
            out.extend(spans.iter().cloned());
        }
    };

    match key {
        "chat.type.text" => {
            literal("<", out);
            arg(0, out);
            literal("> ", out);
            arg(1, out);
        }
        "chat.type.announcement" => {
            literal("[", out);
            arg(0, out);
            literal("] ", out);
            arg(1, out);
        }
        "chat.type.emote" => {
            literal("* ", out);
            arg(0, out);
            literal(" ", out);
            arg(1, out);
        }
        "multiplayer.player.joined" => {
            arg(0, out);
            literal(" joined the game", out);
        }
        "multiplayer.player.left" => {
            arg(0, out);
            literal(" left the game", out);
        }
        "commands.help.footer" | "chat.disabled.missingProfileKey" => literal(key, out),
        _ => {
            // Unknown key: show the arguments, and the key itself if there are
            // none, so nothing silently disappears.
            if args.is_empty() {
                literal(key, out);
            } else {
                for (i, _) in args.iter().enumerate() {
                    if i > 0 {
                        literal(" ", out);
                    }
                    arg(i, out);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds network NBT for a compound of string fields.
    fn compound(fields: &[(&str, &str)]) -> Vec<u8> {
        let mut v = vec![0x0A];
        for (name, value) in fields {
            v.push(0x08); // TAG_String
            v.extend_from_slice(&(name.len() as u16).to_be_bytes());
            v.extend_from_slice(name.as_bytes());
            v.extend_from_slice(&(value.len() as u16).to_be_bytes());
            v.extend_from_slice(value.as_bytes());
        }
        v.push(0x00);
        v
    }

    #[test]
    fn a_plain_text_component_flattens_to_its_text() {
        let bytes = compound(&[("text", "hello")]);
        let value = Value::parse(&bytes).unwrap();
        assert_eq!(to_text(&value), "hello");
    }

    #[test]
    fn colour_and_style_survive() {
        let bytes = compound(&[("text", "warn"), ("color", "red")]);
        let value = Value::parse(&bytes).unwrap();
        let spans = flatten(&value);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].color, Some([0xFF, 0x55, 0x55]));
    }

    #[test]
    fn legacy_codes_inside_text_still_split() {
        let bytes = compound(&[("text", "\u{a7}cred\u{a7}rplain")]);
        let value = Value::parse(&bytes).unwrap();
        let spans = flatten(&value);
        assert_eq!(to_text(&value), "redplain");
        assert_eq!(spans[0].color, Some([0xFF, 0x55, 0x55]));
        assert_eq!(spans[1].color, None);
    }

    #[test]
    fn an_unknown_translation_key_shows_rather_than_vanishing() {
        let bytes = compound(&[("translate", "some.plugin.key")]);
        let value = Value::parse(&bytes).unwrap();
        assert_eq!(to_text(&value), "some.plugin.key");
    }

    #[test]
    fn a_chat_message_reads_as_a_chat_message() {
        // {"translate":"chat.type.text","with":[{"text":"Lukas"},{"text":"hi"}]}
        let mut v = vec![0x0A];
        // translate
        v.push(0x08);
        v.extend_from_slice(&(9u16).to_be_bytes());
        v.extend_from_slice(b"translate");
        v.extend_from_slice(&(14u16).to_be_bytes());
        v.extend_from_slice(b"chat.type.text");
        // with: list of two compounds
        v.push(0x09);
        v.extend_from_slice(&(4u16).to_be_bytes());
        v.extend_from_slice(b"with");
        v.push(0x0A);
        v.extend_from_slice(&2i32.to_be_bytes());
        for text in ["Lukas", "hi"] {
            v.push(0x08);
            v.extend_from_slice(&(4u16).to_be_bytes());
            v.extend_from_slice(b"text");
            v.extend_from_slice(&(text.len() as u16).to_be_bytes());
            v.extend_from_slice(text.as_bytes());
            v.push(0x00);
        }
        v.push(0x00);

        let value = Value::parse(&v).unwrap();
        assert_eq!(to_text(&value), "<Lukas> hi");
    }
}
