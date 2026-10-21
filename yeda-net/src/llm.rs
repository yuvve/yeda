use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::{Value, json};
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

/// `key` is the API key stored for the profile's `url()`, if any.
pub fn complete(p: &Profile, prompt: &Prompt, key: Option<&str>) -> Result<Reply> {
    let key = key.filter(|k| !k.is_empty());
    if key.is_none() && p.needs_key() {
        bail!("no API key for {}", p.url());
    }
    match p.provider {
        Provider::Anthropic => anthropic(p, prompt, key.unwrap_or_default()),
        Provider::Openai => openai(p, prompt, key),
    }
}

fn post(url: &str, headers: &[(&str, String)], body: &Value) -> Result<Value> {
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
    let mut content = Vec::new();
    let mut note = None;
    match &prompt.paper {
        // The limit is on the whole request, where the PDF is base64.
        Some(paper) if paper.pdf.len().div_ceil(3) * 4 + prompt.text.len() + prompt.system.len() <= ANTHROPIC_REQUEST_LIMIT => {
            let data = base64::engine::general_purpose::STANDARD.encode(paper.pdf);
            content.push(json!({"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": data}}));
        }
        Some(paper) => {
            note = Some("PDF too large for the request limit (32 MB); sent extracted text instead".to_string());
            content.push(json!({"type": "text", "text": format!("Paper text:\n{}", (paper.text)()?)}));
        }
        None => note = Some("no PDF; summarized from metadata and notes only".into()),
    }
    content.push(json!({"type": "text", "text": prompt.text}));
    let mut body = json!({
        "model": p.model,
        "max_tokens": p.max_tokens.unwrap_or(32000),
        "system": prompt.system,
        "output_config": {
            "effort": p.effort.as_deref().unwrap_or("high"),
            "format": {"type": "json_schema", "schema": prompt.schema},
        },
        "messages": [{"role": "user", "content": content}],
    });
    let mut headers = vec![("x-api-key", key.to_string()), ("anthropic-version", "2023-06-01".to_string())];
    add_fallbacks(p, &mut body, &mut headers);
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

fn add_fallbacks(p: &Profile, body: &mut Value, headers: &mut Vec<(&str, String)>) {
    let fallback_models = ["claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];
    if p.fallbacks.unwrap_or(p.base_url.is_none() && fallback_models.contains(&p.model.as_str())) {
        body["fallbacks"] = json!("default");
        headers.push(("anthropic-beta", "server-side-fallback-2026-07-01".into()));
    }
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
    let key = key.filter(|k| !k.is_empty());
    if key.is_none() && p.needs_key() {
        bail!("no API key for {}", p.url());
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
    let headers = vec![("x-api-key", key.to_string()), ("anthropic-version", "2023-06-01".to_string())];
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
        let mut headers = headers.clone();
        add_fallbacks(p, &mut body, &mut headers);
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
