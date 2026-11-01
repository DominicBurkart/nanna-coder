use crate::pr_tools::{fenced_untrusted, Budget};

const FIELD_CAP: usize = 200;

pub(crate) fn untrusted_inline(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                '\u{23ce}'
            } else {
                c
            }
        })
        .collect();
    let fenced = fenced_untrusted(&flat, FIELD_CAP, &mut Budget::new(FIELD_CAP)).replace('\n', " ");
    let fenced = unlink(&fenced);
    let mut out = String::with_capacity(fenced.len());
    let mut previous = '\0';
    for c in fenced.chars() {
        match c {
            '@' => out.push_str("&#64;"),
            '`' => out.push_str("&#96;"),
            '#' if previous != '&' => out.push_str("&#35;"),
            c => out.push(c),
        }
        previous = c;
    }
    out
}

fn unlink(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let lower = text.to_ascii_lowercase();
    let mut rest = 0;
    while rest < text.len() {
        let tail = &lower[rest..];
        if tail.starts_with("://") {
            out.push_str(":&#47;&#47;");
            rest += 3;
        } else if tail.starts_with("www.") {
            out.push_str(&text[rest..rest + 3]);
            out.push_str("&#46;");
            rest += 4;
        } else {
            let ch = text[rest..].chars().next().expect("rest is within text");
            out.push(ch);
            rest += ch.len_utf8();
        }
    }
    out
}

const EVIDENCE_CAP: usize = 600;

pub(crate) fn evidence_line(text: &str) -> String {
    let flat: String = text
        .chars()
        .take(EVIDENCE_CAP)
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2069}'
            | '\u{feff}'
            | '\u{ad}'
            | '\u{61c}'
            | '\u{180e}'
            | '\u{e0000}'..='\u{e007f}' => None,
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    let linked = unlink(&flat).replace("//", "&#47;&#47;");
    let mut out = String::with_capacity(linked.len());
    let mut previous = '\0';
    for c in linked.chars() {
        match c {
            '@' => out.push_str("&#64;"),
            '<' => out.push_str("&lt;"),
            '[' => out.push_str("&#91;"),
            ']' => out.push_str("&#93;"),
            '!' => out.push_str("&#33;"),
            '#' if previous != '&' => out.push_str("&#35;"),
            c => out.push(c),
        }
        previous = c;
    }
    out
}

const REF_CAP: usize = 160;

pub(crate) fn safe_ref(text: &str) -> String {
    let filtered: String = text
        .chars()
        .take(REF_CAP)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/') {
                c
            } else {
                '_'
            }
        })
        .collect();
    unlink(&filtered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_references_pass_through_unchanged() {
        for ok in ["owner/repo", "a-b/c.d_e", "/home/dev/work/nanna-coder"] {
            assert_eq!(safe_ref(ok), ok);
        }
    }

    #[test]
    fn anything_that_could_format_or_link_becomes_an_underscore() {
        assert_eq!(safe_ref("a b\n#c@d`e:f"), "a_b__c_d_e_f");
        assert_eq!(safe_ref("www.evil.example"), "www&#46;evil.example");
        assert_eq!(safe_ref("WWW.evil.example"), "WWW&#46;evil.example");
    }

    #[test]
    fn evidence_lines_are_flat_unlinked_and_mention_free_but_keep_ordinary_text() {
        let shown = evidence_line(
            "tool `x` refused\n## Suggested action\r@octocat Fixes #1 http://e www.e\u{202e}\u{1b}",
        );
        assert_eq!(
            shown,
            "tool `x` refused &#35;&#35; Suggested action &#64;octocat Fixes &#35;1 http:&#47;&#47;e www&#46;e"
        );
        assert_eq!(evidence_line("a: b (c) 1.5"), "a: b (c) 1.5");
        assert_eq!(
            evidence_line(&evidence_line("@x #1")),
            evidence_line("@x #1")
        );
        assert_eq!(evidence_line(&"a".repeat(2000)).len(), 600);
    }

    #[test]
    fn evidence_lines_cannot_carry_html_images_links_or_invisible_characters() {
        let payloads = [
            "<img src=//evil/x.png>",
            "<a href=//evil>click</a>",
            "[click](//evil.com)",
            "![x](//evil/p.png)",
            "see //evil.example/track.gif now",
            "ab\u{feff}cd\u{2060}ef\u{ad}gh\u{61c}ij\u{e0041}\u{e007f}kl\u{180e}mn\u{2064}op",
        ];
        for payload in payloads {
            let shown = evidence_line(payload);
            for needle in ["<", "[", "]", "!", "//"] {
                assert!(!shown.contains(needle), "{needle}: {payload} -> {shown}");
            }
            for c in shown.chars() {
                assert!(
                    !matches!(c, '\u{feff}' | '\u{2060}'..='\u{2064}' | '\u{ad}' | '\u{61c}' | '\u{180e}' | '\u{e0000}'..='\u{e007f}'),
                    "{payload} -> {shown:?}"
                );
            }
            assert_eq!(evidence_line(&shown), shown, "{payload}");
        }
        assert_eq!(evidence_line("abcdefghijklmnopq"), "abcdefghijklmnopq");
        assert_eq!(
            evidence_line("ab\u{feff}cd\u{2060}ef\u{ad}gh\u{61c}ij\u{e0041}kl\u{180e}mn\u{2064}op"),
            "abcdefghijklmnop"
        );
        assert_eq!(evidence_line("<b>"), "&lt;b>");
    }

    #[test]
    fn a_reference_is_capped() {
        assert_eq!(safe_ref(&"a".repeat(1000)).len(), 160);
        assert_eq!(safe_ref(&"é".repeat(1000)).chars().count(), 160);
    }

    #[test]
    fn inline_text_is_flattened_fenced_and_neutralised() {
        let shown = untrusted_inline("a\nb @x #1 `c` http://e");
        assert!(!shown.contains('\n') && !shown.contains('@') && !shown.contains('`'));
        assert!(!shown.contains("://"));
    }
}
