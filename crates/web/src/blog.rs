use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use pulldown_cmark::{html, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    pub slug: String,
    pub title: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// "25 October 2026".
    pub display_date: String,
    pub summary: Option<String>,
    pub author: Option<String>,
    pub draft: bool,
    /// Sanitized.
    pub html: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    NoFrontMatter,
    Yaml(String),
    BadDate(String),
    BadSlug(String),
    EmptyTitle,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::NoFrontMatter => write!(
                f,
                "no front matter: the file must start with a --- line and close it with another"
            ),
            ParseError::Yaml(e) => write!(f, "front matter: {e}"),
            ParseError::BadDate(d) => write!(f, "date {d:?} is not a valid YYYY-MM-DD date"),
            ParseError::BadSlug(s) => write!(
                f,
                "slug {s:?} must be lowercase letters, digits, and hyphens"
            ),
            ParseError::EmptyTitle => write!(f, "title is empty"),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontMatter {
    title: String,
    date: String,
    slug: Option<String>,
    summary: Option<String>,
    author: Option<String>,
    #[serde(default)]
    draft: bool,
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn display_date(date: &str) -> Option<String> {
    let mut parts = date.splitn(3, '-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let (y, m, d): (u32, usize, u32) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if d == 0 || d > days {
        return None;
    }
    Some(format!("{d} {} {y}", MONTHS[m - 1]))
}

fn valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

fn split_front_matter(src: &str) -> Option<(&str, &str)> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let rest = src
        .strip_prefix("---\n")
        .or_else(|| src.strip_prefix("---\r\n"))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

/// Markdown headings move down one level so the post title stays the page's
/// only `<h1>`.
pub fn render_markdown(md: &str) -> String {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let demote = |l: HeadingLevel| match l {
        HeadingLevel::H1 => HeadingLevel::H2,
        HeadingLevel::H2 => HeadingLevel::H3,
        HeadingLevel::H3 => HeadingLevel::H4,
        HeadingLevel::H4 => HeadingLevel::H5,
        _ => HeadingLevel::H6,
    };
    let events = Parser::new_ext(md, opts).map(|e| match e {
        Event::Start(Tag::Heading {
            level,
            id,
            classes,
            attrs,
        }) => Event::Start(Tag::Heading {
            level: demote(level),
            id,
            classes,
            attrs,
        }),
        Event::End(TagEnd::Heading(level)) => Event::End(TagEnd::Heading(demote(level))),
        e => e,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    ammonia::clean(&out)
}

pub fn parse(file_stem: &str, src: &str) -> Result<Post, ParseError> {
    let (yaml, body) = split_front_matter(src).ok_or(ParseError::NoFrontMatter)?;
    let fm: FrontMatter =
        serde_saphyr::from_str(yaml).map_err(|e| ParseError::Yaml(e.to_string()))?;
    let title = fm.title.trim().to_string();
    if title.is_empty() {
        return Err(ParseError::EmptyTitle);
    }
    let display_date =
        display_date(&fm.date).ok_or_else(|| ParseError::BadDate(fm.date.clone()))?;
    let slug = fm.slug.unwrap_or_else(|| file_stem.to_string());
    if !valid_slug(&slug) {
        return Err(ParseError::BadSlug(slug));
    }
    Ok(Post {
        slug,
        title,
        date: fm.date,
        display_date,
        summary: fm
            .summary
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        author: fm
            .author
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        draft: fm.draft,
        html: render_markdown(body),
    })
}

struct Entry {
    modified: SystemTime,
    len: u64,
    post: Option<Arc<Post>>,
}

#[derive(Default)]
struct State {
    files: HashMap<PathBuf, Entry>,
    posts: Arc<Vec<Arc<Post>>>,
    scanned: Option<Instant>,
}

/// Posts from a directory of Markdown files. The directory is rescanned at
/// most once per `interval`; only files whose size or modification time
/// changed are read again.
pub struct Blog {
    dir: PathBuf,
    interval: Duration,
    state: Mutex<State>,
}

impl Blog {
    pub fn new(dir: impl Into<PathBuf>, interval: Duration) -> Blog {
        Blog {
            dir: dir.into(),
            interval,
            state: Mutex::new(State::default()),
        }
    }

    /// Published posts, newest first.
    pub fn posts(&self) -> Arc<Vec<Arc<Post>>> {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if st.scanned.is_none_or(|t| t.elapsed() >= self.interval) {
            rescan(&self.dir, &mut st);
            st.scanned = Some(Instant::now());
        }
        st.posts.clone()
    }

    pub fn post(&self, slug: &str) -> Option<Arc<Post>> {
        self.posts().iter().find(|p| p.slug == slug).cloned()
    }
}

fn rescan(dir: &Path, st: &mut State) {
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(dir = %dir.display(), error = %e, "blog: cannot read directory");
            }
            st.files.clear();
            st.posts = Arc::default();
            return;
        }
    };

    let mut seen = HashSet::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let len = meta.len();
        seen.insert(path.clone());

        if let Some(e) = st.files.get(&path) {
            if e.modified == modified && e.len == len {
                continue;
            }
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let post = match std::fs::read_to_string(&path) {
            Ok(src) => match parse(stem, &src) {
                Ok(p) => Some(Arc::new(p)),
                Err(e) => {
                    tracing::warn!(file = %path.display(), "blog: skipped: {e}");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "blog: cannot read file");
                None
            }
        };
        st.files.insert(
            path,
            Entry {
                modified,
                len,
                post,
            },
        );
    }
    st.files.retain(|p, _| seen.contains(p));

    let mut posts: Vec<(&PathBuf, Arc<Post>)> = st
        .files
        .iter()
        .filter_map(|(path, e)| e.post.clone().map(|p| (path, p)))
        .filter(|(_, p)| !p.draft)
        .collect();
    posts.sort_by(|a, b| b.1.date.cmp(&a.1.date).then_with(|| a.0.cmp(b.0)));

    let mut slugs = HashSet::new();
    let mut out = Vec::with_capacity(posts.len());
    for (path, post) in posts {
        if slugs.insert(post.slug.clone()) {
            out.push(post);
        } else {
            tracing::warn!(file = %path.display(), slug = %post.slug, "blog: skipped: slug already used");
        }
    }
    st.posts = Arc::new(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "---\ntitle: Hello\ndate: 2026-10-25\nsummary: First post\n---\n# Section\n\nBody *text*.\n";

    #[test]
    fn parses_front_matter_and_body() {
        let p = parse("hello-world", GOOD).unwrap();
        assert_eq!(p.slug, "hello-world");
        assert_eq!(p.title, "Hello");
        assert_eq!(p.date, "2026-10-25");
        assert_eq!(p.display_date, "25 October 2026");
        assert_eq!(p.summary.as_deref(), Some("First post"));
        assert!(!p.draft);
        assert!(p.html.contains("<h2>Section</h2>"), "{}", p.html);
        assert!(p.html.contains("<em>text</em>"));
    }

    #[test]
    fn crlf_and_bom_are_accepted() {
        let src = "\u{feff}---\r\ntitle: Hi\r\ndate: 2026-01-02\r\n---\r\nBody\r\n";
        assert_eq!(parse("x", src).unwrap().title, "Hi");
    }

    #[test]
    fn slug_can_be_set() {
        let src = "---\ntitle: T\ndate: 2026-10-25\nslug: launch-notes\n---\n";
        assert_eq!(
            parse("2026-10-25-launch", src).unwrap().slug,
            "launch-notes"
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(
            parse("x", "no front matter"),
            Err(ParseError::NoFrontMatter)
        );
        assert_eq!(
            parse("x", "---\ntitle: T\ndate: 2026-10-25\n"),
            Err(ParseError::NoFrontMatter)
        );
        assert!(matches!(
            parse("x", "---\ndate: 2026-10-25\n---\n"),
            Err(ParseError::Yaml(_))
        ));
        assert!(matches!(
            parse("x", "---\ntitle: T\ndate: 2026-10-25\ntitel: typo\n---\n"),
            Err(ParseError::Yaml(_))
        ));
        assert_eq!(
            parse("x", "---\ntitle: '  '\ndate: 2026-10-25\n---\n"),
            Err(ParseError::EmptyTitle)
        );
        for d in [
            "2026-02-29",
            "2026-13-01",
            "2026-1-01",
            "25-10-2026",
            "yesterday",
        ] {
            let src = format!("---\ntitle: T\ndate: {d}\n---\n");
            assert!(
                matches!(parse("x", &src), Err(ParseError::BadDate(_))),
                "{d}"
            );
        }
        assert!(parse("x", "---\ntitle: T\ndate: 2028-02-29\n---\n").is_ok());
        for s in ["Has-Caps", "-lead", "trail-", "a b", "../up", ""] {
            let src = format!("---\ntitle: T\ndate: 2026-10-25\nslug: \"{s}\"\n---\n");
            assert!(
                matches!(parse("x", &src), Err(ParseError::BadSlug(_))),
                "{s:?}"
            );
        }
    }

    #[test]
    fn html_is_sanitized() {
        let html = render_markdown(
            "<script>alert(1)</script>\n\n[x](javascript:alert(1)) <img src=x onerror=alert(1)>",
        );
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("javascript:"), "{html}");
        assert!(!html.contains("onerror"), "{html}");
    }

    #[test]
    fn headings_never_produce_h1() {
        let html = render_markdown("# a\n## b\n###### c");
        assert!(!html.contains("<h1"));
        assert!(
            html.contains("<h2>a</h2>")
                && html.contains("<h3>b</h3>")
                && html.contains("<h6>c</h6>")
        );
    }
}
