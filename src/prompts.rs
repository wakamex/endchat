//! The system prompt, shared by turns and compactions so that both read the same cache entry.

const SYSTEM: &str = include_str!("../prompts/system.md");

/// Remove `<!-- ... -->` comments, which may span lines, along with the line break after each.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let end = after.find("-->").map_or(after.len(), |e| e + 3);
        rest = after[end..].strip_prefix('\n').unwrap_or(&after[end..]);
    }
    out.push_str(rest);
    out
}

/// The system prompt followed by the user's own instructions, when there are any.
pub fn system(instructions: Option<&str>) -> String {
    let mut text = strip_comments(SYSTEM).trim_end().to_owned();
    if let Some(i) = instructions.map(str::trim).filter(|i| !i.is_empty()) {
        text.push_str("\n\n");
        text.push_str(i);
    }
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_are_removed_and_the_gist_text_remains() {
        let s = system(None);
        assert!(s.starts_with("You are Endchat, an AI agent"), "{}", &s[..60]);
        assert!(!s.contains("<!--") && !s.contains("-->"));
        assert!(!s.to_lowercase().contains("unii"));
        assert!(!s.contains("computers"));
        for needle in ["- agent: Endchat's replies", "zoom(id, n) opens line id+n", "Goal: let Endchat work later as well as if it remembered everything.", "Non-ASCII characters cost 2-4 bytes.", "write the call alone on a line"] {
            assert!(s.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn user_instructions_follow_the_prompt() {
        let s = system(Some("  I am Sam. Keep replies short.\n"));
        assert!(s.ends_with("\n\nI am Sam. Keep replies short.\n"));
        assert_eq!(system(Some("  \n")), system(None));
    }

    #[test]
    fn unterminated_comment_drops_the_rest() {
        assert_eq!(strip_comments("a\n<!-- x\ny"), "a\n");
        assert_eq!(strip_comments("a<!-- x -->\nb"), "ab");
    }
}
