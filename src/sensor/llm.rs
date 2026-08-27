//! LLM inference-server status sensors for vLLM and SGLang.
//!
//! The provider intentionally uses only the standard library for HTTP. An
//! unavailable inference server is a normal missing-source condition, so all
//! network and parsing failures are reported as an empty reading set.

use crate::config::LlmSensorConfig;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use super::{SensorDescriptor, SensorProvider, SensorReading};

const SENSOR_KEYS: &[&str] = &[
    "llm_model",
    "llm_engine",
    "llm_tok_s",
    "llm_running",
    "llm_waiting",
    "llm_kv_cache",
];
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const AUTO_PROBE_BACKOFF: Duration = Duration::from_secs(30);
const MODEL_LOOKUP_INTERVAL: Duration = Duration::from_secs(60);
const MIN_RATE_ELAPSED: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Engine {
    Vllm,
    Sglang,
}

impl Engine {
    fn as_str(self) -> &'static str {
        match self {
            Self::Vllm => "vllm",
            Self::Sglang => "sglang",
        }
    }

    fn from_config(value: &str) -> Option<Self> {
        match value {
            "vllm" => Some(Self::Vllm),
            "sglang" => Some(Self::Sglang),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct HttpUrl {
    host: String,
    port: u16,
    authority: String,
    base_path: String,
}

impl HttpUrl {
    fn loopback(port: u16) -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port,
            authority: format!("127.0.0.1:{port}"),
            base_path: String::new(),
        }
    }

    fn endpoint(&self, endpoint: &str) -> String {
        if self.base_path.is_empty() {
            format!("/{endpoint}")
        } else {
            format!("{}/{endpoint}", self.base_path)
        }
    }
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    body: String,
}

#[derive(Debug, Clone)]
struct CounterSample {
    engine: Engine,
    metric: String,
    value: f64,
    sampled_at: Instant,
}

#[derive(Debug, Default)]
struct ParsedMetrics {
    values: HashMap<String, MetricValue>,
    model_name: Option<String>,
}

#[derive(Debug, Default)]
struct MetricValue {
    sum: f64,
}

impl ParsedMetrics {
    fn value(&self, name: &str) -> Option<f64> {
        self.values.get(name).map(|metric| metric.sum)
    }

    fn first_value(&self, names: &[&'static str]) -> Option<(&'static str, f64)> {
        names
            .iter()
            .find_map(|name| self.value(name).map(|value| (*name, value)))
    }

    fn model_name(&self) -> Option<String> {
        self.model_name.clone()
    }
}

/// Reads vLLM/SGLang metrics over a small blocking HTTP client.
pub struct LlmProvider {
    config: LlmSensorConfig,
    needed_keys: Option<HashSet<String>>,
    last_auto_url: Option<HttpUrl>,
    auto_probe_failed_at: Option<Instant>,
    last_model_lookup: Option<Instant>,
    counter_sample: Option<CounterSample>,
}

impl Default for LlmProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmProvider {
    pub fn new() -> Self {
        Self::from_config(&LlmSensorConfig::default())
    }

    pub fn from_config(config: &LlmSensorConfig) -> Self {
        Self {
            config: config.clone(),
            needed_keys: None,
            last_auto_url: None,
            auto_probe_failed_at: None,
            last_model_lookup: None,
            counter_sample: None,
        }
    }

    fn api_key(&self) -> Option<String> {
        if !self.config.api_key.is_empty() {
            return Some(self.config.api_key.clone());
        }

        match self.config.engine.as_str() {
            "vllm" => nonempty_env("VLLM_API_KEY"),
            "sglang" => nonempty_env("SGLANG_API_KEY"),
            _ => nonempty_env("VLLM_API_KEY").or_else(|| nonempty_env("SGLANG_API_KEY")),
        }
    }

    fn fetch_metrics(
        &mut self,
        now: Instant,
        api_key: Option<&str>,
    ) -> Option<(HttpUrl, HttpResponse)> {
        if !self.config.url.is_empty() {
            let base = parse_http_url(&self.config.url)?;
            let response = http_get(&base, "metrics", self.config.timeout_ms, api_key)?;
            return is_success(&response).then_some((base, response));
        }

        if let Some(failed_at) = self.auto_probe_failed_at {
            if now
                .checked_duration_since(failed_at)
                .unwrap_or(Duration::ZERO)
                < AUTO_PROBE_BACKOFF
            {
                return None;
            }
        }

        let mut candidates = Vec::with_capacity(3);
        if let Some(last) = self.last_auto_url.clone() {
            candidates.push(last);
        }
        for port in [8000, 30000] {
            let candidate = HttpUrl::loopback(port);
            if !candidates
                .iter()
                .any(|existing| existing.host == candidate.host && existing.port == candidate.port)
            {
                candidates.push(candidate);
            }
        }

        for base in candidates {
            let Some(response) = http_get(&base, "metrics", self.config.timeout_ms, api_key) else {
                continue;
            };
            if is_success(&response) {
                self.last_auto_url = Some(base.clone());
                self.auto_probe_failed_at = None;
                return Some((base, response));
            }
        }

        self.last_auto_url = None;
        self.auto_probe_failed_at = Some(now);
        None
    }

    fn should_lookup_model(&self, now: Instant, model_known: bool) -> bool {
        if model_known {
            return false;
        }
        if let Some(needed) = &self.needed_keys {
            if !needed.contains("llm_model") {
                return false;
            }
        }
        self.last_model_lookup
            .and_then(|last| now.checked_duration_since(last))
            .map(|elapsed| elapsed >= MODEL_LOOKUP_INTERVAL)
            .unwrap_or(true)
    }

    fn lookup_model(
        &mut self,
        base: &HttpUrl,
        now: Instant,
        api_key: Option<&str>,
    ) -> Option<String> {
        self.last_model_lookup = Some(now);
        let response = http_get(base, "v1/models", self.config.timeout_ms, api_key)?;
        if !is_success(&response) {
            return None;
        }
        let json: serde_json::Value = serde_json::from_str(&response.body).ok()?;
        json.get("data")?
            .as_array()?
            .first()?
            .get("id")?
            .as_str()
            .map(str::to_string)
            .filter(|model| !model.trim().is_empty())
    }

    #[cfg(test)]
    fn readings_from_metrics(&mut self, body: &str, now: Instant) -> Vec<SensorReading> {
        let parsed = parse_prometheus(body);
        let model = parsed.model_name();
        self.readings_for_parsed(body, &parsed, model.as_deref(), now)
    }

    fn readings_for_parsed(
        &mut self,
        body: &str,
        metrics: &ParsedMetrics,
        model: Option<&str>,
        now: Instant,
    ) -> Vec<SensorReading> {
        let engine = self.detect_engine(body, metrics);
        let Some(engine) = engine else {
            return Vec::new();
        };
        if !has_known_metric(engine, metrics) {
            return Vec::new();
        }

        let mut readings = Vec::with_capacity(SENSOR_KEYS.len());
        push_model(&mut readings, model);
        readings.push(reading("llm_engine", engine.as_str(), ""));

        match engine {
            Engine::Vllm => {
                push_numeric(
                    &mut readings,
                    "llm_running",
                    metrics.value("vllm:num_requests_running"),
                    "req",
                );
                push_numeric(
                    &mut readings,
                    "llm_waiting",
                    metrics.value("vllm:num_requests_waiting"),
                    "req",
                );
                let kv = metrics
                    .value("vllm:kv_cache_usage_perc")
                    .or_else(|| metrics.value("vllm:gpu_cache_usage_perc"));
                push_percent(&mut readings, "llm_kv_cache", kv);

                let counter = metrics
                    .first_value(&["vllm:generation_tokens_total", "vllm:generation_tokens"]);
                let counter_rate =
                    counter.map(|(metric, value)| self.counter_rate(engine, metric, value, now));
                let tok_s = counter_rate
                    .flatten()
                    .or_else(|| metrics.value("vllm:avg_generation_throughput_toks_per_s"));
                push_numeric(&mut readings, "llm_tok_s", tok_s, "tok/s");
            }
            Engine::Sglang => {
                push_numeric(
                    &mut readings,
                    "llm_running",
                    metrics.value("sglang:num_running_reqs"),
                    "req",
                );
                push_numeric(
                    &mut readings,
                    "llm_waiting",
                    metrics.value("sglang:num_queue_reqs"),
                    "req",
                );
                push_percent(
                    &mut readings,
                    "llm_kv_cache",
                    metrics.value("sglang:token_usage"),
                );

                let counter_rate = metrics
                    .first_value(&["sglang:generation_tokens_total"])
                    .and_then(|(metric, value)| self.counter_rate(engine, metric, value, now));
                let tok_s = metrics.value("sglang:gen_throughput").or(counter_rate);
                push_numeric(&mut readings, "llm_tok_s", tok_s, "tok/s");
            }
        }

        readings
    }

    fn detect_engine(&self, body: &str, metrics: &ParsedMetrics) -> Option<Engine> {
        if self.config.engine != "auto" {
            return Engine::from_config(&self.config.engine);
        }
        if body.contains("sglang:") {
            return Some(Engine::Sglang);
        }
        if body.contains("vllm:") {
            return Some(Engine::Vllm);
        }
        if metrics
            .values
            .keys()
            .any(|name| name.starts_with("sglang:"))
        {
            return Some(Engine::Sglang);
        }
        if metrics.values.keys().any(|name| name.starts_with("vllm:")) {
            return Some(Engine::Vllm);
        }
        None
    }

    fn counter_rate(
        &mut self,
        engine: Engine,
        metric: &str,
        value: f64,
        now: Instant,
    ) -> Option<f64> {
        if !value.is_finite() {
            return None;
        }

        let previous = self.counter_sample.take();
        let rate = previous.and_then(|previous| {
            if previous.engine != engine || previous.metric != metric || value < previous.value {
                return None;
            }
            let elapsed = now
                .checked_duration_since(previous.sampled_at)
                .unwrap_or(Duration::ZERO);
            if elapsed < MIN_RATE_ELAPSED {
                return None;
            }
            Some((value - previous.value) / elapsed.as_secs_f64())
        });
        self.counter_sample = Some(CounterSample {
            engine,
            metric: metric.to_string(),
            value,
            sampled_at: now,
        });
        rate.filter(|rate| rate.is_finite() && *rate >= 0.0)
    }
}

impl SensorProvider for LlmProvider {
    fn name(&self) -> &str {
        "llm"
    }

    fn set_needed_keys(&mut self, keys: Option<&HashSet<String>>) {
        self.needed_keys = keys.cloned();
    }

    fn wants_any(&self, needed: &HashSet<String>) -> bool {
        SENSOR_KEYS.iter().any(|key| needed.contains(*key))
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
        if let Some(needed) = &self.needed_keys {
            if !SENSOR_KEYS.iter().any(|key| needed.contains(*key)) {
                return Ok(Vec::new());
            }
        }

        let now = Instant::now();
        let api_key = self.api_key();
        let Some((base, response)) = self.fetch_metrics(now, api_key.as_deref()) else {
            return Ok(Vec::new());
        };

        let metrics = parse_prometheus(&response.body);
        let mut model = metrics.model_name();
        if self.should_lookup_model(now, model.is_some()) {
            if let Some(found) = self.lookup_model(&base, now, api_key.as_deref()) {
                model = Some(found);
            }
        }

        // Keep this branch explicit: a model returned by /v1/models is only a
        // fallback, while metric labels remain the primary source of identity.
        if model.as_ref().is_some_and(|model| model.trim().is_empty()) {
            model = None;
        }
        let readings = self.readings_for_parsed(&response.body, &metrics, model.as_deref(), now);
        Ok(readings)
    }

    fn available_sensors(&self) -> Vec<SensorDescriptor> {
        [
            ("llm_model", "LLM Model", ""),
            ("llm_engine", "LLM Engine", ""),
            ("llm_tok_s", "LLM Tokens/s", "tok/s"),
            ("llm_running", "LLM Running", "req"),
            ("llm_waiting", "LLM Waiting", "req"),
            ("llm_kv_cache", "LLM KV Cache", "%"),
        ]
        .into_iter()
        .map(|(key, name, unit)| SensorDescriptor::new(key, name, unit))
        .collect()
    }

    fn declared_keys(&self) -> Vec<&str> {
        SENSOR_KEYS.to_vec()
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn is_success(response: &HttpResponse) -> bool {
    (200..300).contains(&response.status)
}

fn has_known_metric(engine: Engine, metrics: &ParsedMetrics) -> bool {
    let names: &[&str] = match engine {
        Engine::Vllm => &[
            "vllm:num_requests_running",
            "vllm:num_requests_waiting",
            "vllm:kv_cache_usage_perc",
            "vllm:gpu_cache_usage_perc",
            "vllm:generation_tokens_total",
            "vllm:generation_tokens",
            "vllm:avg_generation_throughput_toks_per_s",
        ],
        Engine::Sglang => &[
            "sglang:num_running_reqs",
            "sglang:num_queue_reqs",
            "sglang:token_usage",
            "sglang:gen_throughput",
            "sglang:generation_tokens_total",
        ],
    };
    names.iter().any(|name| metrics.values.contains_key(*name))
}

fn parse_http_url(raw: &str) -> Option<HttpUrl> {
    let rest = raw.strip_prefix("http://")?;
    if rest.is_empty()
        || rest
            .chars()
            .any(|ch| ch.is_ascii_control() || ch.is_whitespace())
    {
        return None;
    }

    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let path = &rest[authority_end..];
    if path.contains('?') || path.contains('#') {
        return None;
    }

    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let close = bracketed.find(']')?;
        let host = &bracketed[..close];
        if host.is_empty() {
            return None;
        }
        let after = &bracketed[close + 1..];
        let port = if after.is_empty() {
            80
        } else {
            let port = after.strip_prefix(':')?.parse().ok()?;
            if port == 0 {
                return None;
            }
            port
        };
        (host.to_string(), port)
    } else {
        if authority.contains(':') && authority.matches(':').count() > 1 {
            return None;
        }
        match authority.rsplit_once(':') {
            Some((host, port_text))
                if !port_text.is_empty() && port_text.chars().all(|ch| ch.is_ascii_digit()) =>
            {
                let port = port_text.parse().ok()?;
                if port == 0 || host.is_empty() {
                    return None;
                }
                (host.to_string(), port)
            }
            _ => (authority.to_string(), 80),
        }
    };

    if host.is_empty() || host.chars().any(|ch| ch.is_ascii_control()) {
        return None;
    }

    let base_path = path.trim_end_matches('/').to_string();
    Some(HttpUrl {
        host,
        port,
        authority: authority.to_string(),
        base_path,
    })
}

fn http_get(
    base: &HttpUrl,
    endpoint: &str,
    timeout_ms: u64,
    api_key: Option<&str>,
) -> Option<HttpResponse> {
    let timeout = Duration::from_millis(timeout_ms);
    let address = if base.host.contains(':') {
        format!("[{}]:{}", base.host, base.port)
    } else {
        format!("{}:{}", base.host, base.port)
    };
    let mut stream = address
        .to_socket_addrs()
        .ok()?
        .filter_map(|addr| TcpStream::connect_timeout(&addr, timeout).ok())
        .next()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;

    let mut request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: thermalwriter-llm/1\r\nConnection: close\r\n",
        base.endpoint(endpoint),
        base.authority
    );
    if let Some(api_key) = api_key {
        if api_key
            .chars()
            .any(|ch| ch == '\r' || ch == '\n' || ch.is_ascii_control())
        {
            return None;
        }
        request.push_str("Authorization: Bearer ");
        request.push_str(api_key);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).ok()?;

    let mut response = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut header_end = None;
    loop {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);
        if header_end.is_none() {
            header_end = find_header_end(&response);
            if header_end.is_none() && response.len() > MAX_HEADER_BYTES {
                return None;
            }
        }
        if let Some(header_end) = header_end {
            if response.len().saturating_sub(header_end) > MAX_RESPONSE_BYTES {
                return None;
            }
        }
        if response.len() > MAX_HEADER_BYTES + MAX_RESPONSE_BYTES {
            return None;
        }
    }

    let header_end = header_end?;
    let headers = std::str::from_utf8(&response[..header_end]).ok()?;
    let status = headers
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let body = String::from_utf8_lossy(&response[header_end + 4..]).into_owned();
    Some(HttpResponse { status, body })
}

fn find_header_end(response: &[u8]) -> Option<usize> {
    response.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_prometheus(text: &str) -> ParsedMetrics {
    let mut parsed = ParsedMetrics::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, labels, value_text)) = split_metric_line(line) else {
            continue;
        };
        let Ok(value) = value_text.parse::<f64>() else {
            continue;
        };
        if !value.is_finite() || name.is_empty() {
            continue;
        }

        let model_name = labels
            .and_then(|labels| parse_label(labels, "model_name"))
            .filter(|model| !model.trim().is_empty());
        if parsed.model_name.is_none() {
            parsed.model_name = model_name;
        }
        let metric = parsed.values.entry(name.to_string()).or_default();
        metric.sum += value;
    }
    parsed
}

fn split_metric_line(line: &str) -> Option<(&str, Option<&str>, &str)> {
    let open = line.find('{');
    match open {
        Some(open) => {
            let close = matching_brace(line, open)?;
            let name = line[..open].trim();
            let labels = &line[open + 1..close];
            let value = line[close + 1..].split_whitespace().next()?;
            Some((name, Some(labels), value))
        }
        None => {
            let mut parts = line.split_whitespace();
            let name = parts.next()?;
            let value = parts.next()?;
            Some((name, None, value))
        }
    }
}

fn matching_brace(line: &str, open: usize) -> Option<usize> {
    let content = line.get(open + 1..)?;
    let mut quoted = false;
    let mut escaped = false;
    for (offset, ch) in content.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quoted {
            escaped = true;
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
        } else if ch == '}' && !quoted {
            return Some(open + 1 + offset);
        }
    }
    None
}

fn parse_label(labels: &str, wanted: &str) -> Option<String> {
    let mut rest = labels;
    loop {
        rest = rest.trim_start();
        if let Some(stripped) = rest.strip_prefix(',') {
            rest = stripped;
            continue;
        }
        if rest.is_empty() {
            return None;
        }

        let equals = rest.find('=')?;
        let name = rest[..equals].trim();
        let mut value = rest[equals + 1..].trim_start();
        if !value.starts_with('"') {
            return None;
        }
        value = &value[1..];
        let mut decoded = String::new();
        let mut escaped = false;
        let mut end = None;
        for (index, ch) in value.char_indices() {
            if escaped {
                decoded.push(match ch {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    other => other,
                });
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                end = Some(index);
                break;
            } else {
                decoded.push(ch);
            }
        }
        let end = end?;
        if name == wanted {
            return Some(decoded);
        }
        rest = &value[end + 1..];
    }
}

fn push_model(readings: &mut Vec<SensorReading>, model: Option<&str>) {
    if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
        readings.push(reading("llm_model", shorten_model(model), ""));
    }
}

fn push_numeric(readings: &mut Vec<SensorReading>, key: &str, value: Option<f64>, unit: &str) {
    if let Some(value) = value.filter(|value| value.is_finite()) {
        readings.push(reading(key, format_integer(value), unit));
    }
}

fn push_percent(readings: &mut Vec<SensorReading>, key: &str, value: Option<f64>) {
    if let Some(value) = value.filter(|value| value.is_finite()) {
        let percentage = (value * 100.0).clamp(0.0, 100.0);
        readings.push(reading(key, format_integer(percentage), "%"));
    }
}

fn reading(key: &str, value: impl Into<String>, unit: &str) -> SensorReading {
    SensorReading {
        key: key.to_string(),
        value: value.into(),
        unit: unit.to_string(),
    }
}

fn format_integer(value: f64) -> String {
    format!("{}", value.round() as i64)
}

fn shorten_model(model: &str) -> String {
    let model = model.trim();
    let short = model.rsplit('/').next().unwrap_or(model);
    if short.is_empty() {
        model.to_string()
    } else {
        short.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn value<'a>(readings: &'a [SensorReading], key: &str) -> Option<&'a str> {
        readings
            .iter()
            .find(|reading| reading.key == key)
            .map(|reading| reading.value.as_str())
    }

    #[test]
    fn prometheus_vllm_metrics_sum_series_and_rate_counters() {
        let first = r#"
# HELP vllm:num_requests_running running
vllm:num_requests_running{model_name="org/llama",replica="0"} 2
vllm:num_requests_running{model_name="org/llama",replica="1"} 3
vllm:num_requests_waiting 1
vllm:kv_cache_usage_perc{model_name="org/llama"} 0.42
vllm:generation_tokens_total{model_name="org/llama"} 1.23e+03
"#;
        let second = first.replace("1.23e+03", "1.33e+03");
        let mut provider = LlmProvider::from_config(&LlmSensorConfig {
            engine: "auto".into(),
            ..Default::default()
        });
        let t0 = Instant::now();
        let first_readings = provider.readings_from_metrics(first, t0);
        assert_eq!(value(&first_readings, "llm_running"), Some("5"));
        assert_eq!(value(&first_readings, "llm_waiting"), Some("1"));
        assert_eq!(value(&first_readings, "llm_kv_cache"), Some("42"));
        assert_eq!(value(&first_readings, "llm_model"), Some("llama"));
        assert!(value(&first_readings, "llm_tok_s").is_none());

        let second_readings = provider.readings_from_metrics(&second, t0 + Duration::from_secs(1));
        assert_eq!(value(&second_readings, "llm_tok_s"), Some("100"));
    }

    #[test]
    fn prometheus_sglang_uses_gauge_throughput() {
        let body = r#"
sglang:num_running_reqs{model_name="meta/Llama"} 4
sglang:num_queue_reqs{model_name="meta/Llama"} 2
sglang:token_usage{model_name="meta/Llama"} 0.28
sglang:gen_throughput{model_name="meta/Llama"} 86.508
"#;
        let mut provider = LlmProvider::from_config(&LlmSensorConfig::default());
        let readings = provider.readings_from_metrics(body, Instant::now());
        assert_eq!(value(&readings, "llm_engine"), Some("sglang"));
        assert_eq!(value(&readings, "llm_running"), Some("4"));
        assert_eq!(value(&readings, "llm_waiting"), Some("2"));
        assert_eq!(value(&readings, "llm_kv_cache"), Some("28"));
        assert_eq!(value(&readings, "llm_tok_s"), Some("87"));
    }

    #[test]
    fn prometheus_legacy_vllm_names_are_supported() {
        let body = r#"
# TYPE vllm:num_requests_running gauge
vllm:num_requests_running 1
vllm:num_requests_waiting 2
vllm:gpu_cache_usage_perc 0.75
vllm:generation_tokens 10
"#;
        let mut provider = LlmProvider::from_config(&LlmSensorConfig {
            engine: "vllm".into(),
            ..Default::default()
        });
        let readings = provider.readings_from_metrics(body, Instant::now());
        assert_eq!(value(&readings, "llm_running"), Some("1"));
        assert_eq!(value(&readings, "llm_waiting"), Some("2"));
        assert_eq!(value(&readings, "llm_kv_cache"), Some("75"));
        assert!(value(&readings, "llm_tok_s").is_none());
    }

    #[test]
    fn counter_reset_replaces_baseline_without_emitting_rate() {
        let mut provider = LlmProvider::from_config(&LlmSensorConfig {
            engine: "vllm".into(),
            ..Default::default()
        });
        let t0 = Instant::now();
        let first = "vllm:generation_tokens_total 100";
        let reset = "vllm:generation_tokens_total 10";
        let after_reset = "vllm:generation_tokens_total 20";
        assert!(value(&provider.readings_from_metrics(first, t0), "llm_tok_s").is_none());
        assert!(
            value(
                &provider.readings_from_metrics(reset, t0 + Duration::from_secs(1)),
                "llm_tok_s"
            )
            .is_none()
        );
        assert_eq!(
            value(
                &provider.readings_from_metrics(after_reset, t0 + Duration::from_secs(2)),
                "llm_tok_s"
            ),
            Some("10")
        );
    }

    #[test]
    fn descriptors_and_declarations_are_static_when_server_is_down() {
        let provider = LlmProvider::new();
        assert_eq!(provider.declared_keys(), SENSOR_KEYS);
        assert_eq!(provider.available_sensors().len(), 6);
        let needed = HashSet::from(["llm_model".to_string()]);
        assert!(provider.wants_any(&needed));
    }

    #[test]
    fn http_url_parser_keeps_base_path_and_rejects_https() {
        let parsed = parse_http_url("http://localhost:1234/api/").expect("valid URL");
        assert_eq!(parsed.host, "localhost");
        assert_eq!(parsed.port, 1234);
        assert_eq!(parsed.endpoint("metrics"), "/api/metrics");
        assert!(parse_http_url("https://localhost:1234").is_none());
    }
}
