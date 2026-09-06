//! Calendar evaluation uses a bundled IANA database, never the machine's zone.
use anyhow::{Context, Result, ensure};
use jiff::{
    Timestamp,
    civil::Date,
    tz::{AmbiguousOffset, TimeZone},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Schedule {
    Once {
        at: i64,
    },
    Daily {
        timezone: String,
        hour: i8,
        minute: i8,
    },
    Triggered {
        poll_seconds: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Occurrence {
    pub(crate) at: i64,
    pub(crate) local_date: String,
    pub(crate) dst_adjusted: bool,
}

impl Schedule {
    pub(crate) fn validate(&self) -> Result<()> {
        match self {
            Self::Triggered { poll_seconds } => ensure!(
                (5..=3600).contains(poll_seconds),
                "trigger poll interval must be 5–3600 seconds"
            ),
            Self::Once { at } => {
                ensure!(*at > 0, "one-shot instant must be after the Unix epoch");
                Timestamp::from_second(*at)?;
            }
            Self::Daily {
                timezone,
                hour,
                minute,
            } => {
                ensure!(
                    timezone.len() <= 128 && (0..24).contains(hour) && (0..60).contains(minute),
                    "invalid daily schedule"
                );
                TimeZone::get(timezone)?;
            }
        }
        Ok(())
    }

    pub(crate) fn first(&self, now: i64) -> Result<Occurrence> {
        self.validate()?;
        match self {
            Self::Triggered { poll_seconds } => Ok(Occurrence {
                at: now.saturating_add(i64::from(*poll_seconds)),
                local_date: String::new(),
                dst_adjusted: false,
            }),
            Self::Once { at } => Ok(Occurrence {
                at: *at,
                local_date: String::new(),
                dst_adjusted: false,
            }),
            Self::Daily { .. } => self
                .after(now)?
                .context("daily schedule has no next occurrence"),
        }
    }

    /// Missed occurrences collapse into the already saved occurrence. Computing
    /// its successor jumps to today's date instead of iterating missed days.
    pub(crate) fn after(&self, now: i64) -> Result<Option<Occurrence>> {
        if let Self::Triggered { .. } = self {
            return self.first(now).map(Some);
        }
        let Self::Daily {
            timezone,
            hour,
            minute,
        } = self
        else {
            return Ok(None);
        };
        let zone = TimeZone::get(timezone)?;
        let mut date = Timestamp::from_second(now)?.to_zoned(zone.clone()).date();
        for _ in 0..3 {
            let occurrence = on_date(&zone, date, *hour, *minute)?;
            if occurrence.at > now {
                return Ok(Some(occurrence));
            }
            date = date.tomorrow()?;
        }
        anyhow::bail!("timezone has no next bounded calendar occurrence")
    }
}

fn on_date(zone: &TimeZone, date: Date, hour: i8, minute: i8) -> Result<Occurrence> {
    let civil = date.at(hour, minute, 0, 0);
    let ambiguous = zone.to_ambiguous_timestamp(civil);
    let (timestamp, dst_adjusted) = match ambiguous.offset() {
        AmbiguousOffset::Gap { after, .. } => {
            // Offset-preserving disambiguation turns 02:30 into 03:30. Instead
            // choose the transition itself: the first existing instant (03:00).
            let before_transition = after.to_timestamp(civil)?;
            let transition = zone
                .following(before_transition)
                .next()
                .context("timezone gap has no transition")?;
            let timestamp = transition.timestamp();
            ensure!(
                timestamp.as_second() - before_transition.as_second() <= 48 * 3600,
                "timezone gap exceeds bound"
            );
            (timestamp, true)
        }
        _ => (ambiguous.earlier()?, false),
    };
    Ok(Occurrence {
        at: timestamp.as_second(),
        local_date: date.to_string(),
        dst_adjusted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seconds(value: &str) -> i64 {
        value.parse::<Timestamp>().unwrap().as_second()
    }
    #[test]
    fn gaps_use_first_valid_instant_and_folds_run_once() {
        let zone = TimeZone::get("America/New_York").unwrap();
        let gap = on_date(&zone, "2026-03-08".parse().unwrap(), 2, 30).unwrap();
        assert_eq!(gap.at, seconds("2026-03-08T07:00:00Z"));
        assert!(gap.dst_adjusted);
        let fold = on_date(&zone, "2026-11-01".parse().unwrap(), 1, 30).unwrap();
        assert_eq!(fold.at, seconds("2026-11-01T05:30:00Z"));
        let schedule = Schedule::Daily {
            timezone: "America/New_York".into(),
            hour: 1,
            minute: 30,
        };
        assert_eq!(
            schedule.after(fold.at).unwrap().unwrap().at,
            seconds("2026-11-02T06:30:00Z")
        );
    }
    #[test]
    fn skipped_date_and_years_of_sleep_are_bounded() {
        let zone = TimeZone::get("Pacific/Apia").unwrap();
        let gap = on_date(&zone, "2011-12-30".parse().unwrap(), 12, 0).unwrap();
        assert_eq!(gap.at, seconds("2011-12-30T10:00:00Z"));
        let schedule = Schedule::Daily {
            timezone: "Etc/UTC".into(),
            hour: 8,
            minute: 0,
        };
        assert_eq!(
            schedule
                .after(seconds("2030-01-01T12:00:00Z"))
                .unwrap()
                .unwrap()
                .at,
            seconds("2030-01-02T08:00:00Z")
        );
    }
}
