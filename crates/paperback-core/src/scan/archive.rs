//! Scanned pages are kept as PNG files in a comic book archive (`.cbz`), so the comic parser and renderer read them, and any comic reader can open one.
//! A `ComicInfo.xml` names the scan and marks it as Paperback's with `ScanInformation`, which is how a scan's OCR lines are told apart from a comic's balloons and joined into paragraphs.
//! A page is appended in place ([`ZipWriter::new_append`]), so a long feeder run writes each page once rather than rewriting the archive every time. The first page, a page replaced by a rescan, and Save Scan As each write a whole new archive beside the old one and rename it over it.
//! An append rewrites only the central directory at the end, so a crash during one can leave the archive in progress unreadable; it's only ever the scan in progress, never a saved one.

use std::{
	fs::{self, File, OpenOptions},
	io::{self, BufReader, BufWriter, Cursor, Read as _, Write as _},
	path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use image::{DynamicImage, ImageFormat};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::parser::cbz::page_names;

pub const EXTENSION: &str = "cbz";

/// The `ComicInfo.xml` field that says who scanned an archive.
const SCANNED_BY: &str = "<ScanInformation>Paperback</ScanInformation>";

/// Far past any feeder's capacity; the name keeps a natural sort even past it, but a limit stops a runaway driver filling the disk.
pub const MAXIMUM_PAGES: usize = 9999;

pub struct ScanArchive {
	path: PathBuf,
	title: String,
	/// Kept here, since this is the archive's only writer, so adding a page doesn't read the archive first.
	pages: usize,
}

impl ScanArchive {
	/// Picks a name in `dir` that no file has yet, `name.cbz` or `name 2.cbz` and so on, which is also the document's title. Nothing is written until the first page.
	pub fn create(dir: &Path, name: &str) -> io::Result<Self> {
		create_private_dir(dir)?;
		let stem = sanitize(name);
		let mut path = dir.join(format!("{stem}.{EXTENSION}"));
		let mut counter = 2;
		let mut title = stem.clone();
		while path.exists() {
			title = format!("{stem} {counter}");
			path = dir.join(format!("{title}.{EXTENSION}"));
			counter += 1;
		}
		Ok(Self { path, title, pages: 0 })
	}

	/// An archive written before, such as the source of Save Scan As. Its pages are counted once, here.
	pub fn open(path: &Path) -> Result<Self> {
		let title = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
		let mut archive = Self { path: path.to_path_buf(), title, pages: 0 };
		archive.pages = page_names(&mut archive.read()?).len();
		Ok(archive)
	}

	/// A copy at `destination` titled `title`, for Save Scan As. The pages are copied as they are; only the title changes.
	pub fn copy_to(&self, destination: &Path, title: &str) -> Result<Self> {
		let copy = Self { path: destination.to_path_buf(), title: title.to_string(), pages: self.pages };
		let mut archive = self.read()?;
		let dir = destination.parent().unwrap_or_else(|| Path::new("."));
		let temp = tempfile::Builder::new().prefix(".scan-").suffix(".tmp").tempfile_in(dir)?;
		let mut writer = ZipWriter::new(BufWriter::new(temp));
		copy.write_info(&mut writer)?;
		for index in 0..archive.len() {
			let entry = archive.by_index_raw(index)?;
			if !entry.name().is_ok_and(|name| is_info(&name)) {
				writer.raw_copy_file(entry)?;
			}
		}
		finish(writer, destination)?;
		Ok(copy)
	}

	#[must_use]
	pub fn path(&self) -> &Path {
		&self.path
	}

	#[must_use]
	pub const fn page_count(&self) -> usize {
		self.pages
	}

	/// Returns the new page's number, counting from 1.
	pub fn append_page(&mut self, page: &DynamicImage) -> Result<usize> {
		self.append_png(&encode_png(page)?)
	}

	pub fn append_png(&mut self, png: &[u8]) -> Result<usize> {
		if self.pages >= MAXIMUM_PAGES {
			anyhow::bail!("a scanned document holds at most {MAXIMUM_PAGES} pages");
		}
		let number = self.pages + 1;
		if self.path.exists() {
			let file = OpenOptions::new().read(true).write(true).open(&self.path)?;
			let mut writer = ZipWriter::new_append(file)?;
			writer.start_file(entry_name(number), page_options())?;
			writer.write_all(png)?;
			writer.finish()?.sync_all()?;
		} else {
			self.rewrite(None, Some((entry_name(number), png)))?;
		}
		self.pages = number;
		Ok(number)
	}

	/// For a page that read better turned around: the image the reader's OCR and any later Batch OCR see is the upright one.
	pub fn replace_page(&mut self, number: usize, page: &DynamicImage) -> Result<()> {
		self.replace_png(number, &encode_png(page)?)
	}

	/// For a page scanned again because it wasn't lined up. Rare, so the archive is rewritten whole.
	pub fn replace_png(&mut self, number: usize, png: &[u8]) -> Result<()> {
		let names = page_names(&mut self.read()?);
		let name = names.get(number.wrapping_sub(1)).with_context(|| format!("page {number} is not in the scan"))?;
		self.rewrite(Some(name.as_str()), Some((name.clone(), png)))
	}

	fn read(&self) -> Result<ZipArchive<BufReader<File>>> {
		let file = File::open(&self.path).with_context(|| format!("failed to open '{}'", self.path.display()))?;
		Ok(ZipArchive::new(BufReader::new(file))?)
	}

	/// `replaced` is left out of the copy; `added` is written where the replaced entry was, or at the end.
	fn rewrite(&self, replaced: Option<&str>, added: Option<(String, &[u8])>) -> Result<()> {
		let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
		let temp = tempfile::Builder::new().prefix(".scan-").suffix(".tmp").tempfile_in(dir)?;
		let mut writer = ZipWriter::new(BufWriter::new(temp));
		let options = page_options();
		let mut added = added;
		if !self.path.exists() {
			self.write_info(&mut writer)?;
		}
		if self.path.exists() {
			let mut archive = self.read()?;
			for index in 0..archive.len() {
				let entry = archive.by_index_raw(index)?;
				if replaced.is_some_and(|name| entry.name().is_ok_and(|entry_name| entry_name == name)) {
					drop(entry);
					if let Some((name, png)) = added.take() {
						writer.start_file(name, options)?;
						writer.write_all(png)?;
					}
					continue;
				}
				writer.raw_copy_file(entry)?;
			}
		}
		if let Some((name, png)) = added {
			writer.start_file(name, options)?;
			writer.write_all(png)?;
		}
		finish(writer, &self.path)
	}

	fn write_info(&self, writer: &mut ZipWriter<BufWriter<tempfile::NamedTempFile>>) -> Result<()> {
		writer.start_file("ComicInfo.xml", SimpleFileOptions::default())?;
		let info = format!(
			"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ComicInfo>\n\t<Title>{}</Title>\n\t{SCANNED_BY}\n</ComicInfo>\n",
			escape_xml(&self.title)
		);
		writer.write_all(info.as_bytes())?;
		Ok(())
	}
}

fn finish(writer: ZipWriter<BufWriter<tempfile::NamedTempFile>>, destination: &Path) -> Result<()> {
	let temp = writer.finish()?.into_inner().map_err(io::IntoInnerError::into_error)?;
	temp.as_file().sync_all()?;
	temp.persist(destination).map_err(|error| error.error)?;
	Ok(())
}

/// PNG is compressed already, so deflating it again only costs time.
fn page_options() -> SimpleFileOptions {
	SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
}

fn is_info(name: &str) -> bool {
	name.eq_ignore_ascii_case("ComicInfo.xml")
}

fn escape_xml(text: &str) -> String {
	text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Whether a comic archive is a scan Paperback made, whose OCR lines are prose to be joined into paragraphs.
#[must_use]
pub fn is_scan(path: &Path) -> bool {
	let Ok(file) = File::open(path) else { return false };
	let Ok(mut archive) = ZipArchive::new(BufReader::new(file)) else { return false };
	let Some(name) =
		archive.file_names().filter_map(Result::ok).find(|name| is_info(name)).map(std::borrow::Cow::into_owned)
	else {
		return false;
	};
	let Ok(entry) = archive.by_name(&name) else { return false };
	let mut info = String::new();
	// The marker is near the start; a huge ComicInfo is no scan of ours.
	io::Read::take(entry, 64 * 1024).read_to_string(&mut info).is_ok() && info.contains(SCANNED_BY)
}

pub fn encode_png(page: &DynamicImage) -> Result<Vec<u8>> {
	let mut png = Cursor::new(Vec::new());
	page.write_to(&mut png, ImageFormat::Png).context("failed to encode the scanned page")?;
	Ok(png.into_inner())
}

/// Zero-padded so that an archive tool lists the pages in order too.
fn entry_name(number: usize) -> String {
	format!("page-{number:04}.png")
}

/// The name comes from a translated string and a date, so only characters some file system refuses are replaced.
fn sanitize(name: &str) -> String {
	let cleaned: String =
		name.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '-' } else { c }).collect();
	let trimmed = cleaned.trim().trim_end_matches('.').to_string();
	if trimmed.is_empty() { "Scan".to_string() } else { trimmed }
}

/// Scans are often of letters and forms, so other accounts on the machine should not be able to read them.
fn create_private_dir(dir: &Path) -> io::Result<()> {
	fs::create_dir_all(dir)?;
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt as _;
		fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{document::ParseSettings, session::DocumentSession};

	fn page(width: u32) -> DynamicImage {
		DynamicImage::new_luma8(width, 3)
	}

	fn widths(archive: &ScanArchive) -> Vec<u32> {
		let mut zip = archive.read().unwrap();
		page_names(&mut zip)
			.iter()
			.map(|name| {
				let mut bytes = Vec::new();
				io::copy(&mut zip.by_name(name).unwrap(), &mut bytes).unwrap();
				image::load_from_memory(&bytes).unwrap().width()
			})
			.collect()
	}

	#[test]
	fn pages_are_added_in_order_and_numbered_from_one() {
		let dir = tempfile::tempdir().unwrap();
		let mut archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		assert_eq!(archive.page_count(), 0);
		assert!(!archive.path().exists(), "nothing is written before the first page");
		assert_eq!(archive.append_page(&page(1)).unwrap(), 1);
		assert_eq!(archive.append_page(&page(2)).unwrap(), 2);
		assert_eq!(archive.append_page(&page(3)).unwrap(), 3);
		assert_eq!(widths(&archive), vec![1, 2, 3]);
		assert_eq!(archive.page_count(), 3);
		assert_eq!(
			ScanArchive::open(archive.path()).unwrap().page_count(),
			3,
			"appended pages are all in the central directory"
		);
	}

	#[test]
	fn appending_writes_each_page_once() {
		let dir = tempfile::tempdir().unwrap();
		let mut archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		let png = encode_png(&DynamicImage::new_luma8(400, 400)).unwrap();
		archive.append_png(&png).unwrap();
		let after_one = fs::metadata(archive.path()).unwrap().len();
		for _ in 0..20 {
			archive.append_png(&png).unwrap();
		}
		let after_many = fs::metadata(archive.path()).unwrap().len();
		// Each page adds its own bytes and a little directory; nothing is written twice into the file.
		let per_page = (after_many - after_one) / 20;
		assert!(per_page < png.len() as u64 + 200, "{per_page} bytes a page for a {} byte page", png.len());
	}

	#[test]
	fn a_replaced_page_keeps_its_place() {
		let dir = tempfile::tempdir().unwrap();
		let mut archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		for width in 1..=3 {
			archive.append_page(&page(width)).unwrap();
		}
		archive.replace_page(2, &page(20)).unwrap();
		assert_eq!(widths(&archive), vec![1, 20, 3]);
		assert!(archive.replace_page(4, &page(1)).is_err());
		assert!(archive.replace_page(0, &page(1)).is_err());
	}

	#[test]
	fn a_new_scan_never_takes_an_existing_file_s_name() {
		let dir = tempfile::tempdir().unwrap();
		let mut first = ScanArchive::create(dir.path(), "Scan").unwrap();
		first.append_page(&page(1)).unwrap();
		let second = ScanArchive::create(dir.path(), "Scan").unwrap();
		assert_ne!(first.path(), second.path());
		assert!(second.path().to_string_lossy().ends_with("Scan 2.cbz"));
	}

	#[test]
	fn characters_a_file_system_refuses_are_replaced() {
		assert_eq!(sanitize("Scan 2026/11/21 14:30"), "Scan 2026-11-21 14-30");
		assert_eq!(sanitize("..\\.."), "..-");
		assert_eq!(sanitize("  "), "Scan");
	}

	#[test]
	fn no_temporary_file_is_left_behind() {
		let dir = tempfile::tempdir().unwrap();
		let mut archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		archive.append_page(&page(1)).unwrap();
		archive.replace_page(1, &page(2)).unwrap();
		let names: Vec<String> = fs::read_dir(dir.path())
			.unwrap()
			.map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
			.collect();
		assert_eq!(names, vec!["Scan.cbz".to_string()]);
	}

	#[test]
	fn a_scan_opens_as_a_comic_with_its_title_and_one_placeholder_per_page() {
		let dir = tempfile::tempdir().unwrap();
		let mut archive = ScanArchive::create(dir.path(), "Letter <draft>").unwrap();
		archive.append_page(&page(8)).unwrap();
		archive.append_page(&page(9)).unwrap();
		let path = archive.path().to_string_lossy().into_owned();
		let session = DocumentSession::new(&path, "", "", ParseSettings::default()).unwrap();
		assert_eq!(session.image_only_pages().len(), 2);
		assert_eq!(session.page_count(), 2);
		assert_eq!(session.title(), "Letter -draft-");
		assert!(is_scan(archive.path()));
	}

	#[test]
	fn a_comic_that_paperback_did_not_scan_is_not_a_scan() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("comic.cbz");
		let mut zip = ZipWriter::new(File::create(&path).unwrap());
		zip.start_file("ComicInfo.xml", SimpleFileOptions::default()).unwrap();
		zip.write_all(b"<ComicInfo><Title>Night Patrol</Title><ScanInformation>someone</ScanInformation></ComicInfo>")
			.unwrap();
		zip.start_file("001.png", SimpleFileOptions::default()).unwrap();
		zip.write_all(&encode_png(&page(2)).unwrap()).unwrap();
		zip.finish().unwrap();
		assert!(!is_scan(&path));
		assert!(!is_scan(&dir.path().join("missing.cbz")));
	}

	#[test]
	fn a_copy_keeps_the_pages_and_takes_the_new_title() {
		let dir = tempfile::tempdir().unwrap();
		let mut archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		archive.append_page(&page(3)).unwrap();
		archive.append_page(&page(4)).unwrap();
		let saved = archive.copy_to(&dir.path().join("Bank letter.cbz"), "Bank & letter").unwrap();
		assert_eq!(widths(&saved), vec![3, 4]);
		assert!(is_scan(saved.path()));
		let path = saved.path().to_string_lossy().into_owned();
		assert_eq!(DocumentSession::new(&path, "", "", ParseSettings::default()).unwrap().title(), "Bank & letter");
		assert!(archive.path().exists(), "the original is left for the caller to discard");
	}

	#[cfg(unix)]
	#[test]
	fn the_scans_folder_and_files_are_private() {
		use std::os::unix::fs::PermissionsExt as _;
		let dir = tempfile::tempdir().unwrap();
		let scans = dir.path().join("scans");
		let mut archive = ScanArchive::create(&scans, "Scan").unwrap();
		archive.append_page(&page(1)).unwrap();
		assert_eq!(fs::metadata(&scans).unwrap().permissions().mode() & 0o777, 0o700);
		assert_eq!(fs::metadata(archive.path()).unwrap().permissions().mode() & 0o077, 0);
	}
}
