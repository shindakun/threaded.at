//! Search query syntax:
//!
//! `rust async "error handling" tag:rust -tag:job domain:example.com
//! submitter:alice.bsky.social commenter:bob title:release sort:new`

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Relevance,
    New,
    Score,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub terms: Vec<String>,
    pub phrases: Vec<String>,
    pub not_terms: Vec<String>,
    pub title_terms: Vec<String>,
    pub tags: Vec<String>,
    pub not_tags: Vec<String>,
    pub domains: Vec<String>,
    pub not_domains: Vec<String>,
    pub submitter: Option<String>,
    pub commenter: Option<String>,
    pub sort: Sort,
}

impl Query {
    pub fn is_empty(&self) -> bool {
        self.fts().is_none()
            && self.tags.is_empty()
            && self.not_tags.is_empty()
            && self.domains.is_empty()
            && self.not_domains.is_empty()
            && self.submitter.is_none()
            && self.commenter.is_none()
    }

    /// An FTS5 MATCH expression with every token quoted, so user input can
    /// never reach FTS5 operator syntax. `None` when there is no text part.
    pub fn fts(&self) -> Option<String> {
        let mut pos: Vec<String> = Vec::new();
        pos.extend(self.terms.iter().map(|t| quote(t)));
        pos.extend(self.phrases.iter().map(|p| quote(p)));
        pos.extend(
            self.title_terms
                .iter()
                .map(|t| format!("title:{}", quote(t))),
        );
        if pos.is_empty() {
            return None;
        }
        let mut expr = pos.join(" AND ");
        for n in &self.not_terms {
            expr.push_str(" NOT ");
            expr.push_str(&quote(n));
        }
        Some(expr)
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn handle(v: &str) -> String {
    v.trim_start_matches(['@', '~']).to_ascii_lowercase()
}

pub fn parse(input: &str) -> Query {
    let mut q = Query::default();
    let mut rest = input.trim();

    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('"') {
            let (phrase, tail) = match after.find('"') {
                Some(end) => (&after[..end], &after[end + 1..]),
                None => (after, ""),
            };
            let phrase = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
            if !phrase.is_empty() {
                q.phrases.push(phrase.to_lowercase());
            }
            rest = tail.trim_start();
            continue;
        }

        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = &rest[..end];
        rest = rest[end..].trim_start();

        let (neg, word) = match word.strip_prefix('-') {
            Some(w) if !w.is_empty() => (true, w),
            _ => (false, word),
        };

        match word.split_once(':') {
            Some(("tag", v)) if !v.is_empty() => {
                let v = v.to_ascii_lowercase();
                if neg {
                    q.not_tags.push(v)
                } else {
                    q.tags.push(v)
                }
            }
            Some(("domain", v)) if !v.is_empty() => {
                let v = v.trim_start_matches("www.").to_ascii_lowercase();
                if neg {
                    q.not_domains.push(v)
                } else {
                    q.domains.push(v)
                }
            }
            Some(("submitter", v)) if !v.is_empty() && !neg => q.submitter = Some(handle(v)),
            Some(("commenter", v)) if !v.is_empty() && !neg => q.commenter = Some(handle(v)),
            Some(("title", v)) if !v.is_empty() && !neg => q.title_terms.push(v.to_lowercase()),
            Some(("sort", v)) if !neg => {
                q.sort = match v {
                    "new" => Sort::New,
                    "score" | "top" => Sort::Score,
                    _ => Sort::Relevance,
                }
            }
            _ => {
                let t = word.to_lowercase();
                if neg {
                    q.not_terms.push(t)
                } else {
                    q.terms.push(t)
                }
            }
        }
    }
    q
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_terms() {
        let q = parse("Rust  async");
        assert_eq!(q.terms, ["rust", "async"]);
        assert_eq!(q.fts().unwrap(), r#""rust" AND "async""#);
    }

    #[test]
    fn operators() {
        let q = parse(
            r#"tag:Rust -tag:job domain:www.Example.com submitter:@Alice.bsky.social commenter:~bob title:release sort:new "error  handling" -crypto"#,
        );
        assert_eq!(q.tags, ["rust"]);
        assert_eq!(q.not_tags, ["job"]);
        assert_eq!(q.domains, ["example.com"]);
        assert_eq!(q.submitter.as_deref(), Some("alice.bsky.social"));
        assert_eq!(q.commenter.as_deref(), Some("bob"));
        assert_eq!(q.title_terms, ["release"]);
        assert_eq!(q.phrases, ["error handling"]);
        assert_eq!(q.not_terms, ["crypto"]);
        assert_eq!(q.sort, Sort::New);
        assert_eq!(
            q.fts().unwrap(),
            r#""error handling" AND title:"release" NOT "crypto""#
        );
    }

    #[test]
    fn fts_syntax_is_quoted_away() {
        let q = parse(r#"NEAR(a b) title:x* "unterminated"#);
        assert_eq!(
            q.fts().unwrap(),
            r#""near(a" AND "b)" AND "unterminated" AND title:"x*""#
        );
    }

    #[test]
    fn embedded_quotes_are_doubled() {
        let q = parse(r#"it"s"#);
        assert_eq!(q.fts().unwrap(), r#""it""s""#);
    }

    #[test]
    fn filters_only_has_no_fts() {
        let q = parse("tag:rust");
        assert!(q.fts().is_none());
        assert!(!q.is_empty());
        assert!(parse("   ").is_empty());
    }

    #[test]
    fn lone_dash_is_a_term() {
        assert_eq!(parse("-").terms, ["-"]);
    }
}
