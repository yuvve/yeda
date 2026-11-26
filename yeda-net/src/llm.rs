use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::fmt;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use yeda_core::llm::{Auth, Bound, CodexCredential};

/// The paper's PDF; `text` extracts its plain text, called only when the server takes no PDF.
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

const NO_PAPER_NOTE: &str = "no PDF; summarized from metadata and notes only";
const TEXT_ONLY_NOTE: &str = "the server took no PDF; sent extracted text instead";
const NO_SEARCH_NOTE: &str = "answered without web search, from what the model knows";

/// Keys hold no whitespace; a long token copied from a terminal can wrap onto several lines.
fn clean(key: Option<&str>) -> Option<String> {
    key.map(|k| k.split_whitespace().collect::<String>()).filter(|k| !k.is_empty())
}

/// The key for a request, or an error when the provider needs one and none is stored.
fn required_key(b: &Bound, key: Option<&str>) -> Result<Option<String>> {
    let key = clean(key);
    if key.is_none() && b.needs_key() {
        bail!("no API key or OAuth credentials for provider '{}' ({})", b.provider, b.url());
    }
    Ok(key)
}

/// `key` is the API key or Codex credentials stored for the profile's provider, if any.
pub fn complete(b: &Bound, prompt: &Prompt, key: Option<&str>) -> Result<Reply> {
    let key = required_key(b, key)?;
    let key = key.as_deref();
    let (text, note) = send_paper(b, prompt, |pdf| match b.auth() {
        Auth::Codex => {
            let (body, note) = responses_body(b, prompt, Some(&prompt.schema), pdf)?;
            Ok((responses(b, key, &body)?, note))
        }
        Auth::ApiKey | Auth::None => {
            let (body, note) = chat_body(b, prompt, pdf)?;
            let v = post(&format!("{}/chat/completions", b.url()), &openai_headers(key), &body)?;
            match v["choices"][0]["finish_reason"].as_str() {
                Some("length" | "model_length") => bail!("output was cut off at the token limit; ask for less in the prompt"),
                Some("content_filter") => bail!("the model declined to answer"),
                _ => {}
            }
            Ok((v["choices"][0]["message"]["content"].as_str().context("no message content in response")?.to_string(), note))
        }
    })?;
    Ok(Reply { text, note })
}

/// A server that refused a PDF for a model does not get it again in this process.
static PDF_REFUSED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Runs `send(true)` with the PDF; when the server rejects the request, runs `send(false)` with
/// the extracted text and says so. `send` returns its result and a note on how the paper went.
fn send_paper<T>(b: &Bound, prompt: &Prompt, send: impl Fn(bool) -> Result<(T, Option<String>)>) -> Result<(T, Option<String>)> {
    let key = format!("{} {}", b.url(), b.model);
    let refused = |refused: &Vec<String>| refused.contains(&key);
    let pdf = prompt.paper.is_some() && !refused(&PDF_REFUSED.lock().unwrap_or_else(|e| e.into_inner()));
    match send(pdf) {
        Err(e) if pdf && e.downcast_ref::<HttpError>().is_some_and(|e| matches!(e.status, 400 | 415 | 422)) => {
            PDF_REFUSED.lock().unwrap_or_else(|e| e.into_inner()).push(key);
            Ok((send(false)?.0, Some(TEXT_ONLY_NOTE.into())))
        }
        sent => sent,
    }
}

static SESSION: OnceLock<String> = OnceLock::new();

/// The same headers go to every server. Some (OpenCode Go) reject a request without a session id
/// and ask clients to name themselves; one session per process groups a run's requests. OpenRouter
/// reads the referer and title to show the traffic as yeda rather than "Unknown app".
fn openai_headers(key: Option<&str>) -> Vec<(&'static str, String)> {
    let mut headers: Vec<_> = key.map(|k| ("authorization", format!("Bearer {k}"))).into_iter().collect();
    let session = SESSION.get_or_init(|| format!("ses_{}", uuid::Uuid::new_v4().simple()));
    headers.push(("x-opencode-session", session.clone()));
    headers.push(("user-agent", format!("yeda/{}", env!("CARGO_PKG_VERSION"))));
    headers.push(("http-referer", "https://github.com/yuvve/yeda".into()));
    headers.push(("x-openrouter-title", "yeda".into()));
    headers
}

/// The credential in the secret store is the whole signed-in record; requests carry only its
/// access token. A pasted bare token parses too.
fn codex_headers(key: &str) -> Result<Vec<(&'static str, String)>> {
    let cred = CodexCredential::parse(key)?;
    Ok(openai_headers(Some(&cred.access_token)))
}

/// A non-success answer; `send_paper` reads its status.
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
        return Err(HttpError { url: url.into(), status: status.as_u16(), body: text.chars().take(500).collect() }.into());
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

/// Maps the OpenAI-style `data[]`, Codex's `models[]` and display names to one list.
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
    let key = required_key(b, key)?;
    let headers = match b.auth() {
        Auth::Codex => codex_headers(key.as_deref().unwrap_or_default())?,
        Auth::ApiKey | Auth::None => openai_headers(key.as_deref()),
    };
    Ok(listed_models(&get(&format!("{}/models", b.url()), &headers)?))
}

fn data_url(pdf: &[u8]) -> String {
    format!("data:application/pdf;base64,{}", base64::engine::general_purpose::STANDARD.encode(pdf))
}

/// The chat completions request for `prompt`, with a note on how the paper went along.
fn chat_body(b: &Bound, prompt: &Prompt, pdf: bool) -> Result<(Value, Option<String>)> {
    let mut note = None;
    let user = match &prompt.paper {
        Some(paper) if pdf => json!([
            {"type": "file", "file": {"filename": "paper.pdf", "file_data": data_url(paper.pdf)}},
            {"type": "text", "text": prompt.text},
        ]),
        Some(paper) => json!(format!("{}\n\nPaper text:\n{}", prompt.text, (paper.text)()?)),
        None => {
            note = Some(NO_PAPER_NOTE.to_string());
            json!(prompt.text)
        }
    };
    let body = json!({
        "model": b.model,
        "messages": [{"role": "system", "content": prompt.system}, {"role": "user", "content": user}],
        "response_format": {"type": "json_schema", "json_schema": {"name": "table_row", "strict": true, "schema": prompt.schema}},
    });
    Ok((body, note))
}

/// The Responses API request for `prompt`, which the Codex token is accepted by.
fn responses_body(b: &Bound, prompt: &Prompt, schema: Option<&Value>, pdf: bool) -> Result<(Value, Option<String>)> {
    let mut note = None;
    let user = match &prompt.paper {
        Some(paper) if pdf => json!([
            {"type": "input_file", "filename": "paper.pdf", "file_data": data_url(paper.pdf)},
            {"type": "input_text", "text": prompt.text},
        ]),
        Some(paper) => json!([{"type": "input_text", "text": format!("{}\n\nPaper text:\n{}", prompt.text, (paper.text)()?)}]),
        None => {
            note = Some(NO_PAPER_NOTE.to_string());
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

/// Posts a Responses `body` with the Codex `key` and returns the answer text.
fn responses(b: &Bound, key: Option<&str>, body: &Value) -> Result<String> {
    let response = stream(&format!("{}/responses", b.url()), &codex_headers(key.unwrap_or_default())?, body)?;
    response_text(&response)
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
            Some("response.incomplete") if event["response"]["incomplete_details"]["reason"] == "content_filter" => bail!("the model declined to answer"),
            Some("response.incomplete") if event["response"]["incomplete_details"]["reason"] == "max_output_tokens" => {
                bail!("output was cut off at the token limit; ask for less in the prompt")
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
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string()?;
        return Err(HttpError { url: url.into(), status, body: text.chars().take(500).collect() }.into());
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
        .map(|(k, v)| format!("{k}={}", crate::oauth::encode(v)))
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
    crate::oauth::token_response(url, status.as_u16(), &text)
}

/// A minimal request, to test the profile and its provider's credentials.
pub fn ping(b: &Bound, key: Option<&str>) -> Result<()> {
    let schema = json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"], "additionalProperties": false});
    let prompt = Prompt { system: "Set ok to true.".into(), text: "Connection test.".into(), paper: None, schema };
    complete(b, &prompt, key).map(|_| ())
}

/// Answers `text` in plain text, without tools or a JSON schema: queries and ranking for `:discover`.
pub fn text(b: &Bound, system: &str, text: &str, key: Option<&str>) -> Result<String> {
    let key = required_key(b, key)?;
    let key = key.as_deref();
    match b.auth() {
        Auth::Codex => {
            let prompt = Prompt { system: system.into(), text: text.into(), paper: None, schema: Value::Null };
            let (body, _) = responses_body(b, &prompt, None, false)?;
            responses(b, key, &body)
        }
        Auth::ApiKey | Auth::None => {
            let body = json!({"model": b.model, "messages": [{"role": "system", "content": system}, {"role": "user", "content": text}]});
            let v = post(&format!("{}/chat/completions", b.url()), &openai_headers(key), &body)?;
            Ok(v["choices"][0]["message"]["content"].as_str().context("no message content in response")?.to_string())
        }
    }
}

/// A plain answer to `text`, from what the model knows: the servers are not asked to search the web.
pub fn search(b: &Bound, system: &str, question: &str, key: Option<&str>) -> Result<Reply> {
    Ok(Reply { text: text(b, system, question, key)?, note: Some(NO_SEARCH_NOTE.into()) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use yeda_core::llm::Provider;

    fn provider(auth: Auth) -> Provider {
        let base_url = if auth == Auth::Codex { "" } else { "https://api.anthropic.com/v1" };
        Provider { base_url: base_url.into(), auth, alias: None }
    }

    fn bound(auth: Auth) -> Bound {
        Bound { profile: "test".into(), provider: "test".into(), model: "test-model".into(), settings: provider(auth) }
    }

    fn plain_http(auth: Auth) -> (std::net::TcpListener, Bound) {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let settings = Provider { base_url: format!("http://{}/v1", server.local_addr().unwrap()), ..provider(auth) };
        (server, Bound { settings, ..bound(auth) })
    }

    fn header<'a>(h: &'a [(&'a str, String)], name: &str) -> Option<&'a str> {
        h.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }

    fn schema() -> Value {
        json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"], "additionalProperties": false})
    }

    fn http_error(status: u16) -> anyhow::Error {
        HttpError { url: "https://x.test".into(), status, body: String::new() }.into()
    }

    #[test]
    fn codex_sends_responses_with_stream_and_schema() {
        let prompt = Prompt { system: "s".into(), text: "t".into(), paper: None, schema: schema() };
        let b = Bound { model: "gpt-5.1".into(), ..bound(Auth::Codex) };
        assert!(b.needs_key());
        assert_eq!(b.url(), "https://api.openai.com/v1");
        let (body, _) = responses_body(&b, &prompt, Some(&prompt.schema), false).unwrap();
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["instructions"], "s");
        assert_eq!(body["model"], "gpt-5.1");
        assert_eq!(body["input"], json!([{"role": "user", "content": [{"type": "input_text", "text": "t"}]}]));
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert_eq!(body["text"]["format"]["schema"], schema());
        assert!(body.get("response_format").is_none());
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn papers_go_as_a_file_part_or_as_text() {
        let text = || Ok("extracted".to_string());
        let paper = || Some(Paper { pdf: b"%PDF", text: &text });
        let prompt = Prompt { system: "s".into(), text: "t".into(), paper: paper(), schema: schema() };
        let file = json!({"filename": "paper.pdf", "file_data": "data:application/pdf;base64,JVBERg=="});

        let (body, note) = chat_body(&bound(Auth::ApiKey), &prompt, true).unwrap();
        assert_eq!(body["messages"][1]["content"], json!([{"type": "file", "file": file}, {"type": "text", "text": "t"}]));
        assert_eq!(body["response_format"], json!({"type": "json_schema", "json_schema": {"name": "table_row", "strict": true, "schema": schema()}}));
        assert!(body.get("max_tokens").is_none() && note.is_none());
        let (body, _) = chat_body(&bound(Auth::ApiKey), &prompt, false).unwrap();
        assert_eq!(body["messages"][1]["content"], "t\n\nPaper text:\nextracted");

        let (body, _) = responses_body(&bound(Auth::Codex), &prompt, None, true).unwrap();
        assert_eq!(body["input"][0]["content"], json!([{"type": "input_file", "filename": "paper.pdf", "file_data": file["file_data"]}, {"type": "input_text", "text": "t"}]));
        let (body, _) = responses_body(&bound(Auth::Codex), &prompt, None, false).unwrap();
        assert_eq!(body["input"][0]["content"], json!([{"type": "input_text", "text": "t\n\nPaper text:\nextracted"}]));

        let none = Prompt { paper: None, ..prompt };
        assert_eq!(chat_body(&bound(Auth::ApiKey), &none, true).unwrap().1.as_deref(), Some(NO_PAPER_NOTE));
    }

    #[test]
    fn a_rejected_pdf_is_resent_as_text_and_remembered() {
        let text = || Ok("extracted".to_string());
        let prompt = Prompt { system: "s".into(), text: "t".into(), paper: Some(Paper { pdf: b"%PDF", text: &text }), schema: schema() };
        let b = Bound { model: "rejects-pdfs".into(), ..bound(Auth::ApiKey) };
        let calls = std::cell::RefCell::new(Vec::new());
        let send = |pdf: bool| {
            calls.borrow_mut().push(pdf);
            if pdf { Err(http_error(400)) } else { Ok(("answer", None)) }
        };
        assert_eq!(send_paper(&b, &prompt, send).unwrap(), ("answer", Some(TEXT_ONLY_NOTE.to_string())));
        assert_eq!(*calls.borrow(), [true, false]);
        calls.borrow_mut().clear();
        assert_eq!(send_paper(&b, &prompt, send).unwrap(), ("answer", None));
        assert_eq!(*calls.borrow(), [false]);
    }

    #[test]
    fn other_failures_are_not_retried_without_the_pdf() {
        let text = || Ok(String::new());
        let prompt = Prompt { system: "s".into(), text: "t".into(), paper: Some(Paper { pdf: b"%PDF", text: &text }), schema: schema() };
        let b = Bound { model: "overloaded".into(), ..bound(Auth::ApiKey) };
        let calls = std::cell::Cell::new(0);
        let err = send_paper::<()>(&b, &prompt, |_| {
            calls.set(calls.get() + 1);
            Err(http_error(500))
        })
        .unwrap_err();
        assert_eq!((calls.get(), err.downcast_ref::<HttpError>().map(|e| e.status)), (1, Some(500)));
    }

    #[test]
    fn responses_carries_the_access_token_not_the_whole_credential() {
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
        let headers = codex_headers(&signed.to_string()).unwrap();
        let token = header(&headers, "authorization").unwrap();
        assert_eq!(token, "Bearer eyJ-access");
        assert!(!token.contains('{'), "{token}");
        assert_eq!(header(&codex_headers("pasted-token").unwrap(), "authorization"), Some("Bearer pasted-token"));
        assert!(codex_headers("{\"tokens\":{}}").is_err());
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

        for (reason, said) in [("content_filter", "declined"), ("max_output_tokens", "cut off")] {
            let line = format!("data: {{\"type\":\"response.incomplete\",\"response\":{{\"incomplete_details\":{{\"reason\":\"{reason}\"}}}}}}");
            let err = sse_response([line].into_iter().map(Ok::<_, std::io::Error>)).unwrap_err();
            assert!(err.to_string().contains(said), "{err}");
        }

        let empty = sse_response(["data: not json"].into_iter().map(|l| Ok::<_, std::io::Error>(l.to_string()))).unwrap_err();
        assert!(empty.to_string().contains("ended without a completed event"), "{empty}");
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
        let claude = json!({"data": [{"id": "claude-x", "display_name": "Claude X"}]});
        assert_eq!(listed_models(&claude), [Model { id: "claude-x".into(), label: "Claude X".into() }]);
        assert!(listed_models(&json!({"data": []})).is_empty());
    }

    #[test]
    fn every_server_gets_the_same_headers_and_one_session() {
        let h = openai_headers(Some("k"));
        assert_eq!(header(&h, "authorization"), Some("Bearer k"));
        assert_eq!(header(&h, "user-agent"), Some(concat!("yeda/", env!("CARGO_PKG_VERSION"))));
        let session = header(&h, "x-opencode-session").unwrap();
        assert!(session.starts_with("ses_") && session.len() == 36, "{session}");
        assert_eq!(header(&openai_headers(None), "x-opencode-session"), Some(session));
        assert_eq!(header(&openai_headers(None), "authorization"), None);
        assert_eq!(header(&h, "http-referer"), Some("https://github.com/yuvve/yeda"));
        assert_eq!(header(&h, "x-openrouter-title"), Some("yeda"));
    }

    #[test]
    fn keys_lose_the_whitespace_a_terminal_wrapped_in() {
        assert_eq!(clean(Some(" sk-ant-api03-ab\ncd \n")).as_deref(), Some("sk-ant-api03-abcd"));
        assert_eq!(clean(Some(" \n")), None);
    }

    #[test]
    fn refuses_plain_http_without_connecting() {
        for auth in [Auth::ApiKey, Auth::Codex] {
            let (server, b) = plain_http(auth);
            let err = ping(&b, Some("key")).unwrap_err().to_string();
            assert!(err.contains("only go over https://"), "{err}");
            assert!(search(&b, "s", "t", Some("key")).is_err());
            assert_eq!(server.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        }
    }

    #[test]
    fn a_provider_with_a_key_needs_one_to_ask() {
        let err = ping(&bound(Auth::ApiKey), None).unwrap_err().to_string();
        assert!(err.contains("no API key"), "{err}");
        assert!(models(&bound(Auth::Codex), Some("  ")).is_err());
    }

    #[test]
    fn each_profile_keeps_its_own_model_on_a_shared_provider() {
        let settings = provider(Auth::ApiKey);
        let a = Bound { profile: "a".into(), provider: "claude".into(), model: "claude-opus-5-5".into(), settings: settings.clone() };
        let b = Bound { profile: "b".into(), provider: "claude".into(), model: "claude-sonnet-5-5".into(), settings };
        assert_eq!((a.url(), b.url()), ("https://api.anthropic.com/v1", "https://api.anthropic.com/v1"));
        assert_eq!((a.model.as_str(), b.model.as_str()), ("claude-opus-5-5", "claude-sonnet-5-5"));
    }
}
