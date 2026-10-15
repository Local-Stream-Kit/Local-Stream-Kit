//! Links in what viewers write (a sub's message, what an alert said),
//! found the same way everywhere: speech leaves them out, and the panel's
//! History makes them links that open in the PC's browser.

use std::ops::Range;

/// Top-level domains a link without "https://" or "www." may end in.
const ENDINGS: &[&str] = &[
    "com", "net", "org", "tv", "gg", "io", "be", "ly", "co", "me", "app", "dev", "link", "uk", "de", "se",
    "no", "dk", "fi", "fr", "nl", "eu", "us", "ca", "au", "ru", "jp", "xyz", "info",
];

/// A link in a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// Where it is in the text, in bytes: the word without the punctuation
    /// around it.
    pub range: Range<usize>,
    /// The address it opens, always `http://…` or `https://…`: the word,
    /// with `https://` in front when it has none.
    pub url: String,
}

/// The links in `text`, in order.
pub fn find(text: &str) -> Vec<Link> {
    text.split_whitespace()
        .filter_map(|word| {
            let at = word.as_ptr() as usize - text.as_ptr() as usize;
            let (from, core) = core(word);
            url(core).map(|url| Link { range: at + from..at + from + core.len(), url })
        })
        .collect()
}

/// Whether the word is a link: "https://…", "www.…", or a domain such as
/// "twitch.tv/name" or "example.com" (a known ending, or any with a path).
/// Punctuation around it doesn't count.
pub fn is_link(word: &str) -> bool {
    url(core(word).1).is_some()
}

/// The word without the punctuation around it ("(example.com)." →
/// "example.com"), and where that starts in it. A closing bracket that
/// closes one in the link stays ("…/wiki/Rust_(language)").
fn core(word: &str) -> (usize, &str) {
    let trimmed = word.trim_start_matches(|c: char| "\"'(<".contains(c));
    let from = word.len() - trimmed.len();
    let mut core = trimmed;
    while let Some(last) = core.chars().next_back() {
        let unopened = last == ')' && core.matches(')').count() > core.matches('(').count();
        if !(".,!?;:\"'>".contains(last) || unopened) {
            break;
        }
        core = &core[..core.len() - last.len_utf8()];
    }
    (from, core)
}

/// The address the word opens, if it's a link.
fn url(word: &str) -> Option<String> {
    let lower = word.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("http://").or_else(|| lower.strip_prefix("https://")) {
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        return (!host.is_empty()).then(|| word.to_string());
    }
    if lower.strip_prefix("www.").is_some_and(|rest| !rest.is_empty()) {
        return Some(format!("https://{word}"));
    }
    let (host, path) = match lower.split_once('/') {
        Some((host, path)) => (host, Some(path)),
        None => (lower.as_str(), None),
    };
    let labels: Vec<&str> = host.split('.').collect();
    let [.., name, ending] = labels.as_slice() else { return None };
    let plain =
        |label: &str| !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let domain = labels.iter().all(|label| plain(label))
        && name.chars().any(|c| c.is_ascii_alphabetic())
        && ending.chars().all(|c| c.is_ascii_alphabetic())
        && (ENDINGS.contains(ending) || (path.is_some() && (2..=6).contains(&ending.len())));
    domain.then(|| format!("https://{word}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str) -> Vec<(&str, String)> {
        find(text).into_iter().map(|link| (&text[link.range], link.url)).collect()
    }

    #[test]
    fn links_and_what_they_open() {
        assert_eq!(
            found("check https://example.com/a?b=1 out!"),
            [("https://example.com/a?b=1", "https://example.com/a?b=1".to_string())]
        );
        assert_eq!(
            found("follow twitch.tv/marrtin and www.Example.org."),
            [
                ("twitch.tv/marrtin", "https://twitch.tv/marrtin".to_string()),
                ("www.Example.org", "https://www.Example.org".to_string()),
            ]
        );
        assert_eq!(
            found("my site (example.com) is up"),
            [("example.com", "https://example.com".to_string())]
        );
        // The path keeps its case.
        assert_eq!(found("youtu.be/AbC123"), [("youtu.be/AbC123", "https://youtu.be/AbC123".to_string())]);
        assert_eq!(found("HTTP://Example.com"), [("HTTP://Example.com", "HTTP://Example.com".to_string())]);
    }

    #[test]
    fn brackets_in_a_link_stay() {
        let wiki = "https://en.wikipedia.org/wiki/Rust_(language)";
        assert_eq!(found(&format!("read {wiki}.")), [(wiki, wiki.to_string())]);
        assert_eq!(found(&format!("(see {wiki})")), [(wiki, wiki.to_string())]);
        assert_eq!(
            found("<https://example.com>"),
            [("https://example.com", "https://example.com".to_string())]
        );
    }

    #[test]
    fn where_they_are() {
        let text = "héllo  \"https://a.io/x\", then b.gg";
        let links = find(text);
        assert_eq!(links.len(), 2);
        assert_eq!(&text[links[0].range.clone()], "https://a.io/x");
        assert_eq!(&text[links[1].range.clone()], "b.gg");
    }

    #[test]
    fn not_links() {
        for text in ["e.g. 1.5 times, ok.then", "https:// http://", "www.", "a.b/c", "1.2/3", "hi... there"] {
            assert_eq!(found(text), [], "{text}");
        }
        assert!(!is_link("javascript:alert(1)"));
        assert!(!is_link("file://C:/x.exe"));
    }
}
