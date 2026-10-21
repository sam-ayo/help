//! The logos of MCP servers. A server can give icons in its `serverInfo`. A server with no icon
//! can have a favicon on its website.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use rmcp::model::{Icon, IconTheme, Implementation};

/// The maximum size of a logo file.
const MAX_BYTES: usize = 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The image of a logo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Logo {
    /// The MIME type that the bytes show, for example `image/png`.
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

/// The logos of a server for a light and a dark background. When the server gives one logo,
/// both are the same. `info` is there when the app could connect to the server.
pub(crate) async fn logos(info: Option<&Implementation>, url: Option<&str>) -> (Option<Logo>, Option<Logo>) {
    let icons = info.and_then(|i| i.icons.as_deref()).unwrap_or_default();
    let light = first(icons, IconTheme::Light).await;
    let dark = first(icons, IconTheme::Dark).await;
    if light.is_some() || dark.is_some() {
        return (light.clone().or_else(|| dark.clone()), dark.or(light));
    }
    // The website of the server comes first, then the address of an HTTP server.
    let website = info.and_then(|i| i.website_url.as_deref());
    for site in [website, url].into_iter().flatten().flat_map(origin) {
        if let Some(logo) = site_icon(&site).await {
            return (Some(logo.clone()), Some(logo));
        }
    }
    (None, None)
}

/// The icon of a website: `/favicon.ico`, or else an icon that the home page links to.
async fn site_icon(site: &str) -> Option<Logo> {
    if let Ok(logo) = fetch(&format!("{site}/favicon.ico")).await {
        return Some(logo);
    }
    let page = page(site).await.ok()?;
    for href in icon_links(&page.text) {
        let Ok(url) = page.url.join(&href) else { continue };
        if let Ok(logo) = fetch(url.as_str()).await {
            return Some(logo);
        }
    }
    None
}

struct Page {
    /// The address after redirects, for relative links.
    url: reqwest::Url,
    text: String,
}

/// The start of the home page of a site, enough for its `<head>`.
async fn page(site: &str) -> Result<Page> {
    let mut response = client().get(site).send().await?.error_for_status()?;
    let url = response.url().clone();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > MAX_BYTES {
            break;
        }
    }
    Ok(Page { url, text: String::from_utf8_lossy(&bytes).into_owned() })
}

/// The `href` of the `<link>` tags with an icon in `rel`. The large Apple icons come first,
/// because they look better than the small ones.
fn icon_links(html: &str) -> Vec<String> {
    let mut links: Vec<(bool, String)> = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(start) = lower[from..].find("<link").map(|i| i + from) {
        let end = lower[start..].find('>').map_or(lower.len(), |i| i + start);
        let tag = &html[start..end];
        from = end;
        let Some(rel) = attribute(tag, "rel").map(|r| r.to_ascii_lowercase()) else { continue };
        if !rel.split_whitespace().any(|r| r.contains("icon")) || rel.contains("mask-icon") {
            continue;
        }
        if let Some(href) = attribute(tag, "href").filter(|h| !h.is_empty()) {
            links.push((rel.contains("apple-touch-icon"), href.replace("&amp;", "&")));
        }
    }
    links.sort_by_key(|(apple, _)| !apple);
    links.into_iter().map(|(_, href)| href).collect()
}

/// The value of an attribute of a tag, with or without quotes.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name).map(|i| i + from) {
        from = i + name.len();
        // The name must be a whole word, as in ` rel=`, not `data-rel=`.
        let before = lower[..i].chars().next_back();
        let rest = lower[from..].trim_start();
        if !before.is_some_and(char::is_whitespace) || !rest.starts_with('=') {
            continue;
        }
        let value = tag[tag.len() - rest.len() + 1..].trim_start();
        return Some(match value.chars().next()? {
            q @ ('"' | '\'') => value[1..].split(q).next()?.to_string(),
            _ => value.split(char::is_whitespace).next()?.to_string(),
        });
    }
    None
}

/// The first icon that the app can read for this theme. An icon with no theme is for all themes.
async fn first(icons: &[Icon], theme: IconTheme) -> Option<Logo> {
    for icon in icons.iter().filter(|i| i.theme.as_ref().is_none_or(|t| *t == theme)) {
        match fetch(&icon.src).await {
            Ok(logo) => return Some(logo),
            Err(err) => tracing::debug!("cannot read the icon {}: {err:#}", short(&icon.src)),
        }
    }
    None
}

/// Read a `data:` URI, or get an `https:` URL. Other schemes are not safe to use.
async fn fetch(src: &str) -> Result<Logo> {
    let bytes = if let Some(data) = src.strip_prefix("data:") {
        let (head, body) = data.split_once(',').context("the data URI has no comma")?;
        if head.ends_with(";base64") {
            base64::engine::general_purpose::STANDARD.decode(body.trim())?
        } else {
            body.as_bytes().to_vec()
        }
    } else if src.starts_with("https://") {
        let response = client().get(src).send().await?.error_for_status()?;
        if response.content_length().is_some_and(|n| n as usize > MAX_BYTES) {
            bail!("the file is too large");
        }
        response.bytes().await?.to_vec()
    } else {
        bail!("the app reads only data: and https: icons");
    };
    if bytes.len() > MAX_BYTES {
        bail!("the file is too large");
    }
    let mime = sniff(&bytes).context("the file is not an image that the app can show")?;
    Ok(Logo { mime, bytes })
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .user_agent(concat!("help/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default()
    })
}

/// The MIME type of an image from its first bytes. A server can give the wrong type.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    let text = || String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    Some(match bytes {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        [0, 0, 1, 0, ..] => "image/x-icon",
        _ if text().contains("<svg") => "image/svg+xml",
        _ => return None,
    })
}

/// `https://host` of an `https:` URL. An MCP server often has a host of its own, such as
/// `mcp.linear.app`, so the main domain `linear.app` comes first.
fn origin(url: &str) -> Vec<String> {
    let Some(rest) = url.strip_prefix("https://") else { return Vec::new() };
    let Some(host) = rest.split(['/', '?', '#']).next().map(|h| h.split(':').next().unwrap_or(h)) else {
        return Vec::new();
    };
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    let mut sites = Vec::new();
    if labels.len() > 2 {
        sites.push(format!("https://{}", labels[labels.len() - 2..].join(".")));
    }
    if labels.len() >= 2 {
        sites.push(format!("https://{}", labels.join(".")));
    }
    sites
}

/// The start of a long `src`, for the log.
fn short(src: &str) -> &str {
    &src[..src.char_indices().nth(60).map_or(src.len(), |(i, _)| i)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_type_comes_from_the_bytes() {
        assert_eq!(sniff(b"\x89PNG\r\n"), Some("image/png"));
        assert_eq!(sniff(b"<?xml version=\"1.0\"?><svg xmlns=\"x\"/>"), Some("image/svg+xml"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8"), Some("image/webp"));
        assert_eq!(sniff(b"<html>"), None);
    }

    #[test]
    fn the_main_domain_comes_before_the_host() {
        assert_eq!(origin("https://mcp.linear.app/mcp?x=1"), ["https://linear.app", "https://mcp.linear.app"]);
        assert_eq!(origin("https://a.b:8443"), ["https://a.b"]);
        assert!(origin("http://a.b/").is_empty());
        assert!(origin("https://localhost/").is_empty());
    }

    #[test]
    fn the_page_links_give_the_icons() {
        let html = r#"<head><link rel="stylesheet" href="/a.css"><link data-rel="icon" href="/no.png">
            <LINK REL="icon" type="image/png" href='/favicon-32.png'><link rel="mask-icon" href="/m.svg">
            <link href="/apple.png?v=1&amp;x=2" rel="apple-touch-icon"><link rel=icon href=/plain.ico></head>"#;
        assert_eq!(icon_links(html), ["/apple.png?v=1&x=2", "/favicon-32.png", "/plain.ico"]);
    }

    #[tokio::test]
    async fn data_uris_give_logos() {
        let svg = "data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg'/>";
        assert_eq!(fetch(svg).await.unwrap().mime, "image/svg+xml");
        let png = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n"));
        assert_eq!(fetch(&png).await.unwrap().mime, "image/png");
        assert!(fetch("file:///etc/hosts").await.is_err());
    }
}
