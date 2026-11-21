//! Scanned pages are kept as PNG files in a comic book archive (`.cbz`), so the comic parser and renderer read them, and any comic reader can open one.
//! A `ComicInfo.xml` names the scan and marks it as Paperback's with `ScanInformation`, which is how a scan's OCR lines are told apart from a comic's balloons and joined into paragraphs.
//! A page is appended in place ([`ZipWriter::new_append`]), so a long feeder run writes each page once rather than rewriting the archive every time. The first page, a page replaced by a rescan, and Save Scan As each write a whole new archive beside the old one and rename it over it.
//! An append overwrites only the central directory at the end. An append that fails (a full disk) puts the old directory back, so the archive is as it was; a crash during one can leave the archive in progress unreadable until [`repair`] recovers its complete pages. It's only ever the scan in progress, never a saved one.

use std::{
	fs::{self, File, OpenOptions},
	io::{self, BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write},
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
		self.append(&encode_png(page)?, "")
	}

	/// A page's image and its recognized lines (as [`crate::ocr::encode_pages`] writes them; empty for none), added together, so each page is written once and the archive carries its own text. If writing fails (a full disk), the archive is put back as it was and the page isn't counted, so the next page takes its number.
	pub fn append(&mut self, png: &[u8], text: &str) -> Result<usize> {
		self.append_through(png, text, Ok)
	}

	/// [`Self::append`], writing through `wrap`, so tests can make the write fail partway.
	fn append_through<W: Read + Write + Seek>(
		&mut self,
		png: &[u8],
		text: &str,
		wrap: impl FnOnce(File) -> io::Result<W>,
	) -> Result<usize> {
		if self.pages >= MAXIMUM_PAGES {
			anyhow::bail!("a scanned document holds at most {MAXIMUM_PAGES} pages");
		}
		let number = self.pages + 1;
		let entries = page_entries(number, png, text);
		if self.path.exists() {
			let file = OpenOptions::new().read(true).write(true).open(&self.path)?;
			// What an append overwrites, kept so a failed one can be undone.
			let old_length = file.metadata()?.len();
			let directory_start = ZipArchive::new(BufReader::new(&file))?.central_directory_start();
			let mut old_directory = Vec::new();
			(&file).seek(SeekFrom::Start(directory_start))?;
			(&file).take(old_length.saturating_sub(directory_start)).read_to_end(&mut old_directory)?;
			let written = wrap(file.try_clone()?).map_err(anyhow::Error::from).and_then(|target| {
				let mut writer = ZipWriter::new_append(target)?;
				for (name, data) in &entries {
					writer.start_file(name.as_str(), page_options())?;
					writer.write_all(data)?;
				}
				writer.finish()?;
				file.sync_all()?;
				Ok(())
			});
			// The writer is gone by now; dropped after an error, it may have tried to write a directory of its own.
			if let Err(error) = written {
				if let Err(undo) = restore_directory(&file, directory_start, &old_directory, old_length) {
					tracing::warn!(path = %self.path.display(), error = %undo, "could not undo a failed append");
				}
				return Err(error.context(format!("page {number} could not be saved")));
			}
		} else if self.pages > 0 {
			// Discarded while a run was still adding pages: writing a new archive would bring it back with pages missing.
			anyhow::bail!("the scan was deleted");
		} else {
			self.rewrite(&[], &entries)?;
		}
		self.pages = number;
		Ok(number)
	}

	/// For a page scanned again because it wasn't lined up: its image and text are replaced together. Rare, so the archive is rewritten whole.
	pub fn replace(&mut self, number: usize, png: &[u8], text: &str) -> Result<()> {
		if number == 0 || number > self.pages {
			anyhow::bail!("page {number} is not in the scan");
		}
		let entries = page_entries(number, png, text);
		self.rewrite(&[entry_name(number), text_entry_name(number)], &entries)
	}

	pub fn replace_page(&mut self, number: usize, page: &DynamicImage) -> Result<()> {
		self.replace(number, &encode_png(page)?, "")
	}

	fn read(&self) -> Result<ZipArchive<BufReader<File>>> {
		let file = File::open(&self.path).with_context(|| format!("failed to open '{}'", self.path.display()))?;
		Ok(ZipArchive::new(BufReader::new(file))?)
	}

	/// Entries named in `replaced` are left out of the copy; `added` is written where the first replaced entry was, or at the end.
	fn rewrite(&self, replaced: &[String], added: &[(String, &[u8])]) -> Result<()> {
		let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
		let temp = tempfile::Builder::new().prefix(".scan-").suffix(".tmp").tempfile_in(dir)?;
		let mut writer = ZipWriter::new(BufWriter::new(temp));
		let mut pending = Some(added);
		let mut write_added = |writer: &mut ZipWriter<BufWriter<tempfile::NamedTempFile>>| -> Result<()> {
			for (name, data) in pending.take().unwrap_or_default() {
				writer.start_file(name.as_str(), page_options())?;
				writer.write_all(data)?;
			}
			Ok(())
		};
		if self.path.exists() {
			let mut archive = self.read()?;
			for index in 0..archive.len() {
				let entry = archive.by_index_raw(index)?;
				if entry.name().is_ok_and(|name| replaced.iter().any(|replaced| *replaced == name)) {
					drop(entry);
					write_added(&mut writer)?;
					continue;
				}
				writer.raw_copy_file(entry)?;
			}
		} else {
			self.write_info(&mut writer)?;
		}
		write_added(&mut writer)?;
		finish(writer, &self.path)
	}

	fn write_info(&self, writer: &mut ZipWriter<BufWriter<tempfile::NamedTempFile>>) -> Result<()> {
		// Stored like the pages, so every entry can be recovered the same way.
		writer.start_file("ComicInfo.xml", page_options())?;
		writer.write_all(Self::info_xml(&self.title).as_bytes())?;
		Ok(())
	}

	fn info_xml(title: &str) -> String {
		format!(
			"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ComicInfo>\n\t<Title>{}</Title>\n\t{SCANNED_BY}\n</ComicInfo>\n",
			escape_xml(title)
		)
	}
}

/// Puts back the central directory a failed append overwrote, and cuts off whatever it wrote after it. Writing within the file's old length needs no new space, so this works on a full disk.
fn restore_directory(file: &File, directory_start: u64, old_directory: &[u8], old_length: u64) -> io::Result<()> {
	let mut file = file;
	file.seek(SeekFrom::Start(directory_start))?;
	file.write_all(old_directory)?;
	file.set_len(old_length)?;
	file.sync_all()
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

/// Whether the archive opens, which a crash in the middle of adding a page can prevent.
#[must_use]
pub fn opens(path: &Path) -> bool {
	File::open(path).is_ok_and(|file| ZipArchive::new(BufReader::new(file)).is_ok())
}

/// What [`repair`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repair {
	/// A new archive with this many complete pages took the old one's place.
	Recovered(usize),
	/// Not one complete page: the file is left as it is, and the caller may delete it.
	NothingToRecover,
}

/// Rebuilds a scan in progress whose last append was cut short. Pages are stored uncompressed with their sizes in their local headers, and an append only ever overwrites the central directory at the end, so every page before the cut is intact: the local headers are walked in order, up to the first entry that is incomplete or fails its checksum, and a new archive with those entries is written beside the old one and renamed over it. One entry is in memory at a time. An error (the file can't be read, the copy can't be written) says nothing about the pages, so the caller must keep the file; only [`Repair::NothingToRecover`] means there is nothing in it to keep.
pub fn repair(path: &Path) -> Result<Repair> {
	let file = File::open(path).with_context(|| format!("failed to open '{}'", path.display()))?;
	let length = file.metadata()?.len();
	let mut entries = IntactEntries { reader: BufReader::new(file), at: 0, length };
	let dir = path.parent().unwrap_or_else(|| Path::new("."));
	let temp = tempfile::Builder::new().prefix(".scan-").suffix(".tmp").tempfile_in(dir)?;
	let mut writer = ZipWriter::new(BufWriter::new(temp));
	let mut pages = 0;
	let mut wrote_info = false;
	while let Some((name, data)) = entries.next_entry()? {
		let keep = if is_info(&name) {
			!wrote_info
		} else if name == entry_name(pages + 1) && data.starts_with(b"\x89PNG") {
			pages += 1;
			true
		} else if pages > 0 && name == text_entry_name(pages) {
			true
		} else {
			// A gap or a stranger: what follows can't be numbered with confidence.
			break;
		};
		if !keep {
			continue;
		}
		if !wrote_info && !is_info(&name) {
			let title = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
			writer.start_file("ComicInfo.xml", page_options())?;
			writer.write_all(ScanArchive::info_xml(&title).as_bytes())?;
		}
		wrote_info = true;
		writer.start_file(name.as_str(), page_options())?;
		writer.write_all(&data)?;
	}
	if pages == 0 {
		return Ok(Repair::NothingToRecover);
	}
	// Windows won't rename over a file that's still open.
	drop(entries);
	finish(writer, path)?;
	tracing::info!(path = %path.display(), pages, "recovered a scan whose last page was cut short");
	Ok(Repair::Recovered(pages))
}

/// Local file headers from the start of the file, while each is whole: its name, and its data, inflated if it was deflated (as an earlier build stored `ComicInfo.xml`).
struct IntactEntries<R> {
	reader: R,
	at: u64,
	length: u64,
}

impl<R: Read + Seek> IntactEntries<R> {
	/// `Ok(None)` at the first entry that isn't whole; `Err` only when the file can't be read.
	fn next_entry(&mut self) -> io::Result<Option<(String, Vec<u8>)>> {
		const LOCAL_HEADER: u32 = 0x0403_4b50;
		const HEADER_LEN: u64 = 30;
		if self.at + HEADER_LEN > self.length {
			return Ok(None);
		}
		self.reader.seek(SeekFrom::Start(self.at))?;
		let mut header = [0u8; 30];
		self.reader.read_exact(&mut header)?;
		let u16_at = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]);
		let u32_at = |at: usize| u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]);
		if u32_at(0) != LOCAL_HEADER {
			return Ok(None);
		}
		let (flags, method, crc, packed, size) = (u16_at(6), u16_at(8), u32_at(14), u32_at(18), u32_at(22));
		let (name_len, extra_len) = (u64::from(u16_at(26)), u64::from(u16_at(28)));
		// Sizes after the data, or past four gigabytes, are never written by Paperback.
		if flags & 0x08 != 0 || packed == u32::MAX || size == u32::MAX {
			return Ok(None);
		}
		let data_start = self.at + HEADER_LEN + name_len + extra_len;
		let data_end = data_start + u64::from(packed);
		// Checked before anything is allocated: a header can claim more than the file holds.
		if data_end > self.length {
			return Ok(None);
		}
		let mut name = vec![0u8; usize::from(u16_at(26))];
		self.reader.read_exact(&mut name)?;
		self.reader.seek(SeekFrom::Start(data_start))?;
		let mut stored = Vec::new();
		(&mut self.reader).take(u64::from(packed)).read_to_end(&mut stored)?;
		let data = match method {
			0 => stored,
			8 => {
				let mut inflated = Vec::new();
				let limit = u64::from(size) + 1;
				if io::Read::take(flate2::read::DeflateDecoder::new(stored.as_slice()), limit)
					.read_to_end(&mut inflated)
					.is_err()
				{
					return Ok(None);
				}
				inflated
			}
			_ => return Ok(None),
		};
		// A header whose sizes were never filled in (the writer stopped before going back to them) checks as empty.
		if data.is_empty() || data.len() != size as usize || crc32fast::hash(&data) != crc {
			return Ok(None);
		}
		self.at = data_end;
		Ok(Some((String::from_utf8_lossy(&name).into_owned(), data)))
	}
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

/// Beside its image; comic readers, Paperback's comic parser among them, pass over entries that aren't images.
fn text_entry_name(number: usize) -> String {
	format!("page-{number:04}.ocr")
}

fn page_entries<'a>(number: usize, png: &'a [u8], text: &'a str) -> Vec<(String, &'a [u8])> {
	let mut entries = vec![(entry_name(number), png)];
	if !text.is_empty() {
		entries.push((text_entry_name(number), text.as_bytes()));
	}
	entries
}

/// Most text read from one archive's `.ocr` entries; a page's text is a few kilobytes.
const MAXIMUM_TEXT_TOTAL: u64 = 256 * 1024 * 1024;
/// Most text read from one page's `.ocr` entry.
const MAXIMUM_PAGE_TEXT: u64 = 16 * 1024 * 1024;

/// The lines recognized on each page, as the scan stored them beside its images. The page is the one the entry's name gives, and only pages the archive has count, so a crafted archive can't make Paperback read more than one entry per page or more than [`MAXIMUM_TEXT_TOTAL`] in all.
#[must_use]
pub fn recognized_pages(path: &Path) -> Vec<(i32, Vec<crate::ocr::RecognizedLine>)> {
	let Ok(file) = File::open(path) else { return Vec::new() };
	let Ok(mut archive) = ZipArchive::new(BufReader::new(file)) else { return Vec::new() };
	let page_count = page_names(&mut archive).len();
	let names: Vec<(String, usize)> = archive
		.file_names()
		.filter_map(Result::ok)
		.filter_map(|name| {
			let number = name.strip_prefix("page-")?.strip_suffix(".ocr")?.parse::<usize>().ok()?;
			(1..=page_count).contains(&number).then(|| (name.into_owned(), number))
		})
		.collect();
	let mut pages = Vec::new();
	let mut budget = MAXIMUM_TEXT_TOTAL;
	for (name, number) in names {
		if budget == 0 {
			break;
		}
		let Ok(entry) = archive.by_name(&name) else { continue };
		let mut text = Vec::new();
		if entry.take(MAXIMUM_PAGE_TEXT.min(budget)).read_to_end(&mut text).is_err() {
			continue;
		}
		budget = budget.saturating_sub(text.len() as u64);
		let Ok(page) = i32::try_from(number) else { continue };
		if let Some((_, lines)) = crate::ocr::decode_pages(text.as_slice()).into_iter().next() {
			pages.push((page, lines));
		}
	}
	pages
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
		archive.append(&png, "").unwrap();
		let after_one = fs::metadata(archive.path()).unwrap().len();
		for _ in 0..20 {
			archive.append(&png, "").unwrap();
		}
		let after_many = fs::metadata(archive.path()).unwrap().len();
		// Each page adds its own bytes and a little directory; nothing is written twice into the file.
		let per_page = (after_many - after_one) / 20;
		assert!(per_page < png.len() as u64 + 200, "{per_page} bytes a page for a {} byte page", png.len());
	}

	fn text(page: i32, words: &str) -> String {
		let line = crate::ocr::RecognizedLine { text: words.to_string(), words: vec![] };
		crate::ocr::encode_pages(&[(page, vec![line])]).unwrap()
	}

	fn texts(archive: &ScanArchive) -> Vec<(i32, String)> {
		recognized_pages(archive.path()).into_iter().map(|(page, lines)| (page, lines[0].text.clone())).collect()
	}

	#[test]
	fn each_page_carries_its_text_and_a_rescan_replaces_both() {
		let dir = tempfile::tempdir().unwrap();
		let mut archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		archive.append(&encode_png(&page(1)).unwrap(), &text(1, "first")).unwrap();
		archive.append(&encode_png(&page(2)).unwrap(), &text(2, "second")).unwrap();
		assert_eq!(texts(&archive), vec![(1, "first".to_string()), (2, "second".to_string())]);
		archive.replace(1, &encode_png(&page(10)).unwrap(), &text(1, "first again")).unwrap();
		assert_eq!(widths(&archive), vec![10, 2]);
		assert_eq!(texts(&archive), vec![(1, "first again".to_string()), (2, "second".to_string())]);
		// The comic parser sees only the images.
		let path = archive.path().to_string_lossy().into_owned();
		assert_eq!(DocumentSession::new(&path, "", "", ParseSettings::default()).unwrap().page_count(), 2);
	}

	/// What a crash leaves when it comes in the middle of adding a page: everything before the cut, with no central directory.
	fn cut_short(archive: &ScanArchive, keep: u64) {
		let file = OpenOptions::new().write(true).open(archive.path()).unwrap();
		file.set_len(keep).unwrap();
	}

	#[test]
	fn a_scan_cut_short_in_its_last_page_is_recovered_up_to_that_page() {
		let dir = tempfile::tempdir().unwrap();
		let mut archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		archive.append(&encode_png(&page(1)).unwrap(), &text(1, "one")).unwrap();
		archive.append(&encode_png(&page(2)).unwrap(), &text(2, "two")).unwrap();
		let before_third = fs::metadata(archive.path()).unwrap().len();
		archive.append(&encode_png(&DynamicImage::new_luma8(300, 300)).unwrap(), &text(3, "three")).unwrap();
		// Partway into the third page's image: the central directory and the rest of the page are gone.
		cut_short(&archive, before_third + 100);
		assert!(!opens(archive.path()));
		assert_eq!(repair(archive.path()).unwrap(), Repair::Recovered(2));
		assert!(opens(archive.path()));
		assert_eq!(widths(&archive), vec![1, 2]);
		assert_eq!(texts(&archive), vec![(1, "one".to_string()), (2, "two".to_string())]);
		assert!(is_scan(archive.path()), "the title and the scan mark survive");
		assert_eq!(ScanArchive::open(archive.path()).unwrap().page_count(), 2);
	}

	#[test]
	fn a_scan_cut_short_only_in_its_directory_keeps_every_page() {
		let dir = tempfile::tempdir().unwrap();
		let mut archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		for width in 1..=3 {
			archive.append(&encode_png(&page(width)).unwrap(), &text(i32::try_from(width).unwrap(), "words")).unwrap();
		}
		let len = fs::metadata(archive.path()).unwrap().len();
		cut_short(&archive, len - 30);
		assert_eq!(repair(archive.path()).unwrap(), Repair::Recovered(3));
		assert_eq!(widths(&archive), vec![1, 2, 3]);
	}

	#[test]
	fn a_page_whose_header_was_never_finished_is_not_recovered() {
		let dir = tempfile::tempdir().unwrap();
		let mut archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		archive.append_page(&page(1)).unwrap();
		// The next page would start where the central directory does.
		let mut bytes = fs::read(archive.path()).unwrap();
		let directory = bytes.windows(4).position(|window| window == [0x50, 0x4b, 0x01, 0x02]).unwrap();
		bytes.truncate(directory);
		// A local header with its sizes and checksum still zero, as the writer leaves it before going back to fill them in.
		bytes.extend_from_slice(&[
			0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 13, 0, 0, 0,
		]);
		bytes.extend_from_slice(b"page-0002.png\x89PNG half a page");
		fs::write(archive.path(), bytes).unwrap();
		assert_eq!(repair(archive.path()).unwrap(), Repair::Recovered(1));
		assert_eq!(widths(&archive), vec![1]);
	}

	#[test]
	fn nothing_to_recover_leaves_the_file_alone() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("Scan.cbz");
		fs::write(&path, b"not a zip at all").unwrap();
		assert_eq!(repair(&path).unwrap(), Repair::NothingToRecover);
		assert_eq!(fs::read(&path).unwrap(), b"not a zip at all");
	}

	#[test]
	fn a_repair_that_cannot_read_the_file_is_an_error_not_nothing_to_recover() {
		let dir = tempfile::tempdir().unwrap();
		assert!(repair(&dir.path().join("missing.cbz")).is_err());
		// A folder where a scan should be can't be read as one either.
		let folder = dir.path().join("Scan.cbz");
		fs::create_dir(&folder).unwrap();
		assert!(repair(&folder).is_err());
	}

	/// A file that takes `budget` more bytes and then reports a full disk.
	struct FullDisk {
		file: File,
		budget: usize,
	}

	impl Read for FullDisk {
		fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
			self.file.read(buf)
		}
	}

	impl Seek for FullDisk {
		fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
			self.file.seek(to)
		}
	}

	impl Write for FullDisk {
		fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
			if self.budget == 0 {
				return Err(io::Error::new(io::ErrorKind::StorageFull, "no space left on device"));
			}
			let take = buf.len().min(self.budget);
			let written = self.file.write(&buf[..take])?;
			self.budget -= written;
			Ok(written)
		}

		fn flush(&mut self) -> io::Result<()> {
			self.file.flush()
		}
	}

	#[test]
	fn an_append_that_fails_partway_leaves_the_archive_as_it_was() {
		let dir = tempfile::tempdir().unwrap();
		let mut archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		archive.append(&encode_png(&page(1)).unwrap(), &text(1, "one")).unwrap();
		archive.append(&encode_png(&page(2)).unwrap(), &text(2, "two")).unwrap();
		let before = fs::read(archive.path()).unwrap();
		let big = encode_png(&DynamicImage::new_luma8(300, 300)).unwrap();
		for budget in [0, 10, 40, 100, big.len() / 2, big.len() + 60] {
			let failed = archive.append_through(&big, &text(3, "three"), |file| Ok(FullDisk { file, budget }));
			assert!(failed.is_err(), "budget {budget}");
			assert_eq!(fs::read(archive.path()).unwrap(), before, "budget {budget}: the archive is as it was");
			assert_eq!(archive.page_count(), 2, "budget {budget}: the page isn't counted");
		}
		// The next page takes the number the failed one would have had, with its own text.
		assert_eq!(archive.append(&encode_png(&page(3)).unwrap(), &text(3, "three")).unwrap(), 3);
		assert_eq!(widths(&archive), vec![1, 2, 3]);
		assert_eq!(texts(&archive), vec![(1, "one".to_string()), (2, "two".to_string()), (3, "three".to_string())]);
	}

	#[test]
	fn a_deleted_scan_is_not_brought_back_by_a_late_page() {
		let dir = tempfile::tempdir().unwrap();
		let mut archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		archive.append_page(&page(1)).unwrap();
		fs::remove_file(archive.path()).unwrap();
		assert!(archive.append_page(&page(2)).is_err());
		assert!(!archive.path().exists());
	}

	#[test]
	fn text_is_read_only_for_pages_the_archive_has_and_by_the_entry_s_name() {
		let dir = tempfile::tempdir().unwrap();
		let mut archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		archive.append(&encode_png(&page(1)).unwrap(), &text(7, "first")).unwrap();
		let mut zip =
			ZipWriter::new_append(OpenOptions::new().read(true).write(true).open(archive.path()).unwrap()).unwrap();
		zip.start_file("page-0009.ocr", page_options()).unwrap();
		zip.write_all(text(9, "stranger").as_bytes()).unwrap();
		zip.finish().unwrap();
		// Page 1's entry said page 7 inside; the name decides. Page 9 doesn't exist.
		assert_eq!(texts(&archive), vec![(1, "first".to_string())]);
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
