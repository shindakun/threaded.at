pub const MAX_LEN: usize = 25;
pub const MAX_PER_STORY: usize = 5;

/// Tag names are lowercase ASCII letters, digits, and `-`, `_`, `+`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_+".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("rust"));
        assert!(valid_name("c++"));
        assert!(valid_name("show-hn_2"));
        assert!(!valid_name(""));
        assert!(!valid_name("Rust"));
        assert!(!valid_name("has space"));
        assert!(!valid_name(&"a".repeat(26)));
    }
}
