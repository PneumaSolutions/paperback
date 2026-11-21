//! Where scans in progress live, and how they go away. The `scans` folder is a workspace, not a library: a scan stays there while it's open, Save Scan As puts a copy where the reader wants to keep it, and a scan closed without saving is deleted with its recognized text. One left behind by a crash or by quitting with the scan open is restored with the other documents, or else cleared at the next start.

use std::{
	fs,
	path::{Path, PathBuf},
};

use paperback_core::{
	config::ConfigManager,
	ocr::{load_pages, save_pages, saved_pages_file},
	scan::archive::ScanArchive,
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
	ScanArchive::open(source).copy_to(destination, &title)?;
	let text = saved_pages_file(&saved_pages_dir(), destination);
	let _ = fs::remove_file(&text);
	save_pages(&text, &lines)?;
	Ok(())
}

/// At start, after the documents left open have been restored: every scan in progress that isn't open is one nobody can reach any more.
pub fn sweep(open: &[PathBuf], config: &ConfigManager) {
	let Ok(entries) = fs::read_dir(scans_dir()) else { return };
	for path in entries.filter_map(Result::ok).map(|entry| entry.path()) {
		let stale = path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("cbz"))
			&& !open.iter().any(|open| same_file(open, &path));
		let leftover = path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("tmp"));
		if stale {
			discard(&path, config);
		} else if leftover {
			let _ = fs::remove_file(&path);
		}
	}
}

#[must_use]
pub fn same_file(a: &Path, b: &Path) -> bool {
	let canonical = |path: &Path| dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
	canonical(a) == canonical(b)
}

#[cfg(test)]
mod tests {
	use super::*;

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
