use std::sync::mpsc::{self, Receiver};
use yeda_core::assist;
use yeda_core::llm::Bound;
use yeda_net::llm::Prompt;

/// Asks the model for a plan in the background; the answer is the reply's JSON text.
pub fn spawn(profile: Bound, key: Option<String>, text: String) -> Receiver<Result<String, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let prompt = Prompt { system: assist::SYSTEM.into(), text, paper: None, schema: assist::schema() };
        let _ = tx.send(yeda_net::llm::complete(&profile, &prompt, key.as_deref()).map(|r| r.text).map_err(|e| format!("{e:#}")));
    });
    rx
}
