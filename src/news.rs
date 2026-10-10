//! DayZ news: the developers' announcements on Steam, from Steam's news API (no key needed).
//!
//! Steam sends each post's start as a plain-text snippet, but stray BBCode and HTML get through,
//! and sentences often run together where the post had line breaks; [`clean`] tidies that up.

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::http;
use crate::steam::DAYZ_APP;

const NEWS: &str = "https://api.steampowered.com/ISteamNews/GetNewsForApp/v2/";
/// The developers' own posts; other feeds are news sites writing about the game.
const FEED: &str = "steam_community_announcements";
/// Posts to ask for.
const COUNT: usize = 10;
/// Characters of each post Steam sends.
const SNIPPET: usize = 300;
/// A post older than this doesn't count as new, even if it hasn't been seen: for the first run,
/// and for coming back after a long break.
const NEW_FOR: u64 = 30 * 24 * 60 * 60;

/// Where all of DayZ's news is, on Steam's store.
pub fn all_news_page() -> String {
    format!("https://store.steampowered.com/news/app/{DAYZ_APP}")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub gid: String,
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// Unix time it was posted; 0 if Steam didn't say.
    pub date: u64,
}

/// The latest posts, newest first.
pub fn fetch() -> Result<Vec<Item>> {
    let (count, snippet) = (COUNT.to_string(), SNIPPET.to_string());
    let body = http::agent()
        .get(NEWS)
        .query("appid", DAYZ_APP.to_string())
        .query("count", &count)
        .query("maxlength", &snippet)
        .query("feeds", FEED)
        .call()?
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_vec()?;
    parse(&body)
}

/// Reads Steam's answer. Posts it can't make sense of are skipped rather than failing the rest.
pub fn parse(body: &[u8]) -> Result<Vec<Item>> {
    let value: Value = serde_json::from_slice(body).context("Steam's answer isn't JSON")?;
    let Some(posts) = value
        .get("appnews")
        .and_then(|a| a.get("newsitems"))
        .and_then(Value::as_array)
    else {
        bail!("Steam's answer has no news in it");
    };
    let mut items: Vec<Item> = posts.iter().filter_map(item).collect();
    items.sort_by_key(|i| std::cmp::Reverse(i.date));
    let mut seen = std::collections::HashSet::new();
    items.retain(|i| i.gid.is_empty() || seen.insert(i.gid.clone()));
    Ok(items)
}

fn item(post: &Value) -> Option<Item> {
    let text = |key: &str| post.get(key).and_then(Value::as_str).unwrap_or_default();
    // Ids are strings, but they're too long for some JSON readers, so allow numbers as well.
    let gid = match post.get("gid") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    let date = match post.get("date") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    };
    let title = clean(text("title"));
    if title.is_empty() {
        return None;
    }
    Some(Item {
        url: link(text("url"), &gid),
        snippet: clean(text("contents")),
        title,
        gid,
        date,
    })
}

/// The post's page: its own address when it's a web page, else its page on Steam.
fn link(url: &str, gid: &str) -> String {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    if (lower.starts_with("https://") || lower.starts_with("http://"))
        && !url.chars().any(char::is_whitespace)
    {
        url.to_string()
    } else if !gid.is_empty() && gid.chars().all(|c| c.is_ascii_digit()) {
        format!("{}/view/{gid}", all_news_page())
    } else {
        all_news_page()
    }
}

/// BBCode tags Steam posts use, which should never show as text.
const BBCODE: &[&str] = &[
    "b",
    "i",
    "u",
    "s",
    "strike",
    "url",
    "img",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "list",
    "olist",
    "*",
    "quote",
    "code",
    "spoiler",
    "noparse",
    "hr",
    "table",
    "tr",
    "td",
    "th",
    "p",
    "previewyoutube",
    "video",
    "carousel",
    "expand",
    "dynamiclink",
];

/// The text of a snippet or title, without markup: drops HTML and BBCode tags (including one cut
/// off at the end), decodes HTML entities, puts single spaces between words, and adds the space
/// missing where Steam joined two sentences ("Survivors!We").
pub fn clean(text: &str) -> String {
    let text = decode_entities(&strip_tags(text));
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_whitespace() || c.is_control() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            continue;
        }
        out.push(c);
        let next = chars.get(i + 1).copied().unwrap_or(' ');
        let before = |n: usize| i.checked_sub(n).map(|j| chars[j]).unwrap_or(' ');
        if matches!(c, '.' | '!' | '?' | ',')
            && next.is_uppercase()
            && (before(1) == ')' || (before(1).is_alphabetic() && before(2).is_alphabetic()))
        {
            out.push(' ');
        }
    }
    let mut out = out.trim().to_string();
    if let Some(cut) = out.strip_suffix("...") {
        out = format!("{}…", cut.trim_end());
    }
    out
}

/// Drops `<...>` and known `[tag]` markup. A tag cut off by the snippet's end is dropped with
/// everything after it.
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let after = &rest[c.len_utf8()..];
        let tag_end = match c {
            '<' if after.starts_with(|n: char| n.is_ascii_alphabetic() || n == '/' || n == '!') => {
                Some(after.find('>'))
            }
            '[' if is_bbcode(after) => Some(after.find(']')),
            _ => None,
        };
        match tag_end {
            // Tags become a space, so words either side don't run together.
            Some(Some(end)) => {
                out.push(' ');
                rest = &after[end + 1..];
            }
            Some(None) => break,
            None => {
                out.push(c);
                rest = after;
            }
        }
    }
    out
}

/// Whether text after a `[` starts a known BBCode tag (`b]`, `/url]`, `url=...`).
fn is_bbcode(after: &str) -> bool {
    let after = after.strip_prefix('/').unwrap_or(after);
    let name: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '*')
        .collect();
    let next = after[name.len()..].chars().next();
    let name = name.to_ascii_lowercase();
    BBCODE.contains(&name.as_str()) && matches!(next, None | Some(']' | '='))
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest.find(';').filter(|&end| end <= 10).and_then(|end| {
            let name = &rest[1..end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => name.strip_prefix('#').and_then(|n| {
                    let code = match n.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => n.parse().ok(),
                    };
                    code.and_then(char::from_u32)
                }),
            };
            c.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The newest post's date, to remember as seen.
pub fn newest(items: &[Item]) -> u64 {
    items.iter().map(|i| i.date).max().unwrap_or(0)
}

/// Whether a post is new to the user: newer than the newest one they've seen, and recent.
pub fn is_new(item: &Item, seen: u64, now: u64) -> bool {
    item.date > seen && now.saturating_sub(item.date) < NEW_FOR
}

/// How many posts are new to the user.
pub fn unseen(items: &[Item], seen: u64, now: u64) -> usize {
    items.iter().filter(|i| is_new(i, seen, now)).count()
}

/// When a post was made, in words ("3 days ago"); empty if unknown.
pub fn ago(date: u64, now: u64) -> String {
    if date == 0 {
        return String::new();
    }
    let secs = now.saturating_sub(date);
    let (n, unit) = match secs {
        0..60 => return "just now".into(),
        60..3_600 => (secs / 60, "minute"),
        3_600..86_400 => (secs / 3_600, "hour"),
        86_400..1_209_600 => (secs / 86_400, "day"),
        1_209_600..5_184_000 => (secs / 604_800, "week"),
        5_184_000..31_536_000 => (secs / 2_592_000, "month"),
        _ => (secs / 31_536_000, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Part of a real answer (2026-10-10), with three of its ten posts.
    const FIXTURE: &str = r#"{
 "appnews": {
  "appid": 221100,
  "newsitems": [
   {
    "gid": "1845383656385600",
    "title": "DayZ Badlands Price Announcement | Creator Program",
    "url": "https://steamstore-a.akamaihd.net/news/externalpost/steam_community_announcements/1845383656385600",
    "is_external_url": true,
    "author": "lynn.zaw",
    "contents": "Hey, Survivors!We are less than two weeks away from the release of Update 1.30 and the accompanying DayZ Badlands expansion. So, we’d like to take a moment to answer some of the questions you may have.Let’s start with the pricing. The DayZ Badlands expansion is priced at approximately $24.99. Region...",
    "feedlabel": "Community Announcements",
    "date": 1790933371,
    "feedname": "steam_community_announcements",
    "feed_type": 1,
    "appid": 221100,
    "tags": [
     "workshop"
    ]
   },
   {
    "gid": "1846018067933489",
    "title": "DayZ Badlands Official Collector’s Vinyl Box Set | Pre-Order Now!",
    "url": "https://steamstore-a.akamaihd.net/news/externalpost/steam_community_announcements/1846018067933489",
    "is_external_url": true,
    "author": "lynn.zaw",
    "contents": "Hey Survivors! To celebrate the launch of DayZ Badlands, IGN and Bohemia Interactive are proud to present the DayZ Badlands: Collector's Vinyl Box Set. Featuring stunning artwork, premium materials, and a suite of items, this is the ultimate tribute to the iconic and ever-lasting DayZ.Within the pre...",
    "feedlabel": "Community Announcements",
    "date": 1791555988,
    "feedname": "steam_community_announcements",
    "feed_type": 1,
    "appid": 221100
   },
   {
    "gid": "1844751498224697",
    "title": "DayZ Badlands | Dev Blog (Week 86) | Lizard and Dogs | Wishlist now!",
    "url": "https://steamstore-a.akamaihd.net/news/externalpost/steam_community_announcements/1844751498224697",
    "is_external_url": true,
    "author": "lynn.zaw",
    "contents": "Hey Survivors!Welcome to Dev Blog 8 (Week 86) of the DayZ Badlands expansion.This dev blog was written by our colleagues from the art and marketing teams, with help from members of our design team. This time we’re covering the wildlife of Nasdara. Our dev blog publishing schedule will be a little un...",
    "feedlabel": "Community Announcements",
    "date": 1790345626,
    "feedname": "steam_community_announcements",
    "feed_type": 1,
    "appid": 221100,
    "tags": [
     "mod_reviewed",
     "ModAct_871423085_1790347515_0",
     "mod_require_rereview"
    ]
   }
  ],
  "count": 497
 }
}"#;

    #[test]
    fn reads_steams_answer() {
        let items = parse(FIXTURE.as_bytes()).unwrap();
        assert_eq!(items.len(), 3);
        // Newest first.
        assert_eq!(items[0].gid, "1846018067933489");
        assert_eq!(
            items[0].title,
            "DayZ Badlands Official Collector\u{2019}s Vinyl Box Set | Pre-Order Now!"
        );
        assert_eq!(items[0].date, 1791555988);
        assert!(
            items[0]
                .url
                .starts_with("https://steamstore-a.akamaihd.net/news/")
        );
        assert!(
            items[0]
                .snippet
                .ends_with("ever-lasting DayZ. Within the pre…")
        );
        assert!(items[1].snippet.starts_with("Hey, Survivors! We are less"));
        assert!(
            items[1]
                .snippet
                .contains("you may have. Let\u{2019}s start")
        );
        assert!(
            items[2]
                .snippet
                .starts_with("Hey Survivors! Welcome to Dev Blog 8 (Week 86)")
        );
        assert_eq!(newest(&items), 1791555988);
    }

    #[test]
    fn odd_answers() {
        assert!(parse(b"{}").is_err(), "Steam's answer to a wrong app");
        assert!(parse(b"<html>").is_err());
        assert!(parse(br#"{"appnews":{"appid":221100}}"#).is_err());
        let items = parse(
            br#"{"appnews":{"newsitems":[
                {"gid":"1","title":"Twice","date":5},
                {"gid":"1","title":"Twice","date":5},
                {"title":"   ","date":9},
                "not a post",
                {"gid":7,"title":"Number id","date":"8","url":"javascript:alert(1)"},
                {"title":"No date"}
            ]}}"#,
        )
        .unwrap();
        let titles: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["Number id", "Twice", "No date"]);
        assert_eq!(items[0].gid, "7");
        assert_eq!(items[0].date, 8);
        // Only web addresses are opened.
        assert_eq!(
            items[0].url,
            "https://store.steampowered.com/news/app/221100/view/7"
        );
        assert_eq!(
            items[2].url,
            "https://store.steampowered.com/news/app/221100"
        );
        assert_eq!(items[2].date, 0);
        assert_eq!(ago(items[2].date, 100), "");
    }

    #[test]
    fn cleans_snippets() {
        assert_eq!(
            clean("[b]Hello[/b] [url=https://x.y/z]there[/url]<br/>friend"),
            "Hello there friend"
        );
        assert_eq!(
            clean("[h1]Patch[/H1]notes [list][*]one[*]two[/list]"),
            "Patch notes one two"
        );
        // Square brackets that aren't markup stay.
        assert_eq!(clean("[PvE] servers [1.30]"), "[PvE] servers [1.30]");
        assert_eq!(clean("[i love this] [b"), "[i love this]");
        assert_eq!(clean("a < b and c > d"), "a < b and c > d");
        // Cut off in the middle of a tag.
        assert_eq!(
            clean("Read the full story on PCGamesN: <a href=\""),
            "Read the full story on PCGamesN:"
        );
        assert_eq!(clean("See [url=https://exam"), "See");
        assert_eq!(
            clean("Fish &amp; chips &quot;now&quot; &#39;hot&#x27; &bogus; & more"),
            "Fish & chips \"now\" 'hot' &bogus; & more"
        );
        assert_eq!(
            clean("Hey\u{a0} Survivors,\n\n\tWelcome"),
            "Hey Survivors, Welcome"
        );
        assert_eq!(
            clean("Hey Survivors,Autumn is here"),
            "Hey Survivors, Autumn is here"
        );
        // Not every full stop before a capital ends a sentence.
        assert_eq!(clean("The U.S.A. and 1.30.It"), "The U.S.A. and 1.30.It");
        assert_eq!(clean("(Week 86).This"), "(Week 86). This");
        assert_eq!(clean("Cut short..."), "Cut short…");
        assert_eq!(clean(""), "");
    }

    #[test]
    fn new_posts() {
        let day = 86_400;
        let now = 1_000 * day;
        let post = |date: u64| Item {
            gid: date.to_string(),
            title: "t".into(),
            url: String::new(),
            snippet: String::new(),
            date,
        };
        let items = [post(now - day), post(now - 3 * day), post(now - 40 * day)];
        assert_eq!(
            unseen(&items, 0, now),
            2,
            "old posts aren't new on the first run"
        );
        assert_eq!(unseen(&items, now - 3 * day, now), 1);
        assert_eq!(unseen(&items, newest(&items), now), 0);
        assert_eq!(unseen(&[], 0, now), 0);
        // A clock behind the post: still new.
        assert!(is_new(&post(now + 60), now - day, now));
        assert!(!is_new(&post(0), 0, now), "undated posts never are");
    }

    #[test]
    fn when_in_words() {
        let now = 2_000_000_000;
        assert_eq!(ago(now, now), "just now");
        assert_eq!(ago(now + 500, now), "just now");
        assert_eq!(ago(now - 60, now), "1 minute ago");
        assert_eq!(ago(now - 7_200, now), "2 hours ago");
        assert_eq!(ago(now - 86_400, now), "1 day ago");
        assert_eq!(ago(now - 3 * 86_400 - 5, now), "3 days ago");
        assert_eq!(ago(now - 20 * 86_400, now), "2 weeks ago");
        assert_eq!(ago(now - 90 * 86_400, now), "3 months ago");
        assert_eq!(ago(now - 800 * 86_400, now), "2 years ago");
    }
}
