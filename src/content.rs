use crate::{office, pdf};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use rayon::prelude::*;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, PartialEq)]
pub struct ContentMatch {
    pub path: String,
    pub line_number: u64,
    pub line: String,
}

const PER_FILE_CAP: usize = 20;

pub fn search<'a>(
    indices: &[usize],
    resolve: impl Fn(usize) -> &'a str + Sync,
    pattern: &str,
    max_filesize: u64,
    pdf_cache: &Path,
    cancel: &AtomicBool,
    tx: &Sender<ContentMatch>,
) -> Result<(), String> {
    let smart_case_insensitive = !pattern.chars().any(|c| c.is_uppercase());
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(smart_case_insensitive)
        .build(pattern)
        .map_err(|e| e.to_string())?;

    let office_cache = office::cache_dir_for(pdf_cache);
    indices.par_iter().for_each(|&i| {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let path = resolve(i);
        let is_pdf = pdf::is_pdf_path(path);
        let is_office = office::is_office_path(path);
        match std::fs::metadata(path) {
            Ok(m) if m.is_file() && (is_pdf || is_office || m.len() <= max_filesize) => {}
            _ => return,
        }
        let mut searcher = SearcherBuilder::new()
            .binary_detection(BinaryDetection::quit(0))
            .line_number(true)
            .build();
        let mut sent = 0usize;
        let mut on_line = |line_number: u64, line: &str| {
            if cancel.load(Ordering::Relaxed) || sent >= PER_FILE_CAP {
                return Ok(false);
            }
            let hit = ContentMatch {
                path: path.to_string(),
                line_number,
                line: line.trim_end().to_string(),
            };
            sent += 1;
            if tx.send(hit).is_err() {
                cancel.store(true, Ordering::Relaxed);
                return Ok(false);
            }
            Ok(true)
        };
        if is_pdf {
            // grep the extracted (and cached) text instead of the raw bytes
            let Ok(text) = pdf::extract_cached(path, pdf_cache) else {
                return;
            };
            let _ = searcher.search_slice(&matcher, text.as_bytes(), UTF8(&mut on_line));
        } else if is_office {
            // DOCX/XLSX are ZIP containers, so search their cached XML text.
            let Ok(text) = office::extract_cached(path, &office_cache) else {
                return;
            };
            let _ = searcher.search_slice(&matcher, text.as_bytes(), UTF8(&mut on_line));
        } else {
            let _ = searcher.search_path(&matcher, path, UTF8(&mut on_line));
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    fn run(
        dir: &std::path::Path,
        files: &[(&str, &[u8])],
        pattern: &str,
        max: u64,
    ) -> Result<Vec<ContentMatch>, String> {
        let mut paths = Vec::new();
        for (name, body) in files {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            paths.push(p.to_string_lossy().into_owned());
        }
        let (tx, rx) = mpsc::channel();
        let cancel = AtomicBool::new(false);
        let indices: Vec<usize> = (0..paths.len()).collect();
        search(
            &indices,
            |i| paths[i].as_str(),
            pattern,
            max,
            &dir.join("pdfcache"),
            &cancel,
            &tx,
        )?;
        drop(tx);
        Ok(rx.into_iter().collect())
    }

    #[test]
    fn finds_matching_lines_with_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let hits = run(
            dir.path(),
            &[("a.txt", b"one\ntwo needle two\nthree\nneedle\n")],
            "needle",
            1024,
        )
        .unwrap();
        assert_eq!(hits.len(), 2);
        assert!(
            hits.iter()
                .any(|h| h.line_number == 2 && h.line.contains("two needle two"))
        );
        assert!(hits.iter().any(|h| h.line_number == 4));
    }

    #[test]
    fn smart_case() {
        let dir = tempfile::tempdir().unwrap();
        let files: &[(&str, &[u8])] = &[("a.txt", b"Needle\nneedle\n")];
        assert_eq!(run(dir.path(), files, "needle", 1024).unwrap().len(), 2);
        assert_eq!(run(dir.path(), files, "Needle", 1024).unwrap().len(), 1);
    }

    #[test]
    fn skips_binary_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let big = "needle\n".repeat(200); // > 1KiB when max is 1024
        let hits = run(
            dir.path(),
            &[
                ("bin.dat", b"needle\x00needle" as &[u8]),
                ("big.txt", big.as_bytes()),
                ("ok.txt", b"needle\n"),
            ],
            "needle",
            1024,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("ok.txt"));
    }

    #[test]
    fn pdf_text_is_searched() {
        let dir = tempfile::tempdir().unwrap();
        let hits = run(
            dir.path(),
            &[(
                "doc.pdf",
                &crate::pdf::minimal_pdf("annual Needle report") as &[u8],
            )],
            "needle",
            1024, // far below the pdf's size: pdfs bypass max_filesize
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("doc.pdf"));
        assert!(hits[0].line.contains("Needle"));
    }

    #[test]
    fn office_text_is_searched() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("word/document.xml", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(
            br#"<w:document xmlns:w="x"><w:body><w:p><w:t>annual Needle report</w:t></w:p></w:body></w:document>"#,
        )
        .unwrap();
        let docx = zip.finish().unwrap().into_inner();
        let mut xlsx = ZipWriter::new(Cursor::new(Vec::new()));
        xlsx.start_file("xl/worksheets/sheet1.xml", SimpleFileOptions::default())
            .unwrap();
        xlsx.write_all(
            br#"<worksheet xmlns="x"><sheetData><row><c t="inlineStr"><is><t>Needle in cells</t></is></c></row></sheetData></worksheet>"#,
        )
        .unwrap();
        let xlsx = xlsx.finish().unwrap().into_inner();
        let dir = tempfile::tempdir().unwrap();
        let hits = run(
            dir.path(),
            &[
                ("doc.docx", docx.as_slice()),
                ("sheet.xlsx", xlsx.as_slice()),
            ],
            "needle",
            1,
        )
        .unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| hit.line_number == 1));
        assert!(hits.iter().all(|hit| hit.line.contains("Needle")));
    }

    #[test]
    fn regex_patterns_work() {
        let dir = tempfile::tempdir().unwrap();
        let hits = run(
            dir.path(),
            &[("a.txt", b"invoice 2024-08\nno match\n")],
            r"invoice \d{4}-\d{2}",
            1024,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn invalid_pattern_is_err() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run(dir.path(), &[], "[bad", 1024).is_err());
    }

    #[test]
    fn per_file_cap_is_20() {
        let dir = tempfile::tempdir().unwrap();
        let body = "needle\n".repeat(100);
        let hits = run(dir.path(), &[("a.txt", body.as_bytes())], "needle", 10_240).unwrap();
        assert_eq!(hits.len(), 20);
    }
}
