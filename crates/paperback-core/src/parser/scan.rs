//! A document scanned by File > Scan document: a zip of page images that [`crate::scan::archive`] writes. Every page starts as an OCR placeholder with the [`MarkerType::ImageOnlyPage`] marker, exactly as a comic page or a scanned PDF page does, so the existing OCR code reads it.

use std::{fs::File, io::BufReader};

use anyhow::{Context, Result};
use zip::ZipArchive;

use crate::{
	document::{Document, DocumentBuffer, Marker, MarkerType, ParserContext},
	ocr::image_only_placeholder,
	parser::{Parser, cbz::page_names, util::path::extract_title_from_path},
	t,
};

pub struct ScanParser;

impl Parser for ScanParser {
	fn parse(&self, context: &ParserContext) -> Result<Document> {
		tracing::debug!(path = %context.file_path, "parsing scanned document");
		let file = File::open(&context.file_path)
			.with_context(|| format!("Failed to open scanned document '{}'", context.file_path))?;
		let mut archive = ZipArchive::new(BufReader::new(file)).map_err(|e| {
			tracing::warn!(path = %context.file_path, error = %e, "failed to open scanned document");
			// TRANSLATORS: Error shown when a scanned document (.pbscan) cannot be opened; {} is the underlying error
			anyhow::anyhow!(t("Failed to open scanned document: {}").replace("{}", &e.to_string()))
		})?;
		let pages = page_names(&mut archive);
		if pages.is_empty() {
			// TRANSLATORS: Error shown when a scanned document holds no page images
			return Err(anyhow::anyhow!(t("This scanned document contains no pages.")));
		}
		let mut document = Document::new().with_title(extract_title_from_path(&context.file_path));
		let (buffer, anchors) = build(&pages);
		document.set_buffer(buffer);
		document.id_positions = anchors.into_iter().collect();
		Ok(document)
	}
}

/// Each page gets an anchor, so a reload while pages are still being added keeps the caret on the page it was on rather than at a proportional guess.
fn build(pages: &[String]) -> (DocumentBuffer, Vec<(String, crate::document::DocPos)>) {
	let mut buffer = DocumentBuffer::new();
	let mut anchors = Vec::with_capacity(pages.len());
	let placeholder = image_only_placeholder();
	for (index, name) in pages.iter().enumerate() {
		let position = buffer.end();
		// The label stays "Page N": the OCR pass reads the page number back out of it.
		buffer.add_marker(Marker::new(MarkerType::PageBreak, position).with_text(format!("Page {}", index + 1)));
		buffer.add_marker(Marker::new(MarkerType::Image, position).with_reference(name.clone()));
		buffer.add_marker(Marker::new(MarkerType::ImageOnlyPage, position));
		anchors.push((page_anchor(index + 1), position));
		buffer.append(&placeholder);
		buffer.append("\n");
	}
	(buffer, anchors)
}

#[must_use]
pub fn page_anchor(page: usize) -> String {
	format!("page-{page}")
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{document::ParseSettings, scan::archive::ScanArchive, session::DocumentSession};

	fn page(width: u32) -> image::DynamicImage {
		image::DynamicImage::new_luma8(width, 4)
	}

	#[test]
	fn every_scanned_page_is_an_ocr_placeholder_with_its_own_anchor() {
		let pages = vec!["page-0001.png".to_string(), "page-0002.png".to_string()];
		let (buffer, anchors) = build(&pages);
		let image_only = buffer.markers.iter().filter(|m| m.mtype == MarkerType::ImageOnlyPage).count();
		assert_eq!(image_only, 2);
		assert_eq!(anchors.len(), 2);
		assert_eq!(anchors[0].0, "page-1");
		assert!(anchors[1].1 > anchors[0].1);
	}

	#[test]
	fn a_scanned_document_opens_with_one_placeholder_per_page() {
		let dir = tempfile::tempdir().unwrap();
		let archive = ScanArchive::create(dir.path(), "Scan test").unwrap();
		archive.append_page(&page(8)).unwrap();
		archive.append_page(&page(9)).unwrap();
		let path = archive.path().to_string_lossy().into_owned();
		let session = DocumentSession::new(&path, "", "", ParseSettings::default()).unwrap();
		assert_eq!(session.image_only_pages().len(), 2);
		assert_eq!(session.page_count(), 2);
		assert_eq!(session.title(), "Scan test");
	}

	#[test]
	fn an_empty_archive_is_an_error_rather_than_an_empty_document() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("empty.pbscan");
		zip::ZipWriter::new(File::create(&path).unwrap()).finish().unwrap();
		let path = path.to_string_lossy().into_owned();
		assert!(DocumentSession::new(&path, "", "", ParseSettings::default()).is_err());
	}
}
