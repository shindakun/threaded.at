//! The numbers moderation and posting run on. Admins change them on
//! `/admin/settings`; a value nobody has set keeps its default here.

use crate::level::{Level, Thresholds};

#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    pub thresholds: Thresholds,
    /// Stories per day and comments per hour for New accounts.
    pub new_limits: (i64, i64),
    /// Stories per day and comments per hour for everyone else.
    pub member_limits: (i64, i64),
    /// A URL posted again within this many days goes to the first story.
    pub duplicate_days: i64,
    /// Stories close to new comments after this many days.
    pub lock_days: i64,
    /// An author may change a story's URL for this many seconds.
    pub url_edit_secs: i64,
    pub story_flag_days: i64,
    /// Stories at or below this score take no more flags.
    pub story_flag_floor: i64,
    pub comment_flag_days: i64,
    pub comment_flag_floor: i64,
    /// Items with this many flags enter the moderation queue.
    pub flags_to_queue: i64,
    /// Below this share of confirmed holds over the last 100, triage stops
    /// holding and only suggests.
    pub hold_precision_floor: f64,
    pub triage_model: String,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            thresholds: Thresholds::default(),
            new_limits: (2, 6),
            member_limits: (10, 30),
            duplicate_days: 30,
            lock_days: 90,
            url_edit_secs: 3_600,
            story_flag_days: 14,
            story_flag_floor: -5,
            comment_flag_days: 7,
            comment_flag_floor: -10,
            flags_to_queue: 3,
            hold_precision_floor: 0.95,
            triage_model: "claude-opus-5-5".to_string(),
        }
    }
}

impl Policy {
    /// Stories per day and comments per hour a level may post.
    pub fn limits(&self, level: Level) -> (i64, i64) {
        match level {
            Level::New => self.new_limits,
            _ => self.member_limits,
        }
    }
}

/// A setting an admin can change on `/admin/settings`.
#[derive(Debug, Clone, Copy)]
pub struct Setting {
    pub key: &'static str,
    pub label: &'static str,
    /// The lowest and highest accepted value, for numbers.
    pub range: Option<(f64, f64)>,
}

const fn n(key: &'static str, label: &'static str, lo: f64, hi: f64) -> Setting {
    Setting {
        key,
        label,
        range: Some((lo, hi)),
    }
}

pub const SETTINGS: &[Setting] = &[
    n(
        "new_days",
        "Days before a new account is a member",
        0.0,
        365.0,
    ),
    n("trusted_karma", "Karma to become trusted", 0.0, 10_000.0),
    n("flag_karma", "Karma to flag comments", 0.0, 10_000.0),
    n("suggest_karma", "Karma to suggest edits", 0.0, 10_000.0),
    n(
        "submit_karma",
        "Lowest karma that can submit",
        -1_000.0,
        0.0,
    ),
    n(
        "new_stories_per_day",
        "Stories per day, new accounts",
        0.0,
        100.0,
    ),
    n(
        "new_comments_per_hour",
        "Comments per hour, new accounts",
        0.0,
        1_000.0,
    ),
    n(
        "member_stories_per_day",
        "Stories per day, everyone else",
        0.0,
        100.0,
    ),
    n(
        "member_comments_per_hour",
        "Comments per hour, everyone else",
        0.0,
        1_000.0,
    ),
    n(
        "duplicate_days",
        "Days a link counts as a duplicate",
        0.0,
        365.0,
    ),
    n("lock_days", "Days before a story closes", 1.0, 3_650.0),
    n(
        "url_edit_minutes",
        "Minutes an author may change a link",
        0.0,
        1_440.0,
    ),
    n("story_flag_days", "Days a story can be flagged", 0.0, 365.0),
    n(
        "story_flag_floor",
        "Score at which a story takes no more flags",
        -1_000.0,
        0.0,
    ),
    n(
        "comment_flag_days",
        "Days a comment can be flagged",
        0.0,
        365.0,
    ),
    n(
        "comment_flag_floor",
        "Score at which a comment takes no more flags",
        -1_000.0,
        0.0,
    ),
    n(
        "flags_to_queue",
        "Flags that put an item in the queue",
        1.0,
        100.0,
    ),
    n(
        "hold_precision_floor",
        "Confirmed share of holds below which triage stops holding",
        0.0,
        1.0,
    ),
    Setting {
        key: "triage_model",
        label: "Triage model ID",
        range: None,
    },
];

impl Policy {
    /// The policy with stored values applied over the defaults. A stored
    /// value that no longer parses is skipped, keeping the default.
    pub fn from_settings<'a>(stored: impl IntoIterator<Item = (&'a str, &'a str)>) -> Policy {
        let mut p = Policy::default();
        for (k, v) in stored {
            let _ = p.set(k, v);
        }
        p
    }

    /// A setting's current value as the settings form shows it.
    pub fn get(&self, key: &str) -> Option<String> {
        let t = &self.thresholds;
        Some(match key {
            "new_days" => t.new_days.to_string(),
            "trusted_karma" => t.trusted_karma.to_string(),
            "flag_karma" => t.flag_karma.to_string(),
            "suggest_karma" => t.suggest_karma.to_string(),
            "submit_karma" => t.submit_karma.to_string(),
            "new_stories_per_day" => self.new_limits.0.to_string(),
            "new_comments_per_hour" => self.new_limits.1.to_string(),
            "member_stories_per_day" => self.member_limits.0.to_string(),
            "member_comments_per_hour" => self.member_limits.1.to_string(),
            "duplicate_days" => self.duplicate_days.to_string(),
            "lock_days" => self.lock_days.to_string(),
            "url_edit_minutes" => (self.url_edit_secs / 60).to_string(),
            "story_flag_days" => self.story_flag_days.to_string(),
            "story_flag_floor" => self.story_flag_floor.to_string(),
            "comment_flag_days" => self.comment_flag_days.to_string(),
            "comment_flag_floor" => self.comment_flag_floor.to_string(),
            "flags_to_queue" => self.flags_to_queue.to_string(),
            "hold_precision_floor" => self.hold_precision_floor.to_string(),
            "triage_model" => self.triage_model.clone(),
            _ => return None,
        })
    }

    /// Sets one value, checking its type and range.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let setting = SETTINGS
            .iter()
            .find(|s| s.key == key)
            .ok_or_else(|| format!("There is no setting {key}."))?;
        let value = value.trim();
        if key == "triage_model" {
            let ok = !value.is_empty()
                && value.len() <= 100
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b));
            if !ok {
                return Err("A model ID is letters, digits, and - . _ only.".into());
            }
            self.triage_model = value.to_string();
            return Ok(());
        }
        let (lo, hi) = setting.range.unwrap_or((f64::MIN, f64::MAX));
        let bad = || format!("{} is a number from {lo} to {hi}.", setting.label);
        if key == "hold_precision_floor" {
            let v: f64 = value.parse().map_err(|_| bad())?;
            if !(lo..=hi).contains(&v) {
                return Err(bad());
            }
            self.hold_precision_floor = v;
            return Ok(());
        }
        let v: i64 = value.parse().map_err(|_| bad())?;
        if !(lo..=hi).contains(&(v as f64)) {
            return Err(bad());
        }
        let t = &mut self.thresholds;
        match key {
            "new_days" => t.new_days = v,
            "trusted_karma" => t.trusted_karma = v,
            "flag_karma" => t.flag_karma = v,
            "suggest_karma" => t.suggest_karma = v,
            "submit_karma" => t.submit_karma = v,
            "new_stories_per_day" => self.new_limits.0 = v,
            "new_comments_per_hour" => self.new_limits.1 = v,
            "member_stories_per_day" => self.member_limits.0 = v,
            "member_comments_per_hour" => self.member_limits.1 = v,
            "duplicate_days" => self.duplicate_days = v,
            "lock_days" => self.lock_days = v,
            "url_edit_minutes" => self.url_edit_secs = v * 60,
            "story_flag_days" => self.story_flag_days = v,
            "story_flag_floor" => self.story_flag_floor = v,
            "comment_flag_days" => self.comment_flag_days = v,
            "comment_flag_floor" => self.comment_flag_floor = v,
            "flags_to_queue" => self.flags_to_queue = v,
            _ => return Err(format!("There is no setting {key}.")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_accounts_post_less() {
        let p = Policy::default();
        assert_eq!(p.limits(Level::New), (2, 6));
        assert_eq!(p.limits(Level::Member), (10, 30));
        assert_eq!(p.limits(Level::Admin), (10, 30));
    }

    #[test]
    fn every_setting_reads_and_writes() {
        let mut p = Policy::default();
        for s in SETTINGS {
            let v = p
                .get(s.key)
                .unwrap_or_else(|| panic!("{} has no getter", s.key));
            p.set(s.key, &v)
                .unwrap_or_else(|e| panic!("{}: {e}", s.key));
        }
        assert_eq!(
            p,
            Policy::default(),
            "writing back the defaults changes nothing"
        );
    }

    #[test]
    fn values_are_checked() {
        let mut p = Policy::default();
        p.set("new_days", "30").unwrap();
        assert_eq!(p.thresholds.new_days, 30);
        p.set("url_edit_minutes", "5").unwrap();
        assert_eq!(p.url_edit_secs, 300);
        assert!(p.set("new_days", "-1").is_err());
        assert!(p.set("new_days", "ten").is_err());
        assert!(p.set("hold_precision_floor", "1.5").is_err());
        assert!(p.set("triage_model", "x; rm").is_err());
        assert!(p.set("nope", "1").is_err());
    }

    #[test]
    fn stored_values_apply_over_defaults_and_bad_ones_are_skipped() {
        let p = Policy::from_settings([("lock_days", "30"), ("flags_to_queue", "x")]);
        assert_eq!(p.lock_days, 30);
        assert_eq!(p.flags_to_queue, 3);
    }
}
