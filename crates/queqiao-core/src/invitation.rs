//! Help invitations: what the person who wants help passes on (by chat or mail) and how the helper's Queqiao reads it
//! back: from the clipboard into the help form, or from a `queqiao://help/<help ID>/<code>` link (the installer makes
//! such links open the device list's help form). Only a link, or a help ID and a code each after its label (English
//! or Chinese), is read: other text on the clipboard never fills the form. Reading one only fills the form; connecting
//! is still the helper's click, and the person helped still decides in a prompt.

use crate::text::{in_all_languages, tr};

pub const SCHEME: &str = "queqiao";
const LINK_PREFIX: &str = "queqiao://help/";

/// The link for a help ID (9 digits) and code.
pub fn link(help_id: &str, code: &str) -> String {
    format!("{LINK_PREFIX}{help_id}/{code}")
}

/// The ID in groups of three, the code in two halves (easier to read out).
pub fn spaced_id(help_id: &str) -> String {
    let mut out = String::new();
    for (at, c) in help_id.chars().enumerate() {
        if at > 0 && at % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

pub fn halved_code(code: &str) -> String {
    if code.len() == 8 && code.is_ascii() {
        format!("{}-{}", &code[..4], &code[4..])
    } else {
        code.to_string()
    }
}

/// The invitation: a line saying what to do, the help ID and code, and the link.
pub fn text(help_id: &str, code: &str) -> String {
    format!(
        "{}\r\n{} {}\r\n{} {}\r\n{}",
        tr(
            "Please help me on my PC with Queqiao: in Queqiao choose Help someone... (or open the link)."
        ),
        tr("Help ID:"),
        spaced_id(help_id),
        tr("Code:"),
        halved_code(code),
        link(help_id, code)
    )
}

/// A help ID is 9 digits; a code 6 to 16 letters and digits (upper case).
fn checked(help_id: String, code: &str) -> Option<(String, String)> {
    let code: String = code
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (help_id.len() == 9
        && help_id.bytes().all(|b| b.is_ascii_digit())
        && (6..=16).contains(&code.len()))
    .then_some((help_id, code))
}

/// The help ID and code in `text`: a queqiao://help link, or both labels each followed by its value.
pub fn parse(text: &str) -> Option<(String, String)> {
    if let Some(at) = text.to_ascii_lowercase().find(LINK_PREFIX) {
        let rest = &text[at + LINK_PREFIX.len()..];
        let (help_id, rest) = rest.split_once('/')?;
        let code: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        return checked(help_id.to_string(), &code);
    }
    let help_id: String = after_label(text, "Help ID:")?
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ' ')
        .filter(char::is_ascii_digit)
        .collect();
    let code: String = after_label(text, "Code:")?
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    checked(help_id, &code)
}

/// What follows `label` (in any language Queqiao speaks) in `text`, spaces after it skipped.
fn after_label<'a>(text: &'a str, label: &'static str) -> Option<&'a str> {
    in_all_languages(label).into_iter().find_map(|label| {
        let at = text.find(label)?;
        Some(text[at + label.len()..].trim_start())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_text_read_back() {
        assert_eq!(
            link("123456789", "ABCDEFGH"),
            "queqiao://help/123456789/ABCDEFGH"
        );
        assert_eq!(
            parse("queqiao://help/123456789/ABCDEFGH"),
            Some(("123456789".into(), "ABCDEFGH".into()))
        );
        // As a browser hands it over: a slash at the end; or inside other text.
        assert_eq!(
            parse("open QUEQIAO://help/987654321/abcd-efgh/ please"),
            Some(("987654321".into(), "ABCDEFGH".into()))
        );
        let invitation = text("123456789", "ABCDEFGH");
        assert!(invitation.contains("123 456 789") && invitation.contains("ABCD-EFGH"));
        assert_eq!(
            parse(&invitation),
            Some(("123456789".into(), "ABCDEFGH".into()))
        );
        // Without the link line, the labels are enough (as someone might retype it).
        let typed = "Help ID: 123 456 789\nCode: ABCD-EFGH";
        assert_eq!(parse(typed), Some(("123456789".into(), "ABCDEFGH".into())));
        let chinese =
            "\u{5e2e}\u{52a9} ID\u{ff1a}123 456 789\n\u{9a8c}\u{8bc1}\u{7801}\u{ff1a}ABCD-EFGH";
        assert_eq!(
            parse(chinese),
            Some(("123456789".into(), "ABCDEFGH".into()))
        );
    }

    #[test]
    fn other_text_is_not_taken() {
        for text in [
            "",
            "123456789 ABCDEFGH",
            "call me at 123 456 789",
            "Help ID: 12345678\nCode: ABCDEFGH",
            "Help ID: 123456789\nCode: AB",
            "Help ID: 123456789",
            "queqiao://help/12345/ABCDEFGH",
            "queqiao://help/123456789",
            "queqiao://help/123456789/AB",
            "queqiao://help/12345678x/ABCDEFGH",
        ] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }
}
