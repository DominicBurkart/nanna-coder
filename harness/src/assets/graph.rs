use super::model::{Asset, AssetKind, Concern, WeightedConcern};
use super::{manifest_path_in, AssetError, MANIFEST_FILE_NAME};
use glob::{MatchOptions, Pattern};
use nanna_effects::Touch;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};

const MATCH_OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

/// A place in code that declared it touches an asset with `touches!`.
///
/// ```
/// use harness::assets::CodeSite;
///
/// let site = CodeSite { module_path: "api::orders".into(), file: "api/src/orders.rs".into(), line: 7 };
/// assert_eq!(site.to_string(), "api/src/orders.rs:7 (api::orders)");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CodeSite {
    pub module_path: String,
    pub file: String,
    pub line: u32,
}

impl fmt::Display for CodeSite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{} ({})", self.file, self.line, self.module_path)
    }
}

/// A non-fatal finding from [`AssetGraph::warnings`].
///
/// ```
/// use harness::assets::AssetGraph;
///
/// let graph = AssetGraph::parse("[concern.c]\nweight = 1\n[asset.\"db.lonely\"]\nkind = \"table\"\nconcerns = [\"c\"]\n").unwrap();
/// let warnings = graph.warnings(None);
/// assert_eq!(warnings.len(), 1);
/// assert!(warnings[0].to_string().contains("db.lonely"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    Orphan {
        file: PathBuf,
        asset: String,
    },
    OwnerMatchesNoFiles {
        file: PathBuf,
        asset: String,
        pattern: String,
    },
    NoConcerns {
        file: PathBuf,
        asset: String,
    },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::Orphan { file, asset } => write!(
                f,
                "{}: asset `{asset}` is read by nothing and owned by nothing",
                file.display()
            ),
            Warning::OwnerMatchesNoFiles {
                file,
                asset,
                pattern,
            } => write!(
                f,
                "{}: field `{}` matches no files: `{pattern}`",
                file.display(),
                field(asset, "owners")
            ),
            Warning::NoConcerns { file, asset } => {
                write!(f, "{}: asset `{asset}` has no concerns", file.display())
            }
        }
    }
}

fn field(asset: &str, key: &str) -> String {
    format!("asset.\"{asset}\".{key}")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    #[serde(default)]
    concern: BTreeMap<String, RawConcern>,
    #[serde(default)]
    asset: BTreeMap<String, RawAsset>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConcern {
    weight: u32,
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAsset {
    kind: String,
    #[serde(default)]
    concerns: Vec<String>,
    #[serde(default)]
    owners: Vec<String>,
    #[serde(default)]
    reads: Vec<String>,
}

/// The loaded state-asset manifest: concerns, assets, who reads what.
///
/// ```
/// use harness::assets::AssetGraph;
///
/// let graph = AssetGraph::parse(r#"
/// [concern.revenue]
/// weight = 10
///
/// [asset."db.orders"]
/// kind = "table"
/// concerns = ["revenue"]
/// owners = ["api/src/orders/**"]
///
/// [asset."job.invoice_nightly"]
/// kind = "job"
/// reads = ["db.orders"]
/// "#).unwrap();
///
/// assert_eq!(graph.assets().count(), 2);
/// assert!(graph.dependents_of("db.orders").contains("job.invoice_nightly"));
/// assert_eq!(graph.concerns_of(["db.orders"])[0].name, "revenue");
/// assert_eq!(graph.owners_matching("api/src/orders/mod.rs")[0].name, "db.orders");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetGraph {
    file: PathBuf,
    concerns: BTreeMap<String, Concern>,
    assets: BTreeMap<String, Asset>,
    sites: BTreeMap<String, BTreeSet<CodeSite>>,
}

impl Default for AssetGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetGraph {
    pub fn new() -> Self {
        Self {
            file: PathBuf::from(MANIFEST_FILE_NAME),
            concerns: BTreeMap::new(),
            assets: BTreeMap::new(),
            sites: BTreeMap::new(),
        }
    }

    /// Parse manifest text; errors name `effects.toml`.
    ///
    /// ```
    /// use harness::assets::{AssetError, AssetGraph};
    ///
    /// let err = AssetGraph::parse("[asset.\"db.x\"]\nkind = \"table\"\nconcerns = [\"ghost\"]\n").unwrap_err();
    /// assert!(matches!(err, AssetError::InvalidField { ref field, .. } if field == "asset.\"db.x\".concerns"));
    /// ```
    pub fn parse(src: &str) -> Result<Self, AssetError> {
        Self::parse_named(src, Path::new(MANIFEST_FILE_NAME))
    }

    /// Parse manifest text, reporting errors against `file`.
    pub fn parse_named(src: &str, file: &Path) -> Result<Self, AssetError> {
        let raw: RawManifest =
            toml::from_str(src).map_err(|source| match duplicate_asset_name(src, &source) {
                Some(asset) => AssetError::DuplicateAsset {
                    file: file.to_path_buf(),
                    asset,
                },
                None => AssetError::Parse {
                    file: file.to_path_buf(),
                    source,
                },
            })?;
        let mut graph = Self::new();
        graph.file = file.to_path_buf();
        for (name, concern) in raw.concern {
            graph.insert_concern(Concern {
                name,
                weight: concern.weight,
                description: concern.description,
            })?;
        }
        for (name, asset) in raw.asset {
            let kind = asset
                .kind
                .parse::<AssetKind>()
                .map_err(|err| graph.invalid(field(&name, "kind"), err.to_string()))?;
            graph.insert_asset(Asset {
                name,
                kind,
                concerns: asset.concerns,
                owners: asset.owners,
                reads: asset.reads,
            })?;
        }
        graph.check_references()?;
        Ok(graph)
    }

    /// Read and parse the manifest at `path`.
    pub fn load(path: &Path) -> Result<Self, AssetError> {
        let src = std::fs::read_to_string(path).map_err(|source| AssetError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse_named(&src, path)
    }

    /// Read and parse `<repo>/.nanna/effects.toml`.
    ///
    /// ```
    /// use harness::assets::{AssetError, AssetGraph};
    ///
    /// let repo = tempfile::tempdir().unwrap();
    /// assert!(matches!(AssetGraph::load_from_repo(repo.path()), Err(AssetError::Io { .. })));
    /// ```
    pub fn load_from_repo(repo: &Path) -> Result<Self, AssetError> {
        Self::load(&manifest_path_in(repo))
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    fn invalid(&self, field: String, reason: String) -> AssetError {
        AssetError::InvalidField {
            file: self.file.clone(),
            field,
            reason,
        }
    }

    /// Add a concern; a repeated name is an error.
    ///
    /// ```
    /// use harness::assets::{AssetGraph, Concern};
    ///
    /// let mut graph = AssetGraph::new();
    /// let concern = Concern { name: "pii".into(), weight: 8, description: None };
    /// graph.insert_concern(concern.clone()).unwrap();
    /// assert!(graph.insert_concern(concern).is_err());
    /// ```
    pub fn insert_concern(&mut self, concern: Concern) -> Result<(), AssetError> {
        let key = format!("concern.\"{}\"", concern.name);
        if concern.name.trim().is_empty() || concern.name.trim() != concern.name {
            return Err(self.invalid(key, "name must be non-empty and untrimmed".into()));
        }
        if self.concerns.contains_key(&concern.name) {
            return Err(self.invalid(key, "concern is declared twice".into()));
        }
        self.concerns.insert(concern.name.clone(), concern);
        Ok(())
    }

    /// Add an asset; a repeated name is [`AssetError::DuplicateAsset`].
    ///
    /// References are checked by [`AssetGraph::check_references`], so edges
    /// may be added in any order.
    ///
    /// ```
    /// use harness::assets::{Asset, AssetError, AssetGraph, AssetKind};
    ///
    /// let mut graph = AssetGraph::new();
    /// graph.insert_asset(Asset::new("db.a", AssetKind::Table)).unwrap();
    /// let err = graph.insert_asset(Asset::new("db.a", AssetKind::Table)).unwrap_err();
    /// assert!(matches!(err, AssetError::DuplicateAsset { .. }));
    /// ```
    pub fn insert_asset(&mut self, asset: Asset) -> Result<(), AssetError> {
        if asset.name.trim().is_empty() || asset.name.trim() != asset.name {
            return Err(self.invalid(
                format!("asset.\"{}\"", asset.name),
                "name must be non-empty and untrimmed".into(),
            ));
        }
        if self.assets.contains_key(&asset.name) {
            return Err(AssetError::DuplicateAsset {
                file: self.file.clone(),
                asset: asset.name,
            });
        }
        for (key, values) in [
            ("concerns", &asset.concerns),
            ("owners", &asset.owners),
            ("reads", &asset.reads),
        ] {
            let mut seen = BTreeSet::new();
            if let Some(dup) = values.iter().find(|value| !seen.insert(value.as_str())) {
                return Err(
                    self.invalid(field(&asset.name, key), format!("`{dup}` is listed twice"))
                );
            }
        }
        for pattern in &asset.owners {
            if escapes_repo(pattern) {
                return Err(self.invalid(
                    field(&asset.name, "owners"),
                    format!("`{pattern}` must be relative to the repository and contain no `..`"),
                ));
            }
            Pattern::new(pattern).map_err(|err| {
                self.invalid(
                    field(&asset.name, "owners"),
                    format!("`{pattern}` is not a valid glob: {err}"),
                )
            })?;
        }
        self.assets.insert(asset.name.clone(), asset);
        Ok(())
    }

    /// Verify every concern and `reads` reference resolves to a declaration.
    pub fn check_references(&self) -> Result<(), AssetError> {
        for asset in self.assets.values() {
            for concern in &asset.concerns {
                if !self.concerns.contains_key(concern) {
                    return Err(self.invalid(
                        field(&asset.name, "concerns"),
                        format!("unknown concern `{concern}`"),
                    ));
                }
            }
            for read in &asset.reads {
                if !self.assets.contains_key(read) {
                    return Err(self.invalid(
                        field(&asset.name, "reads"),
                        format!("unknown asset `{read}`"),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Every asset, ordered by name.
    pub fn assets(&self) -> impl Iterator<Item = &Asset> {
        self.assets.values()
    }

    /// Every concern, ordered by name.
    pub fn concerns(&self) -> impl Iterator<Item = &Concern> {
        self.concerns.values()
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.get(name)
    }

    pub fn concern(&self, name: &str) -> Option<&Concern> {
        self.concerns.get(name)
    }

    /// Code sites that declared they touch `asset` through `touches!`.
    pub fn sites_of(&self, asset: &str) -> impl Iterator<Item = &CodeSite> {
        self.sites.get(asset).into_iter().flatten()
    }

    /// Every asset that transitively reads `asset`, excluding `asset` itself.
    ///
    /// Cycles in `reads` are allowed; the closure always terminates.
    ///
    /// ```
    /// use harness::assets::AssetGraph;
    ///
    /// let graph = AssetGraph::parse(r#"
    /// [asset."db.a"]
    /// kind = "table"
    /// reads = ["job.c"]
    ///
    /// [asset."http.b"]
    /// kind = "endpoint"
    /// reads = ["db.a"]
    ///
    /// [asset."job.c"]
    /// kind = "job"
    /// reads = ["http.b"]
    /// "#).unwrap();
    /// let dependents: Vec<_> = graph.dependents_of("db.a").into_iter().collect();
    /// assert_eq!(dependents, ["http.b", "job.c"]);
    /// assert!(graph.dependents_of("nope").is_empty());
    /// ```
    pub fn dependents_of(&self, asset: &str) -> BTreeSet<String> {
        let mut readers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for candidate in self.assets.values() {
            for read in &candidate.reads {
                readers
                    .entry(read.as_str())
                    .or_default()
                    .push(candidate.name.as_str());
            }
        }
        let mut closure = BTreeSet::new();
        if !self.assets.contains_key(asset) {
            return closure;
        }
        let mut queue = VecDeque::from([asset]);
        while let Some(current) = queue.pop_front() {
            for &reader in readers.get(current).into_iter().flatten() {
                if reader != asset && closure.insert(reader.to_string()) {
                    queue.push_back(reader);
                }
            }
        }
        closure
    }

    /// The union of concerns on `assets`, heaviest first. Unknown names are ignored.
    ///
    /// ```
    /// use harness::assets::AssetGraph;
    ///
    /// let graph = AssetGraph::parse(r#"
    /// [concern.auth]
    /// weight = 9
    /// [concern.pii]
    /// weight = 8
    /// [asset."db.users"]
    /// kind = "table"
    /// concerns = ["pii", "auth"]
    /// "#).unwrap();
    /// let weighted = graph.concerns_of(["db.users", "db.users", "db.missing"]);
    /// let names: Vec<_> = weighted.iter().map(|c| (c.name.as_str(), c.weight)).collect();
    /// assert_eq!(names, [("auth", 9), ("pii", 8)]);
    /// ```
    pub fn concerns_of<I, S>(&self, assets: I) -> Vec<WeightedConcern>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let names: BTreeSet<&str> = assets
            .into_iter()
            .filter_map(|name| self.assets.get_key_value(name.as_ref()))
            .flat_map(|(_, asset)| asset.concerns.iter().map(String::as_str))
            .collect();
        let mut weighted: Vec<WeightedConcern> = names
            .into_iter()
            .filter_map(|name| self.concerns.get(name))
            .map(|concern| WeightedConcern {
                name: concern.name.clone(),
                weight: concern.weight,
            })
            .collect();
        weighted.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.name.cmp(&b.name)));
        weighted
    }

    /// Assets whose owner globs or declared code sites cover `path`.
    ///
    /// `path` is relative to the repository root; `./` prefixes and backslash
    /// separators are normalised.
    ///
    /// ```
    /// use harness::assets::AssetGraph;
    ///
    /// let graph = AssetGraph::parse(r#"
    /// [asset."db.users"]
    /// kind = "table"
    /// owners = ["api/src/auth/**"]
    /// "#).unwrap();
    /// assert_eq!(graph.owners_matching("./api/src/auth/login.rs").len(), 1);
    /// assert!(graph.owners_matching("api/src/orders/mod.rs").is_empty());
    /// ```
    pub fn owners_matching(&self, path: impl AsRef<Path>) -> Vec<&Asset> {
        let path = normalize(path.as_ref());
        self.assets
            .values()
            .filter(|asset| self.owns(asset, &path))
            .collect()
    }

    fn owns(&self, asset: &Asset, path: &str) -> bool {
        let by_glob = asset
            .owners
            .iter()
            .filter_map(|owner| Pattern::new(owner).ok())
            .any(|pattern| pattern.matches_with(path, MATCH_OPTIONS));
        let by_site = self
            .sites
            .get(&asset.name)
            .into_iter()
            .flatten()
            .any(|site| normalize_str(&site.file) == path);
        by_glob || by_site
    }

    /// Merge in-code `touches!` declarations; an undeclared asset is an error.
    ///
    /// Nothing is applied when any declaration conflicts. Repeating a
    /// declaration is a no-op.
    ///
    /// ```
    /// use harness::assets::{AssetError, AssetGraph};
    /// use nanna_effects::Touch;
    ///
    /// let mut graph = AssetGraph::parse("[asset.\"db.orders\"]\nkind = \"table\"\n").unwrap();
    /// graph.merge_declarations([Touch::new("db.orders", "api::orders", "api/src/orders.rs", 3)]).unwrap();
    /// assert_eq!(graph.owners_matching("api/src/orders.rs")[0].name, "db.orders");
    ///
    /// let err = graph.merge_declarations([Touch::new("db.ghost", "m", "f.rs", 1)]).unwrap_err();
    /// assert!(matches!(err, AssetError::UndeclaredAsset { .. }));
    /// ```
    pub fn merge_declarations(
        &mut self,
        touches: impl IntoIterator<Item = Touch>,
    ) -> Result<(), AssetError> {
        let touches: Vec<Touch> = touches.into_iter().collect();
        for touch in &touches {
            if !self.assets.contains_key(touch.asset) {
                return Err(AssetError::UndeclaredAsset {
                    site: format!("{}:{}", touch.file, touch.line),
                    asset: touch.asset.to_string(),
                });
            }
        }
        for touch in touches {
            self.sites
                .entry(touch.asset.to_string())
                .or_default()
                .insert(CodeSite {
                    module_path: touch.module_path.to_string(),
                    file: touch.file.to_string(),
                    line: touch.line,
                });
        }
        Ok(())
    }

    /// Non-fatal findings: assets read by nothing and owned by nothing, and
    /// owner globs that match no file under `repo_root` (skipped when `None`).
    pub fn warnings(&self, repo_root: Option<&Path>) -> Vec<Warning> {
        let read: BTreeSet<&str> = self
            .assets
            .values()
            .flat_map(|asset| asset.reads.iter().map(String::as_str))
            .collect();
        let files: Vec<String> = repo_root
            .map(|root| {
                let mut found = Vec::new();
                super::derive::collect_files(root, root, &mut found);
                found.iter().map(|path| normalize(path)).collect()
            })
            .unwrap_or_default();
        let mut warnings = Vec::new();
        for asset in self.assets.values() {
            if asset.concerns.is_empty() {
                warnings.push(Warning::NoConcerns {
                    file: self.file.clone(),
                    asset: asset.name.clone(),
                });
            }
            let owned = !asset.owners.is_empty() || self.sites.contains_key(&asset.name);
            if !owned && !read.contains(asset.name.as_str()) {
                warnings.push(Warning::Orphan {
                    file: self.file.clone(),
                    asset: asset.name.clone(),
                });
            }
            if repo_root.is_some() {
                for pattern in &asset.owners {
                    if !matches_any_file(&files, pattern) {
                        warnings.push(Warning::OwnerMatchesNoFiles {
                            file: self.file.clone(),
                            asset: asset.name.clone(),
                            pattern: pattern.clone(),
                        });
                    }
                }
            }
        }
        warnings
    }
}

fn matches_any_file(files: &[String], pattern: &str) -> bool {
    Pattern::new(pattern)
        .map(|pattern| {
            files
                .iter()
                .any(|file| pattern.matches_with(file, MATCH_OPTIONS))
        })
        .unwrap_or(false)
}

fn normalize(path: &Path) -> String {
    normalize_str(&path.to_string_lossy())
}

fn normalize_str(path: &str) -> String {
    let replaced = path.replace('\\', "/");
    replaced.trim_start_matches("./").to_string()
}

fn escapes_repo(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    pattern.starts_with(['/', '\\'])
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        || pattern.split(['/', '\\']).any(|part| part == "..")
}

fn duplicate_asset_name(src: &str, error: &toml::de::Error) -> Option<String> {
    let start = error.span()?.start;
    let line_start = src.get(..start)?.rfind('\n').map_or(0, |index| index + 1);
    let header = src.get(line_start..)?.lines().next()?.trim();
    let key = header.strip_prefix("[asset.")?.strip_suffix(']')?;
    let headers = src.lines().filter(|line| line.trim() == header).count();
    (headers >= 2).then(|| key.trim_matches('"').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const FULL: &str = r#"
[concern.revenue]
weight = 10
description = "Anything on the paid path."

[concern.auth]
weight = 9

[concern.pii]
weight = 8

[concern.availability]
weight = 6

[asset."db.users"]
kind = "table"
concerns = ["auth", "pii"]
owners = ["api/src/auth/**"]

[asset."db.orders"]
kind = "table"
concerns = ["revenue"]
owners = ["api/src/orders/**"]

[asset."http.POST /api/v1/checkout"]
kind = "endpoint"
concerns = ["revenue", "availability"]
reads = ["db.orders", "db.users"]

[asset."job.invoice_nightly"]
kind = "job"
concerns = ["revenue"]
reads = ["db.orders"]

[asset."ext.payment_provider"]
kind = "external"
concerns = ["revenue"]
"#;

    fn invalid_field(src: &str) -> String {
        match AssetGraph::parse(src).unwrap_err() {
            AssetError::InvalidField { field, file, .. } => {
                assert_eq!(file, Path::new("effects.toml"));
                field
            }
            other => panic!("expected InvalidField, got {other}"),
        }
    }

    #[test]
    fn issue_example_loads() {
        let graph = AssetGraph::parse(FULL).unwrap();
        assert_eq!(graph.assets().count(), 5);
        assert_eq!(graph.concerns().count(), 4);
        assert_eq!(graph.asset("db.users").unwrap().kind, AssetKind::Table);
        assert_eq!(
            graph.concern("revenue").unwrap().description.as_deref(),
            Some("Anything on the paid path.")
        );
    }

    #[test]
    fn empty_manifest_is_an_empty_graph() {
        let graph = AssetGraph::parse("").unwrap();
        assert_eq!(graph.assets().count(), 0);
        assert!(graph.warnings(None).is_empty());
    }

    #[test]
    fn dependents_are_transitive() {
        let graph = AssetGraph::parse(FULL).unwrap();
        let dependents: Vec<_> = graph.dependents_of("db.orders").into_iter().collect();
        assert_eq!(
            dependents,
            ["http.POST /api/v1/checkout", "job.invoice_nightly"]
        );
        assert!(graph.dependents_of("ext.payment_provider").is_empty());
    }

    #[test]
    fn dependents_chain_through_intermediates() {
        let graph = AssetGraph::parse(
            "[asset.a]\nkind=\"table\"\n[asset.b]\nkind=\"job\"\nreads=[\"a\"]\n[asset.c]\nkind=\"endpoint\"\nreads=[\"b\"]\n",
        )
        .unwrap();
        let dependents: Vec<_> = graph.dependents_of("a").into_iter().collect();
        assert_eq!(dependents, ["b", "c"]);
    }

    #[test]
    fn self_read_and_cycles_terminate_and_exclude_the_start() {
        let graph = AssetGraph::parse(
            "[asset.a]\nkind=\"job\"\nreads=[\"a\", \"b\"]\n[asset.b]\nkind=\"job\"\nreads=[\"a\"]\n",
        )
        .unwrap();
        let dependents: Vec<_> = graph.dependents_of("a").into_iter().collect();
        assert_eq!(dependents, ["b"]);
    }

    #[test]
    fn concerns_of_unions_and_orders_by_weight() {
        let graph = AssetGraph::parse(FULL).unwrap();
        let weighted = graph.concerns_of(["db.users", "http.POST /api/v1/checkout"]);
        let names: Vec<_> = weighted.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["revenue", "auth", "pii", "availability"]);
        assert!(graph.concerns_of(Vec::<String>::new()).is_empty());
    }

    #[test]
    fn owners_matching_respects_glob_depth() {
        let graph = AssetGraph::parse(FULL).unwrap();
        let hit = graph.owners_matching("api/src/auth/deep/er/token.rs");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].name, "db.users");
        assert!(graph.owners_matching("api/src/authx/token.rs").is_empty());
        assert!(graph.owners_matching("api\\src\\orders\\mod.rs").len() == 1);
    }

    #[test]
    fn single_star_does_not_cross_directories() {
        let graph =
            AssetGraph::parse("[asset.a]\nkind=\"table\"\nowners=[\"src/*.rs\"]\n").unwrap();
        assert_eq!(graph.owners_matching("src/a.rs").len(), 1);
        assert!(graph.owners_matching("src/sub/a.rs").is_empty());
    }

    #[test]
    fn unknown_concern_names_field_and_file() {
        let field = invalid_field("[asset.a]\nkind=\"table\"\nconcerns=[\"ghost\"]\n");
        assert_eq!(field, "asset.\"a\".concerns");
    }

    #[test]
    fn unknown_read_names_field() {
        let field = invalid_field("[asset.a]\nkind=\"job\"\nreads=[\"ghost\"]\n");
        assert_eq!(field, "asset.\"a\".reads");
    }

    #[test]
    fn unknown_kind_names_field() {
        let field = invalid_field("[asset.a]\nkind=\"queue\"\n");
        assert_eq!(field, "asset.\"a\".kind");
    }

    #[test]
    fn invalid_glob_names_field() {
        let field = invalid_field("[asset.a]\nkind=\"table\"\nowners=[\"src/[\"]\n");
        assert_eq!(field, "asset.\"a\".owners");
    }

    #[test]
    fn repeated_list_entries_are_rejected() {
        let field = invalid_field("[asset.a]\nkind=\"table\"\nowners=[\"x\", \"x\"]\n");
        assert_eq!(field, "asset.\"a\".owners");
    }

    #[test]
    fn absolute_and_parent_owner_globs_are_rejected() {
        for pattern in [
            "/etc/**", "../x/**", "a/../b", "a\\..\\b", "C:\\x", "**/../y",
        ] {
            let mut graph = AssetGraph::new();
            let mut asset = Asset::new("db.a", AssetKind::Table);
            asset.owners = vec![pattern.to_string()];
            match graph.insert_asset(asset).unwrap_err() {
                AssetError::InvalidField { field, .. } => {
                    assert_eq!(field, "asset.\"db.a\".owners", "{pattern}")
                }
                other => panic!("expected InvalidField for {pattern}, got {other}"),
            }
        }
        let mut graph = AssetGraph::new();
        let mut asset = Asset::new("db.a", AssetKind::Table);
        asset.owners = vec!["api/**".into(), "..hidden/*.rs".into()];
        graph.insert_asset(asset).unwrap();
    }

    #[test]
    fn manifest_with_escaping_owner_is_rejected() {
        assert_eq!(
            invalid_field("[asset.a]\nkind=\"job\"\nowners=[\"/abs/**\"]\n"),
            "asset.\"a\".owners"
        );
    }

    #[test]
    fn code_site_displays_file_line_and_module() {
        let site = CodeSite {
            module_path: "m".into(),
            file: "f.rs".into(),
            line: 3,
        };
        assert_eq!(site.to_string(), "f.rs:3 (m)");
    }

    #[test]
    fn default_graph_is_empty_and_named_after_the_manifest() {
        let graph = AssetGraph::default();
        assert_eq!(graph, AssetGraph::new());
        assert_eq!(graph.file(), Path::new(MANIFEST_FILE_NAME));
        assert_eq!(graph.assets().count(), 0);
    }

    #[test]
    fn blank_or_padded_concern_names_are_rejected() {
        for name in ["", "  ", " pii"] {
            let mut graph = AssetGraph::new();
            let concern = Concern {
                name: name.into(),
                weight: 1,
                description: None,
            };
            assert!(matches!(
                graph.insert_concern(concern),
                Err(AssetError::InvalidField { .. })
            ));
        }
    }

    #[test]
    fn inserting_an_asset_twice_names_the_graph_file() {
        let mut graph = AssetGraph::new();
        graph
            .insert_asset(Asset::new("db.a", AssetKind::Table))
            .unwrap();
        match graph
            .insert_asset(Asset::new("db.a", AssetKind::Table))
            .unwrap_err()
        {
            AssetError::DuplicateAsset { file, asset } => {
                assert_eq!(asset, "db.a");
                assert_eq!(file, Path::new(MANIFEST_FILE_NAME));
            }
            other => panic!("expected DuplicateAsset, got {other}"),
        }
    }

    #[test]
    fn dependents_of_an_unknown_asset_is_empty() {
        let graph = AssetGraph::parse("[asset.a]\nkind=\"job\"\n").unwrap();
        assert!(graph.dependents_of("ghost").is_empty());
    }

    #[test]
    fn duplicate_detection_ignores_message_wording_and_other_tables() {
        let quoted = "[asset.\"a.b\"]\nkind=\"job\"\n[asset.\"a.b\"]\nkind=\"job\"\n";
        assert!(matches!(
            AssetGraph::parse(quoted).unwrap_err(),
            AssetError::DuplicateAsset { ref asset, .. } if asset == "a.b"
        ));
        let concern = "[concern.x]\nweight=1\n[concern.x]\nweight=2\n";
        assert!(matches!(
            AssetGraph::parse(concern).unwrap_err(),
            AssetError::Parse { .. }
        ));
    }

    #[test]
    fn assets_without_concerns_are_warned_about() {
        let graph = AssetGraph::parse(
            "[concern.c]\nweight=1\n[asset.a]\nkind=\"job\"\nowners=[\"x/**\"]\nconcerns=[\"c\"]\n[asset.b]\nkind=\"job\"\nowners=[\"y/**\"]\n",
        )
        .unwrap();
        let warnings = graph.warnings(None);
        assert_eq!(
            warnings,
            [Warning::NoConcerns {
                file: PathBuf::from(MANIFEST_FILE_NAME),
                asset: "b".into()
            }]
        );
        assert!(warnings[0]
            .to_string()
            .contains("asset `b` has no concerns"));
    }

    #[test]
    fn duplicate_concern_is_rejected() {
        let mut graph = AssetGraph::new();
        let concern = Concern {
            name: "pii".into(),
            weight: 1,
            description: None,
        };
        graph.insert_concern(concern.clone()).unwrap();
        assert!(matches!(
            graph.insert_concern(concern),
            Err(AssetError::InvalidField { .. })
        ));
    }

    #[test]
    fn duplicate_asset_tables_are_an_error_naming_the_asset() {
        let err = AssetGraph::parse_named(
            "[asset.\"db.a\"]\nkind=\"table\"\n[asset.\"db.a\"]\nkind=\"table\"\n",
            Path::new("repo/.nanna/effects.toml"),
        )
        .unwrap_err();
        match err {
            AssetError::DuplicateAsset { file, asset } => {
                assert_eq!(asset, "db.a");
                assert_eq!(file, Path::new("repo/.nanna/effects.toml"));
            }
            other => panic!("expected DuplicateAsset, got {other}"),
        }
    }

    #[test]
    fn syntax_errors_and_unknown_keys_name_the_file() {
        let err = AssetGraph::parse_named("[asset.a\n", Path::new("x/effects.toml")).unwrap_err();
        assert!(err.to_string().contains("x/effects.toml"));
        let err = AssetGraph::parse("[asset.a]\nkind=\"table\"\nwat=1\n").unwrap_err();
        assert!(matches!(err, AssetError::Parse { .. }));
        let err = AssetGraph::parse("[asset.a]\n").unwrap_err();
        assert!(err.to_string().contains("kind"));
    }

    #[test]
    fn blank_names_are_rejected() {
        assert!(AssetGraph::parse("[asset.\"\"]\nkind=\"table\"\n").is_err());
        assert!(AssetGraph::parse("[asset.\" a\"]\nkind=\"table\"\n").is_err());
    }

    #[test]
    fn missing_file_is_an_io_error_naming_the_path() {
        let err = AssetGraph::load(Path::new("/var/tmp/definitely/not/effects.toml")).unwrap_err();
        assert!(matches!(err, AssetError::Io { .. }));
        assert!(err.to_string().contains("definitely/not/effects.toml"));
    }

    #[test]
    fn orphans_are_warned_only_when_unread_and_unowned() {
        let graph = AssetGraph::parse(FULL).unwrap();
        let orphans: Vec<_> = graph
            .warnings(None)
            .into_iter()
            .filter_map(|w| match w {
                Warning::Orphan { asset, .. } => Some(asset),
                _ => None,
            })
            .collect();
        assert_eq!(
            orphans,
            [
                "ext.payment_provider",
                "http.POST /api/v1/checkout",
                "job.invoice_nightly"
            ]
        );
    }

    #[test]
    fn owner_globs_matching_nothing_are_warned() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("api/src/auth")).unwrap();
        std::fs::write(repo.path().join("api/src/auth/mod.rs"), "").unwrap();
        let graph = AssetGraph::parse(FULL).unwrap();
        let warnings = graph.warnings(Some(repo.path()));
        let unmatched: Vec<_> = warnings
            .iter()
            .filter_map(|w| match w {
                Warning::OwnerMatchesNoFiles { pattern, .. } => Some(pattern.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(unmatched, ["api/src/orders/**"]);
        let text = warnings.iter().map(ToString::to_string).collect::<String>();
        assert!(text.contains("asset.\"db.orders\".owners"));
        assert!(text.contains("effects.toml"));
    }

    #[test]
    fn declarations_merge_and_conflicts_are_errors() {
        let mut graph = AssetGraph::parse(FULL).unwrap();
        let touch = Touch::new("db.orders", "api::orders", "api/src/orders.rs", 3);
        graph.merge_declarations([touch, touch]).unwrap();
        assert_eq!(graph.sites_of("db.orders").count(), 1);
        assert!(graph
            .owners_matching("api/src/orders.rs")
            .iter()
            .any(|a| a.name == "db.orders"));

        let before = graph.clone();
        let err = graph
            .merge_declarations([
                Touch::new("db.users", "m", "a.rs", 1),
                Touch::new("db.ghost", "m", "b.rs", 9),
            ])
            .unwrap_err();
        assert!(matches!(&err, AssetError::UndeclaredAsset { asset, .. } if asset == "db.ghost"));
        assert!(err.to_string().contains("b.rs:9"));
        assert_eq!(graph, before);
    }

    #[test]
    fn code_declaration_clears_the_orphan_warning() {
        let mut graph = AssetGraph::parse(
            "[concern.c]\nweight=1\n[asset.a]\nkind=\"table\"\nconcerns=[\"c\"]\n",
        )
        .unwrap();
        assert_eq!(graph.warnings(None).len(), 1);
        graph
            .merge_declarations([Touch::new("a", "m", "f.rs", 1)])
            .unwrap();
        assert!(graph.warnings(None).is_empty());
    }

    fn build(edges: &BTreeSet<(usize, usize)>, nodes: usize) -> AssetGraph {
        let mut graph = AssetGraph::new();
        for i in 0..nodes {
            let mut asset = Asset::new(format!("n{i}"), AssetKind::Job);
            asset.reads = edges
                .iter()
                .filter(|(from, _)| *from == i)
                .map(|(_, to)| format!("n{to}"))
                .collect();
            graph.insert_asset(asset).unwrap();
        }
        graph.check_references().unwrap();
        graph
    }

    fn edge_set(nodes: usize) -> impl Strategy<Value = BTreeSet<(usize, usize)>> {
        proptest::collection::btree_set((0..nodes, 0..nodes), 0..24)
    }

    proptest! {
        #[test]
        fn dependents_terminate_and_are_monotonic_under_added_edges(
            base in edge_set(8),
            extra in edge_set(8),
        ) {
            let more: BTreeSet<_> = base.union(&extra).copied().collect();
            let small = build(&base, 8);
            let large = build(&more, 8);
            for i in 0..8 {
                let name = format!("n{i}");
                let before = small.dependents_of(&name);
                let after = large.dependents_of(&name);
                prop_assert!(before.is_subset(&after));
                prop_assert!(!after.contains(&name));
            }
        }

        #[test]
        fn dependents_are_transitively_closed(edges in edge_set(8)) {
            let graph = build(&edges, 8);
            for i in 0..8 {
                let name = format!("n{i}");
                let closure = graph.dependents_of(&name);
                for reader in &closure {
                    for outer in graph.dependents_of(reader) {
                        prop_assert!(outer == name || closure.contains(&outer));
                    }
                }
            }
        }

        #[test]
        fn direct_readers_are_always_dependents(edges in edge_set(6)) {
            let graph = build(&edges, 6);
            for (from, to) in &edges {
                if from != to {
                    let readers = graph.dependents_of(&format!("n{}", to));
                    let reader = format!("n{}", from);
                    prop_assert!(readers.contains(&reader));
                }
            }
        }

        #[test]
        fn concerns_of_is_monotonic_in_the_asset_set(picks in proptest::collection::vec(0usize..4, 0..8)) {
            let graph = AssetGraph::parse(
                "[concern.a]\nweight=1\n[concern.b]\nweight=2\n[asset.x0]\nkind=\"table\"\nconcerns=[\"a\"]\n[asset.x1]\nkind=\"table\"\nconcerns=[\"b\"]\n[asset.x2]\nkind=\"table\"\nconcerns=[\"a\",\"b\"]\n[asset.x3]\nkind=\"table\"\n",
            ).unwrap();
            let names: Vec<String> = picks.iter().map(|i| format!("x{i}")).collect();
            let all = graph.concerns_of(&names);
            for end in 0..=names.len() {
                let prefix = graph.concerns_of(&names[..end]);
                prop_assert!(prefix.iter().all(|c| all.contains(c)));
            }
        }
    }
}
