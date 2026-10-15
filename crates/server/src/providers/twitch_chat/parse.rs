//! Twitch IRC line parsing: `@<tags> :<prefix> <COMMAND> <params> :<trailing>`,
//! where the tag block and prefix are optional (`PING :tmi.twitch.tv`).

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrcMessage {
    /// Tag values are unescaped. Empty values are kept as empty strings.
    pub tags: HashMap<String, String>,
    pub prefix: Option<String>,
    pub command: String,
    /// Middle params, then the trailing param (if any) as the last element.
    pub params: Vec<String>,
}

impl IrcMessage {
    pub fn tag(&self, name: &str) -> Option<&str> {
        self.tags.get(name).map(String::as_str).filter(|value| !value.is_empty())
    }

    /// The last param: the chat text for PRIVMSG / USERNOTICE.
    pub fn trailing(&self) -> Option<&str> {
        self.params.last().map(String::as_str)
    }

    /// The nick from a `nick!user@host` prefix.
    pub fn nick(&self) -> Option<&str> {
        self.prefix.as_deref()?.split('!').next().filter(|nick| !nick.is_empty())
    }
}

pub fn parse_line(line: &str) -> Option<IrcMessage> {
    let mut rest = line.trim_end_matches(['\r', '\n']);
    let mut tags = HashMap::new();
    if let Some(after) = rest.strip_prefix('@') {
        let (raw, remainder) = after.split_once(' ')?;
        for pair in raw.split(';') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            tags.insert(key.to_string(), unescape(value));
        }
        rest = remainder.trim_start_matches(' ');
    }
    let mut prefix = None;
    if let Some(after) = rest.strip_prefix(':') {
        let (raw, remainder) = after.split_once(' ')?;
        prefix = Some(raw.to_string());
        rest = remainder.trim_start_matches(' ');
    }
    let (command, mut rest) = rest.split_once(' ').unwrap_or((rest, ""));
    if command.is_empty() {
        return None;
    }
    let mut params = Vec::new();
    loop {
        rest = rest.trim_start_matches(' ');
        if rest.is_empty() {
            break;
        }
        if let Some(trailing) = rest.strip_prefix(':') {
            params.push(trailing.to_string());
            break;
        }
        let (param, remainder) = rest.split_once(' ').unwrap_or((rest, ""));
        params.push(param.to_string());
        rest = remainder;
    }
    Some(IrcMessage { tags, prefix, command: command.to_string(), params })
}

/// IRCv3 tag value unescaping: `\s` space, `\:` `;`, `\\` `\`, `\r`, `\n`.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some(':') => out.push(';'),
            Some('\\') => out.push('\\'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {} // a lone trailing backslash is dropped
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_without_tags_or_prefix() {
        let message = parse_line("PING :tmi.twitch.tv").unwrap();
        assert_eq!(message.command, "PING");
        assert_eq!(message.trailing(), Some("tmi.twitch.tv"));
        assert!(message.tags.is_empty());
        assert_eq!(message.prefix, None);
    }

    #[test]
    fn privmsg_with_tags_prefix_and_trailing() {
        let message = parse_line(
            "@badges=;display-name=A\\sB;emotes= :ab!ab@ab.tmi.twitch.tv PRIVMSG #chan :hello :) there",
        )
        .unwrap();
        assert_eq!(message.command, "PRIVMSG");
        assert_eq!(message.params, ["#chan", "hello :) there"]);
        assert_eq!(message.tag("display-name"), Some("A B"));
        assert_eq!(message.tag("emotes"), None, "empty tags read as absent");
        assert_eq!(message.nick(), Some("ab"));
    }

    #[test]
    fn unescapes_tag_values() {
        assert_eq!(unescape(r"a\sb\:c\\d\x\"), "a b;c\\dx");
    }

    #[test]
    fn numeric_reply_with_middle_params() {
        let message = parse_line(":tmi.twitch.tv 001 justinfan1 :Welcome, GLHF!").unwrap();
        assert_eq!(message.command, "001");
        assert_eq!(message.params, ["justinfan1", "Welcome, GLHF!"]);
    }

    #[test]
    fn every_fixture_line_parses() {
        let dir = format!("{}/tests/fixtures/irc", env!("CARGO_MANIFEST_DIR"));
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            for line in std::fs::read_to_string(&path).unwrap().lines() {
                assert!(parse_line(line).is_some(), "{}: {line}", path.display());
            }
        }
    }
}
