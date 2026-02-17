use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use reqwest::Url;
use scraper::{Html, Selector};
use serde::Deserialize;
use serde_json::json;

use crate::{Tool, ToolError, ToolResult};

#[derive(Clone, Debug)]
pub struct BrowserToolConfig {
    pub binary_path: Option<PathBuf>,
    pub headless: bool,
    pub timeout_ms: u64,
    pub allowed_domains: Vec<String>,
    pub max_memory_mb: u64,
    pub max_cpu_seconds: u64,
}

impl Default for BrowserToolConfig {
    fn default() -> Self {
        Self {
            binary_path: None,
            headless: true,
            timeout_ms: 30_000,
            allowed_domains: Vec::new(),
            max_memory_mb: 512,
            max_cpu_seconds: 20,
        }
    }
}

pub struct BrowserTool {
    config: BrowserToolConfig,
}

impl BrowserTool {
    pub fn new(config: BrowserToolConfig) -> Self {
        Self { config }
    }

    fn fetch(&self, url: &str) -> Result<ToolResult, ToolError> {
        self.validate_url(url)?;
        let output =
            self.run_chrome([OsString::from("--dump-dom"), OsString::from(url)].as_slice())?;
        if !output.status.success() {
            return Err(ToolError::new(format!(
                "browser fetch failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let html = String::from_utf8_lossy(&output.stdout).to_string();
        let _ = Html::parse_document(&html);
        Ok(ToolResult {
            output: html,
            ok: true,
        })
    }

    fn screenshot(&self, url: &str) -> Result<ToolResult, ToolError> {
        self.validate_url(url)?;
        let png_path = temp_path("ironclaw-browser-shot", "png");
        let screenshot_arg = format!("--screenshot={}", png_path.display());
        let output =
            self.run_chrome([OsString::from(screenshot_arg), OsString::from(url)].as_slice())?;
        if !output.status.success() {
            return Err(ToolError::new(format!(
                "browser screenshot failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let bytes = fs::read(&png_path)
            .map_err(|err| ToolError::new(format!("screenshot read failed: {err}")))?;
        let _ = fs::remove_file(&png_path);
        let output = base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(ToolResult { output, ok: true })
    }

    fn evaluate(&self, url: &str, js: &str) -> Result<ToolResult, ToolError> {
        self.validate_url(url)?;
        let eval_path = temp_path("ironclaw-browser-eval", "html");
        let eval_page = build_eval_page(url, js)?;
        fs::write(&eval_path, eval_page)
            .map_err(|err| ToolError::new(format!("eval page write failed: {err}")))?;

        let eval_url = format!("file://{}", eval_path.display());
        let output = self.run_chrome(
            [
                OsString::from("--disable-web-security"),
                OsString::from("--allow-file-access-from-files"),
                OsString::from("--dump-dom"),
                OsString::from(eval_url),
            ]
            .as_slice(),
        )?;
        let _ = fs::remove_file(&eval_path);
        if !output.status.success() {
            return Err(ToolError::new(format!(
                "browser evaluate failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let dom = String::from_utf8_lossy(&output.stdout).to_string();
        let result = extract_eval_result(&dom)?;
        let ok = result
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        Ok(ToolResult {
            output: result.to_string(),
            ok,
        })
    }

    fn run_chrome(&self, extra_args: &[OsString]) -> Result<ChromeOutput, ToolError> {
        let binary = resolve_browser_binary(&self.config)?;
        let mut command = Command::new(binary);

        if self.config.headless {
            command.arg("--headless");
        }

        let mut args = Vec::new();
        args.push(OsString::from("--disable-gpu"));
        args.push(OsString::from("--disable-extensions"));
        args.push(OsString::from("--disable-dev-shm-usage"));
        args.push(OsString::from("--no-first-run"));
        args.push(OsString::from("--no-default-browser-check"));
        args.push(OsString::from(format!(
            "--virtual-time-budget={}",
            self.config.timeout_ms
        )));
        args.extend(extra_args.iter().cloned());

        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        apply_process_limits(&mut command, &self.config);

        let mut child = command
            .spawn()
            .map_err(|err| ToolError::new(format!("browser exec failed: {err}")))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_reader = thread::spawn(move || read_pipe_bytes(stdout));
        let stderr_reader = thread::spawn(move || read_pipe_bytes(stderr));

        let timeout = Duration::from_millis(self.config.timeout_ms);
        let start = Instant::now();
        let status = loop {
            let status = child
                .try_wait()
                .map_err(|err| ToolError::new(format!("browser wait failed: {err}")))?;
            if let Some(value) = status {
                break value;
            }
            if start.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ToolError::new(format!(
                    "browser timed out after {} ms",
                    self.config.timeout_ms
                )));
            }
            thread::sleep(Duration::from_millis(25));
        };

        let stdout = stdout_reader
            .join()
            .map_err(|_| ToolError::new("browser stdout join failed"))?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| ToolError::new("browser stderr join failed"))?;

        Ok(ChromeOutput {
            status,
            stdout,
            stderr,
        })
    }

    fn validate_url(&self, raw_url: &str) -> Result<Url, ToolError> {
        let trimmed = raw_url.trim();
        let parsed =
            Url::parse(trimmed).map_err(|err| ToolError::new(format!("invalid url: {err}")))?;
        let scheme = parsed.scheme().to_lowercase();
        if scheme == "file" || scheme == "data" || scheme == "javascript" {
            return Err(ToolError::new(format!(
                "dangerous url scheme blocked: {scheme}"
            )));
        }
        if scheme != "http" && scheme != "https" {
            return Err(ToolError::new(format!("unsupported url scheme: {scheme}")));
        }

        let host = parsed
            .host_str()
            .ok_or_else(|| ToolError::new("url host missing"))?
            .to_lowercase();
        if self.config.allowed_domains.is_empty() {
            return Err(ToolError::new(
                "browser url blocked: no allowed domains configured",
            ));
        }
        if !domain_allowed(&host, &self.config.allowed_domains) {
            return Err(ToolError::new(format!(
                "browser url blocked by allowlist: {host}"
            )));
        }
        Ok(parsed)
    }
}

impl Tool for BrowserTool {
    fn run(&self, input: &str) -> Result<ToolResult, ToolError> {
        let request = parse_request(input)?;
        match request {
            BrowserRequest::Fetch { url } => self.fetch(&url),
            BrowserRequest::Screenshot { url } => self.screenshot(&url),
            BrowserRequest::Evaluate { url, js } => self.evaluate(&url, &js),
        }
    }
}

#[derive(Debug)]
struct ChromeOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Debug)]
enum BrowserRequest {
    Fetch { url: String },
    Screenshot { url: String },
    Evaluate { url: String, js: String },
}

#[derive(Debug, Deserialize)]
struct JsonBrowserRequest {
    action: String,
    url: String,
    #[serde(default)]
    js: String,
}

fn parse_request(input: &str) -> Result<BrowserRequest, ToolError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ToolError::new("missing browser input"));
    }

    if trimmed.starts_with('{') {
        let json: JsonBrowserRequest = serde_json::from_str(trimmed)
            .map_err(|err| ToolError::new(format!("browser input parse failed: {err}")))?;
        return from_parts(&json.action, &json.url, &json.js);
    }

    if let Some(url) = trimmed.strip_prefix("fetch ") {
        return from_parts("fetch", url.trim(), "");
    }
    if let Some(url) = trimmed.strip_prefix("screenshot ") {
        return from_parts("screenshot", url.trim(), "");
    }
    if let Some(rest) = trimmed.strip_prefix("evaluate ") {
        let rest = rest.trim_start();
        let Some(split_at) = rest.find(char::is_whitespace) else {
            return Err(ToolError::new("evaluate requires url and js"));
        };
        let url = rest[..split_at].trim();
        let js = rest[split_at..].trim();
        return from_parts("evaluate", url, js);
    }

    Err(ToolError::new(
        "invalid browser input; expected fetch/screenshot/evaluate",
    ))
}

fn from_parts(action: &str, url: &str, js: &str) -> Result<BrowserRequest, ToolError> {
    if url.trim().is_empty() {
        return Err(ToolError::new("missing url"));
    }
    match action {
        "fetch" => Ok(BrowserRequest::Fetch {
            url: url.trim().to_string(),
        }),
        "screenshot" => Ok(BrowserRequest::Screenshot {
            url: url.trim().to_string(),
        }),
        "evaluate" => {
            if js.trim().is_empty() {
                return Err(ToolError::new("missing js for evaluate"));
            }
            Ok(BrowserRequest::Evaluate {
                url: url.trim().to_string(),
                js: js.trim().to_string(),
            })
        }
        _ => Err(ToolError::new(format!(
            "unsupported browser action: {action}"
        ))),
    }
}

fn build_eval_page(url: &str, js: &str) -> Result<String, ToolError> {
    let payload = json!({
        "url": url,
        "js": js,
    });
    let payload_text = serde_json::to_string(&payload)
        .map_err(|err| ToolError::new(format!("eval payload build failed: {err}")))?;
    Ok(format!(
        "<!doctype html><html><body>\
         <pre id=\"ironclaw-eval-result\">{{\"ok\":false,\"error\":\"pending\"}}</pre>\
         <script>\
         const payload = {payload_text};\
         (async () => {{\
           const out = document.getElementById('ironclaw-eval-result');\
           try {{\
             const response = await fetch(payload.url);\
             const html = await response.text();\
             const parser = new DOMParser();\
             const doc = parser.parseFromString(html, 'text/html');\
             let value;\
             try {{\
               const fn = new Function('document', 'window', 'location', payload.js);\
               value = fn(doc, window, new URL(payload.url));\
             }} catch (_err) {{\
               const fn = new Function(\
                 'document',\
                 'window',\
                 'location',\
                 'return (' + payload.js + ');'\
               );\
               value = fn(doc, window, new URL(payload.url));\
             }}\
             out.textContent = JSON.stringify({{ ok: true, result: value }});\
           }} catch (err) {{\
             out.textContent = JSON.stringify({{ ok: false, error: String(err) }});\
           }}\
         }})();\
         </script></body></html>"
    ))
}

fn extract_eval_result(dom: &str) -> Result<serde_json::Value, ToolError> {
    let doc = Html::parse_document(dom);
    let selector = Selector::parse("#ironclaw-eval-result")
        .map_err(|err| ToolError::new(format!("eval selector parse failed: {err}")))?;
    let node = doc
        .select(&selector)
        .next()
        .ok_or_else(|| ToolError::new("eval result node missing"))?;
    let text = node.text().collect::<String>();
    serde_json::from_str(&text)
        .map_err(|err| ToolError::new(format!("eval result parse failed: {err}")))
}

fn resolve_browser_binary(config: &BrowserToolConfig) -> Result<PathBuf, ToolError> {
    if let Some(path) = &config.binary_path {
        if path.exists() {
            return Ok(path.clone());
        }
        return Err(ToolError::new(format!(
            "configured browser binary does not exist: {}",
            path.display()
        )));
    }
    for candidate in [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
    ] {
        if let Some(path) = find_on_path(candidate) {
            return Ok(path);
        }
    }
    Err(ToolError::new("chrome binary not found"))
}

fn find_on_path(bin: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for base in std::env::split_paths(&paths) {
        let candidate = base.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn read_pipe_bytes<T: Read>(pipe: Option<T>) -> Vec<u8> {
    let Some(mut value) = pipe else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let _ = value.read_to_end(&mut out);
    out
}

fn domain_allowed(host: &str, allowed_domains: &[String]) -> bool {
    for domain in allowed_domains {
        let normalized = domain.trim().trim_start_matches('.').to_lowercase();
        if normalized.is_empty() {
            continue;
        }
        if host == normalized {
            return true;
        }
        let suffix = format!(".{normalized}");
        if host.ends_with(&suffix) {
            return true;
        }
    }
    false
}

fn temp_path(prefix: &str, extension: &str) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("{prefix}-{pid}-{ts}.{extension}"))
}

fn apply_process_limits(command: &mut Command, config: &BrowserToolConfig) {
    #[cfg(target_os = "linux")]
    {
        let max_memory = config.max_memory_mb.saturating_mul(1024 * 1024) as libc::rlim_t;
        let max_cpu = config.max_cpu_seconds as libc::rlim_t;
        // set strict process limits before executing chrome.
        unsafe {
            command.pre_exec(move || {
                let mem_limit = libc::rlimit {
                    rlim_cur: max_memory,
                    rlim_max: max_memory,
                };
                if libc::setrlimit(libc::RLIMIT_AS, &mem_limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let cpu_limit = libc::rlimit {
                    rlim_cur: max_cpu,
                    rlim_max: max_cpu,
                };
                if libc::setrlimit(libc::RLIMIT_CPU, &cpu_limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (command, config);
    }
}

#[cfg(test)]
#[path = "browser_test.rs"]
mod browser_test;
