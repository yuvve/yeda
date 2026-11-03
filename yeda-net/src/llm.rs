use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::Duration;
use yeda_core::llm::{Profile, Provider};

/// The paper's PDF; `text` extracts its plain text, called only for providers that need it.
pub struct Paper<'a> {
    pub pdf: &'a [u8],
    pub text: &'a dyn Fn() -> Result<String>,
}

pub struct Prompt<'a> {
    pub system: String,
    pub text: String,
    pub paper: Option<Paper<'a>>,
    pub schema: Value,
}

pub struct Reply {
    pub text: String,
    /// Something the user should know about how the request was made (e.g. truncation).
    pub note: Option<String>,
}

const ANTHROPIC_REQUEST_LIMIT: usize = 32 * 1024 * 1024;

/// Keys hold no whitespace; a long token copied from a terminal can wrap onto several lines.
fn clean(key: Option<&str>) -> Option<String> {
    key.map(|k| k.split_whitespace().collect::<String>()).filter(|k| !k.is_empty())
}

/// `key` is the API key or OAuth token stored for the profile's `url()`, if any.
pub fn complete(p: &Profile, prompt: &Prompt, key: Option<&str>) -> Result<Reply> {
    let key = clean(key);
    let key = key.as_deref();
    if key.is_none() && p.needs_key() {
        bail!("no API key or OAuth token for {}", p.url());
    }
    match p.provider {
        Provider::Anthropic => anthropic(p, prompt, key.unwrap_or_default()),
        Provider::Openai => openai(p, prompt, key),
    }
}

fn post(url: &str, headers: &[(&str, String)], body: &Value) -> Result<Value> {
    // The key goes in a header, so only to https servers (or a local one).
    yeda_core::require_https(url)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15 * 60)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut req = agent.post(url).header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let mut resp = req.send(serde_json::to_string(body)?).with_context(|| url.to_string())?;
    let status = resp.status();
    let text = resp.body_mut().read_to_string()?;
    if !status.is_success() {
        bail!("{url}: HTTP {status}: {}", text.chars().take(500).collect::<String>());
    }
    serde_json::from_str(&text).with_context(|| format!("{url}: response is not JSON"))
}

fn anthropic(p: &Profile, prompt: &Prompt, key: &str) -> Result<Reply> {
    let base = p.url();
    let mut body = json!({
        "model": p.model,
        "max_tokens": p.max_tokens.unwrap_or(32000),
        "system": prompt.system,
        "output_config": {
            "effort": p.effort.as_deref().unwrap_or("high"),
            "format": {"type": "json_schema", "schema": prompt.schema},
        },
    });
    let headers = authorize(p, key, &mut body)?;
    let question = json!({"type": "text", "text": prompt.text});
    let mut note = None;
    body["messages"] = match &prompt.paper {
        Some(paper) => {
            let data = base64::engine::general_purpose::STANDARD.encode(paper.pdf);
            let doc = json!({"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": data}});
            body["messages"] = json!([{"role": "user", "content": [doc, question]}]);
            if serde_json::to_vec(&body)?.len() <= ANTHROPIC_REQUEST_LIMIT {
                body["messages"].take()
            } else {
                note = Some("PDF too large for the request limit (32 MB); sent extracted text instead".to_string());
                let text = json!({"type": "text", "text": format!("Paper text:\n{}", (paper.text)()?)});
                json!([{"role": "user", "content": [text, question]}])
            }
        }
        None => {
            note = Some("no PDF; summarized from metadata and notes only".into());
            json!([{"role": "user", "content": [question]}])
        }
    };
    let v = post(&format!("{base}/v1/messages"), &headers, &body)?;
    match v["stop_reason"].as_str() {
        Some("refusal") => bail!("the model declined ({})", refusal_category(&v)),
        Some("max_tokens") => bail!("output hit max_tokens ({}); raise max_tokens in the llm profile", body["max_tokens"]),
        _ => {}
    }
    let text = v["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<String>();
    Ok(Reply { text, note })
}

/// A token from `claude setup-token`, which uses the Claude subscription instead of API credits.
fn is_oauth(key: &str) -> bool {
    key.starts_with("sk-ant-oat")
}

const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
static CLAUDE_CODE_VERSION: OnceLock<String> = OnceLock::new();

/// The latest Claude Code release: Anthropic rejects OAuth requests from versions far behind it.
fn claude_code_version() -> Result<String> {
    if let Some(v) = CLAUDE_CODE_VERSION.get() {
        return Ok(v.clone());
    }
    let url = "https://registry.npmjs.org/@anthropic-ai/claude-code/latest";
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().into();
    let text = agent.get(url).call().with_context(|| url.to_string())?.body_mut().read_to_string()?;
    let v: Value = serde_json::from_str(&text).with_context(|| format!("{url}: response is not JSON"))?;
    let v = v["version"].as_str().with_context(|| format!("{url}: no version"))?;
    Ok(CLAUDE_CODE_VERSION.get_or_init(|| v.into()).clone())
}

/// The headers for an Anthropic request, adapting `body` (which already has its `system`) to the key.
fn authorize(p: &Profile, key: &str, body: &mut Value) -> Result<Vec<(&'static str, String)>> {
    let mut headers = vec![("anthropic-version", "2023-06-01".to_string())];
    let mut betas = Vec::new();
    if is_oauth(key) {
        headers.push(("authorization", format!("Bearer {key}")));
        headers.push(("user-agent", format!("claude-code/{} (external, cli)", claude_code_version()?)));
        headers.push(("x-app", "cli".into()));
        betas.extend(["claude-code-20250219", "oauth-2025-04-20"]);
        // The API accepts an OAuth token only when the system prompt starts with Claude Code's identity.
        let system = body["system"].take();
        body["system"] = json!([{"type": "text", "text": CLAUDE_CODE_IDENTITY}, {"type": "text", "text": system}]);
    } else {
        headers.push(("x-api-key", key.to_string()));
    }
    let fallback_models = ["claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];
    // Unverified with subscription tokens, so they only get fallbacks when the profile asks.
    let default = p.base_url.is_none() && !is_oauth(key) && fallback_models.contains(&p.model.as_str());
    if p.fallbacks.unwrap_or(default) {
        body["fallbacks"] = json!("default");
        betas.push("server-side-fallback-2026-07-01");
    }
    if !betas.is_empty() {
        headers.push(("anthropic-beta", betas.join(",")));
    }
    Ok(headers)
}

fn refusal_category(v: &Value) -> &str {
    v["stop_details"]["category"].as_str().unwrap_or("no category")
}

fn openai(p: &Profile, prompt: &Prompt, key: Option<&str>) -> Result<Reply> {
    let base = p.url();
    let mut note = None;
    let mut user = prompt.text.clone();
    if let Some(paper) = &prompt.paper {
        let mut paper = (paper.text)()?;
        if let Some(max) = p.max_input_chars.filter(|&m| paper.chars().count() > m) {
            note = Some(format!("paper text truncated from {} to {max} characters", paper.chars().count()));
            paper = paper.chars().take(max).collect();
        }
        user = format!("{user}\n\nPaper text:\n{paper}");
    } else {
        note = Some("no PDF; summarized from metadata and notes only".into());
    }
    let mut body = json!({
        "model": p.model,
        "messages": [{"role": "system", "content": prompt.system}, {"role": "user", "content": user}],
    });
    if p.json_schema.unwrap_or(true) {
        body["response_format"] =
            json!({"type": "json_schema", "json_schema": {"name": "table_row", "strict": true, "schema": prompt.schema}});
    }
    if let Some(m) = p.max_tokens {
        body["max_tokens"] = json!(m);
    }
    let headers: Vec<_> = key.map(|k| ("authorization", format!("Bearer {k}"))).into_iter().collect();
    let v = post(&format!("{base}/chat/completions"), &headers, &body)?;
    if v["choices"][0]["finish_reason"] == "length" {
        bail!("output was cut off at the token limit; raise max_tokens in the llm profile");
    }
    let text = v["choices"][0]["message"]["content"].as_str().context("no message content in response")?;
    Ok(Reply { text: text.to_string(), note })
}

/// A minimal request, to test the profile and key.
pub fn ping(p: &Profile, key: Option<&str>) -> Result<()> {
    let mut p = p.clone();
    p.effort = Some("low".into());
    let schema = json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"], "additionalProperties": false});
    let prompt = Prompt { system: "Set ok to true.".into(), text: "Connection test.".into(), paper: None, schema };
    complete(&p, &prompt, key).map(|_| ())
}

/// A plain answer to `text`, searching the web where the provider can: Anthropic's web search
/// tool, or an OpenAI-compatible server that searches by itself.
pub fn search(p: &Profile, system: &str, text: &str, key: Option<&str>) -> Result<String> {
    let key = clean(key);
    let key = key.as_deref();
    if key.is_none() && p.needs_key() {
        bail!("no API key or OAuth token for {}", p.url());
    }
    match p.provider {
        Provider::Anthropic => anthropic_search(p, system, text, key.unwrap_or_default()),
        Provider::Openai => {
            let base = p.url();
            let body = json!({"model": p.model, "messages": [{"role": "system", "content": system}, {"role": "user", "content": text}]});
            let headers: Vec<_> = key.map(|k| ("authorization", format!("Bearer {k}"))).into_iter().collect();
            let v = post(&format!("{base}/chat/completions"), &headers, &body)?;
            Ok(v["choices"][0]["message"]["content"].as_str().context("no message content in response")?.to_string())
        }
    }
}

fn anthropic_search(p: &Profile, system: &str, text: &str, key: &str) -> Result<String> {
    let base = p.url();
    let mut messages = vec![json!({"role": "user", "content": text})];
    // A long search ends the turn with `pause_turn`; sending the partial answer back continues it.
    for _ in 0..4 {
        let mut body = json!({
            "model": p.model,
            "max_tokens": p.max_tokens.unwrap_or(16000),
            "system": system,
            "output_config": {"effort": p.effort.as_deref().unwrap_or("medium")},
            "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 8}],
            "messages": messages,
        });
        let headers = authorize(p, key, &mut body)?;
        let v = post(&format!("{base}/v1/messages"), &headers, &body)?;
        match v["stop_reason"].as_str() {
            Some("pause_turn") => messages.push(json!({"role": "assistant", "content": v["content"]})),
            Some("refusal") => bail!("the model declined ({})", refusal_category(&v)),
            _ => {
                let blocks = v["content"].as_array().into_iter().flatten();
                return Ok(blocks.filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect());
            }
        }
    }
    bail!("the search did not finish")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(fallbacks: Option<bool>) -> Profile {
        Profile {
            provider: Provider::Anthropic,
            model: "claude-opus-5-5".into(),
            base_url: None,
            max_tokens: None,
            effort: None,
            fallbacks,
            max_input_chars: None,
            json_schema: None,
        }
    }

    fn header<'a>(h: &'a [(&str, String)], name: &str) -> Option<&'a str> {
        h.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }

    #[test]
    fn oauth_token_is_sent_as_claude_code_without_default_fallbacks() {
        let _ = CLAUDE_CODE_VERSION.set("9.9.9".into());
        let mut body = json!({"system": "s"});
        let h = authorize(&profile(None), "sk-ant-oat01-x", &mut body).unwrap();
        assert_eq!(header(&h, "authorization"), Some("Bearer sk-ant-oat01-x"));
        assert_eq!(header(&h, "x-api-key"), None);
        assert_eq!(header(&h, "anthropic-beta"), Some("claude-code-20250219,oauth-2025-04-20"));
        assert_eq!(header(&h, "x-app"), Some("cli"));
        assert_eq!(header(&h, "user-agent"), Some("claude-code/9.9.9 (external, cli)"));
        assert_eq!(body["system"], json!([{"type": "text", "text": CLAUDE_CODE_IDENTITY}, {"type": "text", "text": "s"}]));
        assert!(body.get("fallbacks").is_none());

        let mut body = json!({"system": "s"});
        let h = authorize(&profile(Some(true)), "sk-ant-oat01-x", &mut body).unwrap();
        assert_eq!(header(&h, "anthropic-beta"), Some("claude-code-20250219,oauth-2025-04-20,server-side-fallback-2026-07-01"));
        assert_eq!(body["fallbacks"], "default");
    }

    #[test]
    fn api_key_keeps_plain_system_and_default_fallbacks() {
        let mut body = json!({"system": "s"});
        assert_eq!(clean(Some(" sk-ant-oat01-ab\ncd \n")).as_deref(), Some("sk-ant-oat01-abcd"));
        let h = authorize(&profile(None), "sk-ant-api03-x", &mut body).unwrap();
        assert_eq!(header(&h, "x-api-key"), Some("sk-ant-api03-x"));
        assert_eq!(header(&h, "authorization"), None);
        assert_eq!(header(&h, "anthropic-beta"), Some("server-side-fallback-2026-07-01"));
        assert_eq!(body["system"], "s");
    }
}
