use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crate::Tool;

use super::{BrowserTool, BrowserToolConfig};

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
