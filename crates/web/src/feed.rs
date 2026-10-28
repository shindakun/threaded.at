//! Atom 1.0 feeds (RFC 4287) for the public listings and the blog.
//!
//! A feed is the signed-out view of its page: no reader's hidden stories or
//! tag filters. Feed routes sit outside the cookie layer, so a response sets
//! no cookie and one cached copy serves every subscriber.

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Redirect, Response};
use threaded_store::{Filter, Listing, StoryRow};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::{markdown, AppError, AppState, SITE_NAME};

/// Entries per feed.
pub const FEED_SIZE: i64 = 30;

/// A feed's address and name, for the `<link rel="alternate">` on the page
/// it follows and the visible link under the list.
pub struct FeedLink {
    pub path: String,
    pub title: String,
}

impl FeedLink {
    pub fn new(path: impl Into<String>, title: impl Into<String>) -> FeedLink {
        FeedLink {
            path: path.into(),
            title: title.into(),
        }
    }
}

struct Entry {
    /// An absolute URL on this site; it never changes for the item.
    id: String,
    title: String,
    /// Where the entry points: a story's link, or its page for a text post.
    link: String,
    /// The discussion page, when it differs from `link`.
    replies: Option<String>,
    published: i64,
    updated: i64,
    /// Name and profile URI. A blog post without an author takes the feed's.
    author: Option<(String, Option<String>)>,
    categories: Vec<String>,
    summary: Option<String>,
    /// Sanitized HTML; written escaped, as `type="html"`.
    content: String,
}

struct Feed {
    /// This feed's absolute URL, also its ID.
    url: String,
    /// The page it follows.
    page: String,
    title: String,
    /// For entries without their own author.
    author: Option<String>,
    /// For a feed with no entries.
    now: i64,
    base: String,
    entries: Vec<Entry>,
}

/// Escapes text for the HTML inside an entry's content.
fn html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// RFC 3339 in UTC, as Atom dates require.
fn date(secs: i64) -> String {
    OffsetDateTime::from_unix_timestamp(secs)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Escapes text for XML and drops the characters XML 1.0 cannot carry at
/// all, such as most control characters, which a record from the network
/// could hold. One bad character would make the whole feed unreadable.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{fffe}' || c == '\u{ffff}' => {}
            c => out.push(c),
        }
    }
    out
}

impl Feed {
    fn render(&self) -> String {
        let updated = self
            .entries
            .iter()
            .map(|e| e.updated)
            .max()
            .unwrap_or(self.now);
        let mut x = String::new();
        x.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
        x.push_str("<feed xmlns=\"http://www.w3.org/2005/Atom\">\n");
        x.push_str(&format!("<id>{}</id>\n", esc(&self.url)));
        x.push_str(&format!("<title>{}</title>\n", esc(&self.title)));
        x.push_str(&format!(
            "<link rel=\"self\" type=\"application/atom+xml\" href=\"{}\"/>\n",
            esc(&self.url)
        ));
        x.push_str(&format!(
            "<link rel=\"alternate\" type=\"text/html\" href=\"{}\"/>\n",
            esc(&self.page)
        ));
        x.push_str(&format!("<updated>{}</updated>\n", date(updated)));
        if let Some(a) = &self.author {
            x.push_str(&format!("<author><name>{}</name></author>\n", esc(a)));
        }
        for e in &self.entries {
            x.push_str("<entry>\n");
            x.push_str(&format!("<id>{}</id>\n", esc(&e.id)));
            x.push_str(&format!("<title>{}</title>\n", esc(&e.title)));
            x.push_str(&format!(
                "<link rel=\"alternate\" type=\"text/html\" href=\"{}\"/>\n",
                esc(&e.link)
            ));
            if let Some(r) = &e.replies {
                x.push_str(&format!(
                    "<link rel=\"replies\" type=\"text/html\" href=\"{}\"/>\n",
                    esc(r)
                ));
            }
            x.push_str(&format!("<published>{}</published>\n", date(e.published)));
            x.push_str(&format!("<updated>{}</updated>\n", date(e.updated)));
            if let Some((name, uri)) = &e.author {
                x.push_str(&format!("<author><name>{}</name>", esc(name)));
                if let Some(u) = uri {
                    x.push_str(&format!("<uri>{}</uri>", esc(u)));
                }
                x.push_str("</author>\n");
            }
            for c in &e.categories {
                x.push_str(&format!("<category term=\"{}\"/>\n", esc(c)));
            }
            if let Some(s) = &e.summary {
                x.push_str(&format!("<summary>{}</summary>\n", esc(s)));
            }
            // The base sits on the content alone, where a blog post's
            // relative links need it. On the feed it would make the front
            // page's own link, the base itself, a same-document reference.
            x.push_str(&format!(
                "<content type=\"html\" xml:base=\"{}\">{}</content>\n",
                esc(&self.base),
                esc(&e.content)
            ));
            x.push_str("</entry>\n");
        }
        x.push_str("</feed>\n");
        x
    }

    fn response(&self) -> Response {
        (
            [
                (header::CONTENT_TYPE, "application/atom+xml; charset=utf-8"),
                (header::CACHE_CONTROL, "public, max-age=300"),
            ],
            self.render(),
        )
            .into_response()
    }
}

fn story_entry(st: &AppState, r: StoryRow) -> Entry {
    let o = &st.origin;
    let page = o.url(&format!("/s/{}", r.short_id));
    let author_uri = o.url(&format!("/u/{}", r.author_did));
    let tags: Vec<String> = r.tag_list().iter().map(|t| t.to_string()).collect();
    let mut content = String::new();
    if !r.body.trim().is_empty() {
        content.push_str(&markdown::user(&r.body));
    }
    // The page's own links, with the story's tags, author, and discussion.
    let tag_links: Vec<String> = tags
        .iter()
        .map(|t| format!("<a href=\"{}\">{t}</a>", o.url(&format!("/t/{t}"))))
        .collect();
    content.push_str(&format!(
        "<p>{}{}by <a href=\"{}\">{}</a> | <a href=\"{}\">Comments</a></p>",
        tag_links.join(", "),
        if tag_links.is_empty() { "" } else { " | " },
        html(&author_uri),
        html(&r.handle),
        page,
    ));
    let link = r.url.clone().unwrap_or_else(|| page.clone());
    Entry {
        replies: (link != page).then(|| page.clone()),
        id: page,
        title: r.title,
        link,
        published: r.created_at,
        updated: r.edited_at.unwrap_or(r.created_at).max(r.created_at),
        author: Some((r.handle, Some(author_uri))),
        categories: tags,
        summary: None,
        content,
    }
}

fn story_feed(st: &AppState, path: &str, page: &str, title: String, rows: Vec<StoryRow>) -> Feed {
    Feed {
        url: st.origin.url(path),
        page: st.origin.url(page),
        title,
        author: None,
        now: (st.clock)(),
        base: st.origin.url("/"),
        entries: rows.into_iter().map(|r| story_entry(st, r)).collect(),
    }
}

async fn listed(
    st: &AppState,
    listing: Listing,
    filter: Filter,
) -> Result<Vec<StoryRow>, AppError> {
    Ok(st.store.list(listing, &filter, FEED_SIZE, 0).await?)
}

pub async fn front(State(st): State<AppState>) -> Result<Response, AppError> {
    let rows = listed(&st, Listing::Front, Filter::default()).await?;
    let title = format!("{SITE_NAME}: front page");
    Ok(story_feed(&st, "/feed", "/", title, rows).response())
}

pub async fn newest(State(st): State<AppState>) -> Result<Response, AppError> {
    let rows = listed(&st, Listing::New, Filter::default()).await?;
    let title = format!("{SITE_NAME}: newest stories");
    Ok(story_feed(&st, "/newest/feed", "/newest", title, rows).response())
}

/// Newest first, unlike the ranked tag page: a subscriber wants each new
/// story once, not whichever ones rank this hour.
pub async fn tag(
    State(st): State<AppState>,
    Path(tag): Path<String>,
) -> Result<Response, AppError> {
    if !threaded_core::tag::valid_name(&tag) {
        return Err(AppError::NotFound);
    }
    let filter = Filter {
        tag: Some(tag.clone()),
        ..Default::default()
    };
    let rows = listed(&st, Listing::New, filter).await?;
    let title = format!("{SITE_NAME}: stories tagged {tag}");
    let feed = story_feed(
        &st,
        &format!("/t/{tag}/feed"),
        &format!("/t/{tag}"),
        title,
        rows,
    );
    Ok(feed.response())
}

pub async fn domain(
    State(st): State<AppState>,
    Path(domain): Path<String>,
) -> Result<Response, AppError> {
    let domain = crate::domain_name(&domain).ok_or(AppError::NotFound)?;
    let filter = Filter {
        domain: Some(domain.clone()),
        ..Default::default()
    };
    let rows = listed(&st, Listing::New, filter).await?;
    let title = format!("{SITE_NAME}: stories from {domain}");
    let feed = story_feed(
        &st,
        &format!("/domain/{domain}/feed"),
        &format!("/domain/{domain}"),
        title,
        rows,
    );
    Ok(feed.response())
}

/// A user's stories. The feed's address holds the DID; a handle finds the
/// account and redirects there, since a handle can later pass to someone
/// else and a subscription should stay with the account.
pub async fn user(
    State(st): State<AppState>,
    Path(who): Path<String>,
) -> Result<Response, AppError> {
    let user = st
        .store
        .user_by_handle(&who)
        .await?
        .ok_or(AppError::NotFound)?;
    if user.did != who {
        return Ok(Redirect::permanent(&format!("/u/{}/feed", user.did)).into_response());
    }
    let rows = st.store.stories_by_user(user.id, None, FEED_SIZE).await?;
    let title = format!("{SITE_NAME}: stories by {}", user.name());
    let feed = story_feed(
        &st,
        &format!("/u/{}/feed", user.did),
        &format!("/u/{}", user.did),
        title,
        rows,
    );
    Ok(feed.response())
}

/// Midnight UTC on a blog post's `YYYY-MM-DD` date.
fn blog_date(d: &str) -> i64 {
    let mut parts = d.splitn(3, '-').map(|p| p.parse::<i64>().unwrap_or(1));
    let (y, m, d) = (
        parts.next().unwrap_or(1970) as i32,
        parts.next().unwrap_or(1) as u8,
        parts.next().unwrap_or(1) as u8,
    );
    time::Month::try_from(m)
        .ok()
        .and_then(|m| time::Date::from_calendar_date(y, m, d).ok())
        .map(|d| d.midnight().assume_utc().unix_timestamp())
        .unwrap_or(0)
}

pub async fn blog(State(st): State<AppState>) -> Response {
    let entries = st
        .blog
        .posts()
        .iter()
        .take(FEED_SIZE as usize)
        .map(|p| {
            let url = st.origin.url(&format!("/blog/{}", p.slug));
            let when = blog_date(&p.date);
            Entry {
                id: url.clone(),
                title: p.title.clone(),
                link: url,
                replies: None,
                published: when,
                updated: when,
                author: p.author.clone().map(|a| (a, None)),
                categories: Vec::new(),
                summary: p.summary.clone(),
                content: p.html.clone(),
            }
        })
        .collect();
    Feed {
        url: st.origin.url("/blog/feed"),
        page: st.origin.url("/blog"),
        title: format!("{SITE_NAME} blog"),
        author: Some(SITE_NAME.to_string()),
        now: (st.clock)(),
        base: st.origin.url("/"),
        entries,
    }
    .response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_escaped_and_cleaned_for_xml() {
        assert_eq!(esc(r#"a & <b> "c""#), "a &amp; &lt;b&gt; &quot;c&quot;");
        assert_eq!(
            esc("tab\there\nnull\u{0}bell\u{7}\u{ffff}ok"),
            "tab\there\nnullbellok"
        );
        assert_eq!(esc("café ✓"), "café ✓");
    }

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01T00:00:00Z");
        assert_eq!(date(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(blog_date("2026-10-25"), 1_792_886_400);
        assert_eq!(date(blog_date("2028-02-29")), "2028-02-29T00:00:00Z");
    }
}
