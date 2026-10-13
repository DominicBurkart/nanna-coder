use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// What sort of state an [`Asset`] is.
///
/// ```
/// use harness::assets::AssetKind;
///
/// assert_eq!("table".parse::<AssetKind>(), Ok(AssetKind::Table));
/// assert_eq!(AssetKind::Endpoint.as_str(), "endpoint");
/// assert!("queue".parse::<AssetKind>().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Table,
    Endpoint,
    Job,
    External,
}

impl AssetKind {
    pub const ALL: [AssetKind; 4] = [
        AssetKind::Table,
        AssetKind::Endpoint,
        AssetKind::Job,
        AssetKind::External,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            AssetKind::Table => "table",
            AssetKind::Endpoint => "endpoint",
            AssetKind::Job => "job",
            AssetKind::External => "external",
        }
    }
}

impl fmt::Display for AssetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown asset kind `{0}` (expected one of: table, endpoint, job, external)")]
pub struct UnknownAssetKind(pub String);

impl FromStr for AssetKind {
    type Err = UnknownAssetKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AssetKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == s)
            .ok_or_else(|| UnknownAssetKind(s.to_string()))
    }
}

/// A business concern with a weight, for example revenue or authentication.
///
/// ```
/// use harness::assets::AssetGraph;
///
/// let graph = AssetGraph::parse("[concern.pii]\nweight = 8\ndescription = \"Personal data\"\n").unwrap();
/// let pii = graph.concerns().next().unwrap();
/// assert_eq!((pii.name.as_str(), pii.weight), ("pii", 8));
/// assert_eq!(pii.description.as_deref(), Some("Personal data"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Concern {
    pub name: String,
    pub weight: u32,
    pub description: Option<String>,
}

/// A concern together with its weight, as returned by
/// [`AssetGraph::concerns_of`](super::AssetGraph::concerns_of).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeightedConcern {
    pub name: String,
    pub weight: u32,
}

/// One piece of application state, an endpoint, a job or an external service.
///
/// A table:
///
/// ```
/// use harness::assets::{AssetGraph, AssetKind};
///
/// let graph = AssetGraph::parse(r#"
/// [concern.auth]
/// weight = 9
///
/// [asset."db.users"]
/// kind = "table"
/// concerns = ["auth"]
/// owners = ["api/src/auth/**"]
/// "#).unwrap();
/// let users = graph.asset("db.users").unwrap();
/// assert_eq!(users.kind, AssetKind::Table);
/// assert_eq!(users.owners, ["api/src/auth/**"]);
/// ```
///
/// An endpoint that reads a table:
///
/// ```
/// use harness::assets::{AssetGraph, AssetKind};
///
/// let graph = AssetGraph::parse(r#"
/// [asset."db.orders"]
/// kind = "table"
///
/// [asset."http.POST /api/v1/checkout"]
/// kind = "endpoint"
/// reads = ["db.orders"]
/// "#).unwrap();
/// let checkout = graph.asset("http.POST /api/v1/checkout").unwrap();
/// assert_eq!(checkout.kind, AssetKind::Endpoint);
/// assert_eq!(checkout.reads, ["db.orders"]);
/// ```
///
/// A scheduled job:
///
/// ```
/// use harness::assets::{AssetGraph, AssetKind};
///
/// let graph = AssetGraph::parse("[asset.\"job.invoice_nightly\"]\nkind = \"job\"\n").unwrap();
/// assert_eq!(graph.asset("job.invoice_nightly").unwrap().kind, AssetKind::Job);
/// ```
///
/// An external service:
///
/// ```
/// use harness::assets::{AssetGraph, AssetKind};
///
/// let graph = AssetGraph::parse("[asset.\"ext.payment_provider\"]\nkind = \"external\"\n").unwrap();
/// assert_eq!(graph.asset("ext.payment_provider").unwrap().kind, AssetKind::External);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub kind: AssetKind,
    pub concerns: Vec<String>,
    pub owners: Vec<String>,
    pub reads: Vec<String>,
}

impl Asset {
    pub fn new(name: impl Into<String>, kind: AssetKind) -> Self {
        Self {
            name: name.into(),
            kind,
            concerns: Vec::new(),
            owners: Vec::new(),
            reads: Vec::new(),
        }
    }
}
