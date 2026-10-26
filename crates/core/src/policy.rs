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
}
