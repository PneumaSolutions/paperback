//! Scanners for File > Scan document: list them with their sources, and scan pages as images.
//!
//! A thin layer over [pagescan](https://github.com/PneumaSolutions/pagescan), which talks to each platform's own scanning system: WIA on Windows (2.0, with a 1.0 fallback), ImageCaptureCore on macOS, and SANE on Linux, loaded at run time. Paperback only sees [`Scanner`], so the dialog and its tests run against [`fake::FakeScanner`] as well as the real thing.

use std::time::Duration;

pub use image::DynamicImage;

pub mod fake;
mod system;

pub use system::SystemScanner;

/// What the reader picks in the dialog. Duplex is a source of its own there, since that is how a feeder's two-sided mode reads out loud.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
	Flatbed,
	Feeder,
	FeederDuplex,
}

impl Source {
	#[must_use]
	pub const fn is_feeder(self) -> bool {
		matches!(self, Self::Feeder | Self::FeederDuplex)
	}

	/// The name settings are stored under, which must not change.
	#[must_use]
	pub const fn key(self) -> &'static str {
		match self {
			Self::Flatbed => "flatbed",
			Self::Feeder => "feeder",
			Self::FeederDuplex => "feeder_duplex",
		}
	}

	#[must_use]
	pub fn from_key(key: &str) -> Option<Self> {
		[Self::Flatbed, Self::Feeder, Self::FeederDuplex].into_iter().find(|source| source.key() == key)
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMode {
	/// The default: OCR reads gray as well as color, at a third of the size.
	#[default]
	Gray,
	Color,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannerInfo {
	pub id: String,
	pub name: String,
	/// In the order the dialog offers them; empty when `problem` says why.
	pub sources: Vec<Source>,
	/// Why the scanner's sources could not be read, as the scanner library worded it.
	pub problem: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanSettings {
	pub source: Source,
	pub dpi: u32,
	pub color: ColorMode,
}

impl Default for ScanSettings {
	/// 300 DPI is what OCR engines are tuned for; lower starts dropping small type.
	fn default() -> Self {
		Self { source: Source::Flatbed, dpi: 300, color: ColorMode::Gray }
	}
}

/// Offered in the dialog. Every scanner does 300; the others depend on the scanner, which says so when asked for one it lacks.
pub const RESOLUTIONS: [u32; 5] = [150, 200, 300, 400, 600];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
	/// Linux only: SANE is not installed, which is not the same as having no scanner.
	SaneMissing,
	Cancelled,
	Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanError {
	pub kind: ErrorKind,
	/// A complete sentence from the scanner library, in English; the app shows it after a translated lead-in.
	pub message: String,
}

impl std::fmt::Display for ScanError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(&self.message)
	}
}

impl std::error::Error for ScanError {}

/// How a scan ended. Pages already handed to `on_page` are kept whatever happened, so a jam on page five still leaves four.
#[derive(Debug, Default)]
pub struct ScanSummary {
	pub pages: usize,
	pub error: Option<ScanError>,
}

pub trait Scanner: Send + Sync {
	fn list(&self) -> Result<Vec<ScannerInfo>, ScanError>;

	/// Calls `on_page` with each page as soon as it is scanned, in order.
	fn scan(&self, scanner: &str, settings: &ScanSettings, on_page: &mut dyn FnMut(DynamicImage)) -> ScanSummary;

	/// Stops a running scan at the next point the scanner allows. Safe from any thread.
	fn cancel(&self);

	/// macOS's ImageCaptureCore delivers to the main thread, so there a scan must run on it; elsewhere it runs on a worker.
	fn needs_main_thread(&self) -> bool {
		false
	}
}

/// A scan has to end some time even if the driver never says so.
pub const TIMEOUT: Duration = Duration::from_mins(15);
