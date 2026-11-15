use super::graph::AssetGraph;
use std::collections::BTreeMap;
use std::fmt::Write;

impl AssetGraph {
    /// Plain-text rendering: concerns by weight, then each asset with its
    /// concerns, owners and readers.
    ///
    /// ```
    /// use harness::assets::AssetGraph;
    ///
    /// let graph = AssetGraph::parse(r#"
    /// [concern.revenue]
    /// weight = 10
    /// [asset."db.orders"]
    /// kind = "table"
    /// concerns = ["revenue"]
    /// [asset."job.nightly"]
    /// kind = "job"
    /// reads = ["db.orders"]
    /// "#).unwrap();
    /// let text = graph.render_text();
    /// assert!(text.contains("revenue (weight 10)"));
    /// assert!(text.contains("db.orders [table]"));
    /// assert!(text.contains("read by: job.nightly"));
    /// ```
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str("concerns:\n");
        let mut concerns: Vec<_> = self.concerns().collect();
        concerns.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.name.cmp(&b.name)));
        for concern in concerns {
            let _ = write!(out, "  {} (weight {})", concern.name, concern.weight);
            if let Some(description) = &concern.description {
                let _ = write!(out, ": {description}");
            }
            out.push('\n');
        }
        out.push_str("assets:\n");
        for asset in self.assets() {
            let _ = writeln!(out, "  {} [{}]", asset.name, asset.kind);
            if !asset.concerns.is_empty() {
                let _ = writeln!(out, "    concerns: {}", asset.concerns.join(", "));
            }
            let mut owners = asset.owners.clone();
            owners.extend(self.sites_of(&asset.name).map(|site| site.file.clone()));
            owners.dedup();
            if !owners.is_empty() {
                let _ = writeln!(out, "    owners: {}", owners.join(", "));
            }
            if !asset.reads.is_empty() {
                let _ = writeln!(out, "    reads: {}", asset.reads.join(", "));
            }
            let readers: Vec<&str> = self
                .assets()
                .filter(|other| other.reads.contains(&asset.name))
                .map(|other| other.name.as_str())
                .collect();
            if !readers.is_empty() {
                let _ = writeln!(out, "    read by: {}", readers.join(", "));
            }
        }
        out
    }

    /// Mermaid flowchart: one node per asset, an arrow from each reader to
    /// the asset it reads.
    ///
    /// ```
    /// use harness::assets::AssetGraph;
    ///
    /// let graph = AssetGraph::parse(r#"
    /// [asset."db.orders"]
    /// kind = "table"
    /// [asset."job.nightly"]
    /// kind = "job"
    /// reads = ["db.orders"]
    /// "#).unwrap();
    /// let mermaid = graph.render_mermaid();
    /// assert!(mermaid.starts_with("graph LR\n"));
    /// assert!(mermaid.contains("n1 --> n0"));
    /// ```
    pub fn render_mermaid(&self) -> String {
        let mut ids: BTreeMap<&str, String> = BTreeMap::new();
        let mut out = String::from("graph LR\n");
        for (index, asset) in self.assets().enumerate() {
            let id = format!("n{index}");
            let label = escape(&format!("{} ({})", asset.name, asset.kind));
            let _ = writeln!(out, "    {id}[\"{label}\"]");
            ids.insert(asset.name.as_str(), id);
        }
        for asset in self.assets() {
            for read in &asset.reads {
                if let (Some(from), Some(to)) =
                    (ids.get(asset.name.as_str()), ids.get(read.as_str()))
                {
                    let _ = writeln!(out, "    {from} --> {to}");
                }
            }
        }
        out
    }
}

fn escape(label: &str) -> String {
    label
        .replace('&', "#amp;")
        .replace('"', "#quot;")
        .replace('<', "#lt;")
        .replace('>', "#gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
[concern.revenue]
weight = 10
description = "Paid path"
[concern.auth]
weight = 9
[asset."http.POST /checkout"]
kind = "endpoint"
concerns = ["revenue"]
reads = ["db.orders", "db.users"]
[asset."db.orders"]
kind = "table"
owners = ["api/**"]
[asset."db.users"]
kind = "table"
"#;

    #[test]
    fn text_lists_concerns_heaviest_first_and_edges_both_ways() {
        let text = AssetGraph::parse(SRC).unwrap().render_text();
        assert!(
            text.find("revenue (weight 10): Paid path").unwrap()
                < text.find("auth (weight 9)").unwrap()
        );
        assert!(text.contains("reads: db.orders, db.users"));
        assert!(text.contains("read by: http.POST /checkout"));
        assert!(text.contains("owners: api/**"));
    }

    #[test]
    fn empty_graph_renders_headers_only() {
        let graph = AssetGraph::new();
        assert_eq!(graph.render_text(), "concerns:\nassets:\n");
        assert_eq!(graph.render_mermaid(), "graph LR\n");
    }

    #[test]
    fn mermaid_ids_are_sanitised_and_labels_escaped() {
        let mermaid = AssetGraph::parse(SRC).unwrap().render_mermaid();
        assert!(mermaid.contains("n2[\"http.POST /checkout (endpoint)\"]"));
        assert!(mermaid.contains("n2 --> n0"));
        assert!(mermaid.contains("n2 --> n1"));
        let quoted = AssetGraph::parse("[asset.\"a\\\"b<\"]\nkind=\"job\"\n")
            .unwrap()
            .render_mermaid();
        assert!(quoted.contains("a#quot;b#lt; (job)"));
    }

    #[test]
    fn cycles_render_both_edges() {
        let graph = AssetGraph::parse(
            "[asset.a]\nkind=\"job\"\nreads=[\"b\"]\n[asset.b]\nkind=\"job\"\nreads=[\"a\"]\n",
        )
        .unwrap();
        let mermaid = graph.render_mermaid();
        assert!(mermaid.contains("n0 --> n1") && mermaid.contains("n1 --> n0"));
    }

    #[test]
    fn text_merges_declared_sites_into_owners_without_repeating_them() {
        let mut graph = AssetGraph::parse(SRC).unwrap();
        graph
            .merge_declarations([
                nanna_effects::Touch::new("db.orders", "m", "api/x.rs", 1),
                nanna_effects::Touch::new("db.orders", "m", "api/x.rs", 2),
            ])
            .unwrap();
        let text = graph.render_text();
        assert!(text.contains("owners: api/**, api/x.rs\n"));
        assert!(text.contains("concerns: revenue\n"));
    }

    #[test]
    fn mermaid_skips_edges_to_unknown_assets() {
        let mut graph = AssetGraph::new();
        let mut asset = crate::assets::Asset::new("a", crate::assets::AssetKind::Job);
        asset.reads = vec!["ghost".into()];
        graph.insert_asset(asset).unwrap();
        assert!(!graph.render_mermaid().contains("-->"));
    }
}
