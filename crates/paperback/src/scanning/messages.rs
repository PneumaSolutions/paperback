//! Everything the scan run says, in one place so the wording reads as one voice and the tests can hold it. Spoken in the order a sighted person would notice: the page and its words, then anything odd about it.

use paperback_core::scan::check::Turn;
use paperback_scan::{ErrorKind, ScanError, Source};
use patois::{nt, t};

fn count(n: usize) -> u64 {
	u64::try_from(n).unwrap_or(u64::MAX)
}

#[must_use]
pub fn source_name(source: Source) -> String {
	match source {
		// TRANSLATORS: A scanner source in the Scan Document dialog: the glass a page is laid on
		Source::Flatbed => t("Flatbed"),
		// TRANSLATORS: A scanner source in the Scan Document dialog: the tray that pulls pages through one at a time, scanning one side of each
		Source::Feeder => t("Document feeder, one side"),
		// TRANSLATORS: A scanner source in the Scan Document dialog: the document feeder, scanning the front and back of each sheet and keeping the sides with text
		Source::FeederDuplex => t("Document feeder, both sides"),
	}
}

/// After a page is read: its number and how many words, then whether it was turned. A turned page is put right without asking, since it reads; only a page that may be out of line is worth scanning again ([`not_lined_up`]).
#[must_use]
pub fn page_read(number: usize, words: usize, turn: Option<Turn>) -> String {
	let mut message = if words == 0 {
		// TRANSLATORS: Announced after a scanned page is read and no text is found on it; %d is the page number
		t("Page %d: no text found.").replacen("%d", &number.to_string(), 1)
	} else {
		// TRANSLATORS: Announced after a scanned page is read; the first %d is the page number, the second the number of words found
		let template = nt("Page %d: %d word.", "Page %d: %d words.", count(words));
		template.replacen("%d", &number.to_string(), 1).replacen("%d", &words.to_string(), 1)
	};
	let turned = match turn {
		// TRANSLATORS: Said after a scanned page that was upside down and has been turned the right way up; %d is the page number
		Some(Turn::Half) => Some(t("Page %d was upside down; corrected.")),
		// TRANSLATORS: Said after a scanned page that was sideways and has been turned the right way up; %d is the page number
		Some(Turn::Quarter | Turn::ThreeQuarters) => Some(t("Page %d was sideways; corrected.")),
		Some(Turn::None) | None => None,
	};
	if let Some(turned) = turned {
		message = format!("{message} {}", turned.replacen("%d", &number.to_string(), 1));
	}
	message
}

/// For a page with text against an edge or very little text. `can_rescan` is false for a feeder, which can't take one page back.
#[must_use]
pub fn not_lined_up(cut_off: bool, can_rescan: bool) -> String {
	let problem = if cut_off {
		// TRANSLATORS: Said after a scanned page whose text runs up against an edge of the scan
		t("Some text touches the edge, so the page might not be lined up.")
	} else {
		// TRANSLATORS: Said after a scanned page on which only a few words were found
		t("Only a little text was found, so the page might not be lined up.")
	};
	if can_rescan {
		// TRANSLATORS: Said after a flatbed page that might not be lined up; Rescan and Skip are buttons
		format!("{problem} {}", t("Straighten it and choose Rescan, or choose Skip to keep it as it is."))
	} else {
		problem
	}
}

#[must_use]
pub fn blank_on_flatbed(number: usize) -> String {
	// TRANSLATORS: Said when a page scanned from the flatbed has nothing on it; %d is the page number it would have been
	t("Page %d looks blank. If it is face down, turn it over and choose Rescan. Otherwise choose Skip to leave it out.")
		.replacen("%d", &number.to_string(), 1)
}

#[must_use]
pub fn blank_left_out() -> String {
	// TRANSLATORS: Said during a document feeder scan when a page has nothing on it and is not added
	t("Blank page left out.")
}

#[must_use]
pub fn all_blank() -> String {
	// TRANSLATORS: Said when every page from the document feeder came out blank
	t("Every page was blank. The pages might be face down. Turn the stack over and scan again.")
}

#[must_use]
pub fn flatbed_prompt() -> String {
	// TRANSLATORS: Said after a flatbed page is scanned, saying how to go on; Enter and Escape are keys
	t("Put the next page on the glass and press Enter to scan it, or press Escape to finish.")
}

#[must_use]
pub fn feeder_finished(pages: usize, blanks: usize) -> String {
	// TRANSLATORS: Said when a document feeder scan ends; %d is the number of pages added
	let mut message = nt("Scanned %d page.", "Scanned %d pages.", count(pages)).replacen("%d", &pages.to_string(), 1);
	if blanks > 0 {
		// TRANSLATORS: Said at the end of a one-sided document feeder scan; %d is the number of blank pages that were not added
		let left_out = nt(
			"%d blank page left out. It might have been face down; turn it over and choose Scan More to scan it again.",
			"%d blank pages left out. They might have been face down; turn them over and choose Scan More to scan them again.",
			count(blanks),
		)
		.replacen("%d", &blanks.to_string(), 1);
		message = format!("{message} {left_out}");
	}
	format!("{message} {}", more_prompt())
}

/// After a two-sided scan that kept only the sides with text: how many sheets went through, how many pages that made, and how many blank sides were dropped.
#[must_use]
pub fn both_sides_finished(sheets: usize, pages: usize, skipped: usize) -> String {
	// TRANSLATORS: Part of the summary after a two-sided feeder scan; %d is the number of sheets of paper that went through
	let sheets = nt("%d sheet", "%d sheets", count(sheets)).replacen("%d", &sheets.to_string(), 1);
	// TRANSLATORS: Part of the summary after a two-sided feeder scan; %d is the number of pages kept in the document
	let pages = nt("%d page", "%d pages", count(pages)).replacen("%d", &pages.to_string(), 1);
	// TRANSLATORS: Part of the summary after a two-sided feeder scan; %d is the number of blank sides left out
	let skipped =
		nt("%d blank side skipped", "%d blank sides skipped", count(skipped)).replacen("%d", &skipped.to_string(), 1);
	// TRANSLATORS: The summary after a two-sided feeder scan, built from the three parts above, in this order: sheets, pages, blank sides skipped
	let summary = t("{}, {}, {}.").replacen("{}", &sheets, 1).replacen("{}", &pages, 1).replacen("{}", &skipped, 1);
	format!("{summary} {}", more_prompt())
}

fn more_prompt() -> String {
	// TRANSLATORS: Said after a document feeder scan, saying how to go on; Enter and Escape are keys
	t("To scan more, load them and press Enter. Press Escape to finish.")
}

#[must_use]
pub fn scanning() -> String {
	// TRANSLATORS: Said when a scan starts
	t("Scanning.")
}

#[must_use]
pub fn looking_for_scanners() -> String {
	// TRANSLATORS: Shown while Paperback asks the system which scanners there are
	t("Looking for scanners...")
}

#[must_use]
pub fn no_scanners() -> String {
	// TRANSLATORS: Shown when no scanner is found
	t(
		"No scanners were found. Check that the scanner is turned on and connected, and that the system's own scanning app can see it, then try again.",
	)
}

#[must_use]
pub fn stopped() -> String {
	// TRANSLATORS: Said when the reader stops a scan
	t("Scan stopped.")
}

#[must_use]
pub fn already_scanning() -> String {
	// TRANSLATORS: Said when Scan Document is chosen while a scan is already running
	t("A scan is already running.")
}

/// The scanner library's own message is an English sentence written to be read out, so it follows a translated lead-in rather than being replaced.
#[must_use]
pub fn scan_failed(error: &ScanError) -> String {
	match error.kind {
		ErrorKind::SaneMissing => sane_missing(),
		ErrorKind::Cancelled => stopped(),
		// TRANSLATORS: Said when a scan fails; %s is the scanner's own description of the problem, in English
		ErrorKind::Other => t("The scan stopped: %s").replacen("%s", &error.message, 1),
	}
}

#[must_use]
pub fn sane_missing() -> String {
	// TRANSLATORS: Shown on Linux when scanning is chosen but the SANE scanner library isn't installed
	t(
		"Scanning needs SANE. Install the sane-utils package, and sane-airscan for network scanners, from your package manager.",
	)
}

#[must_use]
pub fn finished(pages: usize) -> String {
	// TRANSLATORS: Said when the Scanning window closes; %d is the number of pages in the scanned document
	nt("Scanned document, %d page.", "Scanned document, %d pages.", count(pages)).replacen("%d", &pages.to_string(), 1)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_page_says_its_number_and_words() {
		assert_eq!(page_read(3, 245, None), "Page 3: 245 words.");
		assert_eq!(page_read(1, 1, None), "Page 1: 1 word.");
		assert_eq!(page_read(2, 0, None), "Page 2: no text found.");
	}

	#[test]
	fn a_turned_page_is_corrected_and_says_so_briefly() {
		assert_eq!(page_read(3, 300, Some(Turn::Half)), "Page 3: 300 words. Page 3 was upside down; corrected.");
		assert!(page_read(4, 300, Some(Turn::Quarter)).ends_with("Page 4 was sideways; corrected."));
	}

	#[test]
	fn a_page_out_of_line_offers_a_rescan_only_on_the_flatbed() {
		assert!(not_lined_up(true, true).contains("touches the edge"));
		assert!(not_lined_up(true, true).contains("Rescan"));
		assert!(not_lined_up(false, true).contains("little text"));
		assert!(!not_lined_up(false, false).contains("Rescan"));
	}

	#[test]
	fn the_feeder_summary_counts_pages_and_blanks() {
		assert!(feeder_finished(12, 0).starts_with("Scanned 12 pages. To scan more"));
		assert!(
			feeder_finished(1, 2).starts_with("Scanned 1 page. 2 blank pages left out. They might have been face down")
		);
	}

	#[test]
	fn the_two_sided_summary_counts_sheets_pages_and_skipped_sides() {
		assert!(both_sides_finished(5, 7, 3).starts_with("5 sheets, 7 pages, 3 blank sides skipped. To scan more"));
		assert!(both_sides_finished(1, 1, 1).starts_with("1 sheet, 1 page, 1 blank side skipped."));
	}

	#[test]
	fn a_failure_leads_with_a_translatable_sentence_and_missing_sane_has_its_own() {
		let error = ScanError { kind: ErrorKind::Other, message: "The scanner reported a paper jam.".into() };
		assert_eq!(scan_failed(&error), "The scan stopped: The scanner reported a paper jam.");
		let missing = ScanError { kind: ErrorKind::SaneMissing, message: String::new() };
		assert_eq!(scan_failed(&missing), sane_missing());
	}
}
