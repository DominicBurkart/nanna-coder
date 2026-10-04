use super::{DeployError, DEPLOY_DIR, DEPLOY_FILE_NAME};
use crate::windows;
use chrono::Duration;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Kind of deployment target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// A container image pushed to a registry and run by a serverless
    /// container platform.
    ContainerRegistryServerless,
}

impl TargetKind {
    /// All supported kinds.
    pub const ALL: [TargetKind; 1] = [TargetKind::ContainerRegistryServerless];

    /// The `target.kind` value naming this kind.
    pub const fn name(self) -> &'static str {
        match self {
            TargetKind::ContainerRegistryServerless => "container-registry+serverless",
        }
    }
}

impl FromStr for TargetKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        TargetKind::ALL
            .into_iter()
            .find(|k| k.name() == s)
            .ok_or_else(|| expected_one_of(s, TargetKind::ALL.iter().map(|k| k.name())))
    }
}

/// Where the deployable is published and which environments it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Kind of target.
    pub kind: TargetKind,
    /// Registry (host and namespace) the image is pushed to.
    pub registry: String,
    /// Image name inside the registry.
    pub image: String,
    /// Environments the deployable can be rolled out to, in declaration order.
    pub environments: Vec<String>,
}

impl Target {
    /// Fully qualified image reference, `<registry>/<image>`.
    pub fn image_ref(&self) -> String {
        format!("{}/{}", self.registry, self.image)
    }
}

/// Static risk class of the system served by the deployable.
///
/// Classes are ordered from least to most consequential; a higher class
/// demands a more cautious rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RiskClass {
    /// Nothing depends on the system yet.
    Unused,
    /// Serves internal users only.
    Internal,
    /// Serves external traffic at the edge of the system.
    Edge,
    /// Serves core production traffic.
    Core,
}

impl RiskClass {
    /// All classes, least consequential first.
    pub const ALL: [RiskClass; 4] = [
        RiskClass::Unused,
        RiskClass::Internal,
        RiskClass::Edge,
        RiskClass::Core,
    ];

    /// The `risk.class` value naming this class.
    pub const fn name(self) -> &'static str {
        match self {
            RiskClass::Unused => "unused",
            RiskClass::Internal => "internal",
            RiskClass::Edge => "edge",
            RiskClass::Core => "core",
        }
    }
}

impl FromStr for RiskClass {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RiskClass::ALL
            .into_iter()
            .find(|c| c.name() == s)
            .ok_or_else(|| expected_one_of(s, RiskClass::ALL.iter().map(|c| c.name())))
    }
}

impl fmt::Display for RiskClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Lower bounds on the blast-radius score for each derived class.
///
/// A score below the lowest declared bound is [`RiskClass::Unused`]; otherwise
/// the class is the highest one whose bound the score reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskThresholds {
    /// Scores at or above this are at least [`RiskClass::Internal`].
    pub internal: Option<u32>,
    /// Scores at or above this are at least [`RiskClass::Edge`].
    pub edge: Option<u32>,
    /// Scores at or above this are [`RiskClass::Core`].
    pub core: Option<u32>,
}

impl RiskThresholds {
    fn bounds(&self) -> [(RiskClass, Option<u32>); 3] {
        [
            (RiskClass::Internal, self.internal),
            (RiskClass::Edge, self.edge),
            (RiskClass::Core, self.core),
        ]
    }

    /// The class a blast-radius score maps to.
    pub fn classify(&self, score: u32) -> RiskClass {
        self.bounds()
            .into_iter()
            .filter(|(_, bound)| bound.is_some_and(|b| score >= b))
            .map(|(class, _)| class)
            .max()
            .unwrap_or(RiskClass::Unused)
    }

    /// The most consequential class these thresholds can produce.
    pub fn highest(&self) -> RiskClass {
        self.classify(u32::MAX)
    }

    fn validate(&self, file: &Path) -> Result<(), DeployError> {
        let declared: Vec<u32> = self
            .bounds()
            .into_iter()
            .filter_map(|(_, bound)| bound)
            .collect();
        if declared.is_empty() {
            return Err(invalid(
                file,
                "risk.thresholds",
                "at least one of internal, edge or core is required",
            ));
        }
        if !declared.windows(2).all(|w| w[0] < w[1]) {
            return Err(invalid(
                file,
                "risk.thresholds",
                "bounds must be strictly increasing from internal to core",
            ));
        }
        Ok(())
    }
}

/// How the risk class of a deployment is determined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskSpec {
    /// The class is fixed in the template.
    Static(RiskClass),
    /// The class is derived from the blast-radius score of each change.
    Derived(RiskThresholds),
}

/// Rollout strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    /// Route all traffic to the new version at once.
    Instant,
    /// Deploy to an inactive slot, swap, retain the previous slot, retire it.
    BlueGreen,
    /// Shift traffic through the configured steps.
    Gradual,
    /// Mirror traffic to the new version first, then shift gradually.
    ShadowThenGradual,
}

impl Strategy {
    /// All strategies.
    pub const ALL: [Strategy; 4] = [
        Strategy::Instant,
        Strategy::BlueGreen,
        Strategy::Gradual,
        Strategy::ShadowThenGradual,
    ];

    /// The `rollout.strategy` value naming this strategy.
    pub const fn name(self) -> &'static str {
        match self {
            Strategy::Instant => "instant",
            Strategy::BlueGreen => "blue-green",
            Strategy::Gradual => "gradual",
            Strategy::ShadowThenGradual => "shadow-then-gradual",
        }
    }
}

impl FromStr for Strategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Strategy::ALL
            .into_iter()
            .find(|v| v.name() == s)
            .ok_or_else(|| expected_one_of(s, Strategy::ALL.iter().map(|v| v.name())))
    }
}

impl fmt::Display for Strategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The `[rollout]` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rollout {
    /// Rollout strategy.
    pub strategy: Strategy,
    /// Traffic percentages, strictly increasing and ending at 100.
    pub steps: Vec<u8>,
    /// Minimum time spent on each step before advancing.
    pub min_step_duration: Duration,
    /// Availability window (by name) that gates production steps.
    pub windows: Option<String>,
}

impl Rollout {
    /// Total minimum span of the rollout: `steps × min_step_duration`.
    pub fn span(&self) -> Duration {
        self.min_step_duration * i32::try_from(self.steps.len()).expect("at most 100 steps")
    }
}

/// The `[health]` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Health {
    /// HTTP paths polled during the bake.
    pub endpoints: Vec<String>,
    /// Error-rate ceiling in `0.0..=1.0`.
    pub error_rate_max: f64,
    /// p99 latency ceiling in milliseconds.
    pub latency_p99_max_ms: u32,
    /// Observation period after each step.
    pub bake_time: Duration,
}

/// What to do when a health gate is breached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnBreach {
    /// Restore the previous version at 100%.
    Rollback,
    /// Re-run the plan with a fixed image.
    RollForward,
    /// Hold the current traffic split and escalate to a human.
    HaltAndEscalate,
}

impl OnBreach {
    /// All breach responses.
    pub const ALL: [OnBreach; 3] = [
        OnBreach::Rollback,
        OnBreach::RollForward,
        OnBreach::HaltAndEscalate,
    ];

    /// The `rollback.on_breach` value naming this response.
    pub const fn name(self) -> &'static str {
        match self {
            OnBreach::Rollback => "rollback",
            OnBreach::RollForward => "roll-forward",
            OnBreach::HaltAndEscalate => "halt-and-escalate",
        }
    }
}

impl FromStr for OnBreach {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        OnBreach::ALL
            .into_iter()
            .find(|v| v.name() == s)
            .ok_or_else(|| expected_one_of(s, OnBreach::ALL.iter().map(|v| v.name())))
    }
}

/// The `[rollback]` section. Absent sections default to manual handling:
/// not automatic, `halt-and-escalate`, nothing retained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollback {
    /// Whether a health breach triggers `on_breach` without a human.
    pub automatic: bool,
    /// Response to a health breach.
    pub on_breach: OnBreach,
    /// How long the previous version is kept deployable after completion.
    pub retain_for: Duration,
}

/// Response attribute compared between the live and shadow versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowCompare {
    /// HTTP status code.
    Status,
    /// Response latency.
    Latency,
}

impl ShadowCompare {
    /// All comparable attributes.
    pub const ALL: [ShadowCompare; 2] = [ShadowCompare::Status, ShadowCompare::Latency];

    /// The `shadow.compare` value naming this attribute.
    pub const fn name(self) -> &'static str {
        match self {
            ShadowCompare::Status => "status",
            ShadowCompare::Latency => "latency",
        }
    }
}

impl FromStr for ShadowCompare {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ShadowCompare::ALL
            .into_iter()
            .find(|v| v.name() == s)
            .ok_or_else(|| expected_one_of(s, ShadowCompare::ALL.iter().map(|v| v.name())))
    }
}

/// Default `[shadow].max_divergence`: 5% of mirrored pairs may disagree.
pub const DEFAULT_MAX_DIVERGENCE: f64 = 0.05;

/// The `[shadow]` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shadow {
    /// Whether traffic is mirrored to the new version before it goes live.
    pub enabled: bool,
    /// Share of traffic mirrored, `1..=100` when enabled.
    pub mirror_percent: u8,
    /// Attributes compared between live and shadow responses.
    pub compare: Vec<ShadowCompare>,
    /// Share of compared pairs, `0.0..=1.0`, above which an attribute's
    /// divergence is a health breach; [`DEFAULT_MAX_DIVERGENCE`] when unset.
    pub max_divergence: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTemplate {
    target: RawTarget,
    risk: RawRisk,
    rollout: RawRollout,
    health: Option<RawHealth>,
    rollback: Option<RawRollback>,
    shadow: Option<RawShadow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTarget {
    kind: String,
    registry: String,
    image: String,
    environments: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRisk {
    class: String,
    thresholds: Option<RawThresholds>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawThresholds {
    internal: Option<i64>,
    edge: Option<i64>,
    core: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRollout {
    strategy: String,
    steps: Option<Vec<i64>>,
    min_step_duration: Option<String>,
    windows: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHealth {
    endpoints: Vec<String>,
    error_rate_max: f64,
    latency_p99_max_ms: i64,
    bake_time: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRollback {
    automatic: bool,
    on_breach: String,
    retain_for: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawShadow {
    enabled: bool,
    mirror_percent: i64,
    compare: Vec<String>,
    max_divergence: Option<f64>,
}

/// A loaded and validated `.nanna/deploy.toml`.
#[derive(Debug, Clone, PartialEq)]
pub struct DeployTemplate {
    file: PathBuf,
    /// The `[target]` section.
    pub target: Target,
    /// The `[risk]` section.
    pub risk: RiskSpec,
    /// The `[rollout]` section.
    pub rollout: Rollout,
    /// The `[health]` section, if present.
    pub health: Option<Health>,
    /// The `[rollback]` section, or its documented default.
    pub rollback: Rollback,
    /// The `[shadow]` section, if present.
    pub shadow: Option<Shadow>,
}

impl DeployTemplate {
    /// Parse template content, validating every field.
    ///
    /// ```
    /// use harness::deploy::{DeployError, DeployTemplate, RiskClass, RiskSpec};
    ///
    /// let template = DeployTemplate::parse(
    ///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"unused\"\n[rollout]\nstrategy = \"instant\"\n",
    /// )
    /// .unwrap();
    /// assert_eq!(template.risk, RiskSpec::Static(RiskClass::Unused));
    /// assert_eq!(template.rollout.steps, [100]);
    ///
    /// let err = DeployTemplate::parse(
    ///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"unused\"\n[rollout]\nstrategy = \"gradual\"\nsteps = [50, 90]\n",
    /// )
    /// .unwrap_err();
    /// assert!(matches!(err, DeployError::InvalidField { field: "rollout.steps", .. }));
    /// ```
    pub fn parse(toml_src: &str) -> Result<Self, DeployError> {
        Self::parse_named(toml_src, Path::new(DEPLOY_FILE_NAME))
    }

    /// Read and parse the template at `path`.
    pub fn load(path: &Path) -> Result<Self, DeployError> {
        let src = std::fs::read_to_string(path).map_err(|source| DeployError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse_named(&src, path)
    }

    /// Read and parse `<repo>/.nanna/deploy.toml`.
    pub fn load_from_repo(repo: &Path) -> Result<Self, DeployError> {
        Self::load(&Self::path_in(repo))
    }

    /// Where the template lives inside a repository.
    pub fn path_in(repo: &Path) -> PathBuf {
        repo.join(DEPLOY_DIR).join(DEPLOY_FILE_NAME)
    }

    /// The file this template was loaded from (`deploy.toml` when parsed from a string).
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// The most consequential class this template can be deployed under.
    ///
    /// For a static class that is the class itself; for a derived class it is
    /// the highest class the thresholds can produce. Validation uses it.
    pub fn highest_risk(&self) -> RiskClass {
        match &self.risk {
            RiskSpec::Static(class) => *class,
            RiskSpec::Derived(thresholds) => thresholds.highest(),
        }
    }

    /// The effective risk class for a change with the given blast-radius score.
    ///
    /// A static class ignores the score. A derived class requires one and
    /// fails with [`DeployError::ScoreRequired`] otherwise.
    ///
    /// ```
    /// use harness::deploy::{DeployError, DeployTemplate, RiskClass};
    ///
    /// let template = DeployTemplate::parse(
    ///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"staging\"]\n[risk]\nclass = \"derived\"\n[risk.thresholds]\ninternal = 10\nedge = 50\n[rollout]\nstrategy = \"gradual\"\nsteps = [10, 50, 100]\nmin_step_duration = \"8h\"\n",
    /// )
    /// .unwrap();
    /// assert_eq!(template.resolve_risk(Some(3)).unwrap(), RiskClass::Unused);
    /// assert_eq!(template.resolve_risk(Some(10)).unwrap(), RiskClass::Internal);
    /// assert_eq!(template.resolve_risk(Some(999)).unwrap(), RiskClass::Edge);
    /// assert!(matches!(template.resolve_risk(None), Err(DeployError::ScoreRequired)));
    /// ```
    pub fn resolve_risk(&self, score: Option<u32>) -> Result<RiskClass, DeployError> {
        match &self.risk {
            RiskSpec::Static(class) => Ok(*class),
            RiskSpec::Derived(thresholds) => score
                .map(|s| thresholds.classify(s))
                .ok_or(DeployError::ScoreRequired),
        }
    }

    fn parse_named(toml_src: &str, file: &Path) -> Result<Self, DeployError> {
        let raw: RawTemplate = toml::from_str(toml_src).map_err(|source| DeployError::Parse {
            file: file.to_path_buf(),
            source,
        })?;
        let template = Self {
            file: file.to_path_buf(),
            target: convert_target(file, raw.target)?,
            risk: convert_risk(file, raw.risk)?,
            rollout: convert_rollout(file, raw.rollout)?,
            health: raw.health.map(|h| convert_health(file, h)).transpose()?,
            rollback: raw
                .rollback
                .map(|r| convert_rollback(file, r))
                .transpose()?
                .unwrap_or(Rollback {
                    automatic: false,
                    on_breach: OnBreach::HaltAndEscalate,
                    retain_for: Duration::zero(),
                }),
            shadow: raw.shadow.map(|s| convert_shadow(file, s)).transpose()?,
        };
        template.validate()?;
        Ok(template)
    }
}

fn expected_one_of<'a>(got: &str, names: impl Iterator<Item = &'a str>) -> String {
    format!(
        "unknown value `{got}` (expected one of: {})",
        names.collect::<Vec<_>>().join(", ")
    )
}

fn invalid(file: &Path, field: &'static str, reason: impl Into<String>) -> DeployError {
    DeployError::InvalidField {
        file: file.to_path_buf(),
        field,
        reason: reason.into(),
    }
}

fn non_blank(file: &Path, field: &'static str, value: String) -> Result<String, DeployError> {
    if value.trim().is_empty() {
        return Err(invalid(file, field, "must not be empty"));
    }
    Ok(value)
}

fn duration(file: &Path, field: &'static str, value: &str) -> Result<Duration, DeployError> {
    windows::parse_duration(value).map_err(|e| invalid(file, field, e.to_string()))
}

fn percent(file: &Path, field: &'static str, value: i64) -> Result<u8, DeployError> {
    u8::try_from(value)
        .ok()
        .filter(|p| (1..=100).contains(p))
        .ok_or_else(|| {
            invalid(
                file,
                field,
                format!("{value} is not a percentage in 1..=100"),
            )
        })
}

fn convert_target(file: &Path, raw: RawTarget) -> Result<Target, DeployError> {
    let kind = raw
        .kind
        .parse()
        .map_err(|e| invalid(file, "target.kind", e))?;
    let registry = non_blank(file, "target.registry", raw.registry)?;
    let image = non_blank(file, "target.image", raw.image)?;
    if raw.environments.is_empty() {
        return Err(invalid(
            file,
            "target.environments",
            "at least one environment is required",
        ));
    }
    let mut environments = Vec::with_capacity(raw.environments.len());
    for env in raw.environments {
        let env = non_blank(file, "target.environments", env)?;
        if environments.contains(&env) {
            return Err(invalid(
                file,
                "target.environments",
                format!("duplicate environment `{env}`"),
            ));
        }
        environments.push(env);
    }
    Ok(Target {
        kind,
        registry,
        image,
        environments,
    })
}

fn convert_risk(file: &Path, raw: RawRisk) -> Result<RiskSpec, DeployError> {
    if raw.class == "derived" {
        let raw_thresholds = raw
            .thresholds
            .ok_or_else(|| invalid(file, "risk.thresholds", "required when class = derived"))?;
        let thresholds = RiskThresholds {
            internal: threshold(file, raw_thresholds.internal)?,
            edge: threshold(file, raw_thresholds.edge)?,
            core: threshold(file, raw_thresholds.core)?,
        };
        thresholds.validate(file)?;
        return Ok(RiskSpec::Derived(thresholds));
    }
    if raw.thresholds.is_some() {
        return Err(invalid(
            file,
            "risk.thresholds",
            "only valid when class = derived",
        ));
    }
    let names = RiskClass::ALL
        .iter()
        .map(|c| c.name())
        .chain(std::iter::once("derived"));
    let class = raw
        .class
        .parse()
        .map_err(|_| invalid(file, "risk.class", expected_one_of(&raw.class, names)))?;
    Ok(RiskSpec::Static(class))
}

fn threshold(file: &Path, value: Option<i64>) -> Result<Option<u32>, DeployError> {
    value
        .map(|v| {
            u32::try_from(v).map_err(|_| {
                invalid(
                    file,
                    "risk.thresholds",
                    format!("{v} is not a non-negative score"),
                )
            })
        })
        .transpose()
}

fn convert_rollout(file: &Path, raw: RawRollout) -> Result<Rollout, DeployError> {
    let strategy = raw
        .strategy
        .parse()
        .map_err(|e| invalid(file, "rollout.strategy", e))?;
    let raw_steps = raw.steps.unwrap_or_else(|| vec![100]);
    let mut steps = Vec::with_capacity(raw_steps.len());
    for value in raw_steps {
        let step = percent(file, "rollout.steps", value)?;
        if steps.last().is_some_and(|last| step <= *last) {
            return Err(invalid(
                file,
                "rollout.steps",
                "must be strictly increasing",
            ));
        }
        steps.push(step);
    }
    if steps.last() != Some(&100) {
        return Err(invalid(file, "rollout.steps", "must end at 100"));
    }
    let min_step_duration = raw
        .min_step_duration
        .as_deref()
        .map(|d| duration(file, "rollout.min_step_duration", d))
        .transpose()?
        .unwrap_or_else(Duration::zero);
    let windows = raw
        .windows
        .map(|w| non_blank(file, "rollout.windows", w))
        .transpose()?;
    Ok(Rollout {
        strategy,
        steps,
        min_step_duration,
        windows,
    })
}

fn convert_health(file: &Path, raw: RawHealth) -> Result<Health, DeployError> {
    if raw.endpoints.is_empty() {
        return Err(invalid(
            file,
            "health.endpoints",
            "at least one endpoint is required",
        ));
    }
    if let Some(bad) = raw.endpoints.iter().find(|e| !e.starts_with('/')) {
        return Err(invalid(
            file,
            "health.endpoints",
            format!("`{bad}` must be an absolute path"),
        ));
    }
    if !(0.0..=1.0).contains(&raw.error_rate_max) {
        return Err(invalid(
            file,
            "health.error_rate_max",
            format!("{} is not in 0.0..=1.0", raw.error_rate_max),
        ));
    }
    let latency_p99_max_ms = u32::try_from(raw.latency_p99_max_ms)
        .ok()
        .filter(|ms| *ms > 0)
        .ok_or_else(|| {
            invalid(
                file,
                "health.latency_p99_max_ms",
                format!(
                    "{} is not a positive millisecond count",
                    raw.latency_p99_max_ms
                ),
            )
        })?;
    let bake_time = duration(file, "health.bake_time", &raw.bake_time)?;
    Ok(Health {
        endpoints: raw.endpoints,
        error_rate_max: raw.error_rate_max,
        latency_p99_max_ms,
        bake_time,
    })
}

fn convert_rollback(file: &Path, raw: RawRollback) -> Result<Rollback, DeployError> {
    let on_breach = raw
        .on_breach
        .parse()
        .map_err(|e| invalid(file, "rollback.on_breach", e))?;
    let retain_for = raw
        .retain_for
        .as_deref()
        .map(|d| duration(file, "rollback.retain_for", d))
        .transpose()?
        .unwrap_or_else(Duration::zero);
    Ok(Rollback {
        automatic: raw.automatic,
        on_breach,
        retain_for,
    })
}

fn convert_shadow(file: &Path, raw: RawShadow) -> Result<Shadow, DeployError> {
    let mirror_percent = if raw.enabled {
        percent(file, "shadow.mirror_percent", raw.mirror_percent)?
    } else {
        u8::try_from(raw.mirror_percent)
            .ok()
            .filter(|p| *p <= 100)
            .ok_or_else(|| {
                invalid(
                    file,
                    "shadow.mirror_percent",
                    format!("{} is not a percentage in 0..=100", raw.mirror_percent),
                )
            })?
    };
    if raw.enabled && raw.compare.is_empty() {
        return Err(invalid(
            file,
            "shadow.compare",
            "at least one attribute is required when shadow is enabled",
        ));
    }
    let compare = raw
        .compare
        .iter()
        .map(|c| c.parse().map_err(|e| invalid(file, "shadow.compare", e)))
        .collect::<Result<Vec<_>, _>>()?;
    let max_divergence = raw.max_divergence.unwrap_or(DEFAULT_MAX_DIVERGENCE);
    if !(0.0..=1.0).contains(&max_divergence) {
        return Err(invalid(
            file,
            "shadow.max_divergence",
            format!("{max_divergence} is not a rate in 0.0..=1.0"),
        ));
    }
    Ok(Shadow {
        enabled: raw.enabled,
        mirror_percent,
        compare,
        max_divergence,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::Duration;

    pub(crate) const FIXTURE: &str = r#"
[target]
kind = "container-registry+serverless"
registry = "registry.example.invalid/ns"
image = "fullstack-fixture"
environments = ["sandbox", "staging", "production"]

[risk]
class = "edge"

[rollout]
strategy = "gradual"
steps = [10, 50, 100]
min_step_duration = "8h"
windows = "business-hours"

[health]
endpoints = ["/health/v1"]
error_rate_max = 0.01
latency_p99_max_ms = 800
bake_time = "30m"

[rollback]
automatic = true
on_breach = "rollback"

[shadow]
enabled = false
mirror_percent = 0
compare = ["status", "latency"]
"#;

    fn field_error(src: &str) -> (&'static str, String) {
        match DeployTemplate::parse(src).unwrap_err() {
            DeployError::InvalidField { field, reason, .. } => (field, reason),
            other => panic!("expected InvalidField, got {other:?}"),
        }
    }

    #[test]
    fn parses_fixture_template() {
        let t = DeployTemplate::parse(FIXTURE).unwrap();
        assert_eq!(t.target.kind, TargetKind::ContainerRegistryServerless);
        assert_eq!(t.target.registry, "registry.example.invalid/ns");
        assert_eq!(t.target.image, "fullstack-fixture");
        assert_eq!(t.target.environments, ["sandbox", "staging", "production"]);
        assert_eq!(t.risk, RiskSpec::Static(RiskClass::Edge));
        assert_eq!(t.rollout.strategy, Strategy::Gradual);
        assert_eq!(t.rollout.steps, [10, 50, 100]);
        assert_eq!(t.rollout.min_step_duration, Duration::hours(8));
        assert_eq!(t.rollout.windows.as_deref(), Some("business-hours"));
        let health = t.health.as_ref().unwrap();
        assert_eq!(health.endpoints, ["/health/v1"]);
        assert_eq!(health.error_rate_max, 0.01);
        assert_eq!(health.latency_p99_max_ms, 800);
        assert_eq!(health.bake_time, Duration::minutes(30));
        assert!(t.rollback.automatic);
        assert_eq!(t.rollback.on_breach, OnBreach::Rollback);
        assert_eq!(t.rollback.retain_for, Duration::zero());
        let shadow = t.shadow.as_ref().unwrap();
        assert!(!shadow.enabled);
        assert_eq!(shadow.mirror_percent, 0);
        assert_eq!(
            shadow.compare,
            [ShadowCompare::Status, ShadowCompare::Latency]
        );
        assert_eq!(shadow.max_divergence, DEFAULT_MAX_DIVERGENCE);
        assert_eq!(t.file(), Path::new(DEPLOY_FILE_NAME));
        assert_eq!(
            t.target.image_ref(),
            "registry.example.invalid/ns/fullstack-fixture"
        );
    }

    #[test]
    fn minimal_template_uses_defaults() {
        let t = DeployTemplate::parse(
            "[target]\nkind = \"container-registry+serverless\"\nregistry = \"r.invalid\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"unused\"\n[rollout]\nstrategy = \"instant\"\n",
        )
        .unwrap();
        assert_eq!(t.rollout.steps, [100]);
        assert_eq!(t.rollout.min_step_duration, Duration::zero());
        assert!(t.rollout.windows.is_none());
        assert!(t.health.is_none());
        assert!(t.shadow.is_none());
        assert!(!t.rollback.automatic);
        assert_eq!(t.rollback.on_breach, OnBreach::HaltAndEscalate);
    }

    #[test]
    fn load_reads_file_and_records_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deploy.toml");
        std::fs::write(&path, FIXTURE).unwrap();
        let t = DeployTemplate::load(&path).unwrap();
        assert_eq!(t.file(), path);
    }

    #[test]
    fn load_from_repo_uses_nanna_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".nanna")).unwrap();
        std::fs::write(dir.path().join(".nanna/deploy.toml"), FIXTURE).unwrap();
        let t = DeployTemplate::load_from_repo(dir.path()).unwrap();
        assert_eq!(t.file(), dir.path().join(".nanna/deploy.toml"));
        assert_eq!(
            DeployTemplate::path_in(dir.path()),
            dir.path().join(".nanna/deploy.toml")
        );
    }

    #[test]
    fn missing_file_is_io_error() {
        let err = DeployTemplate::load(Path::new("/nonexistent/deploy.toml")).unwrap_err();
        assert!(matches!(err, DeployError::Io { .. }));
        assert!(err.to_string().contains("/nonexistent/deploy.toml"));
    }

    #[test]
    fn malformed_toml_is_parse_error() {
        let err = DeployTemplate::parse("[target\n").unwrap_err();
        assert!(matches!(err, DeployError::Parse { .. }));
        assert!(err.to_string().starts_with("failed to parse deploy.toml"));
    }

    #[test]
    fn unknown_key_is_parse_error() {
        let err = DeployTemplate::parse(&FIXTURE.replace("image =", "imag =")).unwrap_err();
        assert!(matches!(err, DeployError::Parse { .. }));
    }

    #[test]
    fn errors_name_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deploy.toml");
        std::fs::write(&path, FIXTURE.replace("\"edge\"", "\"huge\"")).unwrap();
        let err = DeployTemplate::load(&path).unwrap_err();
        assert!(err.to_string().starts_with(&path.display().to_string()));
    }

    #[test]
    fn rejects_unknown_target_kind() {
        let (field, reason) = field_error(&FIXTURE.replace("container-registry+serverless", "vm"));
        assert_eq!(field, "target.kind");
        assert!(reason.contains("container-registry+serverless"));
    }

    #[test]
    fn rejects_empty_registry_and_image() {
        assert_eq!(
            field_error(&FIXTURE.replace(
                "registry = \"registry.example.invalid/ns\"",
                "registry = \"\""
            ))
            .0,
            "target.registry"
        );
        assert_eq!(
            field_error(&FIXTURE.replace("image = \"fullstack-fixture\"", "image = \" \"")).0,
            "target.image"
        );
    }

    #[test]
    fn rejects_bad_environments() {
        let envs = "environments = [\"sandbox\", \"staging\", \"production\"]";
        assert_eq!(
            field_error(&FIXTURE.replace(envs, "environments = []")).0,
            "target.environments"
        );
        assert_eq!(
            field_error(&FIXTURE.replace(envs, "environments = [\"\"]")).0,
            "target.environments"
        );
        let (field, reason) =
            field_error(&FIXTURE.replace(envs, "environments = [\"production\", \"production\"]"));
        assert_eq!(field, "target.environments");
        assert!(reason.contains("duplicate"));
    }

    #[test]
    fn rejects_unknown_risk_class() {
        let (field, reason) = field_error(&FIXTURE.replace("\"edge\"", "\"huge\""));
        assert_eq!(field, "risk.class");
        assert!(reason.contains("unused, internal, edge, core, derived"));
    }

    #[test]
    fn rejects_unknown_strategy() {
        let (field, reason) = field_error(&FIXTURE.replace("\"gradual\"", "\"yolo\""));
        assert_eq!(field, "rollout.strategy");
        assert!(reason.contains("shadow-then-gradual"));
    }

    #[test]
    fn rejects_bad_steps() {
        let steps = "steps = [10, 50, 100]";
        assert_eq!(
            field_error(&FIXTURE.replace(steps, "steps = []")).0,
            "rollout.steps"
        );
        assert_eq!(
            field_error(&FIXTURE.replace(steps, "steps = [10, 50]")).0,
            "rollout.steps"
        );
        assert_eq!(
            field_error(&FIXTURE.replace(steps, "steps = [50, 10, 100]")).0,
            "rollout.steps"
        );
        assert_eq!(
            field_error(&FIXTURE.replace(steps, "steps = [10, 10, 100]")).0,
            "rollout.steps"
        );
        assert_eq!(
            field_error(&FIXTURE.replace(steps, "steps = [0, 50, 100]")).0,
            "rollout.steps"
        );
        assert_eq!(
            field_error(&FIXTURE.replace(steps, "steps = [10, 50, 100, 101]")).0,
            "rollout.steps"
        );
        assert_eq!(
            field_error(&FIXTURE.replace(steps, "steps = [-5, 50, 100]")).0,
            "rollout.steps"
        );
    }

    #[test]
    fn rejects_bad_durations() {
        assert_eq!(
            field_error(&FIXTURE.replace("\"8h\"", "\"soon\"")).0,
            "rollout.min_step_duration"
        );
        assert_eq!(
            field_error(&FIXTURE.replace("\"30m\"", "\"30\"")).0,
            "health.bake_time"
        );
        assert_eq!(
            field_error(&FIXTURE.replace(
                "on_breach = \"rollback\"",
                "on_breach = \"rollback\"\nretain_for = \"x\""
            ))
            .0,
            "rollback.retain_for"
        );
    }

    #[test]
    fn rejects_empty_window_name() {
        assert_eq!(
            field_error(&FIXTURE.replace("\"business-hours\"", "\"\"")).0,
            "rollout.windows"
        );
    }

    #[test]
    fn rejects_bad_health() {
        assert_eq!(
            field_error(&FIXTURE.replace("endpoints = [\"/health/v1\"]", "endpoints = []")).0,
            "health.endpoints"
        );
        assert_eq!(
            field_error(&FIXTURE.replace("\"/health/v1\"", "\"health\"")).0,
            "health.endpoints"
        );
        assert_eq!(
            field_error(&FIXTURE.replace("0.01", "1.5")).0,
            "health.error_rate_max"
        );
        assert_eq!(
            field_error(&FIXTURE.replace("0.01", "-0.5")).0,
            "health.error_rate_max"
        );
        assert_eq!(
            field_error(&FIXTURE.replace("800", "0")).0,
            "health.latency_p99_max_ms"
        );
        assert_eq!(
            field_error(&FIXTURE.replace("800", "-1")).0,
            "health.latency_p99_max_ms"
        );
    }

    #[test]
    fn rejects_unknown_on_breach() {
        let (field, reason) = field_error(&FIXTURE.replace("\"rollback\"", "\"panic\""));
        assert_eq!(field, "rollback.on_breach");
        assert!(reason.contains("halt-and-escalate"));
    }

    #[test]
    fn parses_every_on_breach() {
        for (name, expected) in [
            ("rollback", OnBreach::Rollback),
            ("roll-forward", OnBreach::RollForward),
            ("halt-and-escalate", OnBreach::HaltAndEscalate),
        ] {
            let t = DeployTemplate::parse(&FIXTURE.replace("\"rollback\"", &format!("\"{name}\"")))
                .unwrap();
            assert_eq!(t.rollback.on_breach, expected);
            assert_eq!(expected.name(), name);
        }
    }

    #[test]
    fn rejects_bad_shadow() {
        let enabled = FIXTURE
            .replace(
                "enabled = false\nmirror_percent = 0",
                "enabled = true\nmirror_percent = 0",
            )
            .replace(
                "strategy = \"gradual\"",
                "strategy = \"shadow-then-gradual\"",
            );
        assert_eq!(field_error(&enabled).0, "shadow.mirror_percent");
        assert_eq!(
            field_error(&enabled.replace("mirror_percent = 0", "mirror_percent = 101")).0,
            "shadow.mirror_percent"
        );
        assert_eq!(
            field_error(&enabled.replace("mirror_percent = 0", "mirror_percent = -1")).0,
            "shadow.mirror_percent"
        );
        assert_eq!(
            field_error(&FIXTURE.replace("mirror_percent = 0", "mirror_percent = 101")).0,
            "shadow.mirror_percent"
        );
        assert_eq!(
            field_error(
                &enabled
                    .replace("mirror_percent = 0", "mirror_percent = 10")
                    .replace("compare = [\"status\", \"latency\"]", "compare = []")
            )
            .0,
            "shadow.compare"
        );
        let (field, reason) = field_error(&FIXTURE.replace("\"latency\"", "\"body\""));
        assert_eq!(field, "shadow.compare");
        assert!(reason.contains("status, latency"));
    }

    #[test]
    fn max_divergence_is_a_rate() {
        let with = |value: &str| {
            FIXTURE.replace(
                "compare = [\"status\", \"latency\"]\n",
                &format!("compare = [\"status\", \"latency\"]\nmax_divergence = {value}\n"),
            )
        };
        let t = DeployTemplate::parse(&with("0.2")).unwrap();
        assert_eq!(t.shadow.as_ref().unwrap().max_divergence, 0.2);
        assert_eq!(
            DeployTemplate::parse(&with("0.0"))
                .unwrap()
                .shadow
                .unwrap()
                .max_divergence,
            0.0
        );
        assert_eq!(
            DeployTemplate::parse(&with("1.0"))
                .unwrap()
                .shadow
                .unwrap()
                .max_divergence,
            1.0
        );
        for bad in ["1.5", "-0.1", "nan", "inf"] {
            let (field, reason) = field_error(&with(bad));
            assert_eq!(field, "shadow.max_divergence", "{bad}");
            assert!(reason.contains("0.0..=1.0"), "{reason}");
        }
        let json = serde_json::to_string(t.shadow.as_ref().unwrap()).unwrap();
        assert!(json.contains("\"status\""));
        assert_eq!(
            serde_json::from_str::<Shadow>(&json).unwrap(),
            t.shadow.unwrap()
        );
    }

    #[test]
    fn static_class_resolves_with_or_without_score() {
        let t = DeployTemplate::parse(FIXTURE).unwrap();
        assert_eq!(t.resolve_risk(None).unwrap(), RiskClass::Edge);
        assert_eq!(t.resolve_risk(Some(1_000)).unwrap(), RiskClass::Edge);
        assert_eq!(t.highest_risk(), RiskClass::Edge);
    }

    const DERIVED: &str =
        "[risk]\nclass = \"derived\"\n[risk.thresholds]\ninternal = 10\nedge = 50\ncore = 200\n";

    fn derived(thresholds: &str) -> String {
        FIXTURE
            .replace("[risk]\nclass = \"edge\"\n", thresholds)
            .replace(
                "steps = [10, 50, 100]",
                "steps = [1, 5, 10, 25, 50, 75, 100]",
            )
            .replace("\"8h\"", "\"1d\"")
    }

    #[test]
    fn derived_class_resolves_from_thresholds() {
        let t = DeployTemplate::parse(&derived(DERIVED)).unwrap();
        let thresholds = RiskThresholds {
            internal: Some(10),
            edge: Some(50),
            core: Some(200),
        };
        assert_eq!(t.risk, RiskSpec::Derived(thresholds.clone()));
        assert!(matches!(
            t.resolve_risk(None),
            Err(DeployError::ScoreRequired)
        ));
        assert_eq!(t.resolve_risk(Some(0)).unwrap(), RiskClass::Unused);
        assert_eq!(t.resolve_risk(Some(9)).unwrap(), RiskClass::Unused);
        assert_eq!(t.resolve_risk(Some(10)).unwrap(), RiskClass::Internal);
        assert_eq!(t.resolve_risk(Some(49)).unwrap(), RiskClass::Internal);
        assert_eq!(t.resolve_risk(Some(50)).unwrap(), RiskClass::Edge);
        assert_eq!(t.resolve_risk(Some(199)).unwrap(), RiskClass::Edge);
        assert_eq!(t.resolve_risk(Some(200)).unwrap(), RiskClass::Core);
        assert_eq!(t.resolve_risk(Some(u32::MAX)).unwrap(), RiskClass::Core);
        assert_eq!(t.highest_risk(), RiskClass::Core);
        assert_eq!(thresholds.highest(), RiskClass::Core);
    }

    #[test]
    fn derived_thresholds_may_omit_classes() {
        let t = DeployTemplate::parse(&derived(
            "[risk]\nclass = \"derived\"\n[risk.thresholds]\nedge = 50\n",
        ))
        .unwrap();
        assert_eq!(t.resolve_risk(Some(49)).unwrap(), RiskClass::Unused);
        assert_eq!(t.resolve_risk(Some(50)).unwrap(), RiskClass::Edge);
        assert_eq!(t.highest_risk(), RiskClass::Edge);
    }

    #[test]
    fn derived_requires_thresholds() {
        let (field, reason) = field_error(&derived("[risk]\nclass = \"derived\"\n"));
        assert_eq!(field, "risk.thresholds");
        assert!(reason.contains("derived"));
    }

    #[test]
    fn derived_thresholds_must_be_increasing() {
        let (field, reason) = field_error(&derived(
            "[risk]\nclass = \"derived\"\n[risk.thresholds]\ninternal = 50\nedge = 50\n",
        ));
        assert_eq!(field, "risk.thresholds");
        assert!(reason.contains("increasing"));
        assert_eq!(
            field_error(&derived(
                "[risk]\nclass = \"derived\"\n[risk.thresholds]\nedge = 50\ncore = 10\n"
            ))
            .0,
            "risk.thresholds"
        );
        assert_eq!(
            field_error(&derived(
                "[risk]\nclass = \"derived\"\n[risk.thresholds]\nedge = -1\n"
            ))
            .0,
            "risk.thresholds"
        );
        assert_eq!(
            field_error(&derived("[risk]\nclass = \"derived\"\n[risk.thresholds]\n")).0,
            "risk.thresholds"
        );
    }

    #[test]
    fn static_class_rejects_thresholds() {
        let (field, reason) = field_error(&derived(
            "[risk]\nclass = \"core\"\n[risk.thresholds]\ncore = 1\n",
        ));
        assert_eq!(field, "risk.thresholds");
        assert!(reason.contains("derived"));
    }

    #[test]
    fn names_round_trip() {
        for class in RiskClass::ALL {
            assert_eq!(class.name().parse::<RiskClass>().unwrap(), class);
            assert_eq!(class.to_string(), class.name());
        }
        assert!("derived".parse::<RiskClass>().is_err());
        for strategy in Strategy::ALL {
            assert_eq!(strategy.name().parse::<Strategy>().unwrap(), strategy);
            assert_eq!(strategy.to_string(), strategy.name());
        }
        assert!("instantly".parse::<Strategy>().is_err());
        assert_eq!(
            TargetKind::ContainerRegistryServerless.name(),
            "container-registry+serverless"
        );
        assert_eq!(ShadowCompare::Status.name(), "status");
        assert_eq!(ShadowCompare::Latency.name(), "latency");
        assert!(
            RiskClass::Unused < RiskClass::Internal
                && RiskClass::Internal < RiskClass::Edge
                && RiskClass::Edge < RiskClass::Core
        );
    }
}
