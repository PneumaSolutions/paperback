//! A scanner that hands back pages it was given, for tests of everything above the scanner library.

use std::{
	collections::VecDeque,
	sync::{
		Mutex,
		atomic::{AtomicBool, Ordering},
	},
};

use crate::{DynamicImage, ErrorKind, ScanError, ScanSettings, ScanSummary, Scanner, ScannerInfo};

/// What one call to [`Scanner::scan`] produces.
#[derive(Debug, Clone, Default)]
pub struct Script {
	pub pages: Vec<DynamicImage>,
	/// Reported after the pages, as a jam or an empty feeder would be.
	pub error: Option<ScanError>,
}

impl Script {
	#[must_use]
	pub fn pages(pages: Vec<DynamicImage>) -> Self {
		Self { pages, error: None }
	}

	#[must_use]
	pub fn failing(pages: Vec<DynamicImage>, message: &str) -> Self {
		Self { pages, error: Some(ScanError { kind: ErrorKind::Other, message: message.to_string() }) }
	}
}

pub struct FakeScanner {
	scanners: Result<Vec<ScannerInfo>, ScanError>,
	scripts: Mutex<VecDeque<Script>>,
	calls: Mutex<Vec<(String, ScanSettings)>>,
	cancelled: AtomicBool,
	/// Cancels itself after handing over this many pages, as a reader pressing Stop mid-feed would.
	cancel_after: Option<usize>,
}

impl FakeScanner {
	#[must_use]
	pub fn new(scanners: Vec<ScannerInfo>) -> Self {
		Self::listing(Ok(scanners))
	}

	fn listing(scanners: Result<Vec<ScannerInfo>, ScanError>) -> Self {
		Self {
			scanners,
			scripts: Mutex::new(VecDeque::new()),
			calls: Mutex::new(Vec::new()),
			cancelled: AtomicBool::new(false),
			cancel_after: None,
		}
	}

	/// Listing fails, as it does on Linux without SANE.
	#[must_use]
	pub fn unavailable(error: ScanError) -> Self {
		Self::listing(Err(error))
	}

	#[must_use]
	pub fn with_script(self, script: Script) -> Self {
		self.scripts.lock().unwrap().push_back(script);
		self
	}

	#[must_use]
	pub const fn cancelling_after(mut self, pages: usize) -> Self {
		self.cancel_after = Some(pages);
		self
	}

	/// Each scan's scanner and settings, in order.
	pub fn calls(&self) -> Vec<(String, ScanSettings)> {
		self.calls.lock().unwrap().clone()
	}
}

impl Scanner for FakeScanner {
	fn list(&self) -> Result<Vec<ScannerInfo>, ScanError> {
		self.scanners.clone()
	}

	fn scan(&self, scanner: &str, settings: &ScanSettings, on_page: &mut dyn FnMut(DynamicImage)) -> ScanSummary {
		self.cancelled.store(false, Ordering::Release);
		self.calls.lock().unwrap().push((scanner.to_string(), *settings));
		let known = self.scanners.as_ref().is_ok_and(|list| list.iter().any(|info| info.id == scanner));
		if !known {
			let message = "The scanner was not found.".to_string();
			return ScanSummary { pages: 0, error: Some(ScanError { kind: ErrorKind::Other, message }) };
		}
		let script = self.scripts.lock().unwrap().pop_front().unwrap_or_default();
		let mut pages = 0;
		for page in script.pages {
			if self.cancelled.load(Ordering::Acquire) {
				let message = "The scan was cancelled.".to_string();
				return ScanSummary { pages, error: Some(ScanError { kind: ErrorKind::Cancelled, message }) };
			}
			on_page(page);
			pages += 1;
			if self.cancel_after == Some(pages) {
				self.cancel();
			}
		}
		ScanSummary { pages, error: script.error }
	}

	fn cancel(&self) {
		self.cancelled.store(true, Ordering::Release);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Source;

	fn scanner() -> ScannerInfo {
		ScannerInfo {
			id: "fake".into(),
			name: "Fake".into(),
			sources: vec![Source::Flatbed, Source::Feeder],
			problem: None,
		}
	}

	fn page(width: u32) -> DynamicImage {
		DynamicImage::new_luma8(width, 1)
	}

	#[test]
	fn pages_arrive_one_by_one_in_order() {
		let fake = FakeScanner::new(vec![scanner()]).with_script(Script::pages(vec![page(1), page(2), page(3)]));
		let mut seen = Vec::new();
		let summary = fake.scan("fake", &ScanSettings::default(), &mut |page| seen.push(page.width()));
		assert_eq!(seen, vec![1, 2, 3]);
		assert_eq!(summary.pages, 3);
		assert!(summary.error.is_none());
		assert_eq!(fake.calls(), vec![("fake".to_string(), ScanSettings::default())]);
	}

	#[test]
	fn a_jam_keeps_the_pages_before_it() {
		let fake = FakeScanner::new(vec![scanner()]).with_script(Script::failing(vec![page(1)], "Paper jam."));
		let summary = fake.scan("fake", &ScanSettings::default(), &mut |_| {});
		assert_eq!(summary.pages, 1);
		assert_eq!(summary.error.unwrap().message, "Paper jam.");
	}

	#[test]
	fn a_cancel_stops_at_the_next_page() {
		let fake = FakeScanner::new(vec![scanner()])
			.with_script(Script::pages(vec![page(1), page(2), page(3)]))
			.cancelling_after(2);
		let summary = fake.scan("fake", &ScanSettings::default(), &mut |_| {});
		assert_eq!(summary.pages, 2);
		assert_eq!(summary.error.unwrap().kind, ErrorKind::Cancelled);
	}

	#[test]
	fn an_unknown_scanner_is_an_error() {
		let fake = FakeScanner::new(vec![scanner()]);
		assert!(fake.scan("gone", &ScanSettings::default(), &mut |_| {}).error.is_some());
	}

	#[test]
	fn missing_sane_is_reported_by_list() {
		let fake = FakeScanner::unavailable(ScanError { kind: ErrorKind::SaneMissing, message: "no SANE".into() });
		assert_eq!(fake.list().unwrap_err().kind, ErrorKind::SaneMissing);
	}

	#[test]
	fn settings_keys_round_trip() {
		for source in [Source::Flatbed, Source::Feeder, Source::FeederDuplex] {
			assert_eq!(Source::from_key(source.key()), Some(source));
		}
		assert_eq!(Source::from_key("film"), None);
	}
}
