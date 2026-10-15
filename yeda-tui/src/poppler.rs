use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

fn run(tool: &str, args: &[&OsStr]) -> Result<Vec<u8>> {
    let out = Command::new(tool).args(args).output().with_context(|| format!("running {tool} (install poppler)"))?;
    if !out.status.success() {
        bail!("{tool}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

pub fn pdf_text(pdf: &Path) -> Result<String> {
    Ok(String::from_utf8_lossy(&run("pdftotext", &[pdf.as_os_str(), "-".as_ref()])?).into())
}

/// `pdftotext -bbox` output: every word with its box.
pub fn word_boxes(pdf: &Path) -> Result<String> {
    Ok(String::from_utf8_lossy(&run("pdftotext", &["-bbox".as_ref(), pdf.as_os_str(), "-".as_ref()])?).into())
}

fn page_height(pdf: &Path, page: u32) -> Result<f32> {
    let p = page.to_string();
    let out = run("pdfinfo", &["-f".as_ref(), p.as_ref(), "-l".as_ref(), p.as_ref(), pdf.as_os_str()])?;
    String::from_utf8_lossy(&out)
        .lines()
        .filter(|l| l.starts_with("Page "))
        .find_map(|l| l.split_once(" size:")?.1.split_whitespace().nth(2)?.parse().ok())
        .with_context(|| format!("no size for page {page} of {}", pdf.display()))
}

/// PNG of `rect` (PDF coordinates) on 1-based `page`, whose height is `height`.
pub fn render_region(pdf: &Path, page: u32, height: f32, b: [f32; 4]) -> Result<Vec<u8>> {
    const DPI: f32 = 150.0;
    let px = |v: f32| ((v * DPI / 72.0).round() as i64).to_string();
    let (p, dpi) = (page.to_string(), DPI.to_string());
    let (x, y, w, h) = (px(b[0]), px(height - b[3]), px(b[2] - b[0]), px(b[3] - b[1]));
    let args = ["-png", "-singlefile", "-r", &dpi, "-f", &p, "-l", &p, "-x", &x, "-y", &y, "-W", &w, "-H", &h];
    let mut args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    args.push(pdf.as_os_str());
    run("pdftoppm", &args)
}

pub fn render_area(pdf: &Path, page: u32, rect: [f32; 4]) -> Result<Vec<u8>> {
    render_region(pdf, page, page_height(pdf, page)?, rect)
}
