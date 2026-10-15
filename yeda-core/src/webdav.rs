//! Zotero's WebDAV file layout: each attachment is `<key>.zip`, holding the file, next to
//! `<key>.prop`, holding its modification time and MD5 hash.

use anyhow::{Context, Result};
use md5::{Digest, Md5};
use std::io::{Cursor, Read, Write};
use zip::write::SimpleFileOptions;

pub fn zip_name(key: &str) -> String {
    format!("{key}.zip")
}

pub fn prop_name(key: &str) -> String {
    format!("{key}.prop")
}

pub fn md5_hex(bytes: &[u8]) -> String {
    Md5::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// `<key>.zip` and `<key>.prop` for a file; `mtime_ms` is its modification time in Unix milliseconds.
pub fn pack(file_name: &str, bytes: &[u8], mtime_ms: i64) -> Result<(Vec<u8>, String)> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(file_name, SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated))?;
    zip.write_all(bytes)?;
    let zip = zip.finish()?.into_inner();
    let prop = format!(
        "<properties version=\"1\"><mtime>{mtime_ms}</mtime><hash>{}</hash></properties>",
        md5_hex(bytes)
    );
    Ok((zip, prop))
}

/// The file `file_name` from a `<key>.zip`, or its only file if the name doesn't match.
pub fn unpack(zip: &[u8], file_name: &str) -> Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip)).context("not a zip file")?;
    let index = archive.index_for_name(file_name).or((archive.len() == 1).then_some(0));
    let mut file = archive.by_index(index.with_context(|| format!("{file_name} is not in the zip"))?)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_and_unpacks() {
        let (zip, prop) = pack("A paper.pdf", b"%PDF-1.5 hello", 1_700_000_000_123).unwrap();
        assert_eq!(unpack(&zip, "A paper.pdf").unwrap(), b"%PDF-1.5 hello");
        assert_eq!(unpack(&zip, "renamed.pdf").unwrap(), b"%PDF-1.5 hello");
        assert_eq!(
            prop,
            format!("<properties version=\"1\"><mtime>1700000000123</mtime><hash>{}</hash></properties>", md5_hex(b"%PDF-1.5 hello"))
        );
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    }
}
