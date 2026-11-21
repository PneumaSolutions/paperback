//! Scanned pages are kept as PNG files in a zip, the `.pbscan` format, which [`crate::parser::scan`] reads like a comic archive.
//! Every change writes a whole new archive beside the old one and renames it over it, so a reader opening the file mid-scan, or a crash mid-write, never sees half a page.

use std::{
	fs::{self, File},
	io::{self, BufReader, BufWriter, Cursor, Write as _},
	path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use image::{DynamicImage, ImageFormat};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::parser::cbz::page_names;

pub const EXTENSION: &str = "pbscan";

/// Far past any feeder's capacity; the name keeps a natural sort even past it, but a limit stops a runaway driver filling the disk.
pub const MAXIMUM_PAGES: usize = 9999;

pub struct ScanArchive {
	path: PathBuf,
}

impl ScanArchive {
	/// Picks a name in `dir` that no file has yet, `name.pbscan` or `name 2.pbscan` and so on. Nothing is written until the first page.
	pub fn create(dir: &Path, name: &str) -> io::Result<Self> {
		create_private_dir(dir)?;
		let stem = sanitize(name);
		let mut path = dir.join(format!("{stem}.{EXTENSION}"));
		let mut counter = 2;
		while path.exists() {
			path = dir.join(format!("{stem} {counter}.{EXTENSION}"));
			counter += 1;
		}
		Ok(Self { path })
	}

	#[must_use]
	pub fn open(path: &Path) -> Self {
		Self { path: path.to_path_buf() }
	}

	#[must_use]
	pub fn path(&self) -> &Path {
		&self.path
	}

	pub fn page_count(&self) -> Result<usize> {
		if !self.path.exists() {
			return Ok(0);
		}
		Ok(page_names(&mut self.read()?).len())
	}

	/// Returns the new page's number, counting from 1.
	pub fn append_page(&self, page: &DynamicImage) -> Result<usize> {
		self.append_png(&encode_png(page)?)
	}

	pub fn append_png(&self, png: &[u8]) -> Result<usize> {
		let existing = self.page_count()?;
		if existing >= MAXIMUM_PAGES {
			anyhow::bail!("a scanned document holds at most {MAXIMUM_PAGES} pages");
		}
		let number = existing + 1;
		self.rewrite(None, Some((entry_name(number), png)))?;
		Ok(number)
	}

	/// For a page that read better turned around: the image the reader's OCR and any later Batch OCR see is the upright one.
	pub fn replace_page(&self, number: usize, page: &DynamicImage) -> Result<()> {
		self.replace_png(number, &encode_png(page)?)
	}

	/// Also for a page scanned again because it wasn't lined up.
	pub fn replace_png(&self, number: usize, png: &[u8]) -> Result<()> {
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
		// PNG is compressed already, so deflating it again only costs time.
		let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
		let mut added = added;
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
		let temp = writer.finish()?.into_inner().map_err(io::IntoInnerError::into_error)?;
		temp.as_file().sync_all()?;
		temp.persist(&self.path).map_err(|error| error.error)?;
		Ok(())
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
		let archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		assert_eq!(archive.page_count().unwrap(), 0);
		assert!(!archive.path().exists(), "nothing is written before the first page");
		assert_eq!(archive.append_page(&page(1)).unwrap(), 1);
		assert_eq!(archive.append_page(&page(2)).unwrap(), 2);
		assert_eq!(archive.append_page(&page(3)).unwrap(), 3);
		assert_eq!(widths(&archive), vec![1, 2, 3]);
	}

	#[test]
	fn a_replaced_page_keeps_its_place() {
		let dir = tempfile::tempdir().unwrap();
		let archive = ScanArchive::create(dir.path(), "Scan").unwrap();
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
		let first = ScanArchive::create(dir.path(), "Scan").unwrap();
		first.append_page(&page(1)).unwrap();
		let second = ScanArchive::create(dir.path(), "Scan").unwrap();
		assert_ne!(first.path(), second.path());
		assert!(second.path().to_string_lossy().ends_with("Scan 2.pbscan"));
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
		let archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		archive.append_page(&page(1)).unwrap();
		archive.replace_page(1, &page(2)).unwrap();
		let names: Vec<String> = fs::read_dir(dir.path())
			.unwrap()
			.map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
			.collect();
		assert_eq!(names, vec!["Scan.pbscan".to_string()]);
	}

	#[cfg(unix)]
	#[test]
	fn the_scans_folder_and_files_are_private() {
		use std::os::unix::fs::PermissionsExt as _;
		let dir = tempfile::tempdir().unwrap();
		let scans = dir.path().join("scans");
		let archive = ScanArchive::create(&scans, "Scan").unwrap();
		archive.append_page(&page(1)).unwrap();
		assert_eq!(fs::metadata(&scans).unwrap().permissions().mode() & 0o777, 0o700);
		assert_eq!(fs::metadata(archive.path()).unwrap().permissions().mode() & 0o077, 0);
	}
}
