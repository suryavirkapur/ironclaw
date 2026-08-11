use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crate::Tool;

use super::{
    domain_allowed, parse_request, readable_brave_results, readable_rss, BraveSearchCredentials,
    BrowserRequest, BrowserTool, BrowserToolConfig,
};

fn temp_path(name: &str) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("ironclaw-browser-{name}-{ts}"))
}

fn write_fake_browser() -> PathBuf {
    let path = temp_path("fake-browser.sh");
    let script = r#"#!/usr/bin/env sh
set -eu

shot_path=""
for arg in "$@"; do
  case "$arg" in
    --screenshot=*)
      shot_path="${arg#--screenshot=}"
      ;;
  esac
done

if [ -n "$shot_path" ]; then
  printf '\211PNG\r\n\032\n' >"$shot_path"
  exit 0
fi

if printf '%s\n' "$*" | grep -q "ironclaw-browser-eval"; then
  echo '<html><body><pre id="ironclaw-eval-result">{"ok":true,"result":"ok"}</pre></body></html>'
  exit 0
fi

echo '<html><body>allowed fetch</body></html>'
"#;
    assert!(fs::write(&path, script).is_ok());
    assert!(fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).is_ok());
    path
}

#[test]
fn fetch_allowed_domain_returns_content() {
    let fake = write_fake_browser();
    let tool = BrowserTool::new(BrowserToolConfig {
        binary_path: Some(fake.clone()),
        allowed_domains: vec!["wikipedia.org".to_string()],
        ..BrowserToolConfig::default()
    });

    let result = tool.run(r#"{"action":"fetch","url":"https://www.wikipedia.org"}"#);
    assert!(result.is_ok());
    let value = result.map(|v| v.output).unwrap_or_default();
    assert!(value.contains("allowed fetch"));

    let _ = fs::remove_file(fake);
}

#[test]
fn screenshot_returns_base64_png() {
    let fake = write_fake_browser();
    let tool = BrowserTool::new(BrowserToolConfig {
        binary_path: Some(fake.clone()),
        allowed_domains: vec!["github.com".to_string()],
        ..BrowserToolConfig::default()
    });

    let result = tool.run(r#"{"action":"screenshot","url":"https://github.com"}"#);
    assert!(result.is_ok());
    let value = result.map(|v| v.output).unwrap_or_default();
    assert_eq!(value, "iVBORw0KGgo=");

    let _ = fs::remove_file(fake);
}

#[test]
fn blocked_domain_returns_error() {
    let fake = write_fake_browser();
    let tool = BrowserTool::new(BrowserToolConfig {
        binary_path: Some(fake.clone()),
        allowed_domains: vec!["wikipedia.org".to_string()],
        ..BrowserToolConfig::default()
    });

    let result = tool.run(r#"{"action":"fetch","url":"https://github.com"}"#);
    assert!(result.is_err());

    let _ = fs::remove_file(fake);
}

#[test]
fn native_search_request_uses_query_field() {
    let request = parse_request(r#"{"action":"search","query":"Argentina Spain 2026 final"}"#)
        .expect("parse search request");
    assert!(matches!(
        request,
        BrowserRequest::Search { query } if query == "Argentina Spain 2026 final"
    ));
}

#[test]
fn wildcard_domain_is_explicitly_unrestricted() {
    assert!(domain_allowed("www.fifa.com", &["*".to_string()]));
    assert!(domain_allowed("news.google.com", &["*".to_string()]));
}

#[test]
fn rss_search_results_are_concise_and_keep_evidence_links() {
    let xml = r#"<?xml version="1.0"?><rss><channel>
        <item><title>Spain 1-0 Argentina - BBC</title>
        <link>https://news.google.com/articles/result</link>
        <pubDate>Sun, 19 Jul 2026 23:41:56 GMT</pubDate></item>
        </channel></rss>"#;
    let output = readable_rss(xml);
    assert!(output.contains("1. Spain 1-0 Argentina - BBC"));
    assert!(output.contains("Published: Sun, 19 Jul 2026 23:41:56 GMT"));
    assert!(output.contains("URL: https://news.google.com/articles/result"));
    assert!(!output.contains("<item>"));
}

#[test]
fn brave_search_results_are_concise_and_keep_exact_urls() {
    let response = r#"{
        "web": {
            "results": [{
                "title": "Spain 1-0 Argentina",
                "url": "https://example.com/exact-result",
                "description": "Spain won the 2026 final.",
                "extra_snippets": ["Ferran Torres scored in extra time."]
            }]
        }
    }"#;
    let output = readable_brave_results(response).expect("parse Brave results");
    assert!(output.contains("Live Brave Search results:"));
    assert!(output.contains("URL: https://example.com/exact-result"));
    assert!(output.contains("Snippet: Spain won the 2026 final."));
    assert!(output.contains("Additional snippet: Ferran Torres scored in extra time."));
}

#[test]
fn brave_credentials_are_shared_without_exposing_the_key_in_debug() {
    let credentials = BraveSearchCredentials::default();
    let shared = credentials.clone();
    credentials
        .set_api_key("test-secret-key")
        .expect("store credential");
    assert!(matches!(shared.api_key().as_deref(), Ok("test-secret-key")));
    let debug = format!("{credentials:?}");
    assert!(debug.contains("configured: true"));
    assert!(!debug.contains("test-secret-key"));
}
