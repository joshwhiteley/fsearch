//! Small, cached extractors for the XML parts of DOCX and XLSX files.
//!
//! Office files are ZIP containers. We read only the text-bearing XML parts;
//! nothing is unpacked to disk and no external office tools are required.

use quick_xml::Reader;
use quick_xml::events::{BytesText, Event};
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use zip::ZipArchive;

/// Refuse large containers before opening or decompressing them.
pub const MAX_OFFICE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_ZIP_ENTRIES: usize = 4096;
const MAX_XML_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_CACHE_FILES: usize = 4096;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static IN_GUARD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn in_extract_guard() -> bool {
    IN_GUARD.with(|guard| guard.get())
}

/// Cache Office text separately from the existing PDF cache.
pub fn default_cache_dir() -> PathBuf {
    crate::index::default_cache_path()
        .parent()
        .map(|path| path.join("officetext"))
        .unwrap_or_else(|| PathBuf::from("/tmp/fsearch-officetext"))
}

pub fn cache_dir_for(pdf_cache: &Path) -> PathBuf {
    pdf_cache
        .parent()
        .map(|path| path.join("officetext"))
        .unwrap_or_else(default_cache_dir)
}

pub fn is_office_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "docx" | "xlsx"))
}

/// Extracted text is cached by path, modification time, and size. Both
/// successful parses and parse failures are cached so malformed files do not
/// get reparsed on every query.
pub fn extract_cached(path: &str, cache_dir: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_OFFICE_BYTES {
        return Err(format!(
            "office file larger than {} MiB",
            MAX_OFFICE_BYTES / (1024 * 1024)
        ));
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .hash(&mut hasher);
    let key = format!("office-{:016x}-{mtime}-{}.txt", hasher.finish(), meta.len());
    let cached = cache_dir.join(&key);
    if let Ok(text) = std::fs::read_to_string(&cached) {
        return Ok(text);
    }
    let cached_err = cache_dir.join(format!("{key}.err"));
    if let Ok(message) = std::fs::read_to_string(&cached_err) {
        return Err(message);
    }

    // Keep malformed-container behavior at the API boundary even if a future
    // ZIP/XML dependency regresses and panics on an unusual input.
    let was_guarded = IN_GUARD.with(|guard| {
        let old = guard.get();
        guard.set(true);
        old
    });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| extract(path)))
        .unwrap_or_else(|_| Err("office parser crashed on this file".to_string()));
    IN_GUARD.with(|guard| guard.set(was_guarded));
    let _ = std::fs::create_dir_all(cache_dir);
    let (target, body) = match &result {
        Ok(text) => (&cached, text.as_str()),
        Err(message) => (&cached_err, message.as_str()),
    };
    let nonce = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = cache_dir.join(format!(".{key}.{}-{nonce}.tmp", std::process::id()));
    // a failed write is removed rather than renamed into place, so a
    // disk-full event can't publish truncated text as a cache hit
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, target);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
    evict_oldest(cache_dir);
    result
}

fn extract(path: &str) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("invalid office ZIP: {e}"))?;
    if archive.len() > MAX_ZIP_ENTRIES {
        return Err(format!(
            "office ZIP has too many entries (max {MAX_ZIP_ENTRIES})"
        ));
    }
    match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("docx"))
    {
        Some(true) => extract_docx(&mut archive),
        _ => extract_xlsx(&mut archive),
    }
}

fn extract_docx(archive: &mut ZipArchive<std::fs::File>) -> Result<String, String> {
    let names: Vec<String> = (0..archive.len())
        .map(|i| {
            archive
                .by_index(i)
                .map(|f| f.name().to_string())
                .map_err(|e| format!("reading office ZIP entry: {e}"))
        })
        .collect::<Result<_, _>>()?;
    if !names.iter().any(|n| n == "word/document.xml") {
        return Err("DOCX has no word/document.xml".to_string());
    }

    let parts: Vec<&str> = names
        .iter()
        .filter(|n| {
            n.as_str() == "word/document.xml"
                || ((n.starts_with("word/header") || n.starts_with("word/footer"))
                    && n.ends_with(".xml"))
                || matches!(n.as_str(), "word/footnotes.xml" | "word/endnotes.xml")
        })
        .map(String::as_str)
        .collect();
    let mut output = String::new();
    let mut budget = MAX_XML_BYTES;
    for name in parts {
        let xml = read_entry(archive, name, &mut budget)?;
        parse_docx_xml(&xml, &mut output)?;
    }
    Ok(output)
}

fn extract_xlsx(archive: &mut ZipArchive<std::fs::File>) -> Result<String, String> {
    let names: Vec<String> = (0..archive.len())
        .map(|i| {
            archive
                .by_index(i)
                .map(|f| f.name().to_string())
                .map_err(|e| format!("reading office ZIP entry: {e}"))
        })
        .collect::<Result<_, _>>()?;
    let sheets: Vec<&str> = names
        .iter()
        .filter(|n| n.starts_with("xl/worksheets/") && n.ends_with(".xml"))
        .map(String::as_str)
        .collect();
    if sheets.is_empty() {
        return Err("XLSX has no worksheets".to_string());
    }
    let mut budget = MAX_XML_BYTES;
    let shared = match read_optional_entry(archive, "xl/sharedStrings.xml", &mut budget)? {
        Some(xml) => parse_shared_strings(&xml)?,
        None => Vec::new(),
    };
    let mut output = String::new();
    for name in sheets {
        let xml = read_entry(archive, name, &mut budget)?;
        parse_xlsx_sheet(&xml, &shared, &mut output)?;
    }
    Ok(output)
}

fn read_optional_entry(
    archive: &mut ZipArchive<std::fs::File>,
    name: &str,
    budget: &mut u64,
) -> Result<Option<Vec<u8>>, String> {
    match archive.by_name(name) {
        Ok(mut file) => {
            let size = file.size();
            read_entry_body(name, size, &mut file, budget).map(Some)
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(format!("reading office ZIP entry {name}: {e}")),
    }
}

fn read_entry(
    archive: &mut ZipArchive<std::fs::File>,
    name: &str,
    budget: &mut u64,
) -> Result<Vec<u8>, String> {
    let mut file = archive
        .by_name(name)
        .map_err(|e| format!("reading office ZIP entry {name}: {e}"))?;
    let size = file.size();
    read_entry_body(name, size, &mut file, budget)
}

fn read_entry_body<R: Read>(
    name: &str,
    declared_size: u64,
    file: &mut R,
    budget: &mut u64,
) -> Result<Vec<u8>, String> {
    let limit = MAX_XML_BYTES.min(*budget);
    if declared_size > limit {
        return Err(format!(
            "office XML entries exceed the size limit at {name}"
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("reading office XML {name}: {e}"))?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "office XML entries exceed the size limit at {name}"
        ));
    }
    *budget -= bytes.len() as u64;
    Ok(bytes)
}

fn parse_docx_xml(xml: &[u8], output: &mut String) -> Result<(), String> {
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut in_text = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => match local_name(event.name().as_ref()) {
                b"t" => in_text = true,
                b"br" | b"cr" => append_text(output, "\n")?,
                b"tab" => append_text(output, "\t")?,
                _ => {}
            },
            Ok(Event::Text(event)) if in_text => append_text(output, &xml_text(&event)?)?,
            Ok(Event::CData(event)) if in_text => append_text(output, &xml_cdata(&event)?)?,
            Ok(Event::GeneralRef(event)) if in_text => append_text(output, &xml_ref(&event)?)?,
            Ok(Event::Empty(event)) => match local_name(event.name().as_ref()) {
                b"br" | b"cr" => append_text(output, "\n")?,
                b"tab" => append_text(output, "\t")?,
                _ => {}
            },
            Ok(Event::End(event)) => match local_name(event.name().as_ref()) {
                b"t" => in_text = false,
                b"p" => append_text(output, "\n")?,
                _ => {}
            },
            Ok(Event::Eof) => return Ok(()),
            Err(e) => return Err(format!("invalid office XML: {e}")),
            _ => {}
        }
        buffer.clear();
    }
}

fn parse_shared_strings(xml: &[u8]) -> Result<Vec<String>, String> {
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut strings = Vec::new();
    let mut current = String::new();
    let mut in_si = false;
    let mut in_text = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => match local_name(event.name().as_ref()) {
                b"si" => {
                    current.clear();
                    in_si = true;
                }
                b"t" if in_si => in_text = true,
                _ => {}
            },
            Ok(Event::Text(event)) if in_text => current.push_str(&xml_text(&event)?),
            Ok(Event::CData(event)) if in_text => current.push_str(&xml_cdata(&event)?),
            Ok(Event::GeneralRef(event)) if in_text => current.push_str(&xml_ref(&event)?),
            Ok(Event::End(event)) => match local_name(event.name().as_ref()) {
                b"t" => in_text = false,
                b"si" if in_si => {
                    strings.push(std::mem::take(&mut current));
                    in_si = false;
                }
                _ => {}
            },
            Ok(Event::Eof) => return Ok(strings),
            Err(e) => return Err(format!("invalid shared strings XML: {e}")),
            _ => {}
        }
        buffer.clear();
    }
}

fn parse_xlsx_sheet(xml: &[u8], shared: &[String], output: &mut String) -> Result<(), String> {
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut row = String::new();
    let mut cells = 0usize;
    let mut cell_type = String::new();
    let mut cell_value = String::new();
    let mut in_cell = false;
    let mut in_value = false;
    let mut in_text = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => match local_name(event.name().as_ref()) {
                b"row" => {
                    row.clear();
                    cells = 0;
                }
                b"c" => {
                    cell_type.clear();
                    cell_value.clear();
                    for attr in event.attributes() {
                        let attr = attr.map_err(|e| format!("invalid XLSX cell attribute: {e}"))?;
                        if local_name(attr.key.as_ref()) == b"t" {
                            cell_type = attr
                                .normalized_value(Default::default())
                                .map_err(|e| format!("invalid XLSX cell type: {e}"))?
                                .into_owned();
                        }
                    }
                    in_cell = true;
                }
                b"v" if in_cell => in_value = true,
                b"t" if in_cell => in_text = true,
                _ => {}
            },
            Ok(Event::Empty(event)) => match local_name(event.name().as_ref()) {
                b"c" => append_cell(&mut row, &mut cells, "")?,
                b"row" if cells > 0 => {
                    append_text(output, &row)?;
                    append_text(output, "\n")?;
                    row.clear();
                    cells = 0;
                }
                _ => {}
            },
            Ok(Event::Text(event)) if in_value || in_text => {
                cell_value.push_str(&xml_text(&event)?);
            }
            Ok(Event::CData(event)) if in_value || in_text => {
                cell_value.push_str(&xml_cdata(&event)?);
            }
            Ok(Event::GeneralRef(event)) if in_value || in_text => {
                cell_value.push_str(&xml_ref(&event)?);
            }
            Ok(Event::End(event)) => match local_name(event.name().as_ref()) {
                b"v" => in_value = false,
                b"t" => in_text = false,
                b"c" if in_cell => {
                    if cell_type == "s" {
                        let index = cell_value
                            .parse::<usize>()
                            .map_err(|_| "invalid XLSX shared string index".to_string())?;
                        let value = shared.get(index).ok_or_else(|| {
                            "XLSX shared string index is out of range".to_string()
                        })?;
                        append_cell(&mut row, &mut cells, value)?;
                    } else {
                        append_cell(&mut row, &mut cells, &cell_value)?;
                    }
                    cell_value.clear();
                    in_cell = false;
                }
                b"row" if cells > 0 => {
                    append_text(output, &row)?;
                    append_text(output, "\n")?;
                    row.clear();
                    cells = 0;
                }
                _ => {}
            },
            Ok(Event::Eof) => return Ok(()),
            Err(e) => return Err(format!("invalid worksheet XML: {e}")),
            _ => {}
        }
        buffer.clear();
    }
}

fn append_cell(row: &mut String, cells: &mut usize, value: &str) -> Result<(), String> {
    if *cells > 0 {
        append_text(row, "\t")?;
    }
    append_text(row, value)?;
    *cells += 1;
    Ok(())
}

fn xml_text(event: &BytesText<'_>) -> Result<String, String> {
    let decoded = event
        .decode()
        .map_err(|e| format!("invalid office text encoding: {e}"))?;
    Ok(decoded.into_owned())
}

fn xml_ref(event: &quick_xml::events::BytesRef<'_>) -> Result<String, String> {
    let name = event
        .decode()
        .map_err(|e| format!("invalid office entity encoding: {e}"))?;
    quick_xml::escape::unescape(&format!("&{name};"))
        .map(|s| s.into_owned())
        .map_err(|e| format!("invalid office XML entity: {e}"))
}

fn xml_cdata(event: &quick_xml::events::BytesCData<'_>) -> Result<String, String> {
    event
        .decode()
        .map(|s| s.into_owned())
        .map_err(|e| format!("invalid office text encoding: {e}"))
}

fn append_text(output: &mut String, text: &str) -> Result<(), String> {
    if output.len().saturating_add(text.len()) > MAX_TEXT_BYTES {
        return Err(format!(
            "extracted office text exceeds {} MiB",
            MAX_TEXT_BYTES / (1024 * 1024)
        ));
    }
    output.push_str(text);
    Ok(())
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|b| *b == b':').next().unwrap_or(name)
}

fn evict_oldest(cache_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if !name.starts_with("office-") || !entry.metadata().ok()?.is_file() {
                return None;
            }
            Some((entry.metadata().ok()?.modified().ok()?, path))
        })
        .collect();
    if files.len() <= MAX_CACHE_FILES {
        return;
    }
    files.sort_by_key(|(modified, _)| *modified);
    let excess = files.len() - MAX_CACHE_FILES;
    for (_, path) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::{SimpleFileOptions, ZipWriter};

    fn zip_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, body) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut writer, body.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn extracts_docx_text_and_xml_entities() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.docx");
        std::fs::write(
            &file,
            zip_bytes(&[(
                "word/document.xml",
                r#"<w:document xmlns:w="x"><w:body><w:p><w:r><w:t>Annual &amp; final</w:t></w:r></w:p><w:p><w:t>Report</w:t><w:tab/><w:t>2024</w:t></w:p></w:body></w:document>"#,
            )]),
        )
        .unwrap();
        let text = extract_cached(file.to_str().unwrap(), &dir.path().join("cache")).unwrap();
        assert!(text.contains("Annual & final"));
        assert!(text.contains("Report\t2024"));
    }

    #[test]
    fn extracts_xlsx_shared_and_inline_strings() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sheet.xlsx");
        std::fs::write(
            &file,
            zip_bytes(&[
                (
                    "xl/sharedStrings.xml",
                    r#"<sst xmlns="x"><si><t>Annual report</t></si><si><r><t>Needle</t></r><r><t> value</t></r></si></sst>"#,
                ),
                (
                    "xl/worksheets/sheet1.xml",
                    r#"<worksheet xmlns="x"><sheetData><row><c t="s"><v>0</v></c><c t="inlineStr"><is><t>Needle inline</t></is></c></row><row><c t="s"><v>1</v></c><c><v>42</v></c></row></sheetData></worksheet>"#,
                ),
            ]),
        )
        .unwrap();
        let text = extract_cached(file.to_str().unwrap(), &dir.path().join("cache")).unwrap();
        assert!(text.contains("Annual report\tNeedle inline"));
        assert!(text.contains("Needle value\t42"));
    }

    #[test]
    fn corrupt_and_oversized_files_are_errors_without_panics() {
        let dir = tempfile::tempdir().unwrap();
        let corrupt = dir.path().join("bad.docx");
        std::fs::write(&corrupt, b"not a zip").unwrap();
        let result = std::panic::catch_unwind(|| {
            extract_cached(corrupt.to_str().unwrap(), &dir.path().join("cache"))
        });
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());

        let oversized = dir.path().join("big.xlsx");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(MAX_OFFICE_BYTES + 1).unwrap();
        assert!(extract_cached(oversized.to_str().unwrap(), dir.path()).is_err());
    }

    #[test]
    fn extraction_errors_are_cached() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.xlsx");
        std::fs::write(&file, b"bad").unwrap();
        let cache = dir.path().join("cache");
        let first = extract_cached(file.to_str().unwrap(), &cache).unwrap_err();
        let marker = std::fs::read_dir(&cache)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().ends_with(".err"))
            .unwrap();
        std::fs::write(marker.path(), "sentinel").unwrap();
        assert_eq!(
            extract_cached(file.to_str().unwrap(), &cache).unwrap_err(),
            "sentinel"
        );
        assert_ne!(first, "sentinel");
    }
}
