//! Finding papers with an LLM: the request, and reading the papers out of its answer.

use crate::library::Ref;
use anyhow::{Context, Result};
use serde::Deserialize;

/// A paper the LLM suggested.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Found {
    pub title: String,
    pub authors: String,
    pub year: Option<i32>,
    pub doi: Option<String>,
    pub url: Option<String>,
    /// Why it fits, in a sentence.
    pub why: String,
}

pub const SYSTEM: &str = "You find research papers for a researcher. Search the web when you can. \
Suggest only papers you have seen evidence exist; never invent titles, authors or DOIs. \
Leave out the papers the researcher already has. \
End your answer with a JSON array in a ```json block, one object per paper: \
{\"title\", \"authors\" (first author et al.), \"year\", \"doi\" (without https://doi.org/, null if unknown), \"url\", \"why\" (one sentence)}.";

/// The request: what the user asked for, and the papers it starts from.
pub fn request(prompt: &str, from: &[&Ref]) -> String {
    let mut text = String::new();
    if !prompt.trim().is_empty() {
        text += &format!("Find research papers: {}\n", prompt.trim());
    } else {
        text += "Find research papers closely related to these, which a literature review of them should include.\n";
    }
    if !from.is_empty() {
        text += "\nPapers I already have:\n";
        for r in from {
            let year = r.year.map(|y| format!(" ({y})")).unwrap_or_default();
            let doi = r.doi.as_ref().map(|d| format!(" doi:{d}")).unwrap_or_default();
            text += &format!("- {}{year}{doi}\n", r.title);
        }
    }
    text + "\nSuggest up to 15 papers, the most relevant first."
}

/// The papers in the answer's last JSON array.
pub fn parse(answer: &str) -> Result<Vec<Found>> {
    let start = answer.rfind("```json").map(|i| i + 7).or_else(|| answer.find('[')).context("the answer has no list of papers")?;
    let rest = &answer[start..];
    let (from, to) = (rest.find('[').context("the answer has no list of papers")?, rest.rfind(']').context("the answer's list of papers is cut off")?);
    let mut found: Vec<Found> = serde_json::from_str(&rest[from..=to]).context("the answer's list of papers is not valid JSON")?;
    for f in &mut found {
        f.doi = f.doi.take().map(|d| d.trim().trim_start_matches("https://doi.org/").trim_start_matches("doi:").to_string()).filter(|d| d.starts_with("10."));
    }
    found.retain(|f| !f.title.trim().is_empty());
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_papers() {
        let answer = "Here [1] is what I found.\n```json\n[{\"title\": \"A\", \"authors\": \"Smith et al.\", \"year\": 2020, \"doi\": \"https://doi.org/10.1/x\", \"why\": \"fits\"},\n {\"title\": \"B\", \"doi\": \"unknown\"}, {\"title\": \"\"}]\n```";
        let found = parse(answer).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].doi.as_deref(), found[0].year), (Some("10.1/x"), Some(2020)));
        assert_eq!(found[1].doi, None);
        assert!(parse("no papers").is_err());
        let r = Ref { title: "Seed".into(), year: Some(2019), ..Default::default() };
        assert!(request("", &[&r]).contains("- Seed (2019)"));
    }
}
