use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use url::Url;

const MAX_COUNTER_PAGE_BYTES: usize = 2 * 1024 * 1024;
const FETCH_DEADLINE: Duration = Duration::from_secs(10);
const DEFAULT_WIDTH: f64 = 500.0;
const DEFAULT_HEIGHT: f64 = 300.0;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InstalledCounter {
    pub id: String,
    pub name: String,
    pub author: String,
    pub version: String,
    pub width: f64,
    pub height: f64,
    pub has_settings: bool,
    pub url: Url,
}

pub fn fetch_installed_counters(base_url: &Url) -> Result<Vec<InstalledCounter>> {
    fetch_installed_counters_with_timeout(base_url, FETCH_DEADLINE)
}

fn fetch_installed_counters_with_timeout(
    base_url: &Url,
    timeout: Duration,
) -> Result<Vec<InstalledCounter>> {
    let endpoint = base_url
        .join("/api/counters/search/")
        .with_context(|| format!("failed to construct counter search URL from {base_url}"))?;
    let mut response = ureq::get(endpoint.as_str())
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .call()
        .with_context(|| format!("failed to request installed counters from {endpoint}"))?;

    if !response.status().is_success() {
        bail!(
            "counter search at {} returned HTTP status {}",
            endpoint,
            response.status()
        );
    }

    let mime_type = response.body().mime_type();
    if !matches!(mime_type, Some("text/html" | "application/xhtml+xml")) {
        bail!(
            "counter search at {} returned non-HTML content type {}",
            endpoint,
            mime_type.unwrap_or("missing")
        );
    }

    if response
        .body()
        .content_length()
        .is_some_and(|length| length > MAX_COUNTER_PAGE_BYTES as u64)
    {
        bail!(
            "counter search response at {} exceeds the {} byte limit",
            endpoint,
            MAX_COUNTER_PAGE_BYTES
        );
    }

    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_COUNTER_PAGE_BYTES as u64 + 1)
        .read_to_vec()
        .with_context(|| format!("failed to read counter search response from {endpoint}"))?;
    if bytes.len() > MAX_COUNTER_PAGE_BYTES {
        bail!(
            "counter search response at {} exceeds the {} byte limit",
            endpoint,
            MAX_COUNTER_PAGE_BYTES
        );
    }
    let document = String::from_utf8(bytes)
        .with_context(|| format!("counter search response from {endpoint} is not UTF-8"))?;

    parse_installed_counters(base_url, &document)
        .with_context(|| format!("failed to parse installed counters from {endpoint}"))
}

fn parse_installed_counters(base_url: &Url, source: &str) -> Result<Vec<InstalledCounter>> {
    let card_selector = selector("article.counter-card[data-overlay-id]")?;
    let empty_selector = selector(".empty-state")?;
    let settings_selector = selector(r#"[data-action="counter-settings"]"#)?;
    let preview_selector = selector(".counter-card__preview iframe")?;
    let document = Html::parse_document(source);
    let mut counters = Vec::new();

    for card in document.select(&card_selector) {
        let id = required_attribute(card, "data-overlay-id", "counter card")?.to_owned();
        if id.is_empty() {
            bail!("counter card has an empty data-overlay-id");
        }
        let name = required_attribute(card, "data-counter-name", &id)?.to_owned();
        let author = required_attribute(card, "data-counter-author", &id)?.to_owned();
        let version = required_attribute(card, "data-counter-version", &id)?.to_owned();
        let resolution = resolution(&card, &preview_selector);

        counters.push(InstalledCounter {
            url: counter_url(base_url, &id)?,
            id,
            name,
            author,
            version,
            width: resolution.0,
            height: resolution.1,
            has_settings: card.select(&settings_selector).next().is_some(),
        });
    }

    if counters.is_empty() && document.select(&empty_selector).next().is_none() {
        bail!(
            "counter search markup was not recognized; vosu and vosulay versions may be out of sync"
        );
    }

    Ok(counters)
}

fn selector(source: &str) -> Result<Selector> {
    Selector::parse(source)
        .map_err(|error| anyhow!("invalid built-in selector {source:?}: {error:?}"))
}

fn required_attribute<'a>(element: ElementRef<'a>, name: &str, counter: &str) -> Result<&'a str> {
    element
        .value()
        .attr(name)
        .with_context(|| format!("{counter:?} is missing the {name:?} attribute"))
}

fn resolution(card: &ElementRef<'_>, preview: &Selector) -> (f64, f64) {
    let Some(preview) = card.select(preview).next() else {
        return (DEFAULT_WIDTH, DEFAULT_HEIGHT);
    };
    let width = numeric_attribute(preview, "width");
    let height = numeric_attribute(preview, "height");
    match (width, height) {
        (Some(width), Some(height)) if width > 10.0 && height > 10.0 => (width, height),
        _ => (DEFAULT_WIDTH, DEFAULT_HEIGHT),
    }
}

fn numeric_attribute(element: ElementRef<'_>, attribute: &str) -> Option<f64> {
    let source = element.value().attr(attribute)?;
    source
        .strip_suffix("px")
        .unwrap_or(source)
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn counter_url(base_url: &Url, id: &str) -> Result<Url> {
    let mut url = base_url.clone();
    url.set_query(None);
    url.set_fragment(None);
    url.set_path("/");
    url.path_segments_mut()
        .map_err(|()| anyhow!("base URL {base_url} cannot contain counter paths"))?
        .push(id)
        .push("");
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Instant;

    #[test]
    fn parse_decodes_escaped_metadata_and_preserves_source_order() {
        let source = r#"
            <article class="counter-card" data-overlay-id="folder/&amp;&quot;"
                     data-counter-name="First &amp; Best"
                     data-counter-author="A &quot;Name&quot;"
                     data-counter-version="1.2">
              <header>
                <button type="button" data-action="counter-settings">Settings</button>
              </header>
              <div class="counter-card__preview">
                <iframe src="/folder/" width="640px" height="360px"></iframe>
              </div>
            </article>
            <article class="counter-card is-pinned" data-overlay-id="second"
                     data-counter-name="Second"
                     data-counter-author="Author"
                     data-counter-version="2">
              <div class="counter-card__preview">
                <iframe src="/second/" width="800" height="600"></iframe>
              </div>
            </article>
        "#;
        let base = Url::parse("http://127.0.0.1:24050/base?old=1#fragment")
            .expect("fixture URL should parse");

        let counters = parse_installed_counters(&base, source).expect("fixture should parse");

        assert_eq!(
            counters,
            vec![
                InstalledCounter {
                    id: "folder/&\"".to_owned(),
                    name: "First & Best".to_owned(),
                    author: "A \"Name\"".to_owned(),
                    version: "1.2".to_owned(),
                    width: 640.0,
                    height: 360.0,
                    has_settings: true,
                    url: Url::parse("http://127.0.0.1:24050/folder%2F&%22/")
                        .expect("expected URL should parse"),
                },
                InstalledCounter {
                    id: "second".to_owned(),
                    name: "Second".to_owned(),
                    author: "Author".to_owned(),
                    version: "2".to_owned(),
                    width: 800.0,
                    height: 600.0,
                    has_settings: false,
                    url: Url::parse("http://127.0.0.1:24050/second/")
                        .expect("expected URL should parse"),
                },
            ]
        );
    }

    #[test]
    fn parse_defaults_flexible_non_numeric_and_invalid_resolution() {
        let source = r#"
            <article class="counter-card" data-overlay-id="flexible"
                     data-counter-name="Flexible"
                     data-counter-author="Author"
                     data-counter-version="1">
              <div class="counter-card__preview">
                <iframe src="/flexible/" width="100%" height="300px"></iframe>
              </div>
            </article>
            <article class="counter-card" data-overlay-id="non-numeric"
                     data-counter-name="Non-numeric"
                     data-counter-author="Author"
                     data-counter-version="1">
              <div class="counter-card__preview">
                <iframe src="/non-numeric/" width="ANY" height="auto"></iframe>
              </div>
            </article>
            <article class="counter-card" data-overlay-id="too-small"
                     data-counter-name="Too small"
                     data-counter-author="Author"
                     data-counter-version="1">
              <div class="counter-card__preview">
                <iframe src="/too-small/" width="10px" height="300px"></iframe>
              </div>
            </article>
        "#;
        let base = Url::parse("http://127.0.0.1:24050").expect("fixture URL should parse");

        let counters = parse_installed_counters(&base, source).expect("fixture should parse");

        assert_eq!(counters.len(), 3);
        assert!(
            counters.iter().all(|counter| {
                (counter.width, counter.height) == (DEFAULT_WIDTH, DEFAULT_HEIGHT)
            })
        );
    }
    #[test]
    fn parse_rejects_missing_counter_attribute() {
        let source = r#"
            <article class="counter-card" data-overlay-id="missing-version"
                     data-counter-name="Missing version"
                     data-counter-author="Author">
              <div class="counter-card__preview">
                <iframe src="/missing-version/" width="640px" height="360px"></iframe>
              </div>
            </article>
        "#;
        let base = Url::parse("http://127.0.0.1:24050").expect("fixture URL should parse");

        let error =
            parse_installed_counters(&base, source).expect_err("missing attribute must fail");

        assert_eq!(
            error.to_string(),
            r#""missing-version" is missing the "data-counter-version" attribute"#
        );
    }

    #[test]
    fn parse_accepts_empty_state_without_counters() {
        let source = r#"<div class="empty-state">No counters found.</div>"#;
        let base = Url::parse("http://127.0.0.1:24050").expect("fixture URL should parse");

        let counters = parse_installed_counters(&base, source).expect("empty state should parse");

        assert!(counters.is_empty());
    }

    #[test]
    fn parse_rejects_unrecognized_markup() {
        let source = "<main>Unexpected response</main>";
        let base = Url::parse("http://127.0.0.1:24050").expect("fixture URL should parse");

        let error =
            parse_installed_counters(&base, source).expect_err("unrecognized markup must fail");

        assert_eq!(
            error.to_string(),
            "counter search markup was not recognized; vosu and vosulay versions may be out of sync"
        );
    }

    #[test]
    fn fetch_stops_reading_when_global_deadline_expires() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture server should bind");
        let address = listener
            .local_addr()
            .expect("fixture server should have an address");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("fixture request should connect");
            let mut request = [0_u8; 1024];
            let bytes_read = stream
                .read(&mut request)
                .expect("fixture request should be readable");
            assert!(bytes_read > 0, "fixture request should not be empty");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 100\r\n\r\n<",
                )
                .expect("fixture response headers should be writable");
            thread::sleep(Duration::from_millis(250));
        });
        let base = Url::parse(&format!("http://{address}")).expect("fixture URL should parse");
        let started = Instant::now();

        let error = fetch_installed_counters_with_timeout(&base, Duration::from_millis(50))
            .expect_err("stalled response body must time out");

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "request exceeded its global deadline: {error:#}"
        );
    }
}
