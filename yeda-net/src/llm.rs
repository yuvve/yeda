use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::fmt;
use std::sync::OnceLock;
use std::time::Duration;
use yeda_core::llm::{Bound, Documents, Protocol, Stop, StructuredOutput, Usage};

/// The paper's PDF; `text` extracts its plain text, called only when the profile sends text.
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

#[derive(Debug)]
pub struct Reply {
    pub text: String,
    /// Something the user should know about how the request was made.
    pub note: Option<String>,
    pub usage: Usage,
}

const NO_PAPER_NOTE: &str = "no PDF; summarized from metadata and notes only";
const NO_SEARCH_NOTE: &str = "answered without web search, from what the model knows";

static INSTALL: OnceLock<String> = OnceLock::new();

/// The client's install id, which `{install}` stands for; set once at startup.
pub fn set_install_id(id: String) {
    let _ = INSTALL.set(id);
}

pub fn install_id() -> Option<&'static str> {
    INSTALL.get().map(String::as_str)
}

/// The secret for a request, from what the secret store keeps for the provider, or an error when
/// the provider needs one and none is stored.
fn secret(b: &Bound, stored: Option<&str>) -> Result<Option<String>> {
    let secret = crate::credential::of(&b.settings)?.secret(stored)?;
    if secret.is_none() && b.needs_key() {
        bail!("no API key or sign-in for provider '{}' ({})", b.provider, b.url());
    }
    Ok(secret)
}

/// Whether the stored credential is due for renewal before it is used.
pub fn refresh_due(b: &Bound, stored: &str) -> bool {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|t| t.as_secs() as i64).unwrap_or_default();
    crate::credential::of(&b.settings).is_ok_and(|c| c.refresh_due(stored, now))
}

/// The renewed credential to store in place of `stored`.
pub fn refresh(b: &Bound, stored: &str) -> Result<String> {
    crate::credential::of(&b.settings)?.refresh(stored)
}

/// Every header a request carries: the client's name, the secret where the protocol puts it,
/// and the provider's own.
fn headers(b: &Bound, secret: Option<&str>) -> Result<Vec<(String, String)>> {
    let fields = b.fields();
    let mut headers = vec![("user-agent".to_string(), format!("yeda/{}", env!("CARGO_PKG_VERSION")))];
    if let Some(secret) = secret {
        let name = fields.secret_header.clone().context("the protocol names no secret_header")?;
        headers.push((name, format!("{}{secret}", fields.secret_prefix.unwrap_or_default())));
    }
    headers.extend(b.headers(install_id())?);
    Ok(headers)
}

/// `stored` is what the secret store keeps for the profile's provider, if anything.
pub fn complete(b: &Bound, prompt: &Prompt, stored: Option<&str>) -> Result<Reply> {
    let mut note = None;
    let documents = b.options.documents.unwrap_or(Documents::File);
    let (text, pdf) = match &prompt.paper {
        Some(paper) if documents == Documents::File => (prompt.text.clone(), Some(paper.pdf)),
        Some(paper) => (format!("{}\n\nPaper text:\n{}", prompt.text, (paper.text)()?), None),
        None => {
            note = Some(NO_PAPER_NOTE.to_string());
            (prompt.text.clone(), None)
        }
    };
    let body = body(b, &prompt.system, &text, pdf, Some(&prompt.schema))?;
    let (text, usage) = send(b, stored, &body).map_err(|e| match e.downcast_ref::<HttpError>() {
        Some(h) if pdf.is_some() && matches!(h.status, 400 | 415 | 422) => {
            e.context(format!("the server may not take a PDF for this model; set documents = \"text\" in [llm.{}]", b.profile))
        }
        _ => e,
    })?;
    Ok(Reply { text, note, usage })
}

/// Answers `text` in plain text, without a JSON schema: queries and ranking for `:discover`.
pub fn text(b: &Bound, system: &str, text: &str, stored: Option<&str>) -> Result<Reply> {
    let body = body(b, system, text, None, None)?;
    let (text, usage) = send(b, stored, &body)?;
    Ok(Reply { text, note: None, usage })
}

/// A plain answer to `question`, from what the model knows: the servers are not asked to search the web.
pub fn search(b: &Bound, system: &str, question: &str, stored: Option<&str>) -> Result<Reply> {
    Ok(Reply { note: Some(NO_SEARCH_NOTE.into()), ..text(b, system, question, stored)? })
}

/// A minimal request, to test the profile and its provider's credentials.
pub fn ping(b: &Bound, stored: Option<&str>) -> Result<()> {
    let schema = json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"], "additionalProperties": false});
    let prompt = Prompt { system: "Set ok to true.".into(), text: "Connection test.".into(), paper: None, schema };
    complete(b, &prompt, stored).map(|_| ())
}

fn data_url(pdf: &[u8]) -> String {
    format!("data:application/pdf;base64,{}", base64::engine::general_purpose::STANDARD.encode(pdf))
}

/// The request for the profile's protocol, with the schema as its structured output and the
/// profile's options applied.
fn body(b: &Bound, system: &str, text: &str, pdf: Option<&[u8]>, schema: Option<&Value>) -> Result<Value> {
    let mode = b.options.structured_output.unwrap_or(StructuredOutput::JsonSchema);
    let system = match (schema, mode) {
        (Some(schema), StructuredOutput::JsonObject | StructuredOutput::Prompt) => format!("{system}\n\nAnswer with one JSON object that matches this JSON Schema:\n{schema}"),
        _ => system.to_string(),
    };
    let native = schema.filter(|_| mode == StructuredOutput::JsonSchema);
    let json_object = schema.is_some() && mode == StructuredOutput::JsonObject;
    let mut body = match b.protocol() {
        Protocol::ChatCompletions => {
            let user = match pdf {
                Some(pdf) => json!([{"type": "file", "file": {"filename": "paper.pdf", "file_data": data_url(pdf)}}, {"type": "text", "text": text}]),
                None => json!(text),
            };
            let mut body = json!({"model": b.model, "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}]});
            if let Some(schema) = native {
                body["response_format"] = json!({"type": "json_schema", "json_schema": {"name": "table_row", "strict": true, "schema": schema}});
            } else if json_object {
                body["response_format"] = json!({"type": "json_object"});
            }
            body
        }
        Protocol::Responses => {
            let mut user = vec![json!({"type": "input_text", "text": text})];
            if let Some(pdf) = pdf {
                user.insert(0, json!({"type": "input_file", "filename": "paper.pdf", "file_data": data_url(pdf)}));
            }
            let mut body = json!({"model": b.model, "instructions": system, "input": [{"role": "user", "content": user}], "store": false, "stream": true});
            if let Some(schema) = native {
                body["text"] = json!({"format": {"type": "json_schema", "name": "table_row", "strict": true, "schema": schema}});
            } else if json_object {
                body["text"] = json!({"format": {"type": "json_object"}});
            }
            body
        }
        Protocol::Messages => {
            if json_object {
                bail!("the messages protocol has no json-object mode; set structured_output to json-schema or prompt in [llm.{}]", b.profile);
            }
            if b.options.max_output_tokens.is_none() {
                bail!("the messages protocol needs an output cap; set max_output_tokens in [llm.{}]", b.profile);
            }
            let user = match pdf {
                Some(pdf) => json!([
                    {"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": base64::engine::general_purpose::STANDARD.encode(pdf)}},
                    {"type": "text", "text": text},
                ]),
                None => json!(text),
            };
            let mut body = json!({"model": b.model, "system": system, "messages": [{"role": "user", "content": user}]});
            if let Some(schema) = native {
                body["output_config"] = json!({"format": {"type": "json_schema", "schema": schema}});
            }
            body
        }
    };
    let fields = b.fields();
    if let (Some(effort), Some(at)) = (b.options.reasoning_effort, &fields.reasoning_effort) {
        at.set(&mut body, json!(effort.name()));
    }
    if let (Some(cap), Some(at)) = (b.options.max_output_tokens, &fields.max_output_tokens) {
        at.set(&mut body, json!(cap));
    }
    for (k, v) in b.options.body.iter().flatten() {
        body[k] = v.clone();
    }
    Ok(body)
}

/// Sends `body` and reads the reply: its stop reason first, then its text and usage.
fn send(b: &Bound, stored: Option<&str>, body: &Value) -> Result<(String, Usage)> {
    let secret = secret(b, stored)?;
    let headers = headers(b, secret.as_deref())?;
    let reply = match b.protocol() {
        Protocol::ChatCompletions => post(&format!("{}/chat/completions", b.url()), &headers, body)?,
        Protocol::Responses => stream(&format!("{}/responses", b.url()), &headers, body)?,
        Protocol::Messages => post(&format!("{}/messages", b.url()), &headers, body)?,
    };
    let fields = b.fields();
    match fields.stop(&reply) {
        Some(Stop::CutOff) => bail!("output was cut off at the token limit; ask for less in the prompt"),
        Some(Stop::Refused) => bail!("the model declined to answer"),
        None => {}
    }
    let text = match b.protocol() {
        Protocol::ChatCompletions => reply["choices"][0]["message"]["content"].as_str().context("no message content in response")?.to_string(),
        Protocol::Responses => response_text(&reply)?,
        Protocol::Messages => {
            let text: String = reply["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect();
            anyhow::ensure!(!text.is_empty(), "the response carries no text");
            text
        }
    };
    Ok((text, fields.usage(&reply)))
}

/// A non-success answer; `complete` and `is_unauthorized` read its status.
#[derive(Debug)]
struct HttpError {
    url: String,
    status: u16,
    body: String,
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}: HTTP {}: {}", self.url, self.status, self.body)
    }
}

impl std::error::Error for HttpError {}

/// True when the server refused the credential.
pub fn is_unauthorized(e: &anyhow::Error) -> bool {
    e.downcast_ref::<HttpError>().is_some_and(|e| e.status == 401)
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).https_only(true).build().into()
}

fn post(url: &str, headers: &[(String, String)], body: &Value) -> Result<Value> {
    // The key goes in a header, so only to https servers.
    yeda_core::require_https(url)?;
    let mut req = agent(Duration::from_secs(15 * 60)).post(url).header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let mut resp = req.send(serde_json::to_string(body)?).with_context(|| url.to_string())?;
    let status = resp.status();
    let text = resp.body_mut().read_to_string()?;
    if !status.is_success() {
        return Err(HttpError { url: url.into(), status: status.as_u16(), body: text.chars().take(500).collect() }.into());
    }
    serde_json::from_str(&text).with_context(|| format!("{url}: response is not JSON"))
}

fn get(url: &str, headers: &[(String, String)]) -> Result<Value> {
    yeda_core::require_https(url)?;
    let mut req = agent(Duration::from_secs(30)).get(url);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let mut resp = req.call().with_context(|| url.to_string())?;
    let status = resp.status();
    let text = resp.body_mut().read_to_string()?;
    if !status.is_success() {
        return Err(HttpError { url: url.into(), status: status.as_u16(), body: text.chars().take(300).collect() }.into());
    }
    serde_json::from_str(&text).with_context(|| format!("{url}: response is not JSON"))
}

/// Reads an SSE stream of Responses events, returning the final response object.
fn stream(url: &str, headers: &[(String, String)], body: &Value) -> Result<Value> {
    use std::io::{BufRead, BufReader};
    yeda_core::require_https(url)?;
    let mut req = agent(Duration::from_secs(15 * 60)).post(url).header("content-type", "application/json").header("accept", "text/event-stream");
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let mut resp = req.send(serde_json::to_string(body)?).with_context(|| url.to_string())?;
    if resp.status() != 200 {
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string()?;
        return Err(HttpError { url: url.into(), status, body: text.chars().take(500).collect() }.into());
    }
    sse_response(BufReader::new(resp.body_mut().as_reader()).lines())
}

/// The `response` object of `response.completed` or `response.incomplete` in the SSE `lines`; the
/// deltas fill in its text when it carries none.
fn sse_response(lines: impl Iterator<Item = std::io::Result<String>>) -> Result<Value> {
    let mut text = String::new();
    for line in lines {
        let line = line?;
        let Some(data) = line.strip_prefix("data: ") else { continue };
        let Ok(mut event) = serde_json::from_str::<Value>(data) else { continue };
        match event["type"].as_str() {
            Some("response.output_text.delta") => text.push_str(event["delta"].as_str().unwrap_or_default()),
            Some("response.failed") | None if event["error"].is_object() => {
                bail!("{}: {}", event["type"].as_str().unwrap_or("error"), event["error"]["message"].as_str().unwrap_or("no message"))
            }
            Some("response.completed" | "response.incomplete") => {
                let mut response = event["response"].take();
                if response_text(&response).is_err() {
                    response["output"] = json!([{"type": "message", "content": [{"type": "output_text", "text": text}]}]);
                }
                return Ok(response);
            }
            _ => {}
        }
    }
    bail!("the Responses stream ended without a completed event")
}

/// The answer text of a Responses object.
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

/// A model a provider lists: `id` is what requests use and what yeda.toml keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub id: String,
    pub label: String,
}

/// The models in a model list, as the provider's field map reads it.
fn listed_models(b: &Bound, v: &Value) -> Result<Vec<Model>> {
    let fields = b.fields();
    let rows = fields.models.as_ref().context("the provider's field map has no models")?.get(v).and_then(Value::as_array).context("the model list has no models where the field map says")?;
    let id = fields.model_id.as_ref().context("the provider's field map has no model_id")?;
    let mut models: Vec<Model> = rows
        .iter()
        .filter(|m| fields.model_filter.as_ref().is_none_or(|f| f.pointer.get(m).and_then(Value::as_str) == Some(f.equals.as_str())))
        .filter_map(|m| {
            let id = id.get(m)?.as_str()?;
            let label = fields.model_label.as_ref().and_then(|l| l.get(m)).and_then(Value::as_str).unwrap_or(id);
            Some(Model { id: id.into(), label: label.into() })
        })
        .collect();
    models.dedup_by(|a, b| a.id == b.id);
    Ok(models)
}

/// The models the bound provider lists, for `:setup llm`'s picker. A server without the endpoint
/// returns an error, which the form shows without blocking a typed model name.
pub fn models(b: &Bound, stored: Option<&str>) -> Result<Vec<Model>> {
    let secret = secret(b, stored)?;
    listed_models(b, &get(&format!("{}/models", b.url()), &headers(b, secret.as_deref())?)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use yeda_core::llm::{Profile, Provider};

    fn preset(name: &str) -> Provider {
        toml::from_str(&format!("preset = \"{name}\"\n")).unwrap()
    }

    fn custom(protocol: &str, base_url: &str) -> Provider {
        toml::from_str(&format!("protocol = \"{protocol}\"\nbase_url = \"{base_url}\"\nauth = \"api-key\"\n")).unwrap()
    }

    fn bound(settings: &Provider, profile: &str) -> Bound {
        let p: Profile = toml::from_str(&format!("provider = \"p\"\nmodel = \"m\"\n{profile}")).unwrap();
        Bound::new("test", &p, "p", settings).unwrap()
    }

    fn plain_http(protocol: &str) -> (std::net::TcpListener, Bound) {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let settings = custom(protocol, &format!("http://{}/v1", server.local_addr().unwrap()));
        (server, bound(&settings, "max_output_tokens = 10\n"))
    }

    fn header<'a>(h: &'a [(String, String)], name: &str) -> Option<&'a str> {
        h.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn schema() -> Value {
        json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"], "additionalProperties": false})
    }

    fn http_error(status: u16) -> anyhow::Error {
        HttpError { url: "https://x.test".into(), status, body: String::new() }.into()
    }

    #[test]
    fn chat_completions_sends_the_schema_and_the_pdf() {
        let b = bound(&custom("chat-completions", "https://x.test/v1"), "reasoning_effort = \"low\"\nmax_output_tokens = 500\nbody = { seed = 1 }\n");
        let body = body(&b, "s", "t", Some(b"%PDF"), Some(&schema())).unwrap();
        let file = json!({"filename": "paper.pdf", "file_data": "data:application/pdf;base64,JVBERg=="});
        assert_eq!(body["messages"][1]["content"], json!([{"type": "file", "file": file}, {"type": "text", "text": "t"}]));
        assert_eq!(body["response_format"], json!({"type": "json_schema", "json_schema": {"name": "table_row", "strict": true, "schema": schema()}}));
        assert_eq!((&body["reasoning_effort"], &body["max_completion_tokens"], &body["seed"]), (&json!("low"), &json!(500), &json!(1)));
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn structured_output_modes_move_the_schema_into_the_prompt() {
        let object = bound(&custom("chat-completions", "https://x.test/v1"), "structured_output = \"json-object\"\n");
        let body = body(&object, "s", "t", None, Some(&schema())).unwrap();
        assert_eq!(body["response_format"], json!({"type": "json_object"}));
        assert!(body["messages"][0]["content"].as_str().unwrap().contains("JSON Schema"));
        let prompt = bound(&custom("chat-completions", "https://x.test/v1"), "structured_output = \"prompt\"\n");
        let body = super::body(&prompt, "s", "t", None, Some(&schema())).unwrap();
        assert!(body.get("response_format").is_none() && body["messages"][0]["content"].as_str().unwrap().contains("JSON Schema"));
        let plain = super::body(&prompt, "s", "t", None, None).unwrap();
        assert_eq!(plain["messages"][0]["content"], "s");
    }

    #[test]
    fn responses_streams_with_its_own_parts() {
        let b = bound(&preset("codex"), "reasoning_effort = \"high\"\n");
        let body = body(&b, "s", "t", Some(b"%PDF"), Some(&schema())).unwrap();
        assert_eq!((&body["store"], &body["stream"], &body["instructions"]), (&json!(false), &json!(true), &json!("s")));
        assert_eq!(body["input"][0]["content"][0]["type"], "input_file");
        assert_eq!(body["input"][0]["content"][1], json!({"type": "input_text", "text": "t"}));
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert_eq!(body["reasoning"]["effort"], "high");
    }

    #[test]
    fn messages_sends_a_document_block_and_a_cap() {
        let b = bound(&preset("anthropic"), "reasoning_effort = \"low\"\n");
        let body = body(&b, "s", "t", Some(b"%PDF"), Some(&schema())).unwrap();
        assert_eq!(body["messages"][0]["content"][0], json!({"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": "JVBERg=="}}));
        assert_eq!((&body["system"], &body["max_tokens"]), (&json!("s"), &json!(32000)));
        assert_eq!(body["output_config"], json!({"effort": "low", "format": {"type": "json_schema", "schema": schema()}}));
        let uncapped = bound(&custom("messages", "https://x.test/v1"), "");
        assert!(super::body(&uncapped, "s", "t", None, None).unwrap_err().to_string().contains("max_output_tokens"));
        let object = bound(&preset("anthropic"), "structured_output = \"json-object\"\n");
        assert!(super::body(&object, "s", "t", None, Some(&schema())).unwrap_err().to_string().contains("json-object"));
    }

    #[test]
    fn papers_go_as_text_when_the_profile_says_so() {
        let text = || Ok("extracted".to_string());
        let prompt = Prompt { system: "s".into(), text: "t".into(), paper: Some(Paper { pdf: b"%PDF", text: &text }), schema: schema() };
        let (server, mut b) = plain_http("chat-completions");
        b.options.documents = Some(Documents::Text);
        // Past building the body, the plain-http address stops the request without connecting.
        assert!(complete(&b, &prompt, Some("k")).unwrap_err().to_string().contains("https://"));
        assert_eq!(server.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn each_provider_sends_only_its_own_headers() {
        let version = format!("yeda/{}", env!("CARGO_PKG_VERSION"));
        let custom = headers(&bound(&custom("chat-completions", "https://x.test/v1"), ""), Some("k")).unwrap();
        assert_eq!(custom, [("user-agent".to_string(), version.clone()), ("authorization".into(), "Bearer k".into())]);
        let or = headers(&bound(&preset("openrouter"), ""), Some("k")).unwrap();
        assert_eq!(header(&or, "HTTP-Referer"), Some("https://github.com/yuvve/yeda"));
        assert_eq!(header(&or, "X-OpenRouter-Title"), Some("yeda"));
        assert_eq!(header(&or, "x-opencode-session"), None);
        let claude = headers(&bound(&preset("anthropic"), ""), Some("k")).unwrap();
        assert_eq!((header(&claude, "x-api-key"), header(&claude, "authorization")), (Some("k"), None));
        let go = bound(&preset("opencode-go"), "");
        assert_eq!(header(&headers(&go, None).unwrap(), "x-opencode-session"), Some(go.session.as_str()));
    }

    #[test]
    fn only_a_401_is_unauthorized() {
        assert!(is_unauthorized(&http_error(401)));
        assert!(!is_unauthorized(&http_error(400)));
        assert!(!is_unauthorized(&anyhow::anyhow!("HTTP 401")));
    }

    #[test]
    fn responses_sse_joins_deltas_and_keeps_the_final_object() {
        let lines = [
            "event: x",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}",
            "",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"lo\"}",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"message\",\"content\":[]}],\"usage\":{\"input_tokens\":3}}}",
            "data: after the end",
        ]
        .into_iter()
        .map(|l| Ok::<_, std::io::Error>(l.to_string()));
        let v = sse_response(lines).unwrap();
        assert_eq!(response_text(&v).unwrap(), "Hello");
        assert_eq!(bound(&preset("codex"), "").fields().usage(&v).input_tokens, Some(3));

        let failed = sse_response(["data: {\"type\":\"response.failed\",\"error\":{\"message\":\"no credit\"}}"].into_iter().map(|l| Ok::<_, std::io::Error>(l.to_string()))).unwrap_err();
        assert!(failed.to_string().contains("no credit"), "{failed}");
        let line = "data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}".to_string();
        let v = sse_response([line].into_iter().map(Ok::<_, std::io::Error>)).unwrap();
        assert_eq!(bound(&preset("codex"), "").fields().stop(&v), Some(Stop::CutOff));
        let empty = sse_response(["data: not json"].into_iter().map(|l| Ok::<_, std::io::Error>(l.to_string()))).unwrap_err();
        assert!(empty.to_string().contains("ended without a completed event"), "{empty}");
    }

    #[test]
    fn model_lists_follow_the_field_map() {
        let openai = json!({"data": [{"id": "gpt-5.1"}, {"id": "m", "name": "M"}]});
        assert_eq!(listed_models(&bound(&custom("chat-completions", "https://x.test"), ""), &openai).unwrap(), [
            Model { id: "gpt-5.1".into(), label: "gpt-5.1".into() },
            Model { id: "m".into(), label: "m".into() },
        ]);
        let codex = json!({"models": [
            {"slug": "gpt-5", "display_name": "GPT-5", "visibility": "list"},
            {"slug": "hidden", "display_name": "Hidden", "visibility": "hidden"},
            {"slug": "gpt-5", "display_name": "GPT-5", "visibility": "list"},
        ]});
        assert_eq!(listed_models(&bound(&preset("codex"), ""), &codex).unwrap(), [Model { id: "gpt-5".into(), label: "GPT-5".into() }]);
        let claude = json!({"data": [{"id": "claude-x", "display_name": "Claude X"}]});
        assert_eq!(listed_models(&bound(&preset("anthropic"), ""), &claude).unwrap(), [Model { id: "claude-x".into(), label: "Claude X".into() }]);
        assert!(listed_models(&bound(&preset("anthropic"), ""), &json!({})).is_err());
    }

    #[test]
    fn refuses_plain_http_without_connecting() {
        for protocol in ["chat-completions", "responses", "messages"] {
            let (server, b) = plain_http(protocol);
            let err = ping(&b, Some("key")).unwrap_err().to_string();
            assert!(err.contains("only go over https://"), "{protocol}: {err}");
            assert!(search(&b, "s", "t", Some("key")).is_err());
            assert_eq!(server.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        }
    }

    #[test]
    fn a_provider_with_a_key_needs_one_to_ask() {
        let err = ping(&bound(&preset("openrouter"), ""), None).unwrap_err().to_string();
        assert!(err.contains("no API key"), "{err}");
        assert!(models(&bound(&preset("codex"), ""), Some("  ")).is_err());
    }
}
