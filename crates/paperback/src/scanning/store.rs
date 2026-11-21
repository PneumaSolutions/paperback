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

/// Save Scan As: a copy at `destination`, titled by its file name, with its recognized text, so it opens without OCR. The text is kept by the archive's contents, which the new title changes, so it's copied across. Never into the scans folder: a copy there would still count as a scan in progress, and a copy onto the scan itself would be deleted with it.
pub fn save_copy(source: &Path, destination: &Path) -> anyhow::Result<()> {
	copy_outside(source, destination, &scans_dir())
}

fn copy_outside(source: &Path, destination: &Path, scans: &Path) -> anyhow::Result<()> {
	if is_inside(destination, scans) {
		anyhow::bail!(
			"{}",
			// TRANSLATORS: Shown when Save Scan As is pointed at the folder Paperback keeps unsaved scans in
			patois::t("that folder is where Paperback keeps scans in progress. Choose another folder.")
		);
	}
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
	pub state: Recovery,
}

/// What became of a scan in progress that was checked at start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
	/// It opened as it was.
	Intact,
	/// A crash had cut it short, and its complete pages were recovered.
	Repaired,
	/// It doesn't open and couldn't be repaired just now (locked by another program, a read error, a full disk): it's kept as it is, to try again next time.
	Unreadable,
	/// Not one complete page in it. Only this may be deleted unasked.
	Empty,
}

/// A scan in progress that doesn't open is repaired. Only a file with truly no complete page in it is [`Recovery::Empty`]; any error along the way leaves the file alone as [`Recovery::Unreadable`].
#[must_use]
pub fn recover(path: &Path) -> Recovery {
	if archive::opens(path) {
		return Recovery::Intact;
	}
	match archive::repair(path) {
		Ok(archive::Repair::Recovered(_)) => Recovery::Repaired,
		Ok(archive::Repair::NothingToRecover) => Recovery::Empty,
		Err(error) => {
			tracing::warn!(path = %path.display(), error = format!("{error:#}"), "a scan in progress could not be checked; keeping it");
			Recovery::Unreadable
		}
	}
}

/// The scans in progress in `dir` no tab has open, repaired where need be, as the start-up offer gathers them ([`scans_left`], [`recover`] off the UI thread, then [`clear_empty`]). Only a file with not one complete page in it is deleted, and stray temporary files from an interrupted write. One that couldn't be read or repaired just now is kept and listed as [`Recovery::Unreadable`].
#[cfg(test)]
fn unsaved_in(dir: &Path, open: &[PathBuf], config: &ConfigManager) -> Vec<Unsaved> {
	let checked = left_in(dir, open)
		.into_iter()
		.map(|(path, opens)| {
			let state = if opens { Recovery::Intact } else { recover(&path) };
			Unsaved { path, state }
		})
		.collect();
	clear_empty(checked, config)
}

/// The scans in progress no tab has open, and whether each opens as it is; the ones that don't need [`recover`], which copies the whole archive, so the caller can do that off the UI thread. Stray temporary files from an interrupted write are removed.
#[must_use]
pub fn scans_left(open: &[PathBuf]) -> Vec<(PathBuf, bool)> {
	left_in(&scans_dir(), open)
}

fn left_in(dir: &Path, open: &[PathBuf]) -> Vec<(PathBuf, bool)> {
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
			Some("cbz") if !open.iter().any(|open| same_file(open, &path)) => {
				let opens = archive::opens(&path);
				found.push((path, opens));
			}
			_ => {}
		}
	}
	found
}

/// Deletes the scans with not one complete page in them, and returns the rest.
pub fn clear_empty(checked: Vec<Unsaved>, config: &ConfigManager) -> Vec<Unsaved> {
	checked
		.into_iter()
		.filter(|scan| {
			let empty = scan.state == Recovery::Empty;
			if empty {
				discard(&scan.path, config);
			}
			!empty
		})
		.collect()
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
				Unsaved { path: crashed.clone(), state: Recovery::Repaired },
				Unsaved { path: intact.clone(), state: Recovery::Intact }
			]
		);
		assert!(archive::opens(&crashed), "the crashed scan was repaired in place");
		assert!(intact.exists() && open.exists());
		assert!(!empty.exists(), "a file with not one complete page is cleared");
		assert!(!leftover.exists());
	}

	#[test]
	fn a_scan_that_cannot_be_repaired_just_now_is_kept_not_deleted() {
		let dir = tempfile::tempdir().unwrap();
		// Something named like a scan that can't be read: a folder stands in for a file another program has locked.
		let locked = dir.path().join("Locked.cbz");
		fs::create_dir(&locked).unwrap();
		assert_eq!(recover(&locked), Recovery::Unreadable);
		let found = unsaved_in(dir.path(), &[], &ConfigManager::new());
		assert_eq!(found, vec![Unsaved { path: locked.clone(), state: Recovery::Unreadable }]);
		assert!(locked.exists());
	}

	#[cfg(unix)]
	#[test]
	fn a_repair_that_cannot_write_its_copy_keeps_the_scan() {
		use std::os::unix::fs::PermissionsExt as _;
		let dir = tempfile::tempdir().unwrap();
		let crashed = scan(dir.path(), "Crashed", 3);
		let length = fs::metadata(&crashed).unwrap().len();
		OpenOptions::new().write(true).open(&crashed).unwrap().set_len(length - 40).unwrap();
		let before = fs::read(&crashed).unwrap();
		// As on a full disk: the repaired copy can't be written beside it.
		fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
		let writable = tempfile::tempfile_in(dir.path()).is_ok();
		let state = recover(&crashed);
		fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
		if writable {
			// Running as root: permissions don't stop the copy, so there's nothing to show here.
			return;
		}
		assert_eq!(state, Recovery::Unreadable);
		assert_eq!(fs::read(&crashed).unwrap(), before, "left exactly as it was");
	}

	#[test]
	fn a_scan_is_never_saved_into_the_scans_folder() {
		let dir = tempfile::tempdir().unwrap();
		let source = scan(dir.path(), "Scan", 1);
		let scans = dir.path();
		assert!(copy_outside(&source, &scans.join("Kept.cbz"), scans).is_err());
		assert!(!scans.join("Kept.cbz").exists());
		assert!(copy_outside(&source, &source, scans).is_err(), "nor onto the scan itself");
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
