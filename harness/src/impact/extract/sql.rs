use crate::assets::AssetGraph;
use crate::impact::{Access, Change, ChangedFile, Evidence, ExtractContext, Extractor};
use regex::Regex;
use std::sync::OnceLock;

const IDENT: &str = r#"(?:"[^"]+"|[A-Za-z_][\w$]*)(?:\s*\.\s*(?:"[^"]+"|[A-Za-z_][\w$]*))?"#;

const NOT_TABLES: [&str; 9] = [
    "set", "select", "values", "lateral", "only", "as", "where", "using", "unnest",
];

fn statement_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"(?i)\b(INSERT\s+INTO|UPDATE|DELETE\s+FROM|ALTER\s+TABLE|DROP\s+TABLE|TRUNCATE(?:\s+TABLE)?|CREATE\s+(?:UNLOGGED\s+|TEMP(?:ORARY)?\s+)?TABLE|FROM|JOIN)\s+(?:(?:IF\s+(?:NOT\s+)?EXISTS|ONLY)\s+)*({IDENT})"
        ))
        .expect("static regex")
    })
}

fn more_names_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"^\s*,\s*({IDENT})")).expect("static regex"))
}

fn noise_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"'(?:[^']|'')*'|--[^\n]*|/\*(?s:.*?)\*/").expect("static regex"))
}

fn macro_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\bquery(?:_as|_scalar)?(?:_unchecked)?!\s*\(").expect("static regex")
    })
}

/// How a statement reaches a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlAccess {
    pub table: String,
    pub access: Access,
    pub verb: String,
}

/// Table accesses in SQL text: writes for `INSERT`, `UPDATE`, `DELETE`,
/// `ALTER`, `DROP`, `TRUNCATE` and `CREATE TABLE`; reads for `FROM` and `JOIN`,
/// including inside subselects. Comments and string literals are ignored.
///
/// ```
/// use harness::impact::{sql_accesses, Access};
///
/// let found = sql_accesses("UPDATE orders SET total = (SELECT sum(x) FROM lines) WHERE id = 1");
/// let pairs: Vec<_> = found.iter().map(|a| (a.table.as_str(), a.access)).collect();
/// assert_eq!(pairs, [("orders", Access::Write), ("lines", Access::Read)]);
/// assert!(sql_accesses("-- DROP TABLE orders").is_empty());
/// ```
pub fn sql_accesses(sql: &str) -> Vec<SqlAccess> {
    let cleaned = noise_regex().replace_all(sql, " ");
    let mut out = Vec::new();
    for caps in statement_regex().captures_iter(&cleaned) {
        let whole = caps.get(0).expect("match");
        let verb = caps[1]
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_uppercase();
        let before = cleaned[..whole.start()].trim_end().to_lowercase();
        let preceding_word = before
            .rsplit(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or("");
        let skip = match verb.as_str() {
            "UPDATE" => matches!(preceding_word, "for" | "do"),
            "FROM" => preceding_word == "distinct",
            _ => false,
        };
        if skip {
            continue;
        }
        let access = match verb.as_str() {
            "FROM" | "JOIN" => Access::Read,
            _ => Access::Write,
        };
        let list = matches!(verb.as_str(), "FROM" | "DROP" | "TRUNCATE");
        let mut names = vec![caps[2].to_string()];
        if list {
            let mut rest = &cleaned[whole.end()..];
            while let Some(more) = more_names_regex().captures(rest) {
                names.push(more[1].to_string());
                rest = &rest[more.get(0).expect("match").end()..];
            }
        }
        for name in names {
            let table = name.replace('"', "").split_whitespace().collect::<String>();
            if NOT_TABLES.contains(&table.to_lowercase().as_str()) {
                continue;
            }
            out.push(SqlAccess {
                table,
                access,
                verb: verb.clone(),
            });
        }
    }
    out
}

fn table_asset(graph: &AssetGraph, table: &str) -> Option<String> {
    let bare = table.rsplit('.').next().unwrap_or(table);
    [
        table.to_string(),
        table.to_lowercase(),
        bare.to_string(),
        bare.to_lowercase(),
    ]
    .into_iter()
    .map(|name| format!("db.{name}"))
    .find(|asset| graph.asset(asset).is_some())
}

struct Literal {
    text: String,
    first_line: u32,
    last_line: u32,
}

fn line_of(source: &str, offset: usize) -> u32 {
    source[..offset].bytes().filter(|b| *b == b'\n').count() as u32 + 1
}

fn read_literal(source: &str, mut pos: usize) -> Option<(Literal, usize)> {
    let bytes = source.as_bytes();
    for _ in 0..2 {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= bytes.len() {
            return None;
        }
        let start = pos;
        let (body_start, body_end, next) =
            if bytes[pos] == b'r' && matches!(bytes.get(pos + 1), Some(b'"' | b'#')) {
                let hashes = bytes[pos + 1..].iter().take_while(|b| **b == b'#').count();
                if bytes.get(pos + 1 + hashes) != Some(&b'"') {
                    return None;
                }
                let open = pos + 2 + hashes;
                let closing = format!("\"{}", "#".repeat(hashes));
                let end = source[open..].find(&closing)? + open;
                (open, end, end + closing.len())
            } else if bytes[pos] == b'"' {
                let open = pos + 1;
                let mut i = open;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                if i >= bytes.len() {
                    return None;
                }
                (open, i, i + 1)
            } else {
                let mut depth = 0i32;
                let mut i = pos;
                while i < bytes.len() {
                    match bytes[i] {
                        b'(' | b'[' | b'<' | b'{' => depth += 1,
                        b')' | b']' | b'>' | b'}' => {
                            if depth == 0 {
                                return None;
                            }
                            depth -= 1;
                        }
                        b',' if depth == 0 => break,
                        _ => {}
                    }
                    i += 1;
                }
                pos = i + 1;
                continue;
            };
        return Some((
            Literal {
                text: source[body_start..body_end].to_string(),
                first_line: line_of(source, start),
                last_line: line_of(source, body_end),
            },
            next,
        ));
    }
    None
}

fn macro_literals(source: &str) -> Vec<Literal> {
    macro_regex()
        .find_iter(source)
        .filter_map(|m| read_literal(source, m.end()).map(|(literal, _)| literal))
        .collect()
}

/// Maps SQL in migrations and `sqlx::query!`/`query_as!` string literals to
/// `db.<table>` assets. Writes touch the table; reads are evidence only.
///
/// ```
/// use harness::assets::AssetGraph;
/// use harness::impact::{Change, ChangedFile, ImpactAnalyzer, SqlExtractor};
///
/// let graph = AssetGraph::parse("[asset.\"db.orders\"]\nkind = \"table\"\n[asset.\"db.users\"]\nkind = \"table\"\n").unwrap();
/// let analyzer = ImpactAnalyzer::without_extractors(&graph).with_extractor(Box::new(SqlExtractor));
/// let change = Change {
///     files: vec![ChangedFile::whole("migrations/2_x.sql", "ALTER TABLE orders ADD COLUMN note TEXT;\nSELECT * FROM users;")],
///     actions: vec![],
/// };
/// let radius = analyzer.analyze(&change);
/// assert_eq!(radius.touched, ["db.orders"]);
/// assert_eq!(radius.evidence.len(), 2);
/// ```
pub struct SqlExtractor;

impl SqlExtractor {
    fn scan(
        &self,
        ctx: &ExtractContext<'_>,
        file: &ChangedFile,
        sql: &str,
        out: &mut Vec<Evidence>,
    ) {
        for found in sql_accesses(sql) {
            if let Some(asset) = table_asset(ctx.graph, &found.table) {
                out.push(Evidence::new(
                    self.name(),
                    &file.path,
                    asset,
                    found.access,
                    format!("{} {}", found.verb, found.table),
                ));
            }
        }
    }
}

impl Extractor for SqlExtractor {
    fn name(&self) -> &'static str {
        "sql"
    }

    fn extract(&self, ctx: &ExtractContext<'_>, change: &Change) -> Vec<Evidence> {
        let mut out = Vec::new();
        for file in &change.files {
            if file.path.ends_with(".sql") {
                let text = file
                    .content
                    .clone()
                    .unwrap_or_else(|| format!("{}\n{}", file.added_text(), file.removed_text()));
                self.scan(ctx, file, &text, &mut out);
            } else if file.path.ends_with(".rs") {
                match &file.content {
                    Some(content) => {
                        for literal in macro_literals(content) {
                            let touched = file.added.iter().any(|line| {
                                line.number >= literal.first_line
                                    && line.number <= literal.last_line
                            });
                            if touched {
                                self.scan(ctx, file, &literal.text, &mut out);
                            }
                        }
                        let removed = file.removed_text();
                        for literal in macro_literals(&removed) {
                            self.scan(ctx, file, &literal.text, &mut out);
                        }
                    }
                    None => {
                        let text = format!("{}\n{}", file.added_text(), file.removed_text());
                        for literal in macro_literals(&text) {
                            self.scan(ctx, file, &literal.text, &mut out);
                        }
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::ImpactAnalyzer;
    use proptest::prelude::*;

    fn pairs(sql: &str) -> Vec<(String, Access)> {
        sql_accesses(sql)
            .into_iter()
            .map(|a| (a.table, a.access))
            .collect()
    }

    fn w(name: &str) -> (String, Access) {
        (name.to_string(), Access::Write)
    }

    fn r(name: &str) -> (String, Access) {
        (name.to_string(), Access::Read)
    }

    #[test]
    fn each_write_verb_is_a_write() {
        assert_eq!(pairs("INSERT INTO a (x) VALUES (1)"), [w("a")]);
        assert_eq!(pairs("update a set x = 1"), [w("a")]);
        assert_eq!(pairs("DELETE FROM a WHERE x = 1"), [w("a")]);
        assert_eq!(
            pairs("ALTER TABLE IF EXISTS ONLY a ADD COLUMN y INT"),
            [w("a")]
        );
        assert_eq!(pairs("DROP TABLE IF EXISTS a"), [w("a")]);
        assert_eq!(pairs("TRUNCATE TABLE a"), [w("a")]);
        assert_eq!(pairs("TRUNCATE a"), [w("a")]);
        assert_eq!(pairs("CREATE TABLE IF NOT EXISTS a (id INT)"), [w("a")]);
    }

    #[test]
    fn selects_and_joins_are_reads() {
        assert_eq!(pairs("SELECT * FROM a"), [r("a")]);
        assert_eq!(
            pairs("SELECT * FROM a JOIN b ON a.id = b.id LEFT JOIN c ON c.id = b.id"),
            [r("a"), r("b"), r("c")]
        );
        assert_eq!(pairs("SELECT * FROM a, b"), [r("a"), r("b")]);
    }

    #[test]
    fn subselects_are_found_at_any_depth() {
        assert_eq!(
            pairs("SELECT * FROM (SELECT id FROM inner_t WHERE id IN (SELECT id FROM deepest)) s"),
            [r("inner_t"), r("deepest")]
        );
        assert_eq!(
            pairs("UPDATE orders SET total = (SELECT sum(x) FROM lines WHERE lines.o = orders.id)"),
            [w("orders"), r("lines")]
        );
        assert_eq!(
            pairs("INSERT INTO audit SELECT * FROM orders"),
            [w("audit"), r("orders")]
        );
        assert_eq!(
            pairs("WITH moved AS (DELETE FROM a RETURNING *) INSERT INTO b SELECT * FROM moved"),
            [w("a"), w("b"), r("moved")]
        );
    }

    #[test]
    fn negatives_comments_strings_and_keywords() {
        assert!(pairs("-- DROP TABLE a").is_empty());
        assert!(pairs("/* DELETE FROM a */ SELECT 1").is_empty());
        assert!(pairs("SELECT 'DROP TABLE a; DELETE FROM b'").is_empty());
        assert!(pairs("SELECT 'it''s FROM a'").is_empty());
        assert!(pairs("").is_empty());
        assert!(pairs("SELECT 1 + 1").is_empty());
        assert!(pairs("SELECT * FROM a WHERE x IS DISTINCT FROM y")
            .iter()
            .all(|p| p.0 != "y"));
        assert_eq!(
            pairs("INSERT INTO a VALUES (1) ON CONFLICT (id) DO UPDATE SET x = 2"),
            [w("a")]
        );
        assert_eq!(pairs("SELECT * FROM a FOR UPDATE"), [r("a")]);
    }

    #[test]
    fn schema_qualified_quoted_and_multi_drop_names() {
        assert_eq!(pairs("INSERT INTO public.a VALUES (1)"), [w("public.a")]);
        assert_eq!(pairs("INSERT INTO \"Orders\" VALUES (1)"), [w("Orders")]);
        assert_eq!(pairs("DROP TABLE a, b, c"), [w("a"), w("b"), w("c")]);
    }

    fn graph() -> AssetGraph {
        AssetGraph::parse(
            "[asset.\"db.orders\"]\nkind = \"table\"\n[asset.\"db.lines\"]\nkind = \"table\"\n[asset.\"db.public.users\"]\nkind = \"table\"\n",
        )
        .unwrap()
    }

    fn analyze(file: ChangedFile) -> crate::impact::BlastRadius {
        let graph = graph();
        ImpactAnalyzer::without_extractors(&graph)
            .with_extractor(Box::new(SqlExtractor))
            .analyze(&Change {
                files: vec![file],
                actions: vec![],
            })
    }

    #[test]
    fn schema_prefix_falls_back_to_bare_table_asset() {
        assert_eq!(
            table_asset(&graph(), "public.orders").as_deref(),
            Some("db.orders")
        );
        assert_eq!(
            table_asset(&graph(), "Orders").as_deref(),
            Some("db.orders")
        );
        assert_eq!(
            table_asset(&graph(), "public.users").as_deref(),
            Some("db.public.users")
        );
        assert_eq!(table_asset(&graph(), "nothing"), None);
    }

    #[test]
    fn sqlx_macros_with_every_literal_style() {
        let source = r##"
fn a() {
    sqlx::query!("INSERT INTO orders (id) VALUES ($1)", id);
    sqlx::query_as!(Row, r#"SELECT * FROM lines"#);
    sqlx::query_scalar!(
        "SELECT count(*) FROM lines
         WHERE o IN (SELECT id FROM orders)"
    );
}
"##;
        let radius = analyze(ChangedFile::whole("api/src/q.rs", source));
        assert_eq!(radius.touched, ["db.orders"]);
        let reads: Vec<_> = radius
            .evidence
            .iter()
            .filter(|e| e.access == Access::Read)
            .map(|e| e.asset.as_str())
            .collect();
        assert!(reads.contains(&"db.lines") && reads.contains(&"db.orders"));
    }

    #[test]
    fn plain_rust_strings_and_other_macros_are_ignored() {
        let source = "fn a() {\n    let s = \"DELETE FROM orders\";\n    println!(\"DROP TABLE orders\");\n    format!(\"UPDATE orders\");\n}\n";
        assert!(analyze(ChangedFile::whole("api/src/q.rs", source)).is_empty());
    }

    #[test]
    fn only_macros_overlapping_changed_lines_count() {
        let source = "fn a() {\n    sqlx::query!(\"DELETE FROM orders\");\n    sqlx::query!(\"SELECT 1 FROM lines\");\n}\n";
        let mut file = ChangedFile::whole("api/src/q.rs", source);
        file.added.retain(|line| line.number == 3);
        let radius = analyze(file);
        assert!(radius.touched.is_empty());
        assert_eq!(radius.evidence.len(), 1);
    }

    #[test]
    fn removed_query_still_touches_the_table() {
        let mut file = ChangedFile::new("api/src/q.rs");
        file.removed
            .push("sqlx::query!(\"DELETE FROM orders\");".into());
        assert_eq!(analyze(file).touched, ["db.orders"]);
    }

    #[test]
    fn without_content_added_lines_are_scanned() {
        let mut file = ChangedFile::new("api/src/q.rs");
        file.added.push(crate::impact::AddedLine {
            number: 4,
            text: "sqlx::query!(\"UPDATE orders SET x = 1\");".into(),
        });
        assert_eq!(analyze(file).touched, ["db.orders"]);
    }

    #[test]
    fn migrations_without_content_use_the_diff_lines_and_unrelated_files_are_skipped() {
        let mut sql = ChangedFile::new("migrations/3.sql");
        sql.added.push(crate::impact::AddedLine {
            number: 1,
            text: "ALTER TABLE orders ADD COLUMN x INT;".into(),
        });
        assert_eq!(analyze(sql).touched, ["db.orders"]);
        assert!(analyze(ChangedFile::whole("README.md", "DROP TABLE orders;")).is_empty());
    }

    #[test]
    fn tables_missing_from_the_graph_are_not_reported() {
        assert!(analyze(ChangedFile::whole("m.sql", "DROP TABLE ghosts;")).is_empty());
    }

    #[test]
    fn unterminated_literals_do_not_panic() {
        for source in [
            "sqlx::query!(\"DELETE FROM orders",
            "sqlx::query!(r#\"DELETE",
            "sqlx::query!(",
            "sqlx::query_as!(Row",
        ] {
            let _ = analyze(ChangedFile::whole("q.rs", source));
        }
    }

    proptest! {
        #[test]
        fn never_panics_on_arbitrary_text(text in "\\PC{0,200}") {
            let _ = sql_accesses(&text);
            let _ = analyze(ChangedFile::whole("q.rs", format!("sqlx::query!(\"{text}\")")));
            let _ = analyze(ChangedFile::whole("m.sql", text));
        }

        #[test]
        fn writes_are_found_whatever_surrounds_them(prefix in "[ \\n\\t]{0,5}", table in "[a-z][a-z0-9_]{0,10}") {
            prop_assume!(!NOT_TABLES.contains(&table.as_str()));
            let sql = format!("{prefix}INSERT INTO {table} (a) VALUES (1);");
            prop_assert_eq!(pairs(&sql), [w(&table)]);
        }
    }

    fn literal_texts(source: &str) -> Vec<String> {
        macro_literals(source).into_iter().map(|l| l.text).collect()
    }

    #[test]
    fn raw_prefix_without_a_quote_is_not_a_literal() {
        assert!(literal_texts("sqlx::query!(r#x, \"DELETE FROM a\")").is_empty());
    }

    #[test]
    fn skips_a_non_literal_argument_with_nested_brackets() {
        assert_eq!(
            literal_texts("sqlx::query_as!(Foo<A, B>, \"DELETE FROM a\")"),
            ["DELETE FROM a"]
        );
        assert_eq!(
            literal_texts("sqlx::query_as!(Foo{x: [1, 2]}, \"DELETE FROM b\")"),
            ["DELETE FROM b"]
        );
    }

    #[test]
    fn unbalanced_closer_before_any_literal_yields_nothing() {
        assert!(literal_texts("sqlx::query_as!(Foo)").is_empty());
        assert!(literal_texts("sqlx::query_as!(Foo>, \"DELETE FROM a\")").is_empty());
    }

    #[test]
    fn two_non_literal_arguments_yield_nothing() {
        assert!(literal_texts("sqlx::query_as!(A, B, \"DELETE FROM a\")").is_empty());
        assert!(literal_texts("sqlx::query_as!(A, B").is_empty());
    }

    #[test]
    fn rust_file_without_content_scans_added_and_removed_lines() {
        let graph = AssetGraph::parse("[asset.\"db.orders\"]\nkind = \"table\"\n").unwrap();
        let mut file = ChangedFile::new("src/q.rs");
        file.added.push(crate::impact::AddedLine {
            number: 3,
            text: "sqlx::query!(\"DELETE FROM orders\")".to_string(),
        });
        let radius = ImpactAnalyzer::without_extractors(&graph)
            .with_extractor(Box::new(SqlExtractor))
            .analyze(&Change {
                files: vec![file],
                actions: vec![],
            });
        assert_eq!(radius.touched, ["db.orders"]);
        assert_eq!(radius.evidence[0].detail, "DELETE orders");
    }

    #[test]
    fn removed_query_in_a_file_with_content_touches_the_table() {
        let graph = AssetGraph::parse("[asset.\"db.orders\"]\nkind = \"table\"\n").unwrap();
        let mut file = ChangedFile::new("src/q.rs");
        file.content = Some("fn nothing() {}\n".to_string());
        file.removed
            .push("sqlx::query!(\"DROP TABLE orders\");".to_string());
        let radius = ImpactAnalyzer::without_extractors(&graph)
            .with_extractor(Box::new(SqlExtractor))
            .analyze(&Change {
                files: vec![file],
                actions: vec![],
            });
        assert_eq!(radius.touched, ["db.orders"]);
        assert_eq!(radius.evidence[0].detail, "DROP orders");
    }
}
