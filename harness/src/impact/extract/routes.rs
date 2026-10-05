use crate::impact::{Access, Change, ChangedFile, Evidence, ExtractContext, Extractor};
use regex::Regex;
use std::collections::BTreeSet;
use std::sync::OnceLock;

const METHODS: &str = "get|post|put|delete|patch|head";

fn route_call() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r#"\.route\(\s*"([^"]+)"\s*,\s*web::({METHODS})\(\s*\)\s*\.to\(\s*([\w:]+)\s*\)"#
        ))
        .expect("static regex")
    })
}

fn resource() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"web::resource\(\s*"([^"]+)"\s*\)"#).expect("static regex"))
}

fn resource_route() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"web::({METHODS})\(\s*\)\s*\.to\(\s*([\w:]+)\s*\)"
        ))
        .expect("static regex")
    })
}

fn attribute() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(r#"#\[({METHODS})\(\s*"([^"]+)"[^\]]*\]"#)).expect("static regex")
    })
}

fn after_attribute() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^\s*(?:(?:#\[[^\]]*\]|///[^\n]*)\s*)*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)",
        )
        .expect("static regex")
    })
}

fn function() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)").expect("static regex")
    })
}

/// A route registration found in source: HTTP method, path and handler name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteBinding {
    pub method: String,
    pub path: String,
    pub handler: String,
    pub first_line: u32,
    pub last_line: u32,
}

fn line_of(source: &str, offset: usize) -> u32 {
    source[..offset].bytes().filter(|b| *b == b'\n').count() as u32 + 1
}

fn last_segment(path: &str) -> String {
    path.rsplit("::").next().unwrap_or(path).to_string()
}

/// Route registrations in actix source (a `#[get]` attribute with no function
/// after it, as in removed lines, has an empty handler name): `.route("/p", web::get().to(h))`,
/// `web::resource("/p").route(web::post().to(h))` and `#[get("/p")] fn h`.
///
/// ```
/// use harness::impact::route_bindings;
///
/// let src = "cfg.route(\"/health\", web::get().to(health));\n\n#[post(\"/orders\")]\nasync fn create() {}\n";
/// let found: Vec<_> = route_bindings(src).into_iter().map(|b| (b.method, b.path, b.handler)).collect();
/// assert!(found.contains(&("GET".to_string(), "/health".to_string(), "health".to_string())));
/// assert!(found.contains(&("POST".to_string(), "/orders".to_string(), "create".to_string())));
/// assert!(route_bindings("fn plain() {}").is_empty());
/// ```
pub fn route_bindings(source: &str) -> Vec<RouteBinding> {
    let mut out = Vec::new();
    for caps in route_call().captures_iter(source) {
        let whole = caps.get(0).expect("match");
        out.push(RouteBinding {
            method: caps[2].to_uppercase(),
            path: caps[1].to_string(),
            handler: last_segment(&caps[3]),
            first_line: line_of(source, whole.start()),
            last_line: line_of(source, whole.end()),
        });
    }
    for caps in resource().captures_iter(source) {
        let whole = caps.get(0).expect("match");
        let tail_start = whole.end();
        let mut tail_end = (tail_start + 600).min(source.len());
        while !source.is_char_boundary(tail_end) {
            tail_end -= 1;
        }
        let mut tail = &source[tail_start..tail_end];
        if let Some(cut) = [tail.find("web::resource"), tail.find(';')]
            .into_iter()
            .flatten()
            .min()
        {
            tail = &tail[..cut];
        }
        for route in resource_route().captures_iter(tail) {
            let span = route.get(0).expect("match");
            out.push(RouteBinding {
                method: route[1].to_uppercase(),
                path: caps[1].to_string(),
                handler: last_segment(&route[2]),
                first_line: line_of(source, tail_start + span.start()),
                last_line: line_of(source, tail_start + span.end()),
            });
        }
    }
    for caps in attribute().captures_iter(source) {
        let whole = caps.get(0).expect("match");
        let handler = after_attribute()
            .captures(&source[whole.end()..])
            .map(|next| next[1].to_string())
            .unwrap_or_default();
        out.push(RouteBinding {
            method: caps[1].to_uppercase(),
            path: caps[2].to_string(),
            handler,
            first_line: line_of(source, whole.start()),
            last_line: line_of(source, whole.end()),
        });
    }
    out
}

/// Line spans of function items by name: the first attribute line through the
/// closing brace (or the signature line for bodiless functions).
///
/// ```
/// use harness::impact::function_spans;
///
/// let spans = function_spans("#[get(\"/a\")]\nasync fn a() {\n    if true { 1 } else { 2 };\n}\nfn b() {}\n");
/// assert_eq!(spans, [("a".to_string(), 1, 4), ("b".to_string(), 5, 5)]);
/// ```
pub fn function_spans(source: &str) -> Vec<(String, u32, u32)> {
    let bytes = source.as_bytes();
    let lines: Vec<&str> = source.lines().collect();
    let mut out = Vec::new();
    for caps in function().captures_iter(source) {
        let whole = caps.get(0).expect("match");
        let line_start = source[..whole.start()].rfind('\n').map_or(0, |i| i + 1);
        if !source[line_start..whole.start()].trim().is_empty() {
            continue;
        }
        let sig_line = line_of(source, whole.start());
        let mut first = sig_line;
        while first > 1 {
            let above = lines[(first - 2) as usize].trim_start();
            if above.starts_with("#[") || above.starts_with("///") {
                first -= 1;
            } else {
                break;
            }
        }
        let mut i = whole.end();
        let mut open = None;
        while i < bytes.len() {
            match bytes[i] {
                b'{' => {
                    open = Some(i);
                    break;
                }
                b';' => break,
                _ => i += 1,
            }
        }
        let last = match open {
            None => sig_line,
            Some(start) => {
                let mut depth = 0i32;
                let mut i = start;
                let mut end = bytes.len().saturating_sub(1);
                while i < bytes.len() {
                    match bytes[i] {
                        b'"' => {
                            i += 1;
                            while i < bytes.len() && bytes[i] != b'"' {
                                i += if bytes[i] == b'\\' { 2 } else { 1 };
                            }
                        }
                        b'/' if bytes.get(i + 1) == Some(&b'/') => {
                            while i < bytes.len() && bytes[i] != b'\n' {
                                i += 1;
                            }
                            continue;
                        }
                        b'\'' if bytes.get(i + 2) == Some(&b'\'') => i += 2,
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = i;
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                line_of(source, end)
            }
        };
        out.push((caps[1].to_string(), first, last));
    }
    out
}

fn endpoint_assets(ctx: &ExtractContext<'_>, method: &str, path: &str) -> Vec<String> {
    let exact = format!("http.{method} {path}");
    if ctx.graph.asset(&exact).is_some() {
        return vec![exact];
    }
    let prefix = format!("http.{method} ");
    ctx.graph
        .assets()
        .filter(|asset| asset.name.starts_with(&prefix) && asset.name.ends_with(path))
        .map(|asset| asset.name.clone())
        .collect()
}

/// Maps actix handlers to `http.<METHOD> <path>` endpoint assets: a changed
/// handler body, attribute or route registration touches its endpoint.
///
/// A route registered inside a `web::scope` also matches an asset whose path
/// ends with the registered one.
///
/// ```
/// use harness::assets::AssetGraph;
/// use harness::impact::{Change, ChangedFile, ImpactAnalyzer, ActixRouteExtractor};
///
/// let graph = AssetGraph::parse("[asset.\"http.GET /hello\"]\nkind = \"endpoint\"\n").unwrap();
/// let analyzer = ImpactAnalyzer::without_extractors(&graph).with_extractor(Box::new(ActixRouteExtractor));
/// let source = "#[get(\"/hello\")]\nasync fn hello() {\n    work();\n}\n\nfn helper() {}\n";
/// let mut body = ChangedFile::whole("api/src/lib.rs", source);
/// body.added.retain(|line| line.number == 3);
/// assert_eq!(analyzer.analyze(&Change { files: vec![body], actions: vec![] }).touched, ["http.GET /hello"]);
/// let mut other = ChangedFile::whole("api/src/lib.rs", source);
/// other.added.retain(|line| line.number == 6);
/// assert!(analyzer.analyze(&Change { files: vec![other], actions: vec![] }).touched.is_empty());
/// ```
pub struct ActixRouteExtractor;

impl ActixRouteExtractor {
    fn emit(
        &self,
        ctx: &ExtractContext<'_>,
        file: &ChangedFile,
        binding: &RouteBinding,
        why: &str,
        out: &mut Vec<Evidence>,
    ) {
        for asset in endpoint_assets(ctx, &binding.method, &binding.path) {
            out.push(Evidence::new(
                self.name(),
                &file.path,
                asset,
                Access::Write,
                format!(
                    "{why} for {} {} (handler `{}`)",
                    binding.method, binding.path, binding.handler
                ),
            ));
        }
    }
}

impl Extractor for ActixRouteExtractor {
    fn name(&self) -> &'static str {
        "routes"
    }

    fn extract(&self, ctx: &ExtractContext<'_>, change: &Change) -> Vec<Evidence> {
        let mut out = Vec::new();
        for file in change.files.iter().filter(|f| f.path.ends_with(".rs")) {
            let changed: BTreeSet<u32> = file.added.iter().map(|line| line.number).collect();
            match &file.content {
                Some(content) => {
                    let spans = function_spans(content);
                    for binding in route_bindings(content) {
                        let registration_changed = changed
                            .range(binding.first_line..=binding.last_line)
                            .next()
                            .is_some();
                        let handler_changed = spans
                            .iter()
                            .filter(|(name, _, _)| *name == binding.handler)
                            .any(|(_, first, last)| changed.range(*first..=*last).next().is_some());
                        if registration_changed {
                            self.emit(ctx, file, &binding, "route registration changed", &mut out);
                        } else if handler_changed {
                            self.emit(ctx, file, &binding, "handler changed", &mut out);
                        }
                    }
                }
                None => {
                    for binding in route_bindings(&file.added_text()) {
                        self.emit(ctx, file, &binding, "route registration added", &mut out);
                    }
                }
            }
            for binding in route_bindings(&file.removed_text()) {
                self.emit(ctx, file, &binding, "route registration removed", &mut out);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::AssetGraph;
    use crate::impact::{AddedLine, ImpactAnalyzer};

    const SOURCE: &str = "use actix_web::{web, HttpResponse};\n\n#[get(\"/items\")]\nasync fn list() -> HttpResponse {\n    HttpResponse::Ok().finish()\n}\n\n/// Creates.\n#[post(\"/items\")]\n#[allow(unused)]\npub async fn create() -> HttpResponse {\n    let s = \"}\";\n    HttpResponse::Ok().finish()\n}\n\nasync fn health() -> HttpResponse {\n    HttpResponse::Ok().finish()\n}\n\nasync fn helper() {}\n\npub fn configure(cfg: &mut web::ServiceConfig) {\n    cfg.route(\"/health\", web::get().to(crate::health))\n        .service(\n            web::resource(\"/res\")\n                .route(web::put().to(list))\n                .route(web::delete().to(create)),\n        );\n}\n";

    fn graph() -> AssetGraph {
        AssetGraph::parse(
            "[asset.\"http.GET /items\"]\nkind = \"endpoint\"\n[asset.\"http.POST /items\"]\nkind = \"endpoint\"\n[asset.\"http.GET /health\"]\nkind = \"endpoint\"\n[asset.\"http.PUT /res\"]\nkind = \"endpoint\"\n[asset.\"http.DELETE /res\"]\nkind = \"endpoint\"\n[asset.\"http.GET /api/v1/scoped\"]\nkind = \"endpoint\"\n",
        )
        .unwrap()
    }

    fn line(needle: &str) -> u32 {
        SOURCE.lines().position(|l| l.contains(needle)).unwrap() as u32 + 1
    }

    fn change_at(lines: &[u32]) -> ChangedFile {
        let mut file = ChangedFile::new("api/src/lib.rs");
        file.content = Some(SOURCE.to_string());
        for number in lines {
            file.added.push(AddedLine {
                number: *number,
                text: String::new(),
            });
        }
        file
    }

    fn touched(file: ChangedFile) -> Vec<String> {
        let graph = graph();
        ImpactAnalyzer::without_extractors(&graph)
            .with_extractor(Box::new(ActixRouteExtractor))
            .analyze(&Change {
                files: vec![file],
                actions: vec![],
            })
            .touched
    }

    #[test]
    fn finds_every_binding_style() {
        let found: Vec<_> = route_bindings(SOURCE)
            .into_iter()
            .map(|b| format!("{} {} {}", b.method, b.path, b.handler))
            .collect();
        for expected in [
            "GET /items list",
            "POST /items create",
            "GET /health health",
            "PUT /res list",
            "DELETE /res create",
        ] {
            assert!(
                found.contains(&expected.to_string()),
                "{expected} in {found:?}"
            );
        }
        assert_eq!(found.len(), 5);
    }

    #[test]
    fn changed_handler_body_touches_every_endpoint_bound_to_it() {
        assert_eq!(
            touched(change_at(&[line("HttpResponse::Ok().finish()")])),
            ["http.GET /items", "http.PUT /res"]
        );
        assert_eq!(
            touched(change_at(&[line("let s")])),
            ["http.DELETE /res", "http.POST /items"]
        );
    }

    #[test]
    fn braces_inside_strings_do_not_end_a_handler_early() {
        let last_body_line = SOURCE
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains("HttpResponse::Ok().finish()"))
            .map(|(i, _)| i as u32 + 1)
            .nth(1)
            .unwrap();
        assert!(touched(change_at(&[last_body_line])).contains(&"http.POST /items".to_string()));
    }

    #[test]
    fn changed_attribute_and_signature_touch_the_endpoint() {
        assert_eq!(
            touched(change_at(&[line("#[get")])),
            ["http.GET /items", "http.PUT /res"]
        );
        assert!(touched(change_at(&[line("#[allow(unused)]")]))
            .contains(&"http.POST /items".to_string()));
    }

    #[test]
    fn changed_registration_touches_the_endpoint_via_qualified_handler() {
        assert_eq!(
            touched(change_at(&[line("cfg.route")])),
            ["http.GET /health"]
        );
        assert_eq!(touched(change_at(&[line("web::put()")])), ["http.PUT /res"]);
    }

    #[test]
    fn unrelated_lines_and_helpers_touch_nothing() {
        assert!(touched(change_at(&[1, line("async fn helper")])).is_empty());
        assert!(touched(change_at(&[])).is_empty());
    }

    #[test]
    fn removed_registration_touches_the_endpoint() {
        let mut file = ChangedFile::new("api/src/lib.rs");
        file.removed.push("#[get(\"/items\")]".into());
        assert_eq!(touched(file), ["http.GET /items"]);
    }

    #[test]
    fn without_content_added_registrations_count() {
        let mut file = ChangedFile::new("api/src/lib.rs");
        file.added.push(AddedLine {
            number: 9,
            text: "cfg.route(\"/health\", web::get().to(health));".into(),
        });
        assert_eq!(touched(file), ["http.GET /health"]);
    }

    #[test]
    fn scoped_routes_match_by_path_suffix() {
        let mut file = ChangedFile::new("api/src/lib.rs");
        file.added.push(AddedLine {
            number: 1,
            text: "cfg.route(\"/scoped\", web::get().to(x));".into(),
        });
        assert_eq!(touched(file), ["http.GET /api/v1/scoped"]);
    }

    #[test]
    fn non_rust_files_and_unknown_routes_are_ignored() {
        let mut other = ChangedFile::new("README.md");
        other.content = Some(SOURCE.to_string());
        other.added.push(AddedLine {
            number: 3,
            text: String::new(),
        });
        assert!(touched(other).is_empty());
        let mut file = ChangedFile::new("api/src/lib.rs");
        file.added.push(AddedLine {
            number: 1,
            text: "cfg.route(\"/unknown\", web::get().to(x));".into(),
        });
        assert!(touched(file).is_empty());
    }

    #[test]
    fn function_spans_cover_attributes_and_bodies() {
        let spans = function_spans(SOURCE);
        let create = spans.iter().find(|s| s.0 == "create").unwrap();
        assert_eq!(create.1, line("/// Creates."));
        assert_eq!(create.2, line("let s") + 2);
        assert!(function_spans("fn unterminated() {").len() == 1);
        assert!(function_spans("").is_empty());
    }

    #[test]
    fn extern_signatures_without_bodies_span_one_line() {
        assert_eq!(
            function_spans("trait T {\n    fn m(&self);\n}\n"),
            [("m".to_string(), 2, 2)]
        );
    }
}
