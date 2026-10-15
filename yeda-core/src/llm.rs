use serde::Deserialize;

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

impl Profile {
    /// Anthropic always needs a key; an OpenAI-compatible server only when it is OpenAI's own.
    pub fn needs_key(&self) -> bool {
        self.provider == Provider::Anthropic || self.base_url.is_none()
    }

    /// The environment variable holding this profile's API key.
    pub fn key_env(&self) -> &str {
        self.api_key_env.as_deref().unwrap_or(match self.provider {
            Provider::Anthropic => "ANTHROPIC_API_KEY",
            Provider::Openai => "OPENAI_API_KEY",
        })
    }
}
