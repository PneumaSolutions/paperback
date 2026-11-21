//! Where scans in progress live, and how they go away. The `scans` folder is a workspace, not a library: a scan stays there while it's open, Save Scan As puts a copy where the reader wants to keep it, and a scan closed without saving is deleted with its recognized text, after the reader is asked. One left behind by quitting or a crash is never deleted unasked: it's reopened with the other documents, or offered at the next start, and repaired first if a crash cut its last page short.

use std::{
	fs,
	path::{Path, PathBuf},
};

use paperback_core::{
	config::ConfigManager,
	ocr::{load_pages, save_pages, saved_pages_file},
	scan::archive::{self, ScanArchive},
};

use crate::{config_ext::config_dir, ocr::saved_pages_dir};

#[must_use]
pub fn scans_dir() -> PathBuf {
	config_dir().join("scans")
}

#[must_use]
pub fn is_in_progress(path: &Path) -> bool {
	is_inside(path, &scans_dir())
}

fn is_inside(path: &Path, dir: &Path) -> bool {
	let canonical = |path: &Path| dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
	path.parent().is_some_and(|parent| canonical(parent) == canonical(dir))
}

/// The archive, its recognized text, and its place in Recent Documents. The text is found by the archive's contents, so it's looked up before the archive goes.
pub fn discard(path: &Path, config: &ConfigManager) {
	let text = saved_pages_file(&saved_pages_dir(), path);
	if let Err(error) = fs::remove_file(path) {
		tracing::warn!(path = %path.display(), %error, "could not delete a scan in progress");
	}
	let _ = fs::remove_file(text);
	config.remove_document_history(&path.to_string_lossy());
	config.flush();
	tracing::info!(path = %path.display(), "discarded a scan in progress");
}

/// Save Scan As: a copy at `destination`, titled by its file name, with its recognized text, so it opens without OCR. The text is kept by the archive's contents, which the new title changes, so it's copied across.
pub fn save_copy(source: &Path, destination: &Path) -> anyhow::Result<()> {
	let title = destination.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
	let lines = load_pages(&saved_pages_file(&saved_pages_dir(), source));
	ScanArchive::open(source)?.copy_to(destination, &title)?;
	let text = saved_pages_file(&saved_pages_dir(), destination);
	let _ = fs::remove_file(&text);
	save_pages(&text, &lines)?;
	Ok(())
}

/// A scan in progress found at start that no tab has open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsaved {
	pub path: PathBuf,
	/// It had been cut short by a crash and was repaired.
	pub recovered: bool,
}

/// A scan in progress that doesn't open is repaired. `Some(true)` if it was, `Some(false)` if it opened as it was, `None` if not one page could be saved from it.
#[must_use]
pub fn recover(path: &Path) -> Option<bool> {
	if archive::opens(path) {
		return Some(false);
	}
	match archive::repair(path) {
		Ok(_) => Some(true),
		Err(error) => {
			tracing::warn!(path = %path.display(), %error, "a scan in progress could not be recovered");
			None
		}
	}
}

/// At start, after the documents left open have been restored: the scans in progress no tab has open, repaired where need be. Only a file with not one complete page in it is deleted, and stray temporary files from an interrupted write.
pub fn unsaved_scans(open: &[PathBuf], config: &ConfigManager) -> Vec<Unsaved> {
	unsaved_in(&scans_dir(), open, config)
}

fn unsaved_in(dir: &Path, open: &[PathBuf], config: &ConfigManager) -> Vec<Unsaved> {
	let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
	let mut found = Vec::new();
	let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|entry| entry.path()).collect();
	paths.sort();
	for path in paths {
		let extension = path.extension().map(|extension| extension.to_string_lossy().to_ascii_lowercase());
		match extension.as_deref() {
			Some("tmp") => {
				let _ = fs::remove_file(&path);
			}
			Some("cbz") if !open.iter().any(|open| same_file(open, &path)) => match recover(&path) {
				Some(recovered) => found.push(Unsaved { path, recovered }),
				None => discard(&path, config),
			},
			_ => {}
		}
	}
	found
}

#[must_use]
pub fn same_file(a: &Path, b: &Path) -> bool {
	let canonical = |path: &Path| dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
	canonical(a) == canonical(b)
}

#[cfg(test)]
mod tests {
	use std::fs::OpenOptions;

	use image::DynamicImage;

	use super::*;

	fn scan(dir: &Path, name: &str, pages: u32) -> PathBuf {
		let mut archive = ScanArchive::create(dir, name).unwrap();
		for width in 1..=pages {
			archive.append_page(&DynamicImage::new_luma8(width, 2)).unwrap();
		}
		archive.path().to_path_buf()
	}

	#[test]
	fn scans_left_from_last_time_are_kept_and_repaired_never_deleted() {
		let dir = tempfile::tempdir().unwrap();
		let intact = scan(dir.path(), "Intact", 2);
		let crashed = scan(dir.path(), "Crashed", 3);
		let length = fs::metadata(&crashed).unwrap().len();
		OpenOptions::new().write(true).open(&crashed).unwrap().set_len(length - 40).unwrap();
		let open = scan(dir.path(), "Open", 1);
		let empty = dir.path().join("Empty.cbz");
		fs::write(&empty, b"nothing recoverable").unwrap();
		let leftover = dir.path().join(".scan-123.tmp");
		fs::write(&leftover, b"half written").unwrap();
		let config = ConfigManager::new();
		let found = unsaved_in(dir.path(), std::slice::from_ref(&open), &config);
		assert_eq!(
			found,
			vec![
				Unsaved { path: crashed.clone(), recovered: true },
				Unsaved { path: intact.clone(), recovered: false }
			]
		);
		assert!(archive::opens(&crashed), "the crashed scan was repaired in place");
		assert!(intact.exists() && open.exists());
		assert!(!empty.exists(), "a file with not one complete page is cleared");
		assert!(!leftover.exists());
	}

	#[test]
	fn only_files_directly_in_the_folder_count_as_inside_it() {
		let dir = tempfile::tempdir().unwrap();
		let scans = dir.path().join("scans");
		fs::create_dir_all(scans.join("deeper")).unwrap();
		assert!(is_inside(&scans.join("Scan.cbz"), &scans));
		assert!(!is_inside(&scans.join("deeper").join("Scan.cbz"), &scans));
		assert!(!is_inside(&dir.path().join("Scan.cbz"), &scans));
	}
}
