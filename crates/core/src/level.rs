//! User levels and what each one may do.
//!
//! Levels below Moderator are derived from tenure and karma on every request,
//! so they rise and fall on their own. Moderator and Admin are granted roles.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    New,
    Member,
    Trusted,
    Moderator,
    Admin,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::New => "new",
            Level::Member => "member",
            Level::Trusted => "trusted",
            Level::Moderator => "moderator",
            Level::Admin => "admin",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Role {
    #[default]
    User,
    Moderator,
    Admin,
}

impl Role {
    pub fn from_db(s: &str) -> Role {
        match s {
            "moderator" => Role::Moderator,
            "admin" => Role::Admin,
            _ => Role::User,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Account {
    /// Days since first sign-in here, not the age of the atproto account.
    pub tenure_days: i64,
    pub karma: i64,
    pub role: Role,
    pub banned: bool,
    /// A Trusted user vouched for this account, ending probation early.
    pub vouched: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    pub new_days: i64,
    pub trusted_karma: i64,
    pub flag_karma: i64,
    pub suggest_karma: i64,
    pub submit_karma: i64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            new_days: 70,
            trusted_karma: 50,
            flag_karma: 50,
            suggest_karma: 10,
            submit_karma: -4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Read,
    Upvote,
    Comment,
    Submit,
    /// Use a tag marked `new_users_ok = 0`.
    UseRestrictedTag,
    FlagStory,
    FlagComment,
    SuggestEdit,
    Vouch,
    /// Use a tag marked `privileged`.
    UsePrivilegedTag,
    Moderate,
    ManageTags,
    GrantRoles,
}

pub fn level(a: &Account, t: &Thresholds) -> Level {
    match a.role {
        Role::Admin => return Level::Admin,
        Role::Moderator => return Level::Moderator,
        Role::User => {}
    }
    if a.tenure_days < t.new_days && !a.vouched {
        Level::New
    } else if a.karma >= t.trusted_karma {
        Level::Trusted
    } else {
        Level::Member
    }
}

/// Stories per day and comments per hour a level may post.
pub fn limits(level: Level) -> (i64, i64) {
    match level {
        Level::New => (2, 6),
        _ => (10, 30),
    }
}

pub fn can(a: &Account, cap: Capability, t: &Thresholds) -> bool {
    use Capability::*;
    if a.banned {
        return cap == Read;
    }
    let lvl = level(a, t);
    match cap {
        Read | Upvote | Comment => true,
        Submit => a.karma >= t.submit_karma,
        UseRestrictedTag => lvl >= Level::Member,
        FlagStory => lvl >= Level::Member,
        FlagComment => lvl >= Level::Moderator || (lvl >= Level::Member && a.karma >= t.flag_karma),
        SuggestEdit => lvl >= Level::Member && a.karma >= t.suggest_karma,
        Vouch => lvl >= Level::Trusted,
        UsePrivilegedTag | Moderate => lvl >= Level::Moderator,
        ManageTags | GrantRoles => lvl == Level::Admin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(tenure_days: i64, karma: i64) -> Account {
        Account {
            tenure_days,
            karma,
            ..Default::default()
        }
    }

    #[test]
    fn levels_follow_tenure_and_karma() {
        let t = Thresholds::default();
        assert_eq!(level(&acct(3, 500), &t), Level::New);
        assert_eq!(level(&acct(100, 10), &t), Level::Member);
        assert_eq!(level(&acct(100, 50), &t), Level::Trusted);
    }

    #[test]
    fn vouching_ends_probation() {
        let t = Thresholds::default();
        let a = Account {
            vouched: true,
            ..acct(1, 0)
        };
        assert_eq!(level(&a, &t), Level::Member);
    }

    #[test]
    fn roles_override_karma() {
        let t = Thresholds::default();
        let a = Account {
            role: Role::Moderator,
            ..acct(0, -100)
        };
        assert_eq!(level(&a, &t), Level::Moderator);
        assert!(can(&a, Capability::Moderate, &t));
        assert!(!can(&a, Capability::GrantRoles, &t));
    }

    #[test]
    fn new_users_cannot_flag_or_use_restricted_tags() {
        let t = Thresholds::default();
        let a = acct(5, 0);
        assert!(can(&a, Capability::Comment, &t));
        assert!(can(&a, Capability::Submit, &t));
        assert!(!can(&a, Capability::FlagComment, &t));
        assert!(!can(&a, Capability::UseRestrictedTag, &t));
    }

    #[test]
    fn story_flags_need_tenure_comment_flags_need_karma() {
        let t = Thresholds::default();
        assert!(!can(&acct(5, 500), Capability::FlagStory, &t));
        assert!(!can(&acct(5, 500), Capability::FlagComment, &t));
        assert!(can(&acct(100, 0), Capability::FlagStory, &t));
        assert!(!can(&acct(100, 49), Capability::FlagComment, &t));
        assert!(can(&acct(100, 50), Capability::FlagComment, &t));
    }

    #[test]
    fn negative_karma_blocks_submitting() {
        let t = Thresholds::default();
        assert!(!can(&acct(200, -5), Capability::Submit, &t));
        assert!(can(&acct(200, -4), Capability::Submit, &t));
    }

    #[test]
    fn new_users_post_less() {
        assert_eq!(limits(Level::New), (2, 6));
        assert_eq!(limits(Level::Member), (10, 30));
        assert_eq!(limits(Level::Admin), (10, 30));
    }

    #[test]
    fn banned_users_only_read() {
        let t = Thresholds::default();
        let a = Account {
            banned: true,
            role: Role::Admin,
            ..acct(900, 9000)
        };
        assert!(can(&a, Capability::Read, &t));
        assert!(!can(&a, Capability::Comment, &t));
        assert!(!can(&a, Capability::Moderate, &t));
    }
}
