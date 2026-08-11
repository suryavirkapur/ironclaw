use std::path::{Path, PathBuf};
use std::time::Duration;
use tools::{BrowserAutomationTool, Tool};

pub const MAX_TOOL_OUTPUT_CHARS: usize = 8_000;

pub async fn run_host_tool(
    allowed_tools: &[String],
    allowed_domains: &[String],
    user_id: &str,
    tool: &str,
    input: &str,
) -> Result<String, String> {
    if !allowed_tools.iter().any(|t| t == tool) {
        return Err(format!("tool not allowed: {tool}"));
    }

    let ws_root = host_workspace_root(user_id)?;

    match tool {
        "bash" => run_bash(&ws_root, input).await,
        "file_read" => file_read(&ws_root, input).await,
        "file_write" => file_write(&ws_root, input).await,
        "browser" => run_browser(&ws_root, allowed_domains, input).await,
        "weather" => run_weather(allowed_domains, input).await,
        _ => Err(format!("unknown tool: {tool}")),
    }
}

pub fn truncate_tool_output(output: &str) -> String {
    if output.chars().count() <= MAX_TOOL_OUTPUT_CHARS {
        return output.to_string();
    }

    let mut truncated = String::new();
    for ch in output.chars().take(MAX_TOOL_OUTPUT_CHARS) {
        truncated.push(ch);
    }
    truncated.push_str("\n[output truncated]");
    truncated
}

fn host_workspace_root(user_id: &str) -> Result<PathBuf, String> {
    // Keep host tool I/O away from secrets. This is a per-user sandbox directory on the host.
    let root = PathBuf::from("data")
        .join("users")
        .join(user_id)
        .join("host-workspace");
    std::fs::create_dir_all(&root).map_err(|e| format!("create host workspace failed: {e}"))?;
    Ok(root)
}

async fn run_bash(cwd: &Path, cmd: &str) -> Result<String, String> {
    use tokio::process::Command;

    if let Some(pattern) = blocked_bash_pattern(cmd) {
        return Err(format!("bash blocked by policy: contains '{pattern}'"));
    }

    let output = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Command::new("bash")
            .arg("-lc")
            .arg(cmd)
            .current_dir(cwd)
            .output(),
    )
    .await
    {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("bash failed: {e}")),
        Err(_) => return Err("bash timed out".to_string()),
    };
    let mut out = String::new();
    out.push_str(&String::from_utf8_lossy(&output.stdout));
    out.push_str(&String::from_utf8_lossy(&output.stderr));

    if output.status.success() {
        Ok(out)
    } else {
        Err(out)
    }
}

fn blocked_bash_pattern(cmd: &str) -> Option<&'static str> {
    let lower = cmd.to_lowercase();
    [
        "rm ", "sudo", "curl ", "wget ", "ssh ", "scp ", "nc ", "netcat",
    ]
    .into_iter()
    .find(|pattern| lower.contains(pattern))
}

async fn file_read(root: &Path, path: &str) -> Result<String, String> {
    let full = safe_join(root, path)?;
    tokio::fs::read_to_string(&full)
        .await
        .map_err(|e| format!("read failed: {e}"))
}

async fn file_write(root: &Path, input: &str) -> Result<String, String> {
    let mut parts = input.splitn(2, '\n');
    let path = parts.next().unwrap_or("").trim();
    let contents = parts.next().unwrap_or("");
    if path.is_empty() {
        return Err("missing path".to_string());
    }
    let full = safe_join(root, path)?;
    if let Some(parent) = full.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("mkdir failed: {e}"))?;
    }
    tokio::fs::write(&full, contents)
        .await
        .map_err(|e| format!("write failed: {e}"))?;
    Ok("ok".to_string())
}

async fn run_browser(
    root: &Path,
    allowed_domains: &[String],
    input: &str,
) -> Result<String, String> {
    let tool = BrowserAutomationTool::new(root.to_path_buf(), allowed_domains.to_vec());
    let input = input.to_string();
    tokio::task::spawn_blocking(move || tool.run(&input))
        .await
        .map_err(|e| format!("browser task failed: {e}"))?
        .map(|result| result.output)
        .map_err(|e| e.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WeatherCity {
    name: &'static str,
    latitude: f64,
    longitude: f64,
}

const DUBAI: WeatherCity = WeatherCity {
    name: "Dubai",
    latitude: 25.2048,
    longitude: 55.2708,
};
const DELHI: WeatherCity = WeatherCity {
    name: "Delhi",
    latitude: 28.6139,
    longitude: 77.2090,
};

async fn run_weather(allowed_domains: &[String], input: &str) -> Result<String, String> {
    const WEATHER_DOMAIN: &str = "api.open-meteo.com";
    if !domain_is_allowed(WEATHER_DOMAIN, allowed_domains) {
        return Err(format!(
            "weather network policy does not allow {WEATHER_DOMAIN}"
        ));
    }

    let cities = requested_weather_cities(input)?;
    let latitudes = cities
        .iter()
        .map(|city| city.latitude.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let longitudes = cities
        .iter()
        .map(|city| city.longitude.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut url = reqwest::Url::parse("https://api.open-meteo.com/v1/forecast")
        .map_err(|err| format!("weather URL failed: {err}"))?;
    url.query_pairs_mut()
        .append_pair("latitude", &latitudes)
        .append_pair("longitude", &longitudes)
        .append_pair(
            "current",
            "temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,wind_speed_10m",
        )
        .append_pair("timezone", "auto");

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| format!("weather client failed: {err}"))?;
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|err| format!("live weather request failed: {err}"))?
        .error_for_status()
        .map_err(|err| format!("live weather response failed: {err}"))?;
    let payload: serde_json::Value = response
        .json()
        .await
        .map_err(|err| format!("live weather JSON failed: {err}"))?;
    format_weather_response(&cities, &payload, &url)
}

fn requested_weather_cities(input: &str) -> Result<Vec<WeatherCity>, String> {
    let lower = input.to_ascii_lowercase();
    let mut cities = Vec::new();
    if lower.contains("dubai") {
        cities.push(DUBAI);
    }
    if lower.contains("delhi") {
        cities.push(DELHI);
    }
    if cities.is_empty() {
        return Err("weather supports Dubai and Delhi; name at least one city".to_string());
    }
    Ok(cities)
}

fn domain_is_allowed(domain: &str, allowed_domains: &[String]) -> bool {
    if allowed_domains.is_empty() {
        return true;
    }
    let domain = domain.to_ascii_lowercase();
    allowed_domains.iter().any(|allowed| {
        let allowed = allowed.trim().trim_start_matches("*.").to_ascii_lowercase();
        domain == allowed || domain.ends_with(&format!(".{allowed}"))
    })
}

fn format_weather_response(
    cities: &[WeatherCity],
    payload: &serde_json::Value,
    source_url: &reqwest::Url,
) -> Result<String, String> {
    let reports = payload
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(payload));
    if reports.len() != cities.len() {
        return Err(format!(
            "weather response count mismatch: requested {}, received {}",
            cities.len(),
            reports.len()
        ));
    }

    let mut lines = Vec::with_capacity(cities.len().saturating_add(2));
    for (city, report) in cities.iter().zip(reports) {
        let current = report
            .get("current")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| format!("weather response missing current data for {}", city.name))?;
        let time = current
            .get("time")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("weather response missing time for {}", city.name))?;
        let timezone = report
            .get("timezone")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("weather response missing timezone for {}", city.name))?;
        let temperature = weather_number(current, "temperature_2m", city.name)?;
        let apparent = weather_number(current, "apparent_temperature", city.name)?;
        let humidity = weather_number(current, "relative_humidity_2m", city.name)?;
        let weather_code = weather_number(current, "weather_code", city.name)? as u8;
        let wind = weather_number(current, "wind_speed_10m", city.name)?;
        lines.push(format!(
            "{}: observed {} {} — {:.1}°C, feels {:.1}°C, humidity {:.0}%, {}, wind {:.1} km/h",
            city.name,
            time,
            timezone,
            temperature,
            apparent,
            humidity,
            weather_code_description(weather_code),
            wind
        ));
    }
    lines.push(format!(
        "Fetched live at {} UTC",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
    ));
    lines.push(format!("Source: {source_url}"));
    Ok(lines.join("\n"))
}

fn weather_number(
    current: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    city: &str,
) -> Result<f64, String> {
    current
        .get(field)
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| format!("weather response missing {field} for {city}"))
}

fn weather_code_description(code: u8) -> &'static str {
    match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51 | 53 | 55 => "drizzle",
        56 | 57 => "freezing drizzle",
        61 | 63 | 65 => "rain",
        66 | 67 => "freezing rain",
        71 | 73 | 75 | 77 => "snow",
        80..=82 => "rain showers",
        85 | 86 => "snow showers",
        95 => "thunderstorm",
        96 | 99 => "thunderstorm with hail",
        _ => "unknown conditions",
    }
}

fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim_start_matches('/');
    let candidate = root.join(rel);
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("canonicalize root failed: {e}"))?;

    // parent might not exist yet, so canonicalize a best-effort path.
    let canon_candidate = candidate
        .parent()
        .unwrap_or(&candidate)
        .canonicalize()
        .unwrap_or_else(|_| canon_root.clone())
        .join(candidate.file_name().unwrap_or_default());

    if !canon_candidate.starts_with(&canon_root) {
        return Err("path escapes workspace".to_string());
    }
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::{
        blocked_bash_pattern, domain_is_allowed, file_read, file_write, format_weather_response,
        requested_weather_cities,
    };

    fn temp_root(name: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!("ironclaw-host-tools-{name}-{stamp}"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::create_dir_all(&root);
        root
    }

    #[test]
    fn bash_policy_blocks_sudo_and_rm() {
        assert_eq!(blocked_bash_pattern("sudo ls"), Some("sudo"));
        assert_eq!(blocked_bash_pattern("rm -rf /tmp/demo"), Some("rm "));
        assert_eq!(blocked_bash_pattern("echo safe"), None);
    }

    #[tokio::test]
    async fn file_tools_reject_path_escape() {
        let root = temp_root("escape");
        let read_result = file_read(&root, "../etc/passwd").await;
        assert!(read_result.is_err());

        let write_result = file_write(&root, "../outside.txt\nblocked").await;
        assert!(write_result.is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn file_tools_write_and_read_inside_root() {
        let root = temp_root("roundtrip");
        let write = file_write(&root, "notes/a.txt\nhello").await;
        assert!(write.is_ok());

        let read = file_read(&root, "notes/a.txt").await;
        assert_eq!(read.ok().as_deref(), Some("hello"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn weather_city_selection_and_domain_policy_are_explicit() {
        let cities = requested_weather_cities("latest weather in Delhi and Dubai")
            .expect("supported cities");
        assert_eq!(
            cities.iter().map(|city| city.name).collect::<Vec<_>>(),
            vec!["Dubai", "Delhi"]
        );
        assert!(requested_weather_cities("London").is_err());
        assert!(domain_is_allowed(
            "api.open-meteo.com",
            &["open-meteo.com".to_string()]
        ));
        assert!(!domain_is_allowed(
            "api.open-meteo.com.evil.test",
            &["open-meteo.com".to_string()]
        ));
    }

    #[test]
    fn weather_response_requires_and_formats_live_fields() {
        let cities = requested_weather_cities("Dubai and Delhi").expect("supported weather cities");
        let payload = serde_json::json!([
            {
                "timezone": "Asia/Dubai",
                "current": {
                    "time": "2026-07-26T12:00",
                    "temperature_2m": 39.5,
                    "apparent_temperature": 45.2,
                    "relative_humidity_2m": 43,
                    "weather_code": 1,
                    "wind_speed_10m": 14.0
                }
            },
            {
                "timezone": "Asia/Kolkata",
                "current": {
                    "time": "2026-07-26T13:30",
                    "temperature_2m": 34.0,
                    "apparent_temperature": 42.1,
                    "relative_humidity_2m": 69,
                    "weather_code": 61,
                    "wind_speed_10m": 9.1
                }
            }
        ]);
        let url =
            reqwest::Url::parse("https://api.open-meteo.com/v1/forecast").expect("weather URL");
        let output = format_weather_response(&cities, &payload, &url).expect("format weather");
        assert!(output.contains("Dubai: observed 2026-07-26T12:00 Asia/Dubai"));
        assert!(output.contains("Delhi: observed 2026-07-26T13:30 Asia/Kolkata"));
        assert!(output.contains("mainly clear"));
        assert!(output.contains("rain"));
        assert!(output.contains("Source: https://api.open-meteo.com/v1/forecast"));
    }
}
