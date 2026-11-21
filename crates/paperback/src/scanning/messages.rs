//! Everything the scan run says, in one place so the wording reads as one voice and the tests can hold it. Spoken in the order a sighted person would notice: the page and its words, then anything odd about it.

use paperback_core::scan::check::{TextAmount, Turn};
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
pub fn page_read(number: usize, amount: TextAmount, turn: Option<Turn>) -> String {
	let mut message = if amount.units() == 0 {
		// TRANSLATORS: Announced after a scanned page is read and no text is found on it; %d is the page number
		t("Page %d: no text found.").replacen("%d", &number.to_string(), 1)
	} else if amount.in_characters() {
		// TRANSLATORS: Announced after a scanned page in a language written without spaces (such as Chinese, Japanese or Thai) is read; the first %d is the page number, the second the number of characters found
		let template = nt("Page %d: %d character.", "Page %d: %d characters.", count(amount.units()));
		template.replacen("%d", &number.to_string(), 1).replacen("%d", &amount.units().to_string(), 1)
	} else {
		// TRANSLATORS: Announced after a scanned page is read; the first %d is the page number, the second the number of words found
		let template = nt("Page %d: %d word.", "Page %d: %d words.", count(amount.units()));
		template.replacen("%d", &number.to_string(), 1).replacen("%d", &amount.units().to_string(), 1)
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

/// For a page with text against an edge or very little text. Said without stopping the scan: Rescan is offered beside the next page, never asked. `can_rescan` is false for a feeder, which can't take one page back.
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
		// TRANSLATORS: Said after a flatbed page that might not be lined up; Rescan is a button and Enter a key, and scanning goes on unless Rescan is chosen
		format!(
			"{problem} {}",
			t("To scan it again, straighten it and choose Rescan. Otherwise press Enter to go on to the next page.")
		)
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

/// A one-sided feeder keeps blank pages unless asked to skip them; this is said with the page.
#[must_use]
pub fn looks_blank() -> String {
	// TRANSLATORS: Said after a page from the document feeder that has nothing on it; it is kept, and might be face down
	t("It looks blank; it might be face down.")
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

/// Said when the Scanning window closes while pages are still being read: the count isn't final yet, so it waits for the end.
#[must_use]
pub fn finishing() -> String {
	// TRANSLATORS: Said when the Scanning window is closed while scanned pages are still being read; Paperback speaks again when the document is complete
	t("Finishing the scan. Paperback will say when the document is complete.")
}

/// Save Scan As or closing the tab while the run still owns the archive.
#[must_use]
pub fn still_reading() -> String {
	// TRANSLATORS: Said when Save Scan As or closing the tab is chosen while the pages of a scan are still being read
	t("This scan is still being read. Save or close it once Paperback says the document is complete.")
}

#[must_use]
pub fn quit_while_scanning() -> String {
	// TRANSLATORS: Asked when Paperback is closed while a scan is still being read
	t("A scan is still being read. Quit anyway? Pages already added are kept; pages not yet read are lost.")
}

#[must_use]
pub fn confirm_stop() -> String {
	// TRANSLATORS: Asked when the Scanning window is closed while the document feeder is still scanning
	t("Stop scanning? The pages scanned so far are kept.")
}

/// A page that couldn't be added to the archive (a full disk is the likely reason). Said once: the run stops there.
#[must_use]
pub fn page_not_saved(number: usize, reason: &str, earlier: usize) -> String {
	// TRANSLATORS: Said when a scanned page can't be saved; the first %d is the page number, %s the reason
	let failed = t("Page %d could not be saved: %s.").replacen("%d", &number.to_string(), 1).replacen("%s", reason, 1);
	// TRANSLATORS: Said after a scanned page could not be saved; %d is the number of pages saved before it
	let safe = nt(
		"Scanning stopped. The %d page before it is safe.",
		"Scanning stopped. The %d pages before it are safe.",
		count(earlier),
	)
	.replacen("%d", &earlier.to_string(), 1);
	format!("{failed} {safe}")
}

#[must_use]
pub fn page_unreadable() -> String {
	// TRANSLATORS: Said when a page came from the scanner but its image couldn't be read, so it was left out
	t("A scanned page could not be read and was left out.")
}

/// A feeder's pages aren't each announced; this is said every few seconds instead.
#[must_use]
pub fn pages_so_far(pages: usize) -> String {
	// TRANSLATORS: Said every few seconds while the document feeder scans; %d is the number of pages added so far
	nt("%d page so far.", "%d pages so far.", count(pages)).replacen("%d", &pages.to_string(), 1)
}

/// After a feeder run: what was noticed about particular pages, said once at the end rather than as each page went by.
#[must_use]
pub fn page_notes(notes: &[String]) -> String {
	const SAID: usize = 5;
	let mut message = notes.iter().take(SAID).cloned().collect::<Vec<_>>().join(" ");
	if notes.len() > SAID {
		let more = notes.len() - SAID;
		// TRANSLATORS: Said at the end of a document feeder scan after the first few notes about particular pages; %d is how many more pages had a note
		let rest = nt("%d more page had a note.", "%d more pages had a note.", count(more)).replacen(
			"%d",
			&more.to_string(),
			1,
		);
		message = format!("{message} {rest}");
	}
	message
}

#[must_use]
pub fn ocr_language_hint() -> String {
	// TRANSLATORS: Said once when several scanned pages in a row have ink but no readable text
	t("Several pages in a row had no text that could be read. The OCR language might not match the document.")
}

#[cfg(test)]
mod tests {
	use super::*;

	fn words(words: usize) -> TextAmount {
		TextAmount { words, characters: 0 }
	}

	#[test]
	fn a_page_in_a_script_without_spaces_counts_characters() {
		let amount = TextAmount { words: 2, characters: 512 };
		assert_eq!(page_read(1, amount, None), "Page 1: 514 characters.");
	}

	#[test]
	fn a_page_says_its_number_and_words() {
		assert_eq!(page_read(3, words(245), None), "Page 3: 245 words.");
		assert_eq!(page_read(1, words(1), None), "Page 1: 1 word.");
		assert_eq!(page_read(2, words(0), None), "Page 2: no text found.");
	}

	#[test]
	fn a_turned_page_is_corrected_and_says_so_briefly() {
		assert_eq!(page_read(3, words(300), Some(Turn::Half)), "Page 3: 300 words. Page 3 was upside down; corrected.");
		assert!(page_read(4, words(300), Some(Turn::Quarter)).ends_with("Page 4 was sideways; corrected."));
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

	#[test]
	fn notes_about_pages_are_said_together_and_capped() {
		let notes: Vec<String> = (1..=7).map(|n| format!("Page {n} looks blank.")).collect();
		let said = page_notes(&notes);
		assert!(said.starts_with("Page 1 looks blank. Page 2"));
		assert!(said.contains("Page 5 looks blank."));
		assert!(!said.contains("Page 6"));
		assert!(said.ends_with("2 more pages had a note."));
		assert_eq!(page_notes(&notes[..1]), "Page 1 looks blank.");
	}

	#[test]
	fn a_page_that_could_not_be_saved_says_the_earlier_ones_are_safe() {
		assert_eq!(
			page_not_saved(61, "no space left on device", 60),
			"Page 61 could not be saved: no space left on device. Scanning stopped. The 60 pages before it are safe."
		);
	}
}
