//! Human-availability windows for gated effects.
//!
//! A window is a recurring span of local wall-clock time, in a named IANA
//! timezone, during which humans are available to supervise effects on real
//! systems. Windows are human-authored in `windows.toml`:
//!
//! ```toml
//! [[window]]
//! name = "business-hours"
//! timezone = "Europe/Paris"
//! days = ["mon", "tue", "wed", "thu", "fri"]
//! start = "09:30"
//! end = "17:00"
//! applies_to = ["production"]
//! holidays = ["2026-12-25"]
//! ```
//!
//! Every query takes an explicit `now`, so callers and tests own the clock.
//! Windows are evaluated on the local wall clock of their timezone, which is
//! what keeps them correct across daylight-saving transitions: a `09:30` start
//! means `09:30` on the wall whether or not the UTC offset changed overnight.
//! When a start time falls inside a spring-forward gap the window opens at the
//! transition instant; when it falls inside a fall-back overlap it opens at the
//! earlier of the two instants.
//!
//! ```
//! use chrono::{TimeZone, Utc};
//! use harness::windows::WindowSet;
//!
//! let set = WindowSet::parse(
//!     r#"
//! [[window]]
//! name = "business-hours"
//! timezone = "Europe/Paris"
//! days = ["mon", "tue", "wed", "thu", "fri"]
//! start = "09:30"
//! end = "17:00"
//! applies_to = ["production"]
//! "#,
//! )
//! .unwrap();
//!
//! let saturday = Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap();
//! assert!(!set.is_open("business-hours", saturday).unwrap());
//! let monday_0930_paris = Utc.with_ymd_and_hms(2026, 9, 28, 7, 30, 0).unwrap();
//! assert_eq!(set.next_open("business-hours", saturday).unwrap(), monday_0930_paris);
//! ```
//!
//! Which effect levels need a window at all is a separate policy question,
//! answered by [`WindowGating`]. The scheduler integration that parks a task
//! until [`WindowSet::next_open`] is out of scope for this module.

use chrono::{
    DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Timelike, Utc, Weekday,
};
use chrono_tz::Tz;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;

/// File name of the window configuration inside Nanna's config directory.
pub const WINDOWS_FILE_NAME: &str = "windows.toml";

/// How many calendar days ahead [`WindowSet::next_open`] searches before
/// reporting [`WindowError::NoUpcomingOpening`].
pub const NEXT_OPEN_HORIZON_DAYS: i64 = 730;

const MINUTES_PER_DAY: u32 = 24 * 60;

/// Errors produced while loading or querying availability windows.
#[derive(Debug, Error)]
pub enum WindowError {
    /// The configuration file could not be read.
    #[error("failed to read {}: {source}", path.display())]
    Io {
        /// Path that was being read.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The configuration is not well-formed TOML for this schema.
    #[error("failed to parse windows.toml: {0}")]
    Parse(#[from] toml::de::Error),
    /// A field of one `[[window]]` entry failed validation.
    #[error("window `{window}`: field `{field}` is invalid: {reason}")]
    InvalidField {
        /// Name of the offending window (empty if the name itself is invalid).
        window: String,
        /// The TOML key that failed validation.
        field: &'static str,
        /// Human-readable explanation.
        reason: String,
    },
    /// Two `[[window]]` entries share a name.
    #[error("duplicate window name `{0}`")]
    DuplicateName(String),
    /// A query named a window that is not in the set.
    #[error("unknown window `{0}`")]
    UnknownWindow(String),
    /// An effect level name did not match any [`EffectLevel`].
    #[error("unknown effect level `{0}` (expected one of: local, sandbox, production)")]
    UnknownEffectLevel(String),
    /// [`WindowSet::open_adhoc`] was given a zero or negative duration.
    #[error("ad-hoc window duration must be positive, got {0}")]
    NonPositiveDuration(Duration),
    /// No opening exists within [`NEXT_OPEN_HORIZON_DAYS`] of the query.
    #[error("window `{name}` has no opening within {horizon_days} days of {from}")]
    NoUpcomingOpening {
        /// Window that was queried.
        name: String,
        /// Instant the search started from.
        from: DateTime<Utc>,
        /// Search horizon in days.
        horizon_days: i64,
    },
    /// A duration string such as `2h` could not be parsed.
    #[error("invalid duration `{0}`: expected <integer><unit> with unit m, h or d")]
    InvalidDuration(String),
}

/// Minimal effect classification used to decide whether a window applies.
///
/// This is a local stand-in ordered from least to most consequential; a
/// richer effect model owned elsewhere can map onto it by name via
/// [`FromStr`].
///
/// ```
/// use harness::windows::EffectLevel;
///
/// let level: EffectLevel = "Production".parse().unwrap();
/// assert_eq!(level, EffectLevel::Production);
/// assert!(EffectLevel::Sandbox < EffectLevel::Production);
/// assert_eq!(level.to_string(), "production");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EffectLevel {
    /// Effects confined to the agent's own container or worktree.
    Local,
    /// Effects on a disposable sandbox environment.
    Sandbox,
    /// Effects on systems serving real traffic.
    Production,
}

impl EffectLevel {
    /// Every level, least consequential first.
    pub const ALL: [EffectLevel; 3] = [
        EffectLevel::Local,
        EffectLevel::Sandbox,
        EffectLevel::Production,
    ];

    /// Lower-case name as written in `applies_to`.
    pub const fn name(self) -> &'static str {
        match self {
            EffectLevel::Local => "local",
            EffectLevel::Sandbox => "sandbox",
            EffectLevel::Production => "production",
        }
    }
}

impl fmt::Display for EffectLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for EffectLevel {
    type Err = WindowError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EffectLevel::ALL
            .into_iter()
            .find(|level| level.name().eq_ignore_ascii_case(s))
            .ok_or_else(|| WindowError::UnknownEffectLevel(s.to_string()))
    }
}

/// Policy deciding which effect levels must wait for an open window.
///
/// Production effects are gated by default and sandbox effects are not; a
/// deployment template may opt sandbox in. Local effects are never gated.
///
/// ```
/// use harness::windows::{EffectLevel, WindowGating};
///
/// let default = WindowGating::default();
/// assert!(default.requires_window(EffectLevel::Production));
/// assert!(!default.requires_window(EffectLevel::Sandbox));
///
/// let strict = WindowGating { sandbox: true, ..WindowGating::default() };
/// assert!(strict.requires_window(EffectLevel::Sandbox));
/// assert!(!strict.requires_window(EffectLevel::Local));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGating {
    /// Whether production effects need an open window.
    pub production: bool,
    /// Whether sandbox effects need an open window.
    pub sandbox: bool,
}

impl Default for WindowGating {
    fn default() -> Self {
        Self {
            production: true,
            sandbox: false,
        }
    }
}

impl WindowGating {
    /// Whether an effect at `level` may only run inside an open window.
    pub const fn requires_window(self, level: EffectLevel) -> bool {
        match level {
            EffectLevel::Local => false,
            EffectLevel::Sandbox => self.sandbox,
            EffectLevel::Production => self.production,
        }
    }

    /// [`Self::requires_window`] for a level given by name, case-insensitively.
    pub fn requires_window_named(self, level: &str) -> Result<bool, WindowError> {
        level.parse().map(|level| self.requires_window(level))
    }
}

/// Parse a human-entered duration such as `30m`, `2h` or `1d`.
///
/// ```
/// use chrono::Duration;
/// use harness::windows::parse_duration;
///
/// assert_eq!(parse_duration("2h").unwrap(), Duration::hours(2));
/// assert!(parse_duration("soon").is_err());
/// ```
pub fn parse_duration(input: &str) -> Result<Duration, WindowError> {
    let invalid = || WindowError::InvalidDuration(input.to_string());
    let unit = input.chars().last().ok_or_else(invalid)?;
    let digits = &input[..input.len() - unit.len_utf8()];
    let value = i64::from(digits.parse::<u32>().map_err(|_| invalid())?);
    match unit {
        'm' => Ok(Duration::minutes(value)),
        'h' => Ok(Duration::hours(value)),
        'd' => Ok(Duration::days(value)),
        _ => Err(invalid()),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowsFile {
    #[serde(default)]
    window: Vec<RawWindow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWindow {
    name: String,
    timezone: String,
    days: Vec<String>,
    start: String,
    end: String,
    applies_to: Vec<String>,
    #[serde(default)]
    holidays: Vec<String>,
}

/// One validated recurring availability window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    name: String,
    timezone: Tz,
    days: [bool; 7],
    start: NaiveTime,
    end: NaiveTime,
    applies_to: BTreeSet<EffectLevel>,
    holidays: BTreeSet<NaiveDate>,
}

fn invalid(window: &str, field: &'static str, reason: impl Into<String>) -> WindowError {
    WindowError::InvalidField {
        window: window.to_string(),
        field,
        reason: reason.into(),
    }
}

fn parse_time(window: &str, field: &'static str, value: &str) -> Result<NaiveTime, WindowError> {
    NaiveTime::parse_from_str(value, "%H:%M")
        .map_err(|e| invalid(window, field, format!("`{value}` is not HH:MM ({e})")))
}

impl TryFrom<RawWindow> for Window {
    type Error = WindowError;

    fn try_from(raw: RawWindow) -> Result<Self, Self::Error> {
        let name = raw.name;
        if name.is_empty() {
            return Err(invalid(&name, "name", "must not be empty"));
        }
        let timezone = Tz::from_str(&raw.timezone)
            .map_err(|e| invalid(&name, "timezone", format!("`{}`: {e}", raw.timezone)))?;
        if raw.days.is_empty() {
            return Err(invalid(&name, "days", "must list at least one day"));
        }
        let mut days = [false; 7];
        for day in &raw.days {
            let weekday = Weekday::from_str(day)
                .map_err(|_| invalid(&name, "days", format!("unrecognised day `{day}`")))?;
            let slot = &mut days[weekday.num_days_from_monday() as usize];
            if *slot {
                return Err(invalid(&name, "days", format!("duplicate day `{day}`")));
            }
            *slot = true;
        }
        let start = parse_time(&name, "start", &raw.start)?;
        let end = parse_time(&name, "end", &raw.end)?;
        if end <= start {
            return Err(invalid(
                &name,
                "end",
                format!("`{}` must be after start `{}`", raw.end, raw.start),
            ));
        }
        if raw.applies_to.is_empty() {
            return Err(invalid(
                &name,
                "applies_to",
                "must list at least one effect level",
            ));
        }
        let applies_to = raw
            .applies_to
            .iter()
            .map(|level| {
                level
                    .parse()
                    .map_err(|e: WindowError| invalid(&name, "applies_to", e.to_string()))
            })
            .collect::<Result<BTreeSet<EffectLevel>, WindowError>>()?;
        let holidays = raw
            .holidays
            .iter()
            .map(|date| {
                NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|e| {
                    invalid(
                        &name,
                        "holidays",
                        format!("`{date}` is not YYYY-MM-DD ({e})"),
                    )
                })
            })
            .collect::<Result<BTreeSet<NaiveDate>, WindowError>>()?;
        Ok(Window {
            name,
            timezone,
            days,
            start,
            end,
            applies_to,
            holidays,
        })
    }
}

fn first_instant_at_or_after(tz: &Tz, date: NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    let first_minute = time.num_seconds_from_midnight() / 60;
    (first_minute..MINUTES_PER_DAY)
        .filter_map(|minute| NaiveTime::from_hms_opt(minute / 60, minute % 60, 0))
        .find_map(|candidate| tz.from_local_datetime(&date.and_time(candidate)).earliest())
        .map(|local| local.with_timezone(&Utc))
}

impl Window {
    /// Window name as written in `windows.toml`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// IANA timezone the window's wall-clock times are expressed in.
    pub fn timezone(&self) -> Tz {
        self.timezone
    }

    /// Inclusive local start time.
    pub fn start(&self) -> NaiveTime {
        self.start
    }

    /// Exclusive local end time.
    pub fn end(&self) -> NaiveTime {
        self.end
    }

    /// Days of the week the window recurs on, Monday first.
    pub fn days(&self) -> Vec<Weekday> {
        self.days
            .iter()
            .enumerate()
            .filter(|(_, enabled)| **enabled)
            .filter_map(|(index, _)| Weekday::try_from(index as u8).ok())
            .collect()
    }

    /// Effect levels this window applies to.
    pub fn applies_to(&self) -> &BTreeSet<EffectLevel> {
        &self.applies_to
    }

    /// Local dates on which the window is closed regardless of weekday.
    pub fn holidays(&self) -> Vec<NaiveDate> {
        self.holidays.iter().copied().collect()
    }

    fn day_eligible(&self, date: NaiveDate) -> bool {
        self.days[date.weekday().num_days_from_monday() as usize] && !self.holidays.contains(&date)
    }

    /// Whether the recurring schedule is open at `now`.
    pub fn is_open_at(&self, now: DateTime<Utc>) -> bool {
        let local = now.with_timezone(&self.timezone);
        let time = local.time();
        self.day_eligible(local.date_naive()) && self.start <= time && time < self.end
    }

    /// Earliest instant at or after `now` when the recurring schedule is open,
    /// or `None` if there is none within [`NEXT_OPEN_HORIZON_DAYS`].
    pub fn next_open(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if self.is_open_at(now) {
            return Some(now);
        }
        let today = now.with_timezone(&self.timezone).date_naive();
        (0..=NEXT_OPEN_HORIZON_DAYS)
            .map(|offset| today + Duration::days(offset))
            .filter(|date| self.day_eligible(*date))
            .filter_map(|date| first_instant_at_or_after(&self.timezone, date, self.start))
            .find(|instant| *instant > now && self.is_open_at(*instant))
    }
}

/// A one-off window opened by a human on top of the recurring schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdhocWindow {
    /// Name of the recurring window being temporarily opened.
    pub name: String,
    /// Inclusive instant the override starts.
    pub from: DateTime<Utc>,
    /// Exclusive instant the override ends.
    pub until: DateTime<Utc>,
}

impl AdhocWindow {
    fn covers(&self, name: &str, now: DateTime<Utc>) -> bool {
        self.name == name && self.from <= now && now < self.until
    }
}

/// The windows loaded from one `windows.toml`, plus any ad-hoc overrides.
#[derive(Debug, Clone, Default)]
pub struct WindowSet {
    windows: BTreeMap<String, Window>,
    adhoc: Vec<AdhocWindow>,
}

impl WindowSet {
    /// Parse `windows.toml` content, validating every entry.
    ///
    /// ```
    /// use harness::windows::{WindowError, WindowSet};
    ///
    /// let err = WindowSet::parse(
    ///     "[[window]]\nname = \"w\"\ntimezone = \"Europe/Paris\"\ndays = [\"mon\"]\nstart = \"09:00\"\nend = \"08:00\"\napplies_to = [\"production\"]\n",
    /// )
    /// .unwrap_err();
    /// assert!(matches!(err, WindowError::InvalidField { field: "end", .. }));
    /// ```
    pub fn parse(toml_src: &str) -> Result<Self, WindowError> {
        let file: WindowsFile = toml::from_str(toml_src)?;
        let mut windows = BTreeMap::new();
        for raw in file.window {
            let window = Window::try_from(raw)?;
            if windows.contains_key(&window.name) {
                return Err(WindowError::DuplicateName(window.name));
            }
            windows.insert(window.name.clone(), window);
        }
        Ok(Self {
            windows,
            adhoc: Vec::new(),
        })
    }

    /// Read and parse the file at `path`.
    pub fn load(path: &Path) -> Result<Self, WindowError> {
        let src = std::fs::read_to_string(path).map_err(|source| WindowError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&src)
    }

    /// Look up a window by name.
    pub fn window(&self, name: &str) -> Option<&Window> {
        self.windows.get(name)
    }

    /// All windows, ordered by name.
    pub fn windows(&self) -> impl Iterator<Item = &Window> {
        self.windows.values()
    }

    /// Windows whose `applies_to` includes `level`.
    pub fn windows_for(&self, level: EffectLevel) -> impl Iterator<Item = &Window> {
        self.windows()
            .filter(move |window| window.applies_to.contains(&level))
    }

    /// Ad-hoc overrides added through [`Self::open_adhoc`], oldest first.
    pub fn adhoc_windows(&self) -> &[AdhocWindow] {
        &self.adhoc
    }

    fn get(&self, name: &str) -> Result<&Window, WindowError> {
        self.window(name)
            .ok_or_else(|| WindowError::UnknownWindow(name.to_string()))
    }

    /// Whether the named window is open at `now`, through its recurring
    /// schedule or an ad-hoc override.
    ///
    /// ```
    /// use chrono::{TimeZone, Utc};
    /// use harness::windows::WindowSet;
    ///
    /// let set = WindowSet::parse(
    ///     "[[window]]\nname = \"oncall\"\ntimezone = \"Europe/Paris\"\ndays = [\"mon\"]\nstart = \"10:00\"\nend = \"12:00\"\napplies_to = [\"production\"]\n",
    /// )
    /// .unwrap();
    /// let monday_1100_paris = Utc.with_ymd_and_hms(2026, 9, 28, 9, 0, 0).unwrap();
    /// assert!(set.is_open("oncall", monday_1100_paris).unwrap());
    /// assert!(set.is_open("missing", monday_1100_paris).is_err());
    /// ```
    pub fn is_open(&self, name: &str, now: DateTime<Utc>) -> Result<bool, WindowError> {
        let window = self.get(name)?;
        Ok(window.is_open_at(now) || self.adhoc.iter().any(|adhoc| adhoc.covers(name, now)))
    }

    /// Earliest instant at or after `now` when the named window is open.
    ///
    /// This is the wake time a scheduler should park a task until. The
    /// result is `now` itself when the window is already open.
    ///
    /// ```
    /// use chrono::{TimeZone, Utc};
    /// use harness::windows::WindowSet;
    ///
    /// let set = WindowSet::parse(
    ///     "[[window]]\nname = \"oncall\"\ntimezone = \"Europe/Paris\"\ndays = [\"mon\"]\nstart = \"10:00\"\nend = \"12:00\"\napplies_to = [\"production\"]\n",
    /// )
    /// .unwrap();
    /// let sunday = Utc.with_ymd_and_hms(2026, 9, 27, 12, 0, 0).unwrap();
    /// let monday_1000_paris = Utc.with_ymd_and_hms(2026, 9, 28, 8, 0, 0).unwrap();
    /// assert_eq!(set.next_open("oncall", sunday).unwrap(), monday_1000_paris);
    /// ```
    pub fn next_open(&self, name: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, WindowError> {
        let window = self.get(name)?;
        if self.is_open(name, now)? {
            return Ok(now);
        }
        let pending_adhoc = self
            .adhoc
            .iter()
            .filter(|adhoc| adhoc.name == name && adhoc.from > now)
            .map(|adhoc| adhoc.from);
        window
            .next_open(now)
            .into_iter()
            .chain(pending_adhoc)
            .min()
            .ok_or_else(|| WindowError::NoUpcomingOpening {
                name: name.to_string(),
                from: now,
                horizon_days: NEXT_OPEN_HORIZON_DAYS,
            })
    }

    /// Open the named window for `duration` starting at `now`, regardless of
    /// its recurring schedule. Intended for a human-operated CLI; returns the
    /// override that was recorded.
    ///
    /// ```
    /// use chrono::{Duration, TimeZone, Utc};
    /// use harness::windows::WindowSet;
    ///
    /// let mut set = WindowSet::parse(
    ///     "[[window]]\nname = \"oncall\"\ntimezone = \"Europe/Paris\"\ndays = [\"mon\"]\nstart = \"10:00\"\nend = \"12:00\"\napplies_to = [\"production\"]\n",
    /// )
    /// .unwrap();
    /// let sunday = Utc.with_ymd_and_hms(2026, 9, 27, 12, 0, 0).unwrap();
    /// assert!(!set.is_open("oncall", sunday).unwrap());
    /// set.open_adhoc("oncall", Duration::hours(2), sunday).unwrap();
    /// assert!(set.is_open("oncall", sunday).unwrap());
    /// assert!(!set.is_open("oncall", sunday + Duration::hours(2)).unwrap());
    /// ```
    pub fn open_adhoc(
        &mut self,
        name: &str,
        duration: Duration,
        now: DateTime<Utc>,
    ) -> Result<AdhocWindow, WindowError> {
        let window = self.get(name)?;
        if duration <= Duration::zero() {
            return Err(WindowError::NonPositiveDuration(duration));
        }
        let adhoc = AdhocWindow {
            name: window.name.clone(),
            from: now,
            until: now + duration,
        };
        self.adhoc.push(adhoc.clone());
        Ok(adhoc)
    }
}

impl FromStr for WindowSet {
    type Err = WindowError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use proptest::prelude::*;

    const EXAMPLE: &str = r#"
[[window]]
name = "business-hours"
timezone = "Europe/Paris"
days = ["mon", "tue", "wed", "thu", "fri"]
start = "09:30"
end = "17:00"
applies_to = ["production"]
holidays = ["2026-12-25"]

[[window]]
name = "oncall"
timezone = "Europe/Paris"
days = ["mon", "tue", "wed", "thu"]
start = "10:00"
end = "12:00"
applies_to = ["production"]
"#;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn example() -> WindowSet {
        WindowSet::parse(EXAMPLE).unwrap()
    }

    fn single(days: &str, start: &str, end: &str) -> WindowSet {
        let src = format!(
            "[[window]]\nname = \"w\"\ntimezone = \"Europe/Paris\"\ndays = [{days}]\nstart = \"{start}\"\nend = \"{end}\"\napplies_to = [\"production\"]\n"
        );
        WindowSet::parse(&src).unwrap()
    }

    #[test]
    fn parses_issue_example() {
        let set = example();
        let names: Vec<&str> = set.windows().map(Window::name).collect();
        assert_eq!(names, vec!["business-hours", "oncall"]);
        let bh = set.window("business-hours").unwrap();
        assert_eq!(bh.timezone(), chrono_tz::Europe::Paris);
        assert_eq!(bh.start(), NaiveTime::from_hms_opt(9, 30, 0).unwrap());
        assert_eq!(bh.end(), NaiveTime::from_hms_opt(17, 0, 0).unwrap());
        assert_eq!(
            bh.days(),
            vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri
            ]
        );
        assert!(bh.applies_to().contains(&EffectLevel::Production));
        assert_eq!(
            bh.holidays(),
            vec![NaiveDate::from_ymd_opt(2026, 12, 25).unwrap()]
        );
        assert!(set.window("nope").is_none());
        assert!(set.adhoc_windows().is_empty());
    }

    #[test]
    fn from_str_matches_parse() {
        let set: WindowSet = EXAMPLE.parse().unwrap();
        assert_eq!(set.windows().count(), 2);
    }

    #[test]
    fn windows_for_filters_by_effect_level() {
        let set = example();
        assert_eq!(set.windows_for(EffectLevel::Production).count(), 2);
        assert_eq!(set.windows_for(EffectLevel::Sandbox).count(), 0);
    }

    #[test]
    fn open_during_business_hours() {
        let set = example();
        assert!(set
            .is_open("business-hours", utc(2026, 9, 23, 10, 0))
            .unwrap());
        assert_eq!(
            set.next_open("business-hours", utc(2026, 9, 23, 10, 0))
                .unwrap(),
            utc(2026, 9, 23, 10, 0)
        );
    }

    #[test]
    fn window_next_open_returns_now_when_already_open() {
        let set = example();
        let window = set.window("oncall").unwrap();
        let open = utc(2026, 9, 23, 8, 30);
        assert!(window.is_open_at(open));
        assert_eq!(window.next_open(open), Some(open));
        let closed = utc(2026, 9, 23, 12, 0);
        assert!(!window.is_open_at(closed));
        assert_eq!(window.next_open(closed), Some(utc(2026, 9, 24, 8, 0)));
    }

    #[test]
    fn closed_on_weekend_and_reopens_monday() {
        let set = example();
        let saturday_noon = utc(2026, 9, 26, 10, 0);
        assert!(!set.is_open("business-hours", saturday_noon).unwrap());
        assert_eq!(
            set.next_open("business-hours", saturday_noon).unwrap(),
            utc(2026, 9, 28, 7, 30)
        );
    }

    #[test]
    fn closed_after_hours_reopens_next_morning() {
        let set = example();
        let evening = utc(2026, 9, 23, 16, 0);
        assert!(!set.is_open("business-hours", evening).unwrap());
        assert_eq!(
            set.next_open("business-hours", evening).unwrap(),
            utc(2026, 9, 24, 7, 30)
        );
    }

    #[test]
    fn closed_on_holiday() {
        let set = example();
        let christmas = utc(2026, 12, 25, 10, 0);
        assert!(!set.is_open("business-hours", christmas).unwrap());
        assert_eq!(
            set.next_open("business-hours", christmas).unwrap(),
            utc(2026, 12, 28, 8, 30)
        );
    }

    #[test]
    fn spring_forward_gap_opens_at_transition_instant() {
        let set = single("\"sun\"", "02:30", "04:00");
        let before = utc(2026, 3, 29, 0, 59);
        assert!(!set.is_open("w", before).unwrap());
        let opens = set.next_open("w", before).unwrap();
        assert_eq!(opens, utc(2026, 3, 29, 1, 0));
        assert!(set.is_open("w", opens).unwrap());
        assert!(set.is_open("w", utc(2026, 3, 29, 1, 30)).unwrap());
        assert!(!set.is_open("w", utc(2026, 3, 29, 2, 0)).unwrap());
    }

    #[test]
    fn fall_back_uses_earliest_ambiguous_instant() {
        let set = single("\"sun\"", "02:30", "03:30");
        let before = utc(2026, 10, 24, 23, 0);
        assert_eq!(
            set.next_open("w", before).unwrap(),
            utc(2026, 10, 25, 0, 30)
        );
        assert!(set.is_open("w", utc(2026, 10, 25, 0, 30)).unwrap());
        assert!(!set.is_open("w", utc(2026, 10, 25, 1, 0)).unwrap());
        assert!(set.is_open("w", utc(2026, 10, 25, 1, 30)).unwrap());
        assert!(!set.is_open("w", utc(2026, 10, 25, 2, 30)).unwrap());
    }

    #[test]
    fn fall_back_business_hours_track_local_clock() {
        let set = example();
        assert!(!set
            .is_open("business-hours", utc(2026, 10, 23, 7, 0))
            .unwrap());
        assert!(set
            .is_open("business-hours", utc(2026, 10, 23, 7, 30))
            .unwrap());
        assert!(!set
            .is_open("business-hours", utc(2026, 10, 26, 8, 0))
            .unwrap());
        assert!(set
            .is_open("business-hours", utc(2026, 10, 26, 8, 30))
            .unwrap());
    }

    #[test]
    fn window_spanning_fall_back_lasts_an_extra_hour() {
        let set = single("\"sun\"", "01:00", "05:00");
        assert!(set.is_open("w", utc(2026, 10, 24, 23, 0)).unwrap());
        assert!(set.is_open("w", utc(2026, 10, 25, 3, 59)).unwrap());
        assert!(!set.is_open("w", utc(2026, 10, 25, 4, 0)).unwrap());
    }

    #[test]
    fn skipped_calendar_day_is_never_open() {
        let src = "[[window]]\nname = \"w\"\ntimezone = \"Pacific/Apia\"\ndays = [\"fri\"]\nstart = \"09:00\"\nend = \"17:00\"\napplies_to = [\"production\"]\n";
        let set = WindowSet::parse(src).unwrap();
        let now = utc(2011, 12, 29, 0, 0);
        assert_eq!(set.next_open("w", now).unwrap(), utc(2012, 1, 5, 19, 0));
    }

    #[test]
    fn no_upcoming_opening_when_every_eligible_day_is_a_holiday() {
        let first = NaiveDate::from_ymd_opt(2026, 9, 28).unwrap();
        let holidays: Vec<String> = (0..=NEXT_OPEN_HORIZON_DAYS / 7 + 2)
            .map(|week| format!("\"{}\"", first + Duration::weeks(week)))
            .collect();
        let src = format!(
            "[[window]]\nname = \"w\"\ntimezone = \"UTC\"\ndays = [\"mon\"]\nstart = \"09:00\"\nend = \"17:00\"\napplies_to = [\"production\"]\nholidays = [{}]\n",
            holidays.join(", ")
        );
        let set = WindowSet::parse(&src).unwrap();
        let err = set.next_open("w", utc(2026, 9, 26, 0, 0)).unwrap_err();
        assert!(matches!(err, WindowError::NoUpcomingOpening { .. }));
        assert!(err.to_string().contains("`w`"));
    }

    #[test]
    fn unknown_window_is_an_error() {
        let mut set = example();
        let now = utc(2026, 9, 23, 10, 0);
        assert!(matches!(
            set.is_open("nope", now),
            Err(WindowError::UnknownWindow(n)) if n == "nope"
        ));
        assert!(matches!(
            set.next_open("nope", now),
            Err(WindowError::UnknownWindow(_))
        ));
        assert!(matches!(
            set.open_adhoc("nope", Duration::hours(1), now),
            Err(WindowError::UnknownWindow(_))
        ));
    }

    #[test]
    fn adhoc_window_opens_a_closed_window_temporarily() {
        let mut set = example();
        let saturday_noon = utc(2026, 9, 26, 10, 0);
        let adhoc = set
            .open_adhoc("business-hours", Duration::hours(2), saturday_noon)
            .unwrap();
        assert_eq!(adhoc.name, "business-hours");
        assert_eq!(adhoc.from, saturday_noon);
        assert_eq!(adhoc.until, utc(2026, 9, 26, 12, 0));
        assert_eq!(set.adhoc_windows(), &[adhoc]);
        assert!(set.is_open("business-hours", saturday_noon).unwrap());
        assert!(set
            .is_open("business-hours", utc(2026, 9, 26, 11, 59))
            .unwrap());
        assert!(!set
            .is_open("business-hours", utc(2026, 9, 26, 12, 0))
            .unwrap());
        assert!(!set.is_open("oncall", saturday_noon).unwrap());
        assert_eq!(
            set.next_open("business-hours", utc(2026, 9, 26, 9, 0))
                .unwrap(),
            saturday_noon
        );
        assert_eq!(
            set.next_open("business-hours", utc(2026, 9, 26, 12, 0))
                .unwrap(),
            utc(2026, 9, 28, 7, 30)
        );
    }

    #[test]
    fn adhoc_window_rejects_non_positive_duration() {
        let mut set = example();
        let now = utc(2026, 9, 26, 10, 0);
        assert!(matches!(
            set.open_adhoc("business-hours", Duration::zero(), now),
            Err(WindowError::NonPositiveDuration(_))
        ));
        assert!(matches!(
            set.open_adhoc("business-hours", Duration::minutes(-5), now),
            Err(WindowError::NonPositiveDuration(_))
        ));
        assert!(set.adhoc_windows().is_empty());
    }

    fn invalid_field(src: &str) -> (String, &'static str, String) {
        let WindowError::InvalidField {
            window,
            field,
            reason,
        } = WindowSet::parse(src).unwrap_err()
        else {
            unreachable!()
        };
        (window, field, reason)
    }

    fn with(overrides: &str) -> String {
        let mut fields = std::collections::BTreeMap::from([
            ("name", "\"w\"".to_string()),
            ("timezone", "\"Europe/Paris\"".to_string()),
            ("days", "[\"mon\"]".to_string()),
            ("start", "\"09:00\"".to_string()),
            ("end", "\"17:00\"".to_string()),
            ("applies_to", "[\"production\"]".to_string()),
        ]);
        for line in overrides.lines().filter(|l| !l.is_empty()) {
            let (k, v) = line.split_once(" = ").unwrap();
            fields.insert(k, v.to_string());
        }
        let body: Vec<String> = fields.iter().map(|(k, v)| format!("{k} = {v}")).collect();
        format!("[[window]]\n{}\n", body.join("\n"))
    }

    #[test]
    fn validation_errors_name_the_field() {
        let cases: [(&str, &'static str); 12] = [
            ("name = \"\"", "name"),
            ("timezone = \"Mars/Olympus\"", "timezone"),
            ("days = []", "days"),
            ("days = [\"funday\"]", "days"),
            ("days = [\"mon\", \"mon\"]", "days"),
            ("start = \"9am\"", "start"),
            ("end = \"25:00\"", "end"),
            ("end = \"09:00\"", "end"),
            ("end = \"08:00\"", "end"),
            ("applies_to = []", "applies_to"),
            ("applies_to = [\"galactic\"]", "applies_to"),
            ("holidays = [\"2026-13-01\"]", "holidays"),
        ];
        for (override_line, expected_field) in cases {
            let (window, field, reason) = invalid_field(&with(override_line));
            assert_eq!(field, expected_field, "case {override_line}");
            assert!(!reason.is_empty(), "case {override_line}");
            assert_eq!(window, if expected_field == "name" { "" } else { "w" });
        }
    }

    #[test]
    fn validation_error_display_mentions_window_and_field() {
        let err = WindowSet::parse(&with("start = \"noon\"")).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("`w`"), "{text}");
        assert!(text.contains("`start`"), "{text}");
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let src = format!("{}{}", with(""), with(""));
        assert!(matches!(
            WindowSet::parse(&src),
            Err(WindowError::DuplicateName(n)) if n == "w"
        ));
    }

    #[test]
    fn unknown_keys_are_a_parse_error() {
        let src = with("colour = \"blue\"");
        assert!(matches!(WindowSet::parse(&src), Err(WindowError::Parse(_))));
    }

    #[test]
    fn empty_file_yields_empty_set() {
        let set = WindowSet::parse("").unwrap();
        assert_eq!(set.windows().count(), 0);
    }

    #[test]
    fn day_names_are_case_insensitive_and_accept_long_forms() {
        let set = single("\"Monday\", \"TUE\"", "09:00", "10:00");
        assert_eq!(
            set.window("w").unwrap().days(),
            vec![Weekday::Mon, Weekday::Tue]
        );
    }

    #[test]
    fn load_reads_file_and_reports_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(WINDOWS_FILE_NAME);
        std::fs::write(&path, EXAMPLE).unwrap();
        let set = WindowSet::load(&path).unwrap();
        assert_eq!(set.windows().count(), 2);

        let missing = dir.path().join("absent.toml");
        let err = WindowSet::load(&missing).unwrap_err();
        assert!(matches!(err, WindowError::Io { .. }));
        assert!(err.to_string().contains("absent.toml"));
    }

    #[test]
    fn gating_defaults_gate_production_only() {
        let gating = WindowGating::default();
        assert_eq!(
            gating,
            WindowGating {
                production: true,
                sandbox: false
            }
        );
        assert!(gating.requires_window(EffectLevel::Production));
        assert!(!gating.requires_window(EffectLevel::Sandbox));
        assert!(!gating.requires_window(EffectLevel::Local));
    }

    #[test]
    fn gating_can_opt_sandbox_in_and_production_out() {
        let gating = WindowGating {
            production: false,
            sandbox: true,
        };
        assert!(!gating.requires_window(EffectLevel::Production));
        assert!(gating.requires_window(EffectLevel::Sandbox));
        assert!(!gating.requires_window(EffectLevel::Local));
    }

    #[test]
    fn gating_by_level_name() {
        let gating = WindowGating::default();
        assert!(gating.requires_window_named("Production").unwrap());
        assert!(!gating.requires_window_named("sandbox").unwrap());
        assert!(!gating.requires_window_named("LOCAL").unwrap());
        assert!(matches!(
            gating.requires_window_named("galactic"),
            Err(WindowError::UnknownEffectLevel(n)) if n == "galactic"
        ));
    }

    #[test]
    fn effect_level_round_trips_through_display() {
        for level in EffectLevel::ALL {
            let parsed: EffectLevel = level.to_string().parse().unwrap();
            assert_eq!(parsed, level);
        }
        assert!(EffectLevel::Local < EffectLevel::Sandbox);
        assert!(EffectLevel::Sandbox < EffectLevel::Production);
    }

    #[test]
    fn parse_duration_accepts_minutes_hours_days() {
        assert_eq!(parse_duration("30m").unwrap(), Duration::minutes(30));
        assert_eq!(parse_duration("2h").unwrap(), Duration::hours(2));
        assert_eq!(parse_duration("1d").unwrap(), Duration::days(1));
        assert_eq!(parse_duration("0h").unwrap(), Duration::zero());
    }

    #[test]
    fn parse_duration_rejects_malformed_input() {
        for bad in ["", "2", "h", "2x", "-1h", "1.5h", "２h", "2é"] {
            assert!(
                matches!(parse_duration(bad), Err(WindowError::InvalidDuration(s)) if s == bad),
                "input {bad:?}"
            );
        }
    }

    const ZONES: [&str; 7] = [
        "UTC",
        "Europe/Paris",
        "America/New_York",
        "America/Santiago",
        "Australia/Lord_Howe",
        "Pacific/Apia",
        "Asia/Kolkata",
    ];

    fn arb_window_toml() -> impl Strategy<Value = String> {
        (
            0..ZONES.len(),
            1u8..128,
            0u32..1439,
            proptest::collection::vec(0u32..730, 0..4),
        )
            .prop_flat_map(|(zone, day_mask, start, holiday_offsets)| {
                ((start + 1)..1440).prop_map(move |end| {
                    let days: Vec<String> = (0u8..7)
                        .filter(|bit| day_mask & (1 << bit) != 0)
                        .map(|bit| format!("\"{}\"", Weekday::try_from(bit).unwrap()))
                        .collect();
                    let base = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
                    let holidays: Vec<String> = holiday_offsets
                        .iter()
                        .map(|off| format!("\"{}\"", base + Duration::days(i64::from(*off))))
                        .collect();
                    format!(
                        "[[window]]\nname = \"w\"\ntimezone = \"{}\"\ndays = [{}]\nstart = \"{:02}:{:02}\"\nend = \"{:02}:{:02}\"\napplies_to = [\"production\"]\nholidays = [{}]\n",
                        ZONES[zone],
                        days.join(", "),
                        start / 60,
                        start % 60,
                        end / 60,
                        end % 60,
                        holidays.join(", ")
                    )
                })
            })
    }

    fn arb_now() -> impl Strategy<Value = DateTime<Utc>> {
        (1_735_689_600i64..1_798_761_600).prop_map(|secs| Utc.timestamp_opt(secs, 0).unwrap())
    }

    proptest! {
        #[test]
        fn next_open_is_not_before_now_and_is_open(src in arb_window_toml(), now in arb_now()) {
            let set = WindowSet::parse(&src).unwrap();
            let next = set.next_open("w", now).unwrap();
            prop_assert!(next >= now);
            prop_assert!(set.is_open("w", next).unwrap());
        }

        #[test]
        fn next_open_with_adhoc_override_holds(
            src in arb_window_toml(),
            now in arb_now(),
            opened_ago in 0i64..(48 * 60),
            minutes in 1i64..(24 * 60),
        ) {
            let mut set = WindowSet::parse(&src).unwrap();
            let opened_at = now - Duration::minutes(opened_ago);
            set.open_adhoc("w", Duration::minutes(minutes), opened_at).unwrap();
            let next = set.next_open("w", now).unwrap();
            prop_assert!(next >= now);
            prop_assert!(set.is_open("w", next).unwrap());
        }

        #[test]
        fn is_open_implies_next_open_is_now(src in arb_window_toml(), now in arb_now()) {
            let set = WindowSet::parse(&src).unwrap();
            if set.is_open("w", now).unwrap() {
                prop_assert_eq!(set.next_open("w", now).unwrap(), now);
            }
        }
    }
}
