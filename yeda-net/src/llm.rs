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
        Provider::Openai | Provider::Mistral | Provider::Openrouter | Provider::OpencodeGo => openai(p, prompt, key),
    }
}

static OPENCODE_SESSION: OnceLock<String> = OnceLock::new();

/// The headers for an OpenAI-compatible request. OpenRouter also gets its app attribution headers,
/// so the traffic shows as yeda rather than "Unknown app"; other servers never see them.
fn openai_headers(p: &Profile, key: Option<&str>) -> Vec<(&'static str, String)> {
    let mut headers: Vec<_> = key.map(|k| ("authorization", format!("Bearer {k}"))).into_iter().collect();
    if p.provider == Provider::Openrouter {
        headers.push(("http-referer", "https://github.com/yuvve/yeda".into()));
        headers.push(("x-openrouter-title", "yeda".into()));
    }
    if p.provider == Provider::OpencodeGo {
        // OpenCode Go rejects requests without a session id (HTTP 400 MissingSessionID) and asks
        // clients to name themselves; one session per process groups a run's requests.
        let session = OPENCODE_SESSION.get_or_init(|| format!("ses_{}", uuid::Uuid::new_v4().simple()));
        headers.push(("x-opencode-session", session.clone()));
        headers.push(("user-agent", format!("yeda/{}", env!("CARGO_PKG_VERSION"))));
    }
    headers
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
    let (body, note) = openai_body(p, prompt)?;
    let v = post(&format!("{}/chat/completions", p.url()), &openai_headers(p, key), &body)?;
    if matches!(v["choices"][0]["finish_reason"].as_str(), Some("length" | "model_length")) {
        bail!("output was cut off at the token limit; raise max_tokens in the llm profile");
    }
    let text = v["choices"][0]["message"]["content"].as_str().context("no message content in response")?;
    Ok(Reply { text: text.to_string(), note })
}

/// The chat completions request for `prompt`, with a note on how the paper went along.
fn openai_body(p: &Profile, prompt: &Prompt) -> Result<(Value, Option<String>)> {
    let mut note = None;
    let user = match &prompt.paper {
        Some(paper) if p.provider == Provider::Mistral => {
            let data = base64::engine::general_purpose::STANDARD.encode(paper.pdf);
            let doc = json!({"type": "document_url", "document_url": format!("data:application/pdf;base64,{data}")});
            json!([doc, {"type": "text", "text": prompt.text}])
        }
        Some(paper) if p.provider == Provider::Openrouter => {
            let data = base64::engine::general_purpose::STANDARD.encode(paper.pdf);
            let file = json!({"type": "file", "file": {"filename": "paper.pdf", "file_data": format!("data:application/pdf;base64,{data}")}});
            json!([file, {"type": "text", "text": prompt.text}])
        }
        Some(paper) => {
            let mut paper = (paper.text)()?;
            if let Some(max) = p.max_input_chars.filter(|&m| paper.chars().count() > m) {
                note = Some(format!("paper text truncated from {} to {max} characters", paper.chars().count()));
                paper = paper.chars().take(max).collect();
            }
            json!(format!("{}\n\nPaper text:\n{paper}", prompt.text))
        }
        None => {
            note = Some("no PDF; summarized from metadata and notes only".into());
            json!(prompt.text)
        }
    };
    let mut body = json!({
        "model": p.model,
        "messages": [{"role": "system", "content": prompt.system}, {"role": "user", "content": user}],
    });
    if p.json_schema.unwrap_or(true) {
        body["response_format"] =
            json!({"type": "json_schema", "json_schema": {"name": "table_row", "strict": true, "schema": prompt.schema}});
    }
    // OpenCode Go's models reason before answering, which takes output tokens too.
    let default = (p.provider == Provider::OpencodeGo).then_some(32000);
    if let Some(m) = p.max_tokens.or(default) {
        body["max_tokens"] = json!(m);
    }
    Ok((body, note))
}

/// A minimal request, to test the profile and key.
pub fn ping(p: &Profile, key: Option<&str>) -> Result<()> {
    let mut p = p.clone();
    p.effort = Some("low".into());
    let schema = json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"], "additionalProperties": false});
    let prompt = Prompt { system: "Set ok to true.".into(), text: "Connection test.".into(), paper: None, schema };
    complete(&p, &prompt, key).map(|_| ())
}

/// A plain answer to `text`, searching the web where the provider can: Anthropic's, Mistral's and
/// OpenRouter's web search tools, or an OpenAI-compatible server that searches by itself.
pub fn search(p: &Profile, system: &str, text: &str, key: Option<&str>) -> Result<String> {
    let key = clean(key);
    let key = key.as_deref();
    if key.is_none() && p.needs_key() {
        bail!("no API key or OAuth token for {}", p.url());
    }
    match p.provider {
        Provider::Anthropic => anthropic_search(p, system, text, key.unwrap_or_default()),
        Provider::Mistral => mistral_search(p, system, text, key.unwrap_or_default()),
        Provider::Openai | Provider::Openrouter | Provider::OpencodeGo => {
            let base = p.url();
            let mut body = json!({"model": p.model, "messages": [{"role": "system", "content": system}, {"role": "user", "content": text}]});
            if p.provider == Provider::Openrouter {
                body["tools"] = json!([{"type": "openrouter:web_search"}]);
            }
            let v = post(&format!("{base}/chat/completions"), &openai_headers(p, key), &body)?;
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

fn mistral_search(p: &Profile, system: &str, text: &str, key: &str) -> Result<String> {
    let mut body = json!({
        "model": p.model,
        "instructions": system,
        "inputs": text,
        "tools": [{"type": "web_search"}],
        "stream": false,
        "store": false,
    });
    if let Some(m) = p.max_tokens {
        body["completion_args"] = json!({"max_tokens": m});
    }
    let v = post(&format!("{}/conversations", p.url()), &[("authorization", format!("Bearer {key}"))], &body)?;
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
    fn only_openrouter_gets_attribution_headers() {
        for provider in [Provider::Openai, Provider::Mistral, Provider::Anthropic] {
            let h = openai_headers(&Profile { provider, ..profile(None) }, Some("k"));
            assert_eq!(header(&h, "authorization"), Some("Bearer k"));
            assert_eq!(header(&h, "http-referer"), None);
            assert_eq!(header(&h, "x-openrouter-title"), None);
        }
        let h = openai_headers(&Profile { provider: Provider::Openrouter, ..profile(None) }, Some("k"));
        assert_eq!(header(&h, "authorization"), Some("Bearer k"));
        assert_eq!(header(&h, "http-referer"), Some("https://github.com/yuvve/yeda"));
        assert_eq!(header(&h, "x-openrouter-title"), Some("yeda"));
        assert_eq!(header(&h, "x-openrouter-app-visibility"), None);
    }

    #[test]
    fn refuses_a_local_plain_http_server() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let p = Profile { provider: Provider::Openai, base_url: Some(format!("http://{}/v1", server.local_addr().unwrap())), ..profile(None) };
        let err = ping(&p, Some("key")).unwrap_err().to_string();
        assert!(err.contains("only go over https://"), "{err}");
        assert!(search(&p, "s", "t", None).is_err());
        assert_eq!(server.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
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

    fn opencode_go() -> Profile {
        Profile { provider: Provider::OpencodeGo, model: "glm-5.3-flash".into(), ..profile(None) }
    }

    #[test]
    fn opencode_go_sends_one_session_id_and_its_own_user_agent() {
        let h = openai_headers(&opencode_go(), Some("oc-key"));
        assert_eq!(header(&h, "authorization"), Some("Bearer oc-key"));
        let session = header(&h, "x-opencode-session").unwrap();
        assert!(session.starts_with("ses_") && session.len() == 36, "{session}");
        assert_eq!(header(&openai_headers(&opencode_go(), None), "x-opencode-session"), Some(session));
        assert_eq!(header(&h, "user-agent"), Some(concat!("yeda/", env!("CARGO_PKG_VERSION"))));

        let openai = Profile { provider: Provider::Openai, ..opencode_go() };
        assert_eq!(openai_headers(&openai, Some("k")), [("authorization", "Bearer k".to_string())]);
    }

    #[test]
    fn opencode_go_needs_a_key_and_https() {
        let p = opencode_go();
        assert!(p.needs_key());
        assert_eq!(p.url(), "https://opencode.ai/inference/go/openai/v1");
        assert!(ping(&p, None).unwrap_err().to_string().contains("no API key"));

        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let p = Profile { base_url: Some(format!("http://{}/v1", server.local_addr().unwrap())), ..opencode_go() };
        let err = ping(&p, Some("oc-key")).unwrap_err().to_string();
        assert!(err.contains("only go over https://"), "{err}");
        assert!(search(&p, "s", "t", Some("oc-key")).is_err());
        assert_eq!(server.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn opencode_go_asks_for_the_schema_with_room_to_reason() {
        let schema = json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"], "additionalProperties": false});
        let prompt = Prompt { system: "s".into(), text: "t".into(), paper: None, schema: schema.clone() };
        let (body, _) = openai_body(&opencode_go(), &prompt).unwrap();
        assert_eq!(body["response_format"], json!({"type": "json_schema", "json_schema": {"name": "table_row", "strict": true, "schema": schema}}));
        assert_eq!(body["model"], "glm-5.3-flash");
        assert_eq!(body["max_tokens"], 32000);

        let (body, _) = openai_body(&Profile { max_tokens: Some(1000), json_schema: Some(false), ..opencode_go() }, &prompt).unwrap();
        assert_eq!(body["max_tokens"], 1000);
        assert!(body.get("response_format").is_none());
        let (body, _) = openai_body(&Profile { provider: Provider::Openai, ..opencode_go() }, &prompt).unwrap();
        assert!(body.get("max_tokens").is_none());
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
        let h = authorize(&profile(None), "sk-ant-api03-x", &mut body).unwrap();
        assert_eq!(header(&h, "x-api-key"), Some("sk-ant-api03-x"));
        assert_eq!(header(&h, "authorization"), None);
        assert_eq!(header(&h, "anthropic-beta"), Some("server-side-fallback-2026-07-01"));
        assert_eq!(body["system"], "s");
    }
}
