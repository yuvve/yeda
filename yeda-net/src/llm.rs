use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::Duration;
use yeda_core::llm::{Auth, Bound, CodexCredential, Protocol};

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

/// `key` is the API key or OAuth credentials stored for the profile's provider, if any.
pub fn complete(b: &Bound, prompt: &Prompt, key: Option<&str>) -> Result<Reply> {
    let key = clean(key);
    let key = key.as_deref();
    if key.is_none() && b.needs_key() {
        bail!("no API key or OAuth credentials for provider '{}' ({})", b.provider, b.url());
    }
    match b.protocol() {
        Protocol::Anthropic => anthropic(b, prompt, key.unwrap_or_default()),
        Protocol::OpenaiResponses => codex(b, prompt, key.unwrap_or_default()),
        Protocol::Openai | Protocol::Mistral | Protocol::Openrouter | Protocol::OpencodeGo => openai(b, prompt, key),
    }
}

static OPENCODE_SESSION: OnceLock<String> = OnceLock::new();

/// The headers for an OpenAI-compatible request. OpenRouter also gets its app attribution headers,
/// so the traffic shows as yeda rather than "Unknown app"; other servers never see them.
fn openai_headers(b: &Bound, key: Option<&str>) -> Vec<(&'static str, String)> {
    let mut headers: Vec<_> = key.map(|k| ("authorization", format!("Bearer {k}"))).into_iter().collect();
    if b.protocol() == Protocol::Openrouter {
        headers.push(("http-referer", "https://github.com/yuvve/yeda".into()));
        headers.push(("x-openrouter-title", "yeda".into()));
    }
    if b.protocol() == Protocol::OpencodeGo {
        // OpenCode Go rejects requests without a session id (HTTP 400 MissingSessionID) and asks
        // clients to name themselves; one session per process groups a run's requests.
        let session = OPENCODE_SESSION.get_or_init(|| format!("ses_{}", uuid::Uuid::new_v4().simple()));
        headers.push(("x-opencode-session", session.clone()));
        headers.push(("user-agent", format!("yeda/{}", env!("CARGO_PKG_VERSION"))));
    }
    headers
}

/// The credential in the secret store is the whole signed-in record; requests carry only its
/// access token. A pasted bare token parses too.
fn codex_headers(b: &Bound, key: &str) -> Result<Vec<(&'static str, String)>> {
    let cred = CodexCredential::parse(key)?;
    Ok(openai_headers(b, Some(&cred.access_token)))
}

fn post(url: &str, headers: &[(&str, String)], body: &Value) -> Result<Value> {
    // The key goes in a header, so only to https servers.
    yeda_core::require_https(url)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15 * 60)))
        .http_status_as_error(false)
        .https_only(true)
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

/// A model a provider lists: `id` is what requests use and what yeda.toml keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub id: String,
    pub label: String,
}

fn get(url: &str, headers: &[(&str, String)]) -> Result<Value> {
    yeda_core::require_https(url)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .https_only(true)
        .build()
        .into();
    let mut req = agent.get(url);
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let mut resp = req.call().with_context(|| url.to_string())?;
    let status = resp.status();
    let text = resp.body_mut().read_to_string()?;
    if !status.is_success() {
        bail!("{url}: HTTP {status}: {}", text.chars().take(300).collect::<String>());
    }
    serde_json::from_str(&text).with_context(|| format!("{url}: response is not JSON"))
}

/// Maps the OpenAI-style `data[]`, Codex's `models[]` and Anthropic's display names to one list.
fn listed_models(v: &Value) -> Vec<Model> {
    let rows = v.get("data").and_then(Value::as_array).or_else(|| v.get("models").and_then(Value::as_array));
    let mut models: Vec<Model> = rows
        .into_iter()
        .flatten()
        .filter_map(|m| {
            if m.get("visibility").and_then(Value::as_str).is_some_and(|v| v != "list") {
                return None;
            }
            let id = m.get("id").or_else(|| m.get("slug")).and_then(Value::as_str)?;
            let label = m.get("display_name").or_else(|| m.get("name")).and_then(Value::as_str).unwrap_or(id);
            Some(Model { id: id.into(), label: label.into() })
        })
        .collect();
    models.dedup_by(|a, b| a.id == b.id);
    models
}

/// The models the bound provider lists, for `:setup llm`'s picker. A server without the endpoint
/// returns an error, which the form shows without blocking a typed model name.
pub fn models(b: &Bound, key: Option<&str>) -> Result<Vec<Model>> {
    let key = clean(key);
    if key.is_none() && b.needs_key() {
        bail!("no API key or OAuth credentials for provider '{}' ({})", b.provider, b.url());
    }
    let key = key.as_deref().unwrap_or_default();
    let headers: Vec<(&'static str, String)> = match b.protocol() {
        Protocol::OpenaiResponses => codex_headers(b, key)?,
        Protocol::Anthropic if b.auth() == Auth::Oauth => vec![
            ("anthropic-version", "2023-06-01".into()),
            ("authorization", format!("Bearer {key}")),
            ("anthropic-beta", "oauth-2025-04-20".into()),
        ],
        Protocol::Anthropic => vec![("anthropic-version", "2023-06-01".into()), ("x-api-key", key.into())],
        _ => openai_headers(b, (!key.is_empty()).then_some(key)),
    };
    Ok(listed_models(&get(&models_url(b), &headers)?))
}

/// Anthropic's base URL omits `/v1`; every other protocol's base includes it.
fn models_url(b: &Bound) -> String {
    match b.protocol() {
        Protocol::Anthropic => format!("{}/v1/models", b.url()),
        _ => format!("{}/models", b.url()),
    }
}

fn anthropic(b: &Bound, prompt: &Prompt, key: &str) -> Result<Reply> {
    let base = b.url();
    let mut body = json!({
        "model": b.model,
        "max_tokens": 32000,
        "system": prompt.system,
        "output_config": {
            "effort": "high",
            "format": {"type": "json_schema", "schema": prompt.schema},
        },
    });
    let headers = authorize(b, key, &mut body)?;
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
        Some("max_tokens") => bail!("output hit the 32000-token limit; ask for less in the prompt"),
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
fn authorize(b: &Bound, key: &str, body: &mut Value) -> Result<Vec<(&'static str, String)>> {
    let mut headers = vec![("anthropic-version", "2023-06-01".to_string())];
    let mut betas = Vec::new();
    if b.auth() == Auth::Oauth {
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
    // Unverified with subscription tokens, so they are left to API keys.
    if b.auth() != Auth::Oauth {
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

fn openai(b: &Bound, prompt: &Prompt, key: Option<&str>) -> Result<Reply> {
    let (body, note) = openai_body(b, prompt)?;
    let v = post(&format!("{}/chat/completions", b.url()), &openai_headers(b, key), &body)?;
    if matches!(v["choices"][0]["finish_reason"].as_str(), Some("length" | "model_length")) {
        bail!("output was cut off at the token limit; ask for less in the prompt");
    }
    let text = v["choices"][0]["message"]["content"].as_str().context("no message content in response")?;
    Ok(Reply { text: text.to_string(), note })
}

/// The chat completions request for `prompt`, with a note on how the paper went along.
fn openai_body(b: &Bound, prompt: &Prompt) -> Result<(Value, Option<String>)> {
    let mut note = None;
    let user = match &prompt.paper {
        Some(paper) if b.protocol() == Protocol::Mistral => {
            let data = base64::engine::general_purpose::STANDARD.encode(paper.pdf);
            let doc = json!({"type": "document_url", "document_url": format!("data:application/pdf;base64,{data}")});
            json!([doc, {"type": "text", "text": prompt.text}])
        }
        Some(paper) if b.protocol() == Protocol::Openrouter => {
            let data = base64::engine::general_purpose::STANDARD.encode(paper.pdf);
            let file = json!({"type": "file", "file": {"filename": "paper.pdf", "file_data": format!("data:application/pdf;base64,{data}")}});
            json!([file, {"type": "text", "text": prompt.text}])
        }
        Some(paper) => json!(format!("{}\n\nPaper text:\n{}", prompt.text, (paper.text)()?)),
        None => {
            note = Some("no PDF; summarized from metadata and notes only".into());
            json!(prompt.text)
        }
    };
    let mut body = json!({
        "model": b.model,
        "messages": [{"role": "system", "content": prompt.system}, {"role": "user", "content": user}],
        "response_format": {"type": "json_schema", "json_schema": {"name": "table_row", "strict": true, "schema": prompt.schema}},
    });
    // OpenCode Go's models reason before answering, which takes output tokens too.
    if b.protocol() == Protocol::OpencodeGo {
        body["max_tokens"] = json!(32000);
    }
    Ok((body, note))
}

/// The request for `prompt` against the Responses API, which the Codex token is accepted by.
fn codex_body(b: &Bound, prompt: &Prompt, schema: Option<&Value>) -> Result<(Value, Option<String>)> {
    let mut note = None;
    let user = match &prompt.paper {
        Some(paper) => json!([{"type": "input_text", "text": format!("{}\n\nPaper text:\n{}", prompt.text, (paper.text)()?)}]),
        None => {
            note = Some("no PDF; summarized from metadata and notes only".into());
            json!([{"type": "input_text", "text": prompt.text}])
        }
    };
    let mut body = json!({
        "model": b.model,
        "instructions": prompt.system,
        "input": [{"role": "user", "content": user}],
        "store": false,
        "stream": true,
    });
    if let Some(schema) = schema {
        body["text"] = json!({"format": {"type": "json_schema", "name": "table_row", "strict": true, "schema": schema}});
    }
    Ok((body, note))
}

fn codex(b: &Bound, prompt: &Prompt, key: &str) -> Result<Reply> {
    let (body, note) = codex_body(b, prompt, Some(&prompt.schema))?;
    let response = stream(&format!("{}/responses", b.url()), &codex_headers(b, key)?, &body)?;
    let text = response_text(&response)?;
    Ok(Reply { text, note })
}

/// Reads Responses events out of the SSE `lines`, returning the `response` object of
/// `response.completed`; the deltas are joined when the completed event carries no text.
fn sse_response(lines: impl Iterator<Item = std::io::Result<String>>) -> Result<Value> {
    let mut text = String::new();
    let mut completed = None;
    for line in lines {
        let line = line?;
        let Some(data) = line.strip_prefix("data: ") else { continue };
        let Ok(mut event) = serde_json::from_str::<Value>(data) else { continue };
        match event["type"].as_str() {
            Some("response.output_text.delta") => text.push_str(event["delta"].as_str().unwrap_or_default()),
            Some("response.completed") => {
                let mut response = event["response"].take();
                if response_text(&response).is_err() {
                    response["output"] = json!([{"type": "message", "content": [{"type": "output_text", "text": text}]}]);
                }
                completed = Some(response);
                break;
            }
            Some("response.failed" | "response.incomplete") | None if event["error"].is_object() => {
                bail!("{}: {}", event["type"].as_str().unwrap_or("error"), event["error"]["message"].as_str().unwrap_or("no message"))
            }
            _ => {}
        }
    }
    completed.context("the Responses stream ended without a completed event")
}

/// Reads an SSE stream of Responses events, returning the `response` object of `response.completed`.
fn stream(url: &str, headers: &[(&str, String)], body: &Value) -> Result<Value> {
    use std::io::{BufRead, BufReader};
    yeda_core::require_https(url)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15 * 60)))
        .http_status_as_error(false)
        .https_only(true)
        .build()
        .into();
    let mut req = agent.post(url).header("content-type", "application/json").header("accept", "text/event-stream");
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let mut resp = req.send(serde_json::to_string(body)?).with_context(|| url.to_string())?;
    if resp.status() != 200 {
        let text = resp.body_mut().read_to_string()?;
        bail!("{url}: HTTP {}: {}", resp.status(), text.chars().take(500).collect::<String>());
    }
    sse_response(BufReader::new(resp.body_mut().as_reader()).lines())
}

/// The answer text from a `response.completed` object: the deltas when any, else the output.
fn response_text(response: &Value) -> Result<String> {
    let text = response["output"].as_array().into_iter().flatten()
        .filter(|item| item["type"] == "message")
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str())
        .collect::<String>();
    if text.is_empty() {
        bail!("the response carries no text")
    }
    Ok(text)
}

/// Turns stored Codex credentials into a fresh access token, keeping the account identity.
/// Returns the new credential to store, or an error asking for a fresh sign-in.
pub fn codex_refresh(key: &str) -> Result<String> {
    let cred = CodexCredential::parse(key)?;
    let refresh = cred.refresh_token.as_ref().context(cred.expired_message())?;
    let Some(client_id) = cred.client_id.as_ref() else {
        bail!("the stored Codex credentials have no client_id; sign in again");
    };
    // Credentials yeda signed in are refreshed at OpenAI's open-source-app endpoint, form-encoded
    // and with the resource; Codex CLI's own (`app_…`) refresh at its endpoint, JSON-encoded.
    if client_id.starts_with("app_") {
        let body = json!({"grant_type": "refresh_token", "refresh_token": refresh, "client_id": client_id});
        let v = post("https://auth.openai.com/oauth/token", &[], &body)?;
        let access = v["access_token"].as_str().context("the token refresh returned no access_token")?;
        return Ok(cred.refreshed(access, Some(v["refresh_token"].as_str().unwrap_or(refresh))));
    }
    let form = [("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", client_id), ("resource", "https://api.openai.com/v1")]
        .map(|(k, v)| format!("{k}={}", crate::codex::encode(v)))
        .join("&");
    let v = post_form("https://auth.openai.com/api/accounts/oauth/token", &form)?;
    let access = v["access_token"].as_str().context("the token refresh returned no access_token")?;
    Ok(cred.refreshed(access, Some(v["refresh_token"].as_str().unwrap_or(refresh))))
}

/// A form-encoded POST, for the token endpoint that only accepts one.
fn post_form(url: &str, form: &str) -> Result<Value> {
    yeda_core::require_https(url)?;
    // The endpoint's error body carries the machine-readable reason the refresh failed.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .https_only(true)
        .build()
        .into();
    let mut resp = agent.post(url).header("content-type", "application/x-www-form-urlencoded").send(form).with_context(|| url.to_string())?;
    let status = resp.status();
    let text = resp.body_mut().read_to_string()?;
    crate::codex::token_response(url, status.as_u16(), &text)
}

/// A minimal request, to test the profile and its provider's credentials.
pub fn ping(b: &Bound, key: Option<&str>) -> Result<()> {
    let schema = json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"], "additionalProperties": false});
    let prompt = Prompt { system: "Set ok to true.".into(), text: "Connection test.".into(), paper: None, schema };
    complete(b, &prompt, key).map(|_| ())
}

/// A plain answer to `text`, searching the web where the provider can: Anthropic's, Mistral's and
/// OpenRouter's web search tools, or an OpenAI-compatible server that searches by itself.
/// Says so when `b` answers a search from what the model knows instead of from the web.
/// Generic OpenAI-compatible servers aren't asked to search, and OpenCode Go ignores search tools.
pub fn search_note(b: &Bound) -> Option<&'static str> {
    match b.protocol() {
        Protocol::Openai | Protocol::OpencodeGo | Protocol::OpenaiResponses => Some("answered without web search, from what the model knows"),
        Protocol::Anthropic | Protocol::Mistral | Protocol::Openrouter => None,
    }
}

/// Answers `text` in plain text, without tools or a JSON schema: queries and ranking for `:discover`.
pub fn text(b: &Bound, system: &str, text: &str, key: Option<&str>) -> Result<String> {
    let key = clean(key);
    let key = key.as_deref();
    if key.is_none() && b.needs_key() {
        bail!("no API key or OAuth credentials for provider '{}' ({})", b.provider, b.url());
    }
    match b.protocol() {
        Protocol::Anthropic => {
            let mut body = json!({
                "model": b.model,
                "max_tokens": 8000,
                "system": system,
                "output_config": {"effort": "low"},
                "messages": [{"role": "user", "content": text}],
            });
            let headers = authorize(b, key.unwrap_or_default(), &mut body)?;
            let v = post(&format!("{}/v1/messages", b.url()), &headers, &body)?;
            match v["stop_reason"].as_str() {
                Some("refusal") => bail!("the model declined ({})", refusal_category(&v)),
                Some("max_tokens") => bail!("output hit the 8000-token limit; ask for less in the prompt"),
                _ => {}
            }
            Ok(v["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect())
        }
        Protocol::OpenaiResponses => {
            let prompt = Prompt { system: system.into(), text: text.into(), paper: None, schema: Value::Null };
            let (body, _) = codex_body(b, &prompt, None)?;
            let response = stream(&format!("{}/responses", b.url()), &codex_headers(b, key.unwrap_or_default())?, &body)?;
            response_text(&response)
        }
        _ => {
            let body = json!({"model": b.model, "messages": [{"role": "system", "content": system}, {"role": "user", "content": text}]});
            let v = post(&format!("{}/chat/completions", b.url()), &openai_headers(b, key), &body)?;
            Ok(v["choices"][0]["message"]["content"].as_str().context("no message content in response")?.to_string())
        }
    }
}

/// Answers `text`, searching the web where the provider can; the note says when it didn't.
pub fn search(b: &Bound, system: &str, text: &str, key: Option<&str>) -> Result<Reply> {
    let key = clean(key);
    let key = key.as_deref();
    if key.is_none() && b.needs_key() {
        bail!("no API key or OAuth credentials for provider '{}' ({})", b.provider, b.url());
    }
    let text = match b.protocol() {
        Protocol::Anthropic => anthropic_search(b, system, text, key.unwrap_or_default())?,
        Protocol::Mistral => mistral_search(b, system, text, key.unwrap_or_default())?,
        Protocol::OpenaiResponses => {
            let prompt = Prompt { system: system.into(), text: text.into(), paper: None, schema: Value::Null };
            let (body, _) = codex_body(b, &prompt, None)?;
            let response = stream(&format!("{}/responses", b.url()), &codex_headers(b, key.unwrap_or_default())?, &body)?;
            response_text(&response)?
        }
        Protocol::Openai | Protocol::Openrouter | Protocol::OpencodeGo => {
            let base = b.url();
            let mut body = json!({"model": b.model, "messages": [{"role": "system", "content": system}, {"role": "user", "content": text}]});
            if b.protocol() == Protocol::Openrouter {
                body["tools"] = json!([{"type": "openrouter:web_search"}]);
            }
            let v = post(&format!("{base}/chat/completions"), &openai_headers(b, key), &body)?;
            v["choices"][0]["message"]["content"].as_str().context("no message content in response")?.to_string()
        }
    };
    Ok(Reply { text, note: search_note(b).map(String::from) })
}

fn anthropic_search(b: &Bound, system: &str, text: &str, key: &str) -> Result<String> {
    let base = b.url();
    let mut messages = vec![json!({"role": "user", "content": text})];
    // A long search ends the turn with `pause_turn`; sending the partial answer back continues it.
    for _ in 0..4 {
        let mut body = json!({
            "model": b.model,
            "max_tokens": 16000,
            "system": system,
            "output_config": {"effort": "medium"},
            "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 8}],
            "messages": messages,
        });
        let headers = authorize(b, key, &mut body)?;
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

fn mistral_search(b: &Bound, system: &str, text: &str, key: &str) -> Result<String> {
    let body = json!({
        "model": b.model,
        "instructions": system,
        "inputs": text,
        "tools": [{"type": "web_search"}],
        "stream": false,
        "store": false,
    });
    let v = post(&format!("{}/conversations", b.url()), &[("authorization", format!("Bearer {key}"))], &body)?;
    Ok(conversation_text(&v))
}

/// The answer in a Mistral conversation response: its messages' text, without the source references.
fn conversation_text(v: &Value) -> String {
    let messages = v["outputs"].as_array().into_iter().flatten().filter(|o| o["type"] == "message.output");
    messages
        .flat_map(|m| match &m["content"] {
            Value::String(s) => vec![s.as_str()],
            c => c.as_array().into_iter().flatten().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yeda_core::llm::Provider;

    fn provider(protocol: Protocol, auth: Auth) -> Provider {
        let base_url = match protocol {
            Protocol::Anthropic => "https://api.anthropic.com",
            Protocol::Openai | Protocol::OpenaiResponses => "https://api.openai.com/v1",
            Protocol::Mistral => "https://api.mistral.ai/v1",
            Protocol::Openrouter => "https://openrouter.ai/api/v1",
            Protocol::OpencodeGo => "https://opencode.ai/inference/go/openai/v1",
        };
        Provider { base_url: base_url.into(), protocol, auth }
    }

    fn bound(protocol: Protocol, auth: Auth) -> Bound {
        Bound { profile: "test".into(), provider: "test".into(), model: "test-model".into(), settings: provider(protocol, auth) }
    }

    fn header<'a>(h: &'a [(&'a str, String)], name: &str) -> Option<&'a str> {
        h.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }

    #[test]
    fn codex_sends_responses_with_stream_and_schema() {
        let schema = json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"], "additionalProperties": false});
        let prompt = Prompt { system: "s".into(), text: "t".into(), paper: None, schema: schema.clone() };
        let b = Bound { model: "gpt-5.1".into(), ..bound(Protocol::OpenaiResponses, Auth::Oauth) };
        assert!(b.needs_key());
        assert_eq!(b.url(), "https://api.openai.com/v1");
        let (body, _) = codex_body(&b, &prompt, Some(&prompt.schema)).unwrap();
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["instructions"], "s");
        assert_eq!(body["model"], "gpt-5.1");
        assert_eq!(body["input"], json!([{"role": "user", "content": [{"type": "input_text", "text": "t"}]}]));
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert_eq!(body["text"]["format"]["schema"], schema);
        assert!(body.get("response_format").is_none());
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn responses_carries_the_access_token_not_the_whole_credential() {
        let b = bound(Protocol::OpenaiResponses, Auth::Oauth);
        let signed = json!({
            "client_id": "oaiapp_x",
            "id_token": "id.jwt",
            "scope": "chatgpt.tokens.use.direct offline_access",
            "tokens": {
                "access_token": "eyJ-access",
                "refresh_token": "r",
                "oauth_provenance": {"client_id": "oaiapp_x"},
                "account_id": "acc",
            },
        });
        let headers = codex_headers(&b, &signed.to_string()).unwrap();
        let token = header(&headers, "authorization").unwrap();
        assert_eq!(token, "Bearer eyJ-access");
        assert!(!token.contains('{'), "{token}");
        assert_eq!(header(&codex_headers(&b, "pasted-token").unwrap(), "authorization"), Some("Bearer pasted-token"));
        assert!(codex_headers(&b, "{\"tokens\":{}}").is_err());
    }

    #[test]
    fn codex_sse_joins_deltas_and_reports_failures() {
        let lines = [
            "event: x",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}",
            "",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"lo\"}",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"message\",\"content\":[]}]}}",
            "data: after the end",
        ]
        .into_iter()
        .map(|l| Ok::<_, std::io::Error>(l.to_string()));
        let v = sse_response(lines.into_iter()).unwrap();
        assert_eq!(response_text(&v).unwrap(), "Hello");

        let failed = sse_response(["data: {\"type\":\"response.failed\",\"error\":{\"message\":\"no credit\"}}"].into_iter().map(|l| Ok::<_, std::io::Error>(l.to_string()))).unwrap_err();
        assert!(failed.to_string().contains("no credit"), "{failed}");

        let empty = sse_response(["data: not json"].into_iter().map(|l| Ok::<_, std::io::Error>(l.to_string()))).unwrap_err();
        assert!(empty.to_string().contains("ended without a completed event"), "{empty}");
    }

    #[test]
    fn codex_is_not_a_chat_completions_server() {
        let b = bound(Protocol::OpenaiResponses, Auth::Oauth);
        assert_eq!(search_note(&b).unwrap(), search_note(&bound(Protocol::Openai, Auth::ApiKey)).unwrap());
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let http = Bound { settings: Provider { base_url: format!("http://{}/v1", server.local_addr().unwrap()), ..provider(Protocol::OpenaiResponses, Auth::Oauth) }, ..bound(Protocol::OpenaiResponses, Auth::Oauth) };
        let err = ping(&http, Some("k")).unwrap_err().to_string();
        assert!(err.contains("only go over https://"), "{err}");
    }

    #[test]
    fn model_lists_map_every_response_shape() {
        let openai = json!({"data": [{"id": "gpt-5.1"}, {"id": "m", "name": "M"}]});
        assert_eq!(listed_models(&openai), [
            Model { id: "gpt-5.1".into(), label: "gpt-5.1".into() },
            Model { id: "m".into(), label: "M".into() },
        ]);
        let codex = json!({"models": [
            {"slug": "gpt-5", "display_name": "GPT-5", "visibility": "list"},
            {"slug": "hidden", "display_name": "Hidden", "visibility": "hidden"},
            {"slug": "gpt-5", "display_name": "GPT-5", "visibility": "list"},
        ]});
        assert_eq!(listed_models(&codex), [Model { id: "gpt-5".into(), label: "GPT-5".into() }]);
        let anthropic = json!({"data": [{"id": "claude-x", "display_name": "Claude X"}]});
        assert_eq!(listed_models(&anthropic), [Model { id: "claude-x".into(), label: "Claude X".into() }]);
        assert!(listed_models(&json!({"data": []})).is_empty());
    }

    #[test]
    fn model_list_urls_follow_each_base_url_convention() {
        assert_eq!(models_url(&bound(Protocol::Openai, Auth::ApiKey)), "https://api.openai.com/v1/models");
        assert_eq!(models_url(&bound(Protocol::OpenaiResponses, Auth::Oauth)), "https://api.openai.com/v1/models");
        assert_eq!(models_url(&bound(Protocol::Mistral, Auth::ApiKey)), "https://api.mistral.ai/v1/models");
        assert_eq!(models_url(&bound(Protocol::Openrouter, Auth::ApiKey)), "https://openrouter.ai/api/v1/models");
        assert_eq!(models_url(&bound(Protocol::Anthropic, Auth::ApiKey)), "https://api.anthropic.com/v1/models");
        assert_eq!(models_url(&bound(Protocol::OpencodeGo, Auth::ApiKey)), "https://opencode.ai/inference/go/openai/v1/models");
    }

    #[test]
    fn only_openrouter_gets_attribution_headers() {
        for protocol in [Protocol::Openai, Protocol::Mistral, Protocol::Anthropic] {
            let h = openai_headers(&bound(protocol, Auth::ApiKey), Some("k"));
            assert_eq!(header(&h, "authorization"), Some("Bearer k"));
            assert_eq!(header(&h, "http-referer"), None);
            assert_eq!(header(&h, "x-openrouter-title"), None);
        }
        let h = openai_headers(&bound(Protocol::Openrouter, Auth::ApiKey), Some("k"));
        assert_eq!(header(&h, "authorization"), Some("Bearer k"));
        assert_eq!(header(&h, "http-referer"), Some("https://github.com/yuvve/yeda"));
        assert_eq!(header(&h, "x-openrouter-title"), Some("yeda"));
        assert_eq!(header(&h, "x-openrouter-app-visibility"), None);
    }

    #[test]
    fn refuses_a_local_plain_http_server() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let b = Bound { settings: Provider { base_url: format!("http://{}/v1", server.local_addr().unwrap()), ..provider(Protocol::Openai, Auth::ApiKey) }, ..bound(Protocol::Openai, Auth::ApiKey) };
        let err = ping(&b, Some("key")).unwrap_err().to_string();
        assert!(err.contains("only go over https://"), "{err}");
        assert!(search(&b, "s", "t", None).is_err());
        assert_eq!(server.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn oauth_token_is_sent_as_claude_code_without_fallbacks() {
        let _ = CLAUDE_CODE_VERSION.set("9.9.9".into());
        let mut body = json!({"system": "s"});
        let h = authorize(&bound(Protocol::Anthropic, Auth::Oauth), "sk-ant-oat01-x", &mut body).unwrap();
        assert_eq!(header(&h, "authorization"), Some("Bearer sk-ant-oat01-x"));
        assert_eq!(header(&h, "x-api-key"), None);
        assert_eq!(header(&h, "anthropic-beta"), Some("claude-code-20250219,oauth-2025-04-20"));
        assert_eq!(header(&h, "x-app"), Some("cli"));
        assert_eq!(header(&h, "user-agent"), Some("claude-code/9.9.9 (external, cli)"));
        assert_eq!(body["system"], json!([{"type": "text", "text": CLAUDE_CODE_IDENTITY}, {"type": "text", "text": "s"}]));
        assert!(body.get("fallbacks").is_none());
    }

    fn opencode_go() -> Bound {
        Bound { model: "glm-5.3-flash".into(), ..bound(Protocol::OpencodeGo, Auth::ApiKey) }
    }

    #[test]
    fn opencode_go_sends_one_session_id_and_its_own_user_agent() {
        let h = openai_headers(&opencode_go(), Some("oc-key"));
        assert_eq!(header(&h, "authorization"), Some("Bearer oc-key"));
        let session = header(&h, "x-opencode-session").unwrap();
        assert!(session.starts_with("ses_") && session.len() == 36, "{session}");
        assert_eq!(header(&openai_headers(&opencode_go(), None), "x-opencode-session"), Some(session));
        assert_eq!(header(&h, "user-agent"), Some(concat!("yeda/", env!("CARGO_PKG_VERSION"))));

        let openai = bound(Protocol::Openai, Auth::ApiKey);
        assert_eq!(openai_headers(&openai, Some("k")), [("authorization", "Bearer k".to_string())]);
    }

    #[test]
    fn opencode_go_needs_a_key_and_https() {
        let b = opencode_go();
        assert!(b.needs_key());
        assert_eq!(b.url(), "https://opencode.ai/inference/go/openai/v1");
        assert!(ping(&b, None).unwrap_err().to_string().contains("no API key"));

        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let b = Bound { settings: Provider { base_url: format!("http://{}/v1", server.local_addr().unwrap()), ..provider(Protocol::OpencodeGo, Auth::ApiKey) }, ..opencode_go() };
        let err = ping(&b, Some("oc-key")).unwrap_err().to_string();
        assert!(err.contains("only go over https://"), "{err}");
        assert!(search(&b, "s", "t", Some("oc-key")).is_err());
        assert_eq!(server.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn says_when_a_search_skipped_the_web() {
        for (protocol, auth, searches) in [
            (Protocol::OpencodeGo, Auth::ApiKey, false),
            (Protocol::Openai, Auth::ApiKey, false),
            (Protocol::OpenaiResponses, Auth::Oauth, false),
            (Protocol::Anthropic, Auth::ApiKey, true),
            (Protocol::Mistral, Auth::ApiKey, true),
            (Protocol::Openrouter, Auth::ApiKey, true),
        ] {
            let note = search_note(&bound(protocol, auth));
            assert_eq!(note.is_none(), searches, "{protocol:?}");
        }
        assert!(search_note(&opencode_go()).unwrap().contains("without web search"));
    }

    #[test]
    fn opencode_go_asks_for_the_schema_with_room_to_reason() {
        let schema = json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"], "additionalProperties": false});
        let prompt = Prompt { system: "s".into(), text: "t".into(), paper: None, schema: schema.clone() };
        let (body, _) = openai_body(&opencode_go(), &prompt).unwrap();
        assert_eq!(body["response_format"], json!({"type": "json_schema", "json_schema": {"name": "table_row", "strict": true, "schema": schema}}));
        assert_eq!(body["model"], "glm-5.3-flash");
        assert_eq!(body["max_tokens"], 32000);
        let (body, _) = openai_body(&bound(Protocol::Openai, Auth::ApiKey), &prompt).unwrap();
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn each_profile_keeps_its_own_model_on_a_shared_provider() {
        let settings = provider(Protocol::Anthropic, Auth::ApiKey);
        let a = Bound { profile: "a".into(), provider: "claude".into(), model: "claude-opus-5-5".into(), settings: settings.clone() };
        let b = Bound { profile: "b".into(), provider: "claude".into(), model: "claude-sonnet-5-5".into(), settings };
        assert_eq!((a.url(), b.url()), ("https://api.anthropic.com", "https://api.anthropic.com"));
        assert_eq!((a.model.as_str(), b.model.as_str()), ("claude-opus-5-5", "claude-sonnet-5-5"));
    }

    #[test]
    fn mistral_conversation_text_skips_tool_entries_and_references() {
        let v = json!({"outputs": [
            {"type": "tool.execution", "name": "web_search"},
            {"type": "message.output", "content": [
                {"type": "text", "text": "See "},
                {"type": "tool_reference", "tool": "web_search", "title": "t", "url": "u"},
                {"type": "text", "text": "this."},
            ]},
            {"type": "message.output", "content": " Done."},
        ]});
        assert_eq!(conversation_text(&v), "See this. Done.");
    }

    #[test]
    fn api_key_keeps_plain_system_and_default_fallbacks() {
        let mut body = json!({"system": "s"});
        assert_eq!(clean(Some(" sk-ant-oat01-ab\ncd \n")).as_deref(), Some("sk-ant-oat01-abcd"));
        let h = authorize(&bound(Protocol::Anthropic, Auth::ApiKey), "sk-ant-api03-x", &mut body).unwrap();
        assert_eq!(header(&h, "x-api-key"), Some("sk-ant-api03-x"));
        assert_eq!(header(&h, "authorization"), None);
        assert_eq!(header(&h, "anthropic-beta"), Some("server-side-fallback-2026-07-01"));
        assert_eq!(body["system"], "s");
    }
}
