use crate::annotations;
use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    /// OpenAI-compatible chat completions: OpenAI, Ollama, llama.cpp, LM Studio, vLLM, OpenRouter, …
    Openai,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub provider: Provider,
    pub model: String,
    pub base_url: Option<String>,
    /// Name of the environment variable holding the API key; keys never live in config files.
    pub api_key_env: Option<String>,
    pub max_tokens: Option<u32>,
    /// Anthropic `output_config.effort`.
    pub effort: Option<String>,
    /// Anthropic server-side refusal fallbacks; defaults on for models that support them.
    pub fallbacks: Option<bool>,
    /// Truncate the paper text sent to OpenAI-compatible servers (small local context windows).
    pub max_input_chars: Option<usize>,
    /// Ask the server to enforce the JSON schema; turn off for servers that reject `response_format`.
    pub json_schema: Option<bool>,
}

pub struct Prompt<'a> {
    pub system: String,
    pub text: String,
    pub pdf: Option<&'a Path>,
    pub schema: Value,
}

pub struct Reply {
    pub text: String,
    /// Something the user should know about how the request was made (e.g. truncation).
    pub note: Option<String>,
}

const ANTHROPIC_PDF_LIMIT: u64 = 32 * 1024 * 1024;

pub fn complete(p: &Profile, prompt: &Prompt) -> Result<Reply> {
    match p.provider {
        Provider::Anthropic => anthropic(p, prompt),
        Provider::Openai => openai(p, prompt),
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

fn api_key(p: &Profile, default_env: &str, required: bool) -> Result<Option<String>> {
    let env = p.api_key_env.as_deref().unwrap_or(default_env);
    match std::env::var(env) {
        Ok(k) if !k.is_empty() => Ok(Some(k)),
        _ if required => bail!("set {env} to your API key (or api_key_env in the llm profile)"),
        _ => Ok(None),
    }
}

fn anthropic(p: &Profile, prompt: &Prompt) -> Result<Reply> {
    let base = p.base_url.as_deref().unwrap_or("https://api.anthropic.com").trim_end_matches('/');
    let key = api_key(p, "ANTHROPIC_API_KEY", true)?.unwrap();
    let mut content = Vec::new();
    let mut note = None;
    match prompt.pdf {
        Some(pdf) if fs_len(pdf) <= ANTHROPIC_PDF_LIMIT => {
            let data = base64::engine::general_purpose::STANDARD.encode(std::fs::read(pdf)?);
            content.push(json!({"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": data}}));
        }
        Some(pdf) => {
            note = Some("PDF over 32 MB; sent extracted text instead".to_string());
            content.push(json!({"type": "text", "text": format!("Paper text:\n{}", annotations::pdf_text(pdf)?)}));
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
    let mut headers = vec![("x-api-key", key), ("anthropic-version", "2023-06-01".to_string())];
    let fallback_models = ["claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];
    if p.fallbacks.unwrap_or(p.base_url.is_none() && fallback_models.contains(&p.model.as_str())) {
        body["fallbacks"] = json!("default");
        headers.push(("anthropic-beta", "server-side-fallback-2026-07-01".into()));
    }
    let v = post(&format!("{base}/v1/messages"), &headers, &body)?;
    match v["stop_reason"].as_str() {
        Some("refusal") => bail!("the model declined ({})", v["stop_details"]["category"].as_str().unwrap_or("no category")),
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

fn openai(p: &Profile, prompt: &Prompt) -> Result<Reply> {
    let base = p.base_url.as_deref().unwrap_or("https://api.openai.com/v1").trim_end_matches('/');
    let key = api_key(p, "OPENAI_API_KEY", p.base_url.is_none())?;
    let mut note = None;
    let mut user = prompt.text.clone();
    if let Some(pdf) = prompt.pdf {
        let mut paper = annotations::pdf_text(pdf)?;
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

fn fs_len(p: &Path) -> u64 {
    std::fs::metadata(p).map_or(u64::MAX, |m| m.len())
}
