use std::sync::atomic::{AtomicBool, Ordering};

use pagescan::{ScanOptions, SourceChoice, SourceKind};

use super::{
	ColorMode, DynamicImage, ErrorKind, ScanError, ScanSettings, ScanSummary, Scanner, ScannerInfo, Source, TIMEOUT,
};

/// The platform's own scanners, through pagescan.
#[derive(Default)]
pub struct SystemScanner {
	cancelled: AtomicBool,
}

impl SystemScanner {
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}
}

impl Scanner for SystemScanner {
	fn list(&self) -> Result<Vec<ScannerInfo>, ScanError> {
		let devices = pagescan::list_scanners().map_err(|error| convert(&error, false))?;
		Ok(devices.into_iter().map(info).collect())
	}

	fn scan(&self, scanner: &str, settings: ScanSettings, on_page: &mut dyn FnMut(DynamicImage)) -> ScanSummary {
		let options = options(scanner, settings);
		let mut pages = 0;
		// pagescan forgets a cancel made just before its scan starts, so one made since `prepare` is passed on again as soon as the scan reports anything.
		let pass_on_cancel = || {
			if self.cancelled.load(Ordering::Acquire) {
				pagescan::cancel();
			}
		};
		// Streamed: each page is handed over by value and pagescan keeps none of them.
		let outcome = pagescan::scan_with_pages(
			&options,
			|_| pass_on_cancel(),
			|_, page| {
				pages += 1;
				on_page(page);
				pass_on_cancel();
			},
		);
		let error = outcome.error.map(|error| convert(&error, self.cancelled.load(Ordering::Acquire)));
		ScanSummary { pages, error }
	}

	fn cancel(&self) {
		self.cancelled.store(true, Ordering::Release);
		pagescan::cancel();
	}

	fn prepare(&self) {
		self.cancelled.store(false, Ordering::Release);
	}

	fn needs_main_thread(&self) -> bool {
		cfg!(target_os = "macos")
	}
}

fn options(scanner: &str, settings: ScanSettings) -> ScanOptions {
	let mut options = ScanOptions::new(scanner);
	options.source = if settings.source.is_feeder() { SourceChoice::Feeder } else { SourceChoice::Flatbed };
	options.duplex = settings.source == Source::FeederDuplex;
	options.dpi = settings.dpi;
	options.color = match settings.color {
		ColorMode::Gray => pagescan::ColorMode::Gray,
		ColorMode::Color => pagescan::ColorMode::Color,
	};
	options.timeout = TIMEOUT;
	// On macOS the scan runs on the main thread; this keeps the Scanning window answering, so Stop can be pressed.
	options.app_events = cfg!(target_os = "macos");
	options
}

fn info(device: pagescan::Device) -> ScannerInfo {
	let mut sources = Vec::new();
	if device.sources.iter().any(|source| source.kind == SourceKind::Flatbed) {
		sources.push(Source::Flatbed);
	}
	if device.sources.iter().any(|source| source.kind == SourceKind::Feeder) {
		sources.push(Source::Feeder);
	}
	if device.sources.iter().any(|source| source.kind == SourceKind::Feeder && source.duplex) {
		sources.push(Source::FeederDuplex);
	}
	ScannerInfo { id: device.id, name: device.name, sources, problem: device.error }
}

fn convert(error: &pagescan::ScanError, cancelled: bool) -> ScanError {
	let kind = if error.kind == pagescan::ErrorKind::SaneMissing {
		ErrorKind::SaneMissing
	} else if cancelled {
		ErrorKind::Cancelled
	} else {
		ErrorKind::Other
	};
	ScanError { kind, message: error.message.clone() }
}

#[cfg(test)]
mod tests {
	use super::*;

	fn device(sources: &[(SourceKind, bool)]) -> pagescan::Device {
		pagescan::Device {
			id: "id".into(),
			name: "Scanner".into(),
			sources: sources
				.iter()
				.map(|(kind, duplex)| pagescan::Source { id: "x".into(), kind: *kind, duplex: *duplex })
				.collect(),
			error: None,
		}
	}

	#[test]
	fn a_two_sided_feeder_offers_both_feeder_choices() {
		let info = info(device(&[(SourceKind::Flatbed, false), (SourceKind::Feeder, true)]));
		assert_eq!(info.sources, vec![Source::Flatbed, Source::Feeder, Source::FeederDuplex]);
	}

	#[test]
	fn a_flatbed_only_scanner_offers_only_the_flatbed() {
		assert_eq!(info(device(&[(SourceKind::Flatbed, false)])).sources, vec![Source::Flatbed]);
	}

	#[test]
	fn settings_become_pagescan_options() {
		let settings = ScanSettings { source: Source::FeederDuplex, dpi: 200, color: ColorMode::Gray };
		let options = options("dev", settings);
		assert_eq!(options.device, "dev");
		assert_eq!(options.source, SourceChoice::Feeder);
		assert!(options.duplex);
		assert_eq!(options.dpi, 200);
		assert_eq!(options.color, pagescan::ColorMode::Gray);
		let flatbed = options_for(Source::Flatbed);
		assert_eq!(flatbed.source, SourceChoice::Flatbed);
		assert!(!flatbed.duplex);
	}

	fn options_for(source: Source) -> ScanOptions {
		options("dev", ScanSettings { source, ..ScanSettings::default() })
	}

	#[test]
	fn missing_sane_keeps_its_kind_and_a_cancel_is_recognized() {
		let missing = pagescan::ScanError::new("no sane").with_kind(pagescan::ErrorKind::SaneMissing);
		assert_eq!(convert(&missing, false).kind, ErrorKind::SaneMissing);
		let stopped = pagescan::ScanError::new("The scan was cancelled.");
		assert_eq!(convert(&stopped, true).kind, ErrorKind::Cancelled);
		assert_eq!(convert(&stopped, false).kind, ErrorKind::Other);
	}
}

/// These run only with `PAPERBACK_SANE_TEST=1` on Linux, with SANE installed and `SANE_CONFIG_DIR` pointing at a `dll.conf` that lists `test`, as the Linux CI job sets up. SANE's test scanner has a flatbed and a 10-sheet feeder.
#[cfg(all(test, target_os = "linux"))]
mod sane_test_backend {
	use super::*;
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
		let summary = scanner.scan("test:0", settings(Source::Flatbed), &mut |page| widths.push(page.width()));
		assert!(summary.error.is_none(), "{:?}", summary.error);
		assert_eq!(summary.pages, 1);
		assert_eq!(widths.len(), 1);
		let mut pages = 0;
		let summary = scanner.scan("test:0", settings(Source::Feeder), &mut |page| {
			assert!(matches!(page, DynamicImage::ImageLuma8(_)));
			pages += 1;
		});
		assert!(summary.error.is_none(), "{:?}", summary.error);
		assert_eq!((summary.pages, pages), (10, 10));
	}
}
