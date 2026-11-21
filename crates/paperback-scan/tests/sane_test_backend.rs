//! Runs only with `PAPERBACK_SANE_TEST=1` on Linux, with SANE installed and `SANE_CONFIG_DIR` pointing at a `dll.conf` that lists `test`, as the Linux CI job sets up. SANE's test scanner has a flatbed and a 10-sheet feeder.

#![cfg(target_os = "linux")]

use paperback_scan::{ColorMode, ScanSettings, Scanner, Source, SystemScanner};

fn enabled() -> bool {
	std::env::var_os("PAPERBACK_SANE_TEST").is_some()
}

fn settings(source: Source) -> ScanSettings {
	ScanSettings { source, dpi: 150, color: ColorMode::Gray }
}

#[test]
fn the_test_scanner_is_listed_with_its_flatbed_and_feeder() {
	if !enabled() {
		return;
	}
	let scanners = SystemScanner::new().list().unwrap();
	let test = scanners.iter().find(|info| info.id == "test:0").expect("SANE's test scanner is listed");
	assert_eq!(test.sources, vec![Source::Flatbed, Source::Feeder]);
}

#[test]
fn a_flatbed_scan_gives_one_gray_page_and_a_feeder_scan_every_sheet() {
	if !enabled() {
		return;
	}
	let scanner = SystemScanner::new();
	let mut widths = Vec::new();
	let summary = scanner.scan("test:0", &settings(Source::Flatbed), &mut |page| widths.push(page.width()));
	assert!(summary.error.is_none(), "{:?}", summary.error);
	assert_eq!(summary.pages, 1);
	assert_eq!(widths.len(), 1);
	let mut pages = 0;
	let summary = scanner.scan("test:0", &settings(Source::Feeder), &mut |page| {
		assert!(matches!(page, paperback_scan::DynamicImage::ImageLuma8(_)));
		pages += 1;
	});
	assert!(summary.error.is_none(), "{:?}", summary.error);
	assert_eq!((summary.pages, pages), (10, 10));
}
