use crate::windows::{WindowError, WindowSet};
use chrono::{DateTime, Utc};

/// Instant a task should be parked until so that it runs inside the named
/// availability window, or `None` when the window is open at `now`.
///
/// Feed the result into [`super::QueuedTask::with_not_before`]; the
/// dispatcher then skips the entry, without charging a slot, until the
/// window opens.
///
/// ```
/// use chrono::{TimeZone, Utc};
/// use harness::scheduler::parked_until;
/// use harness::windows::WindowSet;
///
/// let set = WindowSet::parse(
///     "[[window]]\nname = \"oncall\"\ntimezone = \"Europe/Paris\"\ndays = [\"mon\"]\nstart = \"10:00\"\nend = \"12:00\"\napplies_to = [\"production\"]\n",
/// )
/// .unwrap();
/// let sunday = Utc.with_ymd_and_hms(2026, 9, 27, 12, 0, 0).unwrap();
/// let monday_1000_paris = Utc.with_ymd_and_hms(2026, 9, 28, 8, 0, 0).unwrap();
/// assert_eq!(parked_until(&set, "oncall", sunday).unwrap(), Some(monday_1000_paris));
/// assert_eq!(parked_until(&set, "oncall", monday_1000_paris).unwrap(), None);
/// assert!(parked_until(&set, "missing", sunday).is_err());
/// ```
pub fn parked_until(
    windows: &WindowSet,
    window_name: &str,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, WindowError> {
    let opens_at = windows.next_open(window_name, now)?;
    Ok((opens_at > now).then_some(opens_at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const CONFIG: &str = "[[window]]\nname = \"oncall\"\ntimezone = \"UTC\"\ndays = [\"mon\"]\nstart = \"10:00\"\nend = \"12:00\"\napplies_to = [\"production\"]\n";

    #[test]
    fn parks_until_next_opening() {
        let set = WindowSet::parse(CONFIG).unwrap();
        let sunday = Utc.with_ymd_and_hms(2026, 9, 27, 12, 0, 0).unwrap();
        let monday = Utc.with_ymd_and_hms(2026, 9, 28, 10, 0, 0).unwrap();
        assert_eq!(parked_until(&set, "oncall", sunday).unwrap(), Some(monday));
    }

    #[test]
    fn open_window_needs_no_parking() {
        let set = WindowSet::parse(CONFIG).unwrap();
        let monday = Utc.with_ymd_and_hms(2026, 9, 28, 11, 0, 0).unwrap();
        assert_eq!(parked_until(&set, "oncall", monday).unwrap(), None);
    }

    #[test]
    fn unknown_window_is_an_error() {
        let set = WindowSet::parse(CONFIG).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 9, 28, 11, 0, 0).unwrap();
        assert!(matches!(
            parked_until(&set, "missing", now),
            Err(WindowError::UnknownWindow(_))
        ));
    }
}
