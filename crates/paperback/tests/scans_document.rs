#![cfg(target_os = "windows")]

//! Needs a real scanner with a printed page on its glass, so it only runs with `PAPERBACK_SCANNER_TEST=1` and isn't in CI's list of UI tests.

mod common;

use std::time::Duration;

use common::Notifications;

#[test]
#[ignore = "drives the real GUI via UI Automation and a real scanner; needs an interactive desktop and PAPERBACK_SCANNER_TEST=1"]
fn a_flatbed_page_is_scanned_read_and_opened_as_a_document() {
	if std::env::var_os("PAPERBACK_SCANNER_TEST").is_none() {
		return;
	}
	let app = common::launch("scans-document");
	common::wait_for_reader_focus(app.pid);
	let notifications = Notifications::listen(app.pid);
	common::alt('F');
	std::thread::sleep(Duration::from_millis(500));
	common::press('S');
	// The first time, the scanners are listed before the settings appear.
	assert!(common::window_appears(app.pid, "Scan Document", Duration::from_secs(60)), "no Scan Document dialog");
	std::thread::sleep(Duration::from_secs(1));
	common::enter();
	assert!(common::window_appears(app.pid, "Scanning", Duration::from_secs(20)), "no Scanning window: {}", app.logs());
	let read = notifications.wait_for_prefix("Page 1:", Duration::from_secs(180));
	assert!(read.contains("word"), "{read}");
	common::escape();
	common::wait_for_reader_focus(app.pid);
	common::wait_for_title(app.pid, "Scan - Paperback");
	let text = common::reader_text(app.pid).unwrap_or_default();
	assert!(!text.contains("[Image only"), "the page's text never arrived: {text}");
	let scans = app.config_dir().join("scans");
	assert!(scans.join("Scan.pbscan").exists(), "no archive in {}", scans.display());
}
