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
