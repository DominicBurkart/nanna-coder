use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

pub type AssetId = String;

pub type Weight = u32;

/// How a piece of code or an action reaches an asset.
///
/// Only [`Access::Write`] puts an asset in [`BlastRadius::touched`]; a read is
/// kept as evidence but does not modify state.
///
/// ```
/// use harness::impact::Access;
///
/// assert!(Access::Write > Access::Read);
/// assert_eq!(Access::Write.to_string(), "write");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    Write,
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Access::Read => "read",
            Access::Write => "write",
        })
    }
}

/// Why an asset appears in a [`BlastRadius`].
///
/// ```
/// use harness::impact::{Access, Evidence};
///
/// let evidence = Evidence::new("sql", "migrations/1.sql", "db.orders", Access::Write, "INSERT INTO orders");
/// assert_eq!(evidence.to_string(), "[sql] migrations/1.sql write db.orders: INSERT INTO orders");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Evidence {
    pub extractor: String,
    pub subject: String,
    pub asset: AssetId,
    pub access: Access,
    pub detail: String,
}

impl Evidence {
    pub fn new(
        extractor: impl Into<String>,
        subject: impl Into<String>,
        asset: impl Into<AssetId>,
        access: Access,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            extractor: extractor.into(),
            subject: subject.into(),
            asset: asset.into(),
            access,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Evidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} {} {}: {}",
            self.extractor, self.subject, self.access, self.asset, self.detail
        )
    }
}

/// The assets a change or action reaches and what is at stake.
///
/// `touched` and `downstream` are sorted and disjoint; `concerns` maps each
/// concern on either set to its weight; `score` is computed as documented on
/// [`BlastRadius::compute`].
///
/// ```
/// use harness::impact::BlastRadius;
///
/// let empty = BlastRadius::default();
/// assert_eq!(empty.score, 0);
/// assert!(empty.is_empty());
/// assert_eq!(serde_json::to_value(&empty).unwrap()["score"], 0);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlastRadius {
    pub touched: Vec<AssetId>,
    pub downstream: Vec<AssetId>,
    pub concerns: BTreeMap<String, Weight>,
    pub score: u32,
    pub evidence: Vec<Evidence>,
}

impl BlastRadius {
    pub fn is_empty(&self) -> bool {
        self.touched.is_empty() && self.downstream.is_empty()
    }

    /// Human-readable multi-line rendering.
    ///
    /// ```
    /// use harness::impact::BlastRadius;
    ///
    /// let text = BlastRadius::default().render_text();
    /// assert!(text.starts_with("score: 0\n"));
    /// ```
    pub fn render_text(&self) -> String {
        let mut out = format!("score: {}\n", self.score);
        let list = |items: &[String]| {
            if items.is_empty() {
                "(none)".to_string()
            } else {
                items.join(", ")
            }
        };
        out.push_str(&format!("touched: {}\n", list(&self.touched)));
        out.push_str(&format!("downstream: {}\n", list(&self.downstream)));
        let concerns: Vec<String> = self
            .concerns
            .iter()
            .map(|(name, weight)| format!("{name} ({weight})"))
            .collect();
        out.push_str(&format!("concerns: {}\n", list(&concerns)));
        if !self.evidence.is_empty() {
            out.push_str("evidence:\n");
            for item in &self.evidence {
                out.push_str(&format!("  {item}\n"));
            }
        }
        out
    }
}
