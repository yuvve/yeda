use std::collections::BTreeMap;
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Every preset provider, sign-in and protocol default, in the format `yeda.toml` takes.
pub const PRESETS: &str = include_str!("presets.toml");

const APP: &str = "yeda";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// The wire format: how a request is laid out and a reply read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    ChatCompletions,
    Responses,
    Messages,
}

impl Protocol {
    pub const ALL: [Protocol; 3] = [Protocol::ChatCompletions, Protocol::Responses, Protocol::Messages];

    pub fn name(self) -> &'static str {
        match self {
            Protocol::ChatCompletions => "chat-completions",
            Protocol::Responses => "responses",
            Protocol::Messages => "messages",
        }
    }

    /// The top-level request keys the protocol sets itself, which `body` may not.
    pub fn reserved_body(self) -> &'static [&'static str] {
        match self {
            Protocol::ChatCompletions => &["model", "messages", "response_format", "stream"],
            Protocol::Responses => &["model", "instructions", "input", "text", "store", "stream"],
            Protocol::Messages => &["model", "system", "messages", "stream"],
        }
    }

    /// Where each value sits in this protocol's requests and replies, from `presets.toml`.
    pub fn fields(self) -> &'static Fields {
        &presets().protocols[&self]
    }
}

/// How the secret is obtained. `Oauth` names a sign-in preset in `sign_in`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Auth {
    ApiKey,
    None,
    Oauth,
}

impl Auth {
    pub fn name(self) -> &'static str {
        match self {
            Auth::ApiKey => "api-key",
            Auth::None => "none",
            Auth::Oauth => "oauth",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StructuredOutput {
    /// The protocol's strict JSON schema.
    JsonSchema,
    /// JSON mode, with the schema in the system prompt.
    JsonObject,
    /// Only the schema in the system prompt.
    Prompt,
}

impl StructuredOutput {
    pub const ALL: [StructuredOutput; 3] = [StructuredOutput::JsonSchema, StructuredOutput::JsonObject, StructuredOutput::Prompt];

    pub fn name(self) -> &'static str {
        match self {
            StructuredOutput::JsonSchema => "json-schema",
            StructuredOutput::JsonObject => "json-object",
            StructuredOutput::Prompt => "prompt",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Documents {
    /// The PDF itself, as the protocol's document part.
    File,
    /// The text extracted from the PDF.
    Text,
}

impl Documents {
    pub const ALL: [Documents; 2] = [Documents::File, Documents::Text];

    pub fn name(self) -> &'static str {
        match self {
            Documents::File => "file",
            Documents::Text => "text",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

impl ReasoningEffort {
    pub fn name(self) -> &'static str {
        match self {
            ReasoningEffort::None => "none",
            ReasoningEffort::Minimal => "minimal",
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::Xhigh => "xhigh",
        }
    }
}

/// An RFC 6901 JSON pointer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Pointer(String);

impl TryFrom<String> for Pointer {
    type Error = String;

    fn try_from(s: String) -> std::result::Result<Pointer, String> {
        if !s.starts_with('/') {
            return Err(format!("{s:?}: a JSON pointer starts with /"));
        }
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
                return Err(format!("{s:?}: ~ is written ~0 and / is written ~1"));
            }
        }
        Ok(Pointer(s))
    }
}

impl Pointer {
    pub fn get<'a>(&self, v: &'a Value) -> Option<&'a Value> {
        v.pointer(&self.0)
    }

    /// Sets the value, making the objects on the way.
    pub fn set(&self, v: &mut Value, value: Value) {
        let tokens: Vec<String> = self.0[1..].split('/').map(|t| t.replace("~1", "/").replace("~0", "~")).collect();
        let mut at = v;
        for token in tokens {
            if !at.is_object() {
                *at = Value::Object(Map::new());
            }
            at = at.as_object_mut().expect("an object").entry(token).or_insert(Value::Null);
        }
        *at = value;
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a reply's raw stop reason means, when it is not a normal end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    CutOff,
    Refused,
}

/// Keeps a listed model only when the value at `pointer` is `equals`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub pointer: Pointer,
    pub equals: String,
}

/// Where values sit in requests, replies and model lists. A provider's entries replace its
/// protocol's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fields {
    /// The header the secret goes in, and what precedes it there.
    pub secret_header: Option<String>,
    pub secret_prefix: Option<String>,
    pub reasoning_effort: Option<Pointer>,
    pub max_output_tokens: Option<Pointer>,
    pub stop_reason: Option<Pointer>,
    #[serde(default)]
    pub stop_reasons: BTreeMap<String, Stop>,
    pub input_tokens: Option<Pointer>,
    pub output_tokens: Option<Pointer>,
    pub cached_tokens: Option<Pointer>,
    pub reasoning_tokens: Option<Pointer>,
    pub cost: Option<Pointer>,
    /// The array of models in the model list, and each model's id and label in it.
    pub models: Option<Pointer>,
    pub model_id: Option<Pointer>,
    pub model_label: Option<Pointer>,
    pub model_filter: Option<Filter>,
}

impl Fields {
    /// These entries, with `base`'s where these have none.
    pub fn over(&self, base: &Fields) -> Fields {
        let mut stop_reasons = base.stop_reasons.clone();
        stop_reasons.extend(self.stop_reasons.clone());
        Fields {
            secret_header: self.secret_header.clone().or_else(|| base.secret_header.clone()),
            secret_prefix: self.secret_prefix.clone().or_else(|| base.secret_prefix.clone()),
            reasoning_effort: self.reasoning_effort.clone().or_else(|| base.reasoning_effort.clone()),
            max_output_tokens: self.max_output_tokens.clone().or_else(|| base.max_output_tokens.clone()),
            stop_reason: self.stop_reason.clone().or_else(|| base.stop_reason.clone()),
            stop_reasons,
            input_tokens: self.input_tokens.clone().or_else(|| base.input_tokens.clone()),
            output_tokens: self.output_tokens.clone().or_else(|| base.output_tokens.clone()),
            cached_tokens: self.cached_tokens.clone().or_else(|| base.cached_tokens.clone()),
            reasoning_tokens: self.reasoning_tokens.clone().or_else(|| base.reasoning_tokens.clone()),
            cost: self.cost.clone().or_else(|| base.cost.clone()),
            models: self.models.clone().or_else(|| base.models.clone()),
            model_id: self.model_id.clone().or_else(|| base.model_id.clone()),
            model_label: self.model_label.clone().or_else(|| base.model_label.clone()),
            model_filter: self.model_filter.clone().or_else(|| base.model_filter.clone()),
        }
    }

    /// The stop reason in `reply`, when it means something other than a normal end.
    pub fn stop(&self, reply: &Value) -> Option<Stop> {
        let raw = self.stop_reason.as_ref()?.get(reply)?.as_str()?;
        self.stop_reasons.get(raw).copied()
    }

    pub fn usage(&self, reply: &Value) -> Usage {
        let count = |p: &Option<Pointer>| p.as_ref().and_then(|p| p.get(reply)).and_then(Value::as_u64);
        Usage {
            input_tokens: count(&self.input_tokens),
            output_tokens: count(&self.output_tokens),
            cached_tokens: count(&self.cached_tokens),
            reasoning_tokens: count(&self.reasoning_tokens),
            cost: self.cost.as_ref().and_then(|p| p.get(reply)).and_then(Value::as_f64),
        }
    }
}

/// What the calls of one command used, as far as the replies say.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cost: Option<f64>,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        fn sum<T: std::ops::Add<Output = T> + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a + b),
                (a, b) => a.or(b),
            }
        }
        self.input_tokens = sum(self.input_tokens, other.input_tokens);
        self.output_tokens = sum(self.output_tokens, other.output_tokens);
        self.cached_tokens = sum(self.cached_tokens, other.cached_tokens);
        self.reasoning_tokens = sum(self.reasoning_tokens, other.reasoning_tokens);
        self.cost = sum(self.cost, other.cost);
    }

    /// `320k in / 12k out · 0.41 credits`, or `None` when no reply reported anything.
    pub fn summary(&self) -> Option<String> {
        let tokens = |n: u64| if n >= 10_000 { format!("{}k", n / 1000) } else { n.to_string() };
        let mut parts = Vec::new();
        if self.input_tokens.is_some() || self.output_tokens.is_some() {
            parts.push(format!("{} in / {} out", tokens(self.input_tokens.unwrap_or(0)), tokens(self.output_tokens.unwrap_or(0))));
        }
        if let Some(cost) = self.cost {
            parts.push(format!("cost {cost:.4}"));
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

const VARIABLES: [&str; 6] = ["app", "version", "repository", "install", "session", "request"];

/// A header or parameter value with `{variable}`s; `{{` and `}}` are literal braces.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Template(String);

impl TryFrom<String> for Template {
    type Error = String;

    fn try_from(s: String) -> std::result::Result<Template, String> {
        if s.contains(['\r', '\n']) {
            return Err(format!("{s:?}: a header holds no line break"));
        }
        let t = Template(s);
        t.walk(|piece| match piece {
            Piece::Var(name) if !VARIABLES.contains(&name) => Err(format!("{{{name}}} is not one of {}", VARIABLES.map(|v| format!("{{{v}}}")).join(", "))),
            _ => Ok(()),
        })?;
        Ok(t)
    }
}

enum Piece<'a> {
    Text(&'a str),
    Var(&'a str),
}

/// The values a template's variables take.
pub struct Vars<'a> {
    /// Filled in by the client; a template that uses it fails without one.
    pub install: Option<&'a str>,
    pub session: &'a str,
}

impl Template {
    fn walk(&self, mut f: impl FnMut(Piece) -> std::result::Result<(), String>) -> std::result::Result<(), String> {
        let s = self.0.as_str();
        let mut rest = s;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("{{") {
                f(Piece::Text("{"))?;
                rest = after;
            } else if let Some(after) = rest.strip_prefix("}}") {
                f(Piece::Text("}"))?;
                rest = after;
            } else if let Some(after) = rest.strip_prefix('{') {
                let end = after.find('}').ok_or_else(|| format!("{s:?}: {{ without }}; write {{{{ for a brace"))?;
                f(Piece::Var(&after[..end]))?;
                rest = &after[end + 1..];
            } else if rest.starts_with('}') {
                return Err(format!("{s:?}: }} without {{; write }}}} for a brace"));
            } else {
                let end = rest.find(['{', '}']).unwrap_or(rest.len());
                f(Piece::Text(&rest[..end]))?;
                rest = &rest[end..];
            }
        }
        Ok(())
    }

    pub fn render(&self, vars: &Vars) -> Result<String> {
        let mut out = String::new();
        self.walk(|piece| {
            match piece {
                Piece::Text(t) => out.push_str(t),
                Piece::Var("app") => out.push_str(APP),
                Piece::Var("version") => out.push_str(VERSION),
                Piece::Var("repository") => out.push_str(REPOSITORY),
                Piece::Var("install") => out.push_str(vars.install.ok_or("{install} has no value: the client set no install id")?),
                Piece::Var("session") => out.push_str(vars.session),
                Piece::Var(_) => out.push_str(&uuid::Uuid::new_v4().to_string()),
            }
            Ok(())
        })
        .map_err(anyhow::Error::msg)?;
        Ok(out)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a request carries besides the prompt; they depend on the model, so profiles set them and
/// providers give defaults.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub structured_output: Option<StructuredOutput>,
    pub documents: Option<Documents>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_output_tokens: Option<u32>,
    pub body: Option<Map<String, Value>>,
}

impl Options {
    /// These options, with `base`'s where these have none; `body` keys merge.
    pub fn over(&self, base: &Options) -> Options {
        let body = match (&base.body, &self.body) {
            (Some(b), Some(s)) => {
                let mut merged = b.clone();
                merged.extend(s.clone());
                Some(merged)
            }
            (b, s) => s.clone().or_else(|| b.clone()),
        };
        Options {
            structured_output: self.structured_output.or(base.structured_output),
            documents: self.documents.or(base.documents),
            reasoning_effort: self.reasoning_effort.or(base.reasoning_effort),
            max_output_tokens: self.max_output_tokens.or(base.max_output_tokens),
            body,
        }
    }

    fn check(&self, protocol: Protocol) -> Result<()> {
        for key in self.body.iter().flat_map(|b| b.keys()) {
            if protocol.reserved_body().contains(&key.as_str()) {
                bail!("body cannot set {key:?}: the {} protocol sets it", protocol.name());
            }
        }
        Ok(())
    }
}

/// A sign-in preset: an OAuth 2.0 authorization-code flow with PKCE.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignIn {
    pub authorize_url: String,
    pub token_url: String,
    /// The client a first sign-in names; the callback may return the client it registered.
    pub client_id: String,
    pub scope: String,
    /// A scope the grant must include.
    pub required_scope: Option<String>,
    /// RFC 8707 resource, sent with every authorization and token request.
    pub resource: Option<String>,
    /// When set, the id token's issuer, audience and nonce are checked.
    pub issuer: Option<String>,
    #[serde(default)]
    pub authorize_params: BTreeMap<String, Template>,
    /// Sent only when no client was registered yet.
    #[serde(default)]
    pub registration_params: BTreeMap<String, Template>,
}

struct Presets {
    providers: BTreeMap<String, Provider>,
    sign_ins: BTreeMap<String, SignIn>,
    protocols: BTreeMap<Protocol, Fields>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetsToml {
    providers: BTreeMap<String, ProviderToml>,
    #[serde(rename = "sign-in")]
    sign_ins: BTreeMap<String, SignIn>,
    protocols: BTreeMap<Protocol, Fields>,
}

fn parse_presets() -> Result<Presets> {
    let raw: PresetsToml = toml::from_str(PRESETS).context("presets.toml")?;
    for protocol in Protocol::ALL {
        anyhow::ensure!(raw.protocols.contains_key(&protocol), "presets.toml has no [protocols.{}]", protocol.name());
    }
    let mut providers = BTreeMap::new();
    for (name, p) in raw.providers {
        anyhow::ensure!(p.preset.is_none() && p.alias.is_none(), "presets.toml: providers.{name} sets preset or alias");
        providers.insert(name.clone(), p.custom().with_context(|| format!("presets.toml: providers.{name}"))?);
    }
    Ok(Presets { providers, sign_ins: raw.sign_ins, protocols: raw.protocols })
}

fn presets() -> &'static Presets {
    static PARSED: LazyLock<Presets> = LazyLock::new(|| parse_presets().expect("presets.toml is valid, which a test checks"));
    &PARSED
}

/// The preset providers' names, sorted.
pub fn preset_names() -> impl Iterator<Item = &'static str> {
    presets().providers.keys().map(String::as_str)
}

pub fn preset(name: &str) -> Option<&'static Provider> {
    presets().providers.get(name)
}

pub fn sign_in(name: &str) -> Option<&'static SignIn> {
    presets().sign_ins.get(name)
}

/// A `[providers.<name>]` entry: a preset, or every field set by hand. Its secret is shared by
/// every profile bound to it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "ProviderToml")]
pub struct Provider {
    /// The preset this provider is, if it is one; its fields are then the preset's.
    pub preset: Option<String>,
    pub protocol: Protocol,
    pub base_url: String,
    pub auth: Auth,
    /// The sign-in preset of an `oauth` provider.
    pub sign_in: Option<String>,
    pub headers: BTreeMap<String, Template>,
    pub fields: Fields,
    pub defaults: Options,
    /// Distinguishes this provider's environment variable from others on the same server.
    pub alias: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderToml {
    preset: Option<String>,
    protocol: Option<Protocol>,
    base_url: Option<String>,
    auth: Option<Auth>,
    sign_in: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, Template>,
    #[serde(default)]
    fields: Fields,
    #[serde(default)]
    defaults: Options,
    alias: Option<String>,
}

impl ProviderToml {
    fn custom(self) -> Result<Provider> {
        Ok(Provider {
            preset: None,
            protocol: self.protocol.context("set protocol: chat-completions, responses or messages; or preset = \"<name>\"")?,
            base_url: self.base_url.context("set base_url")?,
            auth: self.auth.context("set auth: api-key, none or oauth")?,
            sign_in: self.sign_in,
            headers: self.headers,
            fields: self.fields,
            defaults: self.defaults,
            alias: self.alias,
        })
    }
}

impl TryFrom<ProviderToml> for Provider {
    type Error = String;

    fn try_from(t: ProviderToml) -> std::result::Result<Provider, String> {
        let Some(name) = t.preset.clone() else {
            return t.custom().map_err(|e| format!("{e:#}"));
        };
        let customized = t.protocol.is_some() || t.base_url.is_some() || t.auth.is_some() || t.sign_in.is_some() || !t.headers.is_empty() || t.fields != Fields::default() || t.defaults != Options::default();
        if customized {
            return Err(format!("preset \"{name}\" takes only alias; to change it, copy its entry from :help providers into a provider of your own"));
        }
        let Some(p) = preset(&name) else {
            return Err(format!("no preset \"{name}\"; the presets are {}", preset_names().collect::<Vec<_>>().join(", ")));
        };
        Ok(Provider { preset: Some(name), alias: t.alias, ..p.clone() })
    }
}

/// Header names a provider cannot set: the client and the protocol set them.
fn reserved_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(name.as_str(), "authorization" | "host" | "cookie" | "user-agent") || name.starts_with("content-")
}

fn header_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

impl Provider {
    /// Where requests, and so the credential, go.
    pub fn url(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }

    pub fn needs_key(&self) -> bool {
        self.auth != Auth::None
    }

    /// The protocol's field map with this provider's entries over it.
    pub fn field_map(&self) -> Fields {
        self.fields.over(self.protocol.fields())
    }

    /// Checks what a request would otherwise fail on.
    pub fn check(&self) -> Result<()> {
        if !self.base_url.starts_with("https://") {
            bail!("{}: the base URL starts with https://", self.base_url);
        }
        match (self.auth, &self.sign_in) {
            (Auth::Oauth, None) => bail!("auth = \"oauth\" needs sign_in = \"<preset>\""),
            (Auth::Oauth, Some(name)) if sign_in(name).is_none() => bail!("no sign-in preset \"{name}\""),
            (Auth::ApiKey | Auth::None, Some(_)) => bail!("sign_in goes only with auth = \"oauth\""),
            _ => {}
        }
        let fields = self.field_map();
        let secret = fields.secret_header.clone().unwrap_or_default();
        anyhow::ensure!(header_name(&secret) && !matches!(secret.to_ascii_lowercase().as_str(), "host" | "cookie" | "user-agent") && !secret.to_ascii_lowercase().starts_with("content-"), "fields.secret_header: {secret:?} is not a header yeda can put the secret in");
        for name in self.headers.keys() {
            anyhow::ensure!(header_name(name), "headers: {name:?} is not a header name");
            anyhow::ensure!(!reserved_header(name) && !name.eq_ignore_ascii_case(&secret), "headers cannot set {name}: yeda sets it");
        }
        self.defaults.check(self.protocol).context("defaults")
    }
}

/// A `[llm.<name>]` entry: which provider to ask, which of its models, and what to send it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub provider: String,
    pub model: String,
    pub structured_output: Option<StructuredOutput>,
    pub documents: Option<Documents>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_output_tokens: Option<u32>,
    pub body: Option<Map<String, Value>>,
}

impl Profile {
    pub fn new(provider: &str, model: &str) -> Profile {
        Profile { provider: provider.into(), model: model.into(), structured_output: None, documents: None, reasoning_effort: None, max_output_tokens: None, body: None }
    }

    pub fn options(&self) -> Options {
        Options {
            structured_output: self.structured_output,
            documents: self.documents,
            reasoning_effort: self.reasoning_effort,
            max_output_tokens: self.max_output_tokens,
            body: self.body.clone(),
        }
    }
}

/// A profile together with the provider it names: what the requests of one command need.
#[derive(Debug, Clone, PartialEq)]
pub struct Bound {
    pub profile: String,
    pub provider: String,
    pub model: String,
    pub settings: Provider,
    /// The profile's options over the provider's defaults.
    pub options: Options,
    /// `{session}`: one per bound profile, so one per command.
    pub session: String,
}

impl Bound {
    pub fn new(profile: &str, p: &Profile, provider: &str, settings: &Provider) -> Result<Bound> {
        let options = p.options().over(&settings.defaults);
        options.check(settings.protocol).with_context(|| format!("llm.{profile}"))?;
        Ok(Bound {
            profile: profile.into(),
            provider: provider.into(),
            model: p.model.clone(),
            settings: settings.clone(),
            options,
            session: uuid::Uuid::new_v4().to_string(),
        })
    }

    /// Where requests, and so the credential, go.
    pub fn url(&self) -> &str {
        self.settings.url()
    }

    pub fn protocol(&self) -> Protocol {
        self.settings.protocol
    }

    pub fn auth(&self) -> Auth {
        self.settings.auth
    }

    pub fn needs_key(&self) -> bool {
        self.settings.needs_key()
    }

    pub fn fields(&self) -> Fields {
        self.settings.field_map()
    }

    /// The provider's headers, rendered for one request.
    pub fn headers(&self, install: Option<&str>) -> Result<Vec<(String, String)>> {
        let vars = Vars { install, session: &self.session };
        self.settings.headers.iter().map(|(k, t)| Ok((k.clone(), t.render(&vars).with_context(|| format!("headers.{k}"))?))).collect()
    }
}

/// Credentials from a sign-in, as stored in the secret store: JSON, or a bare access token.
#[derive(Debug, Clone, PartialEq)]
pub struct OauthCredential {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// The client the sign-in registered, which refreshes and reauthorizes.
    pub client_id: Option<String>,
    /// The id token, sent as a hint on the next sign-in.
    pub id_token: Option<String>,
    pub scope: Option<String>,
    /// Seconds since the epoch when the access token stops working.
    pub expires_at: Option<i64>,
}

impl OauthCredential {
    pub fn parse(key: &str) -> Result<OauthCredential> {
        let key = key.trim();
        if key.starts_with('{') {
            let v: Value = serde_json::from_str(key).map_err(|e| anyhow::anyhow!("{e:#}"))?;
            let text = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(String::from);
            let access = text("access_token").context("the stored sign-in has no access_token; sign in again")?;
            Ok(OauthCredential {
                expires_at: jwt_expires_at(&access),
                access_token: access,
                refresh_token: text("refresh_token"),
                client_id: text("client_id"),
                id_token: text("id_token"),
                scope: text("scope"),
            })
        } else {
            let access = key.split_whitespace().collect::<String>();
            if access.is_empty() {
                bail!("no access token");
            }
            Ok(OauthCredential { expires_at: jwt_expires_at(&access), access_token: access, refresh_token: None, client_id: None, id_token: None, scope: None })
        }
    }

    /// What the secret store keeps.
    pub fn to_json(&self) -> String {
        json!({
            "access_token": self.access_token,
            "refresh_token": self.refresh_token,
            "client_id": self.client_id,
            "id_token": self.id_token,
            "scope": self.scope,
        })
        .to_string()
    }

    /// The credential after a refresh, keeping its client; a refresh that rotates no refresh
    /// token keeps the old one.
    pub fn refreshed(&self, access_token: &str, refresh_token: Option<&str>) -> OauthCredential {
        OauthCredential {
            access_token: access_token.into(),
            refresh_token: refresh_token.map(String::from).or_else(|| self.refresh_token.clone()),
            expires_at: jwt_expires_at(access_token),
            ..self.clone()
        }
    }

    /// True when the access token runs out within `margin` seconds and a refresh token exists.
    pub fn needs_refresh(&self, margin: i64, now: i64) -> bool {
        self.refresh_token.is_some() && self.expires_at.is_some_and(|e| e - now < margin)
    }
}

/// `exp` from a JWT payload; `None` when the token is not a three-part JWT.
fn jwt_expires_at(jwt: &str) -> Option<i64> {
    use base64::Engine;
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v["exp"].as_i64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(text: &str) -> Result<Provider> {
        toml::from_str(text).map_err(Into::into)
    }

    #[test]
    fn every_preset_parses_and_checks() {
        parse_presets().unwrap();
        for name in preset_names() {
            let p = provider(&format!("preset = \"{name}\"\n")).unwrap();
            p.check().unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert_eq!(p.preset.as_deref(), Some(name));
        }
        assert_eq!(preset_names().collect::<Vec<_>>(), ["anthropic", "codex", "opencode-go", "openrouter"]);
    }

    #[test]
    fn a_preset_takes_only_an_alias() {
        let p = provider("preset = \"openrouter\"\nalias = \"work\"\n").unwrap();
        assert_eq!((p.url(), p.protocol, p.auth, p.alias.as_deref()), ("https://openrouter.ai/api/v1", Protocol::ChatCompletions, Auth::ApiKey, Some("work")));
        let err = provider("preset = \"openrouter\"\nbase_url = \"https://x.test\"\n").unwrap_err().to_string();
        assert!(err.contains("takes only alias") && err.contains(":help providers"), "{err}");
        assert!(provider("preset = \"openrouter\"\nheaders = { X = \"y\" }\n").is_err());
        assert!(provider("preset = \"nowhere\"\n").unwrap_err().to_string().contains("the presets are"));
    }

    #[test]
    fn a_preset_entry_works_as_a_custom_provider() {
        let presets: toml::Table = toml::from_str(PRESETS).unwrap();
        for (name, entry) in presets["providers"].as_table().unwrap() {
            let p: Provider = entry.clone().try_into().unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(p, Provider { preset: None, ..preset(name).unwrap().clone() });
        }
    }

    #[test]
    fn custom_providers_check_their_address_auth_and_headers() {
        let local = provider("protocol = \"chat-completions\"\nbase_url = \"https://localhost:8080/v1/\"\nauth = \"none\"\n").unwrap();
        assert_eq!(local.url(), "https://localhost:8080/v1");
        assert!(!local.needs_key());
        local.check().unwrap();
        let http = provider("protocol = \"chat-completions\"\nbase_url = \"http://x.test/v1\"\nauth = \"api-key\"\n").unwrap();
        assert!(http.check().unwrap_err().to_string().contains("https://"));
        assert!(provider("base_url = \"https://x.test\"\nauth = \"api-key\"\n").unwrap_err().to_string().contains("set protocol"));
        assert!(provider("protocol = \"openai\"\nbase_url = \"https://x.test\"\nauth = \"api-key\"\n").is_err());
        let oauth = provider("protocol = \"responses\"\nbase_url = \"https://x.test\"\nauth = \"oauth\"\n").unwrap();
        assert!(oauth.check().unwrap_err().to_string().contains("sign_in"));
        let custom = |headers: &str| provider(&format!("protocol = \"messages\"\nbase_url = \"https://x.test\"\nauth = \"api-key\"\nheaders = {{ {headers} }}\n"));
        custom("\"X-Client\" = \"{app}/{version}\"").unwrap().check().unwrap();
        for reserved in ["Authorization", "user-agent", "Content-Type", "x-api-key"] {
            assert!(custom(&format!("\"{reserved}\" = \"v\"")).unwrap().check().is_err(), "{reserved}");
        }
        assert!(custom("\"X\" = \"{secret}\"").unwrap_err().to_string().contains("is not one of"));
        assert!(custom("\"X\" = \"a\\nb\"").is_err());
        assert!(custom("\"bad name\" = \"v\"").unwrap().check().is_err());
    }

    #[test]
    fn templates_render_their_variables_and_braces() {
        let t = Template::try_from("{app}/{version} {{x}} {session}".to_string()).unwrap();
        let vars = Vars { install: None, session: "s1" };
        assert_eq!(t.render(&vars).unwrap(), format!("yeda/{VERSION} {{x}} s1"));
        let install = Template::try_from("urn:uuid:{install}".to_string()).unwrap();
        assert!(install.render(&vars).is_err());
        assert_eq!(install.render(&Vars { install: Some("i"), session: "" }).unwrap(), "urn:uuid:i");
        let request = Template::try_from("{request}".to_string()).unwrap();
        assert_ne!(request.render(&vars).unwrap(), request.render(&vars).unwrap());
        assert!(Template::try_from("{app".to_string()).is_err());
        assert!(Template::try_from("a}".to_string()).is_err());
    }

    #[test]
    fn pointers_read_and_write_nested_values() {
        let p = Pointer::try_from("/output_config/effort".to_string()).unwrap();
        let mut v = json!({"model": "m"});
        p.set(&mut v, json!("low"));
        assert_eq!(v, json!({"model": "m", "output_config": {"effort": "low"}}));
        assert_eq!(p.get(&v), Some(&json!("low")));
        assert!(Pointer::try_from("usage".to_string()).is_err());
        assert!(Pointer::try_from("/a~2".to_string()).is_err());
        let escaped = Pointer::try_from("/a~1b".to_string()).unwrap();
        let mut w = json!({});
        escaped.set(&mut w, json!(1));
        assert_eq!(w, json!({"a/b": 1}));
    }

    #[test]
    fn field_maps_read_stops_and_usage() {
        let or = preset("openrouter").unwrap().field_map();
        let reply = json!({"choices": [{"finish_reason": "length"}], "usage": {"prompt_tokens": 10, "completion_tokens": 2, "cost": 0.5}});
        assert_eq!(or.stop(&reply), Some(Stop::CutOff));
        assert_eq!(or.stop(&json!({"choices": [{"finish_reason": "stop"}]})), None);
        let usage = or.usage(&reply);
        assert_eq!((usage.input_tokens, usage.output_tokens, usage.cost), (Some(10), Some(2), Some(0.5)));
        let mut total = usage;
        total.add(&Usage { input_tokens: Some(30_000), ..Default::default() });
        assert_eq!(total.summary().as_deref(), Some("30k in / 2 out · cost 0.5000"));
        assert_eq!(Usage::default().summary(), None);
        let claude = preset("anthropic").unwrap().field_map();
        assert_eq!((claude.secret_header.as_deref(), claude.stop(&json!({"stop_reason": "refusal"}))), (Some("x-api-key"), Some(Stop::Refused)));
    }

    #[test]
    fn profiles_override_the_providers_defaults() {
        let claude = preset("anthropic").unwrap();
        let p: Profile = toml::from_str("provider = \"c\"\nmodel = \"m\"\ndocuments = \"text\"\nbody = { metadata = { a = 1 } }\n").unwrap();
        let b = Bound::new("x", &p, "c", claude).unwrap();
        assert_eq!((b.options.documents, b.options.max_output_tokens), (Some(Documents::Text), Some(32000)));
        let capped: Profile = toml::from_str("provider = \"c\"\nmodel = \"m\"\nmax_output_tokens = 100\n").unwrap();
        assert_eq!(Bound::new("x", &capped, "c", claude).unwrap().options.max_output_tokens, Some(100));
        let reserved: Profile = toml::from_str("provider = \"c\"\nmodel = \"m\"\nbody = { messages = [] }\n").unwrap();
        assert!(format!("{:#}", Bound::new("x", &reserved, "c", claude).unwrap_err()).contains("body cannot set \"messages\""));
        assert!(toml::from_str::<Profile>("provider = \"c\"\nmodel = \"m\"\nbase_url = \"https://x\"\n").is_err());
        assert_ne!(Bound::new("x", &p, "c", claude).unwrap().session, Bound::new("x", &p, "c", claude).unwrap().session);
    }

    #[test]
    fn headers_render_per_bound_profile() {
        let go = preset("opencode-go").unwrap();
        let b = Bound::new("x", &Profile::new("go", "m"), "go", go).unwrap();
        let h = b.headers(None).unwrap();
        assert_eq!(h, [("x-opencode-session".to_string(), b.session.clone())]);
        assert_eq!(b.headers(None).unwrap(), h, "one session for every request of a command");
    }

    #[test]
    fn oauth_credentials_parse_and_refresh() {
        let jwt = "eyJhbGciOiJub25lIn0.eyJleHAiOjE5MDAwMDAwMDB9.x";
        let stored = json!({"access_token": jwt, "refresh_token": "r", "client_id": "c", "id_token": "id", "scope": "s"}).to_string();
        let cred = OauthCredential::parse(&stored).unwrap();
        assert_eq!((cred.expires_at, cred.client_id.as_deref(), cred.id_token.as_deref()), (Some(1_900_000_000), Some("c"), Some("id")));
        assert!(cred.needs_refresh(600, 1_900_000_000 - 100));
        assert!(!cred.needs_refresh(600, 0));
        let renewed = cred.refreshed("a2", None);
        assert_eq!((renewed.access_token.as_str(), renewed.refresh_token.as_deref(), renewed.client_id.as_deref()), ("a2", Some("r"), Some("c")));
        assert_eq!(OauthCredential::parse(&renewed.to_json()).unwrap(), renewed);
        let bare = OauthCredential::parse(" eyJ-a\n eyJ-b ").unwrap();
        assert_eq!((bare.access_token.as_str(), bare.refresh_token.is_none()), ("eyJ-aeyJ-b", true));
        assert!(OauthCredential::parse("{}").is_err());
    }
}
