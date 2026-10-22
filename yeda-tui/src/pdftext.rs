use crate::poppler;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::{fs, thread};

pub type Extracted = (String, Result<String, String>);

/// Extracts the text of each `(key, pdf)` in the background, reusing what `cache` kept from an
/// earlier run unless the PDF changed since.
pub fn spawn(pdfs: Vec<(String, PathBuf)>, cache: Option<PathBuf>) -> Receiver<Extracted> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for (key, pdf) in pdfs {
            let cached = cache.as_ref().map(|c| cache_file(c, &pdf));
            let text = cached.as_deref().and_then(|c| fresh(c, &pdf)).map(Ok).unwrap_or_else(|| {
                let text = poppler::pdf_text(&pdf).map_err(|e| format!("{e:#}"))?;
                if let Some(c) = &cached {
                    let _ = c.parent().map(fs::create_dir_all);
                    let _ = fs::write(c, &text);
                }
                Ok(text)
            });
            if tx.send((key, text)).is_err() {
                break;
            }
        }
    });
    rx
}

// ponytail: DefaultHasher may change between Rust releases; that only re-extracts the cache once.
fn cache_file(cache: &Path, pdf: &Path) -> PathBuf {
    let mut h = DefaultHasher::new();
    pdf.hash(&mut h);
    cache.join("text").join(format!("{:016x}.txt", h.finish()))
}

fn fresh(cached: &Path, pdf: &Path) -> Option<String> {
    let modified = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
    (modified(cached)? >= modified(pdf)?).then(|| fs::read_to_string(cached).ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuses_cache_until_the_pdf_changes() {
        let dir = std::env::temp_dir().join(format!("yeda-pdftext-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("a.pdf");
        fs::write(&pdf, "pdf").unwrap();
        let cached = cache_file(&dir, &pdf);
        assert_ne!(cached, cache_file(&dir, &dir.join("b.pdf")));
        fs::create_dir_all(cached.parent().unwrap()).unwrap();
        fs::write(&cached, "text").unwrap();
        assert_eq!(fresh(&cached, &pdf).as_deref(), Some("text"));
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        fs::File::options().write(true).open(&pdf).unwrap().set_modified(later).unwrap();
        assert_eq!(fresh(&cached, &pdf), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
