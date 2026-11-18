//! Web search providers for `:discover`: one request to one provider, normalised to hits.

use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use yeda_core::discover::Hit;
use yeda_core::web_search::{Provider, searxng_host};

/// A search request outlives no phase deadline.
pub const TIMEOUT: Duration = Duration::from_secs(15);

const MAX_REPLY: u64 = 8 << 20;
const USER_AGENT: &str = concat!("yeda/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The provider said it is limited; `wait` is its own answer, if it gave one.
    RateLimited { wait: Option<Duration> },
    /// Any other failure; the provider serves this process no more.
    Down,
    /// A setting is missing or wrong; the provider is not down.
    Setting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub reason: String,
    pub kind: Kind,
}

impl Failure {
    pub fn rate_limited(wait: Option<Duration>) -> Self {
        Failure { reason: "rate limited".into(), kind: Kind::RateLimited { wait } }
    }

    pub fn down(reason: impl Into<String>) -> Self {
        Failure { reason: reason.into(), kind: Kind::Down }
    }

    pub fn setting(reason: impl Into<String>) -> Self {
        Failure { reason: reason.into(), kind: Kind::Setting }
    }

    /// A rate limit, whether or not the provider named a wait.
    pub fn limited(&self) -> bool {
        matches!(self.kind, Kind::RateLimited { .. })
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for Failure {}

/// A provider's answer: its hits, and when it says its quota returns, if it does.
pub struct Answer {
    pub hits: Vec<Hit>,
    pub reset_in: Option<Duration>,
}

pub struct Search<'a> {
    pub provider: Provider,
    pub query: &'a str,
    pub limit: usize,
    /// The provider's own API key, if the store has one.
    pub key: Option<&'a str>,
    pub searxng_url: Option<&'a str>,
}

/// Searches one provider. `model_provider_web_search` is not here: it uses the LLM profile.
pub fn search(s: &Search<'_>) -> Result<Answer, Failure> {
    match s.provider {
        Provider::Keenable => keenable(s),
        Provider::Exa => exa(s),
        Provider::Parallel => parallel(s),
        Provider::Firecrawl => firecrawl(s),
        Provider::Searxng => searxng(s),
        Provider::ModelProviderWebSearch => Err(Failure::setting("no search on this profile")),
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent(USER_AGENT)
        .http_status_as_error(false)
        .https_only(true)
        .max_redirects(0)
        .build()
        .into()
}

struct Ctx<'a> {
    provider: Provider,
    keyed: bool,
    key: Option<&'a str>,
}

fn headers(ctx: &Ctx<'_>) -> Vec<(String, String)> {
    let accept = if !ctx.keyed && matches!(ctx.provider, Provider::Exa | Provider::Parallel) {
        "application/json, text/event-stream"
    } else {
        "application/json"
    };
    let mut out: Vec<(String, String)> = vec![("accept".into(), accept.into()), ("user-agent".into(), USER_AGENT.into())];
    if let Some(key) = ctx.key {
        let (name, value) = match ctx.provider {
            Provider::Keenable => ("x-api-key", key.to_string()),
            Provider::Exa | Provider::Parallel => ("x-api-key", key.to_string()),
            Provider::Firecrawl => ("authorization", format!("Bearer {key}")),
            _ => ("", String::new()),
        };
        if !name.is_empty() {
            out.push((name.into(), value));
        }
    }
    if !ctx.keyed && ctx.provider == Provider::Keenable {
        // Keyless Keenable asks each caller to name itself.
        out.push(("x-keenable-title".into(), USER_AGENT.into()));
    }
    out
}

struct Reply {
    reset_in: Option<Duration>,
    body: String,
}

fn call(result: Result<ureq::http::Response<ureq::Body>, ureq::Error>, ctx: &Ctx<'_>) -> Result<Reply, Failure> {
    let mut resp = match result {
        Ok(resp) => resp,
        Err(e) => return Err(transport(e)),
    };
    let status = resp.status().as_u16();
    let headers = resp.headers();
    let retry_after = headers.get("retry-after").and_then(|v| v.to_str().ok()).and_then(parse_retry_after);
    let reset = headers.get("x-ratelimit-reset").and_then(|v| v.to_str().ok()).and_then(parse_reset);
    let remaining = headers.get("x-ratelimit-remaining").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    let text = resp.body_mut().with_config().limit(MAX_REPLY).read_to_string().map_err(|_| Failure::down("bad response"))?;
    if !(200..300).contains(&status) {
        return Err(status_failure(status, retry_after, reset, ctx));
    }
    if remaining == Some(0)
        && let Some(reset) = reset
    {
        return Ok(Reply { reset_in: Some(reset), body: text });
    }
    Ok(Reply { reset_in: None, body: text })
}

fn transport(e: ureq::Error) -> Failure {
    match e {
        ureq::Error::Timeout(_) => Failure::down("timeout"),
        ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::TimedOut => Failure::down("timeout"),
        _ => Failure::down("can't connect"),
    }
}

fn status_failure(status: u16, retry_after: Option<Duration>, reset: Option<Duration>, ctx: &Ctx<'_>) -> Failure {
    if status == 429 {
        return Failure::rate_limited(retry_after.or(reset));
    }
    if matches!(status, 503) && retry_after.is_some() {
        return Failure::rate_limited(retry_after);
    }
    if (300..400).contains(&status) {
        return Failure::down("unexpected redirect");
    }
    if matches!(status, 401 | 403) {
        if ctx.keyed {
            return Failure::down("key rejected");
        }
        return Failure::down(match ctx.provider {
            Provider::Exa => "blocked: needs direct IPv4".to_string(),
            Provider::Searxng => "no JSON output: enable json in search.formats".to_string(),
            _ => format!("HTTP {status}"),
        });
    }
    if status >= 500 {
        return Failure::down(format!("server error (HTTP {status})"));
    }
    Failure::down(format!("HTTP {status}"))
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    if let Ok(secs) = value.trim().parse::<u64>() {
        return (secs > 0).then(|| Duration::from_secs(secs));
    }
    http_date(value).and_then(|then| then.duration_since(SystemTime::now()).ok())
}

fn parse_reset(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().and_then(|secs| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs))?.duration_since(SystemTime::now()).ok())
}

/// `Sun, 06 Nov 1994 08:49:37 GMT`, the HTTP-date form of `Retry-After`.
fn http_date(value: &str) -> Option<SystemTime> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    let (day, month, year, time) = match parts.as_slice() {
        [_, day, month, year, time, _] => (*day, *month, *year, *time),
        _ => return None,
    };
    let day: u64 = day.trim_end_matches(',').parse().ok()?;
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|m| *m == month)? as u64 + 1;
    let year: u64 = year.parse().ok()?;
    let mut clock = time.split(':');
    let (h, m, s): (u64, u64, u64) = (clock.next()?.parse().ok()?, clock.next()?.parse().ok()?, clock.next()?.parse().ok()?);
    let y = if month <= 2 { year - 1 } else { year };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + m * 60 + s;
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

fn host_of(url: &str) -> Option<&str> {
    url.strip_prefix("https://")?.split(['/', '?']).next()
}

fn post(url: &str, host: &str, ctx: &Ctx<'_>, body: &Value) -> Result<Reply, Failure> {
    if host_of(url) != Some(host) {
        return Err(Failure::down("unexpected redirect"));
    }
    let agent = agent();
    let mut req = agent.post(url);
    for (k, v) in headers(ctx) {
        req = req.header(&k, &v);
    }
    req = req.header("content-type", "application/json");
    call(req.send(body.to_string()), ctx)
}

fn get(url: &str, host: &str, ctx: &Ctx<'_>, params: &[(&str, String)]) -> Result<Reply, Failure> {
    if host_of(url) != Some(host) {
        return Err(Failure::down("unexpected redirect"));
    }
    let agent = agent();
    let mut req = agent.get(url);
    for (k, v) in headers(ctx) {
        req = req.header(&k, &v);
    }
    for (k, v) in params {
        req = req.query(k, v);
    }
    call(req.call(), ctx)
}

fn hits(v: &Value) -> Vec<Hit> {
    let mut out = Vec::new();
    walk(v, &mut out);
    out
}

fn walk(v: &Value, out: &mut Vec<Hit>) {
    match v {
        Value::Object(o) => {
            if let Some(url) = o.get("url").and_then(Value::as_str) {
                let title = ["title", "name", "display_name"].iter().find_map(|k| o.get(*k).and_then(Value::as_str)).unwrap_or_default().to_string();
                let text = ["snippet", "description", "content", "text", "summary"]
                    .iter()
                    .find_map(|k| o.get(*k).and_then(Value::as_str))
                    .map(String::from)
                    .or_else(|| {
                        ["excerpts", "highlights"].iter().find_map(|k| o.get(*k).and_then(Value::as_array)).map(|a| {
                            a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")
                        })
                    });
                out.push(hit(title, url, text));
            }
            for (_, child) in o {
                walk(child, out);
            }
        }
        Value::Array(a) => {
            for child in a {
                walk(child, out);
            }
        }
        _ => {}
    }
}

fn hit(title: String, url: &str, text: Option<String>) -> Hit {
    let doi = url.strip_prefix("https://doi.org/").or_else(|| url.strip_prefix("http://doi.org/")).and_then(yeda_core::discover::normalize_doi);
    let arxiv = url
        .split("arxiv.org/abs/")
        .nth(1)
        .or_else(|| url.split("arxiv.org/pdf/").nth(1))
        .map(|id| id.split(['?', '#', '/']).next().unwrap_or(id).trim_end_matches(".pdf").to_string());
    Hit { title, url: Some(url.to_string()), doi, arxiv, text }
}

fn json_reply(reply: &Reply) -> Result<Value, Failure> {
    serde_json::from_str(&reply.body).map_err(|_| Failure::down("bad response"))
}

fn keenable(s: &Search<'_>) -> Result<Answer, Failure> {
    let ctx = Ctx { provider: s.provider, keyed: s.key.is_some(), key: s.key };
    let (url, host) = if ctx.keyed {
        ("https://api.keenable.ai/v1/search", "api.keenable.ai")
    } else {
        ("https://api.keenable.ai/v1/search/public", "api.keenable.ai")
    };
    let reply = post(url, host, &ctx, &json!({"query": s.query, "max_results": s.limit}))?;
    Ok(Answer { hits: hits(&json_reply(&reply)?), reset_in: reply.reset_in })
}

fn exa(s: &Search<'_>) -> Result<Answer, Failure> {
    let ctx = Ctx { provider: s.provider, keyed: s.key.is_some(), key: s.key };
    if ctx.keyed {
        let body = json!({"query": s.query, "numResults": s.limit, "contents": {"highlights": true}});
        let reply = post("https://api.exa.ai/search", "api.exa.ai", &ctx, &body)?;
        Ok(Answer { hits: hits(&json_reply(&reply)?), reset_in: reply.reset_in })
    } else {
        mcp(&ctx, "https://mcp.exa.ai/mcp", "mcp.exa.ai", "web_search_exa", json!({"query": s.query, "numResults": s.limit}))
    }
}

fn parallel(s: &Search<'_>) -> Result<Answer, Failure> {
    let ctx = Ctx { provider: s.provider, keyed: s.key.is_some(), key: s.key };
    if ctx.keyed {
        let body = json!({"objective": s.query, "search_queries": [s.query], "advanced_settings": {"max_results": s.limit}});
        let reply = post("https://api.parallel.ai/v1/search", "api.parallel.ai", &ctx, &body)?;
        Ok(Answer { hits: hits(&json_reply(&reply)?), reset_in: reply.reset_in })
    } else {
        let args = json!({"objective": s.query, "search_queries": [s.query]});
        mcp(&ctx, "https://search.parallel.ai/mcp", "search.parallel.ai", "web_search", args)
    }
}

fn firecrawl(s: &Search<'_>) -> Result<Answer, Failure> {
    let ctx = Ctx { provider: s.provider, keyed: s.key.is_some(), key: s.key };
    let reply = post("https://api.firecrawl.dev/v2/search", "api.firecrawl.dev", &ctx, &json!({"query": s.query, "limit": s.limit}))?;
    Ok(Answer { hits: hits(&json_reply(&reply)?), reset_in: reply.reset_in })
}

fn searxng(s: &Search<'_>) -> Result<Answer, Failure> {
    let Some(base) = s.searxng_url else { return Err(Failure::setting("no URL")) };
    let Some(host) = searxng_host(base) else { return Err(Failure::setting("no URL")) };
    let ctx = Ctx { provider: s.provider, keyed: false, key: None };
    let url = format!("{}/search", base.trim_end_matches('/'));
    let reply = get(&url, &host, &ctx, &[("q", s.query.to_string()), ("format", "json".to_string())])?;
    let body = json_reply(&reply)?;
    if body["results"].as_array().is_none() {
        return Err(Failure::down("bad response"));
    }
    Ok(Answer { hits: hits(&body["results"]), reset_in: reply.reset_in })
}

/// One JSON-RPC tool call to a hosted MCP server; the result's text is JSON or markdown links.
fn mcp(ctx: &Ctx<'_>, url: &str, host: &str, tool: &str, arguments: Value) -> Result<Answer, Failure> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": arguments}});
    let reply = post(url, host, ctx, &body)?;
    let value = match serde_json::from_str(&reply.body) {
        Ok(v) => v,
        Err(_) => sse_json(&reply.body).ok_or_else(|| Failure::down("bad response"))?,
    };
    if value.get("error").is_some_and(|e| !e.is_null()) {
        return Err(Failure::down("bad response"));
    }
    let mut out = Vec::new();
    for content in value["result"]["content"].as_array().into_iter().flatten() {
        let Some(text) = content["text"].as_str() else { continue };
        match serde_json::from_str::<Value>(text) {
            Ok(v) => out.extend(hits(&v)),
            Err(_) => out.extend(markdown_links(text)),
        }
    }
    if out.is_empty() && value["result"]["content"].as_array().is_none() {
        return Err(Failure::down("bad response"));
    }
    Ok(Answer { hits: out, reset_in: reply.reset_in })
}

fn sse_json(body: &str) -> Option<Value> {
    body.lines().find_map(|line| serde_json::from_str(line.strip_prefix("data:").unwrap_or(line).trim()).ok())
}

/// `[title](https://…)` links in an MCP answer written as text.
fn markdown_links(text: &str) -> Vec<Hit> {
    let mut out = Vec::new();
    for (i, _) in text.match_indices("](http") {
        let title_start = text[..i].rfind('[').map(|j| j + 1).unwrap_or(i);
        let url_start = i + 2;
        let Some(url_end) = text[url_start..].find(')').map(|j| url_start + j) else { continue };
        let url = text[url_start..url_end].trim();
        out.push(hit(text[title_start..i].trim().to_string(), url, None));
    }
    out
}

/// arXiv's Atom answer, one combined query for every query of a `:discover`.
pub fn arxiv(query: &str, max_results: usize) -> Result<Answer, Failure> {
    let ctx = Ctx { provider: Provider::Firecrawl, keyed: false, key: None };
    let reply = get(
        "https://export.arxiv.org/api/query",
        "export.arxiv.org",
        &ctx,
        &[("search_query", query.to_string()), ("start", "0".to_string()), ("max_results", max_results.to_string())],
    )?;
    let mut out = Vec::new();
    for entry in reply.body.split("<entry>").skip(1) {
        let entry = entry.split("</entry>").next().unwrap_or(entry);
        let field = |name: &str| xml_field(entry, name);
        let Some(id) = field("id") else { continue };
        let title = field("title").unwrap_or_default();
        let text = field("summary");
        out.push(hit(title, &id, text));
    }
    Ok(Answer { hits: out, reset_in: reply.reset_in })
}

fn xml_field(entry: &str, name: &str) -> Option<String> {
    let start = entry.find(&format!("<{name}>"))? + name.len() + 2;
    let end = entry[start..].find(&format!("</{name}>"))? + start;
    let text = entry[start..end].trim();
    let text = text.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'");
    (!text.is_empty()).then(|| text.split_whitespace().collect::<Vec<_>>().join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_provider_shape() {
        let keen = json!({"results": [{"title": "A", "url": "https://doi.org/10.1/X", "snippet": "s"}]});
        let h = hits(&keen);
        assert_eq!((h[0].title.as_str(), h[0].doi.as_deref()), ("A", Some("10.1/x")));
        let exa = json!({"results": [{"title": "B", "url": "https://arxiv.org/abs/2401.01234", "highlights": ["one", "two"]}]});
        assert_eq!(hits(&exa)[0].text.as_deref(), Some("one\ntwo"));
        assert_eq!(hits(&exa)[0].arxiv.as_deref(), Some("2401.01234"));
        let fire = json!({"success": true, "data": {"web": [{"title": "C", "url": "https://c.org", "description": "d"}]}});
        assert_eq!(hits(&fire)[0].title, "C");
    }

    #[test]
    fn reads_sse_markdown_and_atom() {
        assert_eq!(sse_json("event: message\ndata: {\"result\":{}}").unwrap()["result"].as_object().unwrap().len(), 0);
        let links = markdown_links("1. [Paper](https://arxiv.org/abs/2401.00001) - text");
        assert_eq!(links[0].url.as_deref(), Some("https://arxiv.org/abs/2401.00001"));
        assert_eq!(links[0].title, "Paper");
        let entry = "<entry><id>http://arxiv.org/abs/2401.01234v1</id><title>  A &amp; B </title><summary>x\ny</summary></entry>";
        assert_eq!(xml_field(entry, "title").as_deref(), Some("A & B"));
        assert_eq!(xml_field(entry, "summary").as_deref(), Some("x y"));
    }

    #[test]
    fn classifies_failures_and_waits() {
        let ctx = Ctx { provider: Provider::Exa, keyed: false, key: None };
        assert_eq!(status_failure(429, Some(Duration::from_secs(30)), None, &ctx), Failure::rate_limited(Some(Duration::from_secs(30))));
        assert_eq!(status_failure(403, None, None, &ctx), Failure::down("blocked: needs direct IPv4"));
        assert_eq!(status_failure(503, Some(Duration::from_secs(5)), None, &ctx).reason, "rate limited");
        assert_eq!(status_failure(500, None, None, &ctx).reason, "server error (HTTP 500)");
        assert_eq!(status_failure(302, None, None, &ctx).reason, "unexpected redirect");
        let keyed = Ctx { provider: Provider::Exa, keyed: true, key: Some("k") };
        assert_eq!(status_failure(401, None, None, &keyed).reason, "key rejected");
        assert!(parse_retry_after("Sun, 06 Nov 2094 08:49:37 GMT").is_some());
        assert_eq!(parse_retry_after("120"), Some(Duration::from_secs(120)));
    }
}
