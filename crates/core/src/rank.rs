/// Seconds of age that cost one order of magnitude of score.
pub const RANK_WINDOW: f64 = 60.0 * 60.0 * 22.0;

/// Bonus for a link story submitted by its own author.
pub const AUTHOR_BONUS: f64 = 0.25;

#[derive(Debug, Clone, Copy, Default)]
pub struct RankInput {
    pub score: i64,
    /// Sum of `(comment score + 1)` over comments not written by the submitter.
    pub comment_points: f64,
    /// Sum of `rank_boost` over the story's tags.
    pub tag_boost: f64,
    pub user_is_author: bool,
    pub is_link: bool,
    /// Unix seconds.
    pub created_at: i64,
}

/// Higher is hotter. Stored on the story row and sorted descending.
pub fn compute(h: RankInput) -> f64 {
    let base = h.tag_boost
        + if h.user_is_author && h.is_link {
            AUTHOR_BONUS
        } else {
            0.0
        };

    // Capping at the raw score would let a negative score cancel its own
    // weight in `order`, so a story at -10 would rank level with one at 0.
    let cpoints = if base < 0.0 {
        0.0
    } else {
        (h.comment_points * 0.5).min(h.score.max(0) as f64)
    };

    let order = (((h.score + 1).abs() as f64) + cpoints).max(1.0).log10();
    let sign = h.score.signum() as f64;

    order * sign + base + h.created_at as f64 / RANK_WINDOW
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_790_000_000;

    fn at(score: i64, created_at: i64) -> RankInput {
        RankInput {
            score,
            created_at,
            ..Default::default()
        }
    }

    #[test]
    fn newer_beats_older_at_equal_score() {
        assert!(compute(at(5, T0 + 3600)) > compute(at(5, T0)));
    }

    #[test]
    fn higher_score_beats_lower_at_equal_age() {
        assert!(compute(at(20, T0)) > compute(at(3, T0)));
    }

    #[test]
    fn ten_times_the_score_offsets_one_window() {
        let old = compute(at(99, T0));
        let new = compute(at(9, T0 + RANK_WINDOW as i64));
        assert!((old - new).abs() < 1e-9);
    }

    #[test]
    fn negative_score_sinks() {
        assert!(compute(at(-3, T0)) < compute(at(0, T0)));
    }

    #[test]
    fn comment_points_are_capped_at_score() {
        let quiet = RankInput {
            score: 2,
            comment_points: 4.0,
            created_at: T0,
            ..Default::default()
        };
        let flame = RankInput {
            comment_points: 400.0,
            ..quiet
        };
        assert_eq!(compute(quiet), compute(flame));
    }

    #[test]
    fn negative_tag_boost_ignores_comments() {
        let h = RankInput {
            score: 10,
            comment_points: 20.0,
            tag_boost: -0.5,
            created_at: T0,
            ..Default::default()
        };
        let silent = RankInput {
            comment_points: 0.0,
            ..h
        };
        assert_eq!(compute(h), compute(silent));
    }

    #[test]
    fn author_bonus_needs_a_link() {
        let text = RankInput {
            score: 1,
            user_is_author: true,
            created_at: T0,
            ..Default::default()
        };
        let link = RankInput {
            is_link: true,
            ..text
        };
        assert!((compute(link) - compute(text) - AUTHOR_BONUS).abs() < 1e-9);
    }
}
