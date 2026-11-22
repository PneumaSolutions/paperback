//! The two windows of File > Scan document: the settings, and the Scanning window that stays open while pages come in. Neither shows anything of the scanner's own; every step is spoken instead.

use std::{cell::RefCell, rc::Rc};

use patois::t;
use wxdragon::prelude::*;

use super::{add_ok_cancel_footer, build_ok_cancel_buttons, dialog_padding};
use crate::scanning::{
	messages,
	remembered::Remembered,
	scanner::{ColorMode, RESOLUTIONS, ScanSettings, ScannerInfo, Source},
};

pub enum SettingsChoice {
	Scan { scanner: ScannerInfo, settings: ScanSettings, skip_blank_sides: bool },
	FindScanners,
	Cancel,
}

/// The reader asked for the list again rather than scanning.
const ID_FIND_SCANNERS: i32 = ID_HIGHEST + 1;

/// `offer_find` is for the remembered scanner alone, shown without asking the system for the list.
pub fn show_scan_settings_dialog(
	parent: &Frame,
	scanners: &[ScannerInfo],
	remembered: &Remembered,
	offer_find: bool,
) -> SettingsChoice {
	if scanners.is_empty() {
		return SettingsChoice::Cancel;
	}
	// TRANSLATORS: Title of the dialog that sets up a scan
	let dialog = Dialog::builder(parent, &t("Scan Document")).build();
	let _destroy = super::DestroyOnDrop(dialog);
	let padding = dialog_padding(&dialog);
	// TRANSLATORS: Label for the list of scanners in the Scan Document dialog
	let scanner_label = StaticText::builder(&dialog).with_label(&t("Sc&anner:")).build();
	let scanner_choice = Choice::builder(&dialog).build();
	for scanner in scanners {
		let name = match &scanner.problem {
			// TRANSLATORS: A scanner in the Scan Document dialog whose details could not be read; %s is its name
			Some(_) => t("%s (not responding)").replacen("%s", &scanner.name, 1),
			None => scanner.name.clone(),
		};
		scanner_choice.append(&name);
	}
	let first =
		remembered.last.as_ref().and_then(|id| scanners.iter().position(|scanner| scanner.id == *id)).unwrap_or(0);
	scanner_choice.set_selection(u32::try_from(first).unwrap_or(0));
	// TRANSLATORS: Label for the choice of where the scanner takes the paper from
	let source_label = StaticText::builder(&dialog).with_label(&t("So&urce:")).build();
	let source_choice = Choice::builder(&dialog).build();
	// TRANSLATORS: Label for the choice between scanning in grayscale or in color
	let color_label = StaticText::builder(&dialog).with_label(&t("C&olor:")).build();
	let color_choice = Choice::builder(&dialog).build();
	// TRANSLATORS: A choice in the Scan Document dialog: scan in shades of gray, which OCR reads as well as color
	color_choice.append(&t("Grayscale"));
	// TRANSLATORS: A choice in the Scan Document dialog: scan in color
	color_choice.append(&t("Color"));
	// TRANSLATORS: Label for the choice of scanning resolution, in dots per inch
	let resolution_label = StaticText::builder(&dialog).with_label(&t("&Resolution:")).build();
	let resolution_choice = Choice::builder(&dialog).build();
	for dpi in RESOLUTIONS {
		// TRANSLATORS: A scanning resolution; %d is the number of dots per inch
		resolution_choice.append(&t("%d DPI").replacen("%d", &dpi.to_string(), 1));
	}
	// TRANSLATORS: Checkbox in the Scan Document dialog, for the document feeder: leave out pages (or sides of a page) with no text on them
	let skip_blank_sides = CheckBox::builder(&dialog).with_label(&t("S&kip blank pages")).build();
	let sources_shown: Rc<RefCell<Vec<Source>>> = Rc::new(RefCell::new(Vec::new()));
	let feeder_chosen = {
		let sources_shown = Rc::clone(&sources_shown);
		move || {
			let sources = sources_shown.borrow();
			source_choice
				.get_selection()
				.and_then(|index| sources.get(usize::try_from(index).ok()?).copied())
				.is_some_and(Source::is_feeder)
		}
	};
	let show_settings = {
		let sources_shown = Rc::clone(&sources_shown);
		let remembered = remembered.clone();
		let scanners = scanners.to_vec();
		move |index: usize| {
			let Some(scanner) = scanners.get(index) else { return };
			let settings = remembered.settings_for(scanner);
			let sources = if scanner.sources.is_empty() { vec![Source::Flatbed] } else { scanner.sources.clone() };
			source_choice.clear();
			for source in &sources {
				source_choice.append(&messages::source_name(*source));
			}
			let selected = sources.iter().position(|source| *source == settings.source).unwrap_or(0);
			source_choice.set_selection(u32::try_from(selected).unwrap_or(0));
			*sources_shown.borrow_mut() = sources;
			skip_blank_sides.set_value(remembered.skip_blank_sides(scanner));
			skip_blank_sides.enable(settings.source.is_feeder());
			color_choice.set_selection(u32::from(settings.color == ColorMode::Color));
			let dpi = RESOLUTIONS.iter().position(|dpi| *dpi == settings.dpi).unwrap_or(2);
			resolution_choice.set_selection(u32::try_from(dpi).unwrap_or(2));
		}
	};
	show_settings(first);
	source_choice.on_selection_changed(move |_| skip_blank_sides.enable(feeder_chosen()));
	scanner_choice.on_selection_changed(move |event| {
		if let Some(index) = event.get_selection().and_then(|index| usize::try_from(index).ok()) {
			show_settings(index);
		}
	});
	let grid = FlexGridSizer::builder(0, 2).with_vgap(padding / 2).with_hgap(padding).build();
	grid.add_growable_col(1, 1);
	for (label, control) in [
		(scanner_label, scanner_choice),
		(source_label, source_choice),
		(color_label, color_choice),
		(resolution_label, resolution_choice),
	] {
		grid.add(&label, 0, SizerFlag::AlignCenterVertical, 0);
		grid.add(&control, 1, SizerFlag::Expand, 0);
	}
	let content = BoxSizer::builder(Orientation::Vertical).build();
	content.add_sizer(&grid, 0, SizerFlag::Expand | SizerFlag::All, padding);
	content.add(&skip_blank_sides, 0, SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, padding);
	if offer_find {
		// TRANSLATORS: Button in the Scan Document dialog that lists every scanner again, to pick another one
		let find = Button::builder(&dialog).with_id(ID_FIND_SCANNERS).with_label(&t("&Find Other Scanners...")).build();
		find.on_click(move |_| dialog.end_modal(ID_FIND_SCANNERS));
		content.add(&find, 0, SizerFlag::Left | SizerFlag::Right, padding);
	}
	// TRANSLATORS: Button in the Scan Document dialog that starts scanning
	let (ok_button, cancel_button) = build_ok_cancel_buttons(&dialog, &t("&Scan"));
	add_ok_cancel_footer(content, ok_button, cancel_button);
	dialog.set_sizer_and_fit(content, true);
	dialog.centre();
	// The scanner is usually the one used last, so the first thing worth changing is where the paper is.
	source_choice.set_focus();
	match dialog.show_modal() {
		ID_OK => {
			let index = scanner_choice.get_selection().and_then(|index| usize::try_from(index).ok()).unwrap_or(0);
			let scanner = scanners[index.min(scanners.len() - 1)].clone();
			let sources = sources_shown.borrow();
			let source = source_choice
				.get_selection()
				.and_then(|index| sources.get(usize::try_from(index).ok()?).copied())
				.unwrap_or(Source::Flatbed);
			let color = if color_choice.get_selection() == Some(1) { ColorMode::Color } else { ColorMode::Gray };
			let dpi = resolution_choice
				.get_selection()
				.and_then(|index| RESOLUTIONS.get(usize::try_from(index).ok()?).copied())
				.unwrap_or_else(|| ScanSettings::default().dpi);
			SettingsChoice::Scan {
				scanner,
				settings: ScanSettings { source, dpi, color },
				skip_blank_sides: source.is_feeder() && skip_blank_sides.is_checked(),
			}
		}
		ID_FIND_SCANNERS => SettingsChoice::FindScanners,
		_ => SettingsChoice::Cancel,
	}
}

/// The Scanning window. Built here; what its buttons do is the scan run's business.
#[derive(Clone, Copy)]
pub struct ScanningWindow {
	pub dialog: Dialog,
	/// Read by the screen reader as each step is announced, and there to read again.
	pub status: StaticText,
	/// "Scan Next Page" for the flatbed, "Scan More" for the feeder.
	pub next: Button,
	pub rescan: Button,
	pub skip: Button,
	pub stop: Button,
	/// Escape and the close box press it; the scan run decides whether the window closes.
	pub done: Button,
}

const ID_SCAN_NEXT: i32 = ID_HIGHEST + 2;
const ID_RESCAN: i32 = ID_HIGHEST + 3;
const ID_SKIP: i32 = ID_HIGHEST + 4;
const ID_STOP: i32 = ID_HIGHEST + 5;
const ID_DONE: i32 = ID_HIGHEST + 8;

impl ScanningWindow {
	pub fn build(parent: &Frame, feeder: bool) -> Self {
		// TRANSLATORS: Title of the window that stays open while pages are scanned
		let dialog = Dialog::builder(parent, &t("Scanning")).build();
		let padding = dialog_padding(&dialog);
		let status = StaticText::builder(&dialog).with_label(&messages::scanning()).build();
		status.wrap(480);
		let next_label = if feeder {
			// TRANSLATORS: Button in the Scanning window that runs the document feeder again for more pages
			t("Scan &More")
		} else {
			// TRANSLATORS: Button in the Scanning window that scans the next page on the flatbed
			t("Scan &Next Page")
		};
		let next = Button::builder(&dialog).with_id(ID_SCAN_NEXT).with_label(&next_label).build();
		// TRANSLATORS: Button in the Scanning window that scans the last page again
		let rescan = Button::builder(&dialog).with_id(ID_RESCAN).with_label(&t("&Rescan")).build();
		// TRANSLATORS: Button in the Scanning window that goes on without scanning the last page again
		let skip = Button::builder(&dialog).with_id(ID_SKIP).with_label(&t("S&kip")).build();
		// TRANSLATORS: Button in the Scanning window that stops the scan in progress
		let stop = Button::builder(&dialog).with_id(ID_STOP).with_label(&t("S&top")).build();
		// TRANSLATORS: Button in the Scanning window that closes it and goes to the scanned document
		let done = Button::builder(&dialog).with_id(ID_DONE).with_label(&t("&Done")).build();
		// Not the Cancel ID, so Escape doesn't close the window by itself: a feeder still scanning asks first.
		dialog.set_escape_id(ID_DONE);
		let buttons = BoxSizer::builder(Orientation::Horizontal).build();
		for button in [next, rescan, skip, stop, done] {
			buttons.add(&button, 0, SizerFlag::Right, padding / 2);
		}
		let content = BoxSizer::builder(Orientation::Vertical).build();
		content.add(&status, 1, SizerFlag::Expand | SizerFlag::All, padding);
		content.add_sizer(&buttons, 0, SizerFlag::All | SizerFlag::AlignRight, padding);
		dialog.set_sizer(content, true);
		dialog.set_min_size(Size::new(520, 160));
		dialog.fit();
		dialog.centre();
		Self { dialog, status, next, rescan, skip, stop, done }
	}

	pub fn say(&self, message: &str) {
		self.status.set_label(message);
		self.status.wrap(480);
		self.dialog.layout();
		live_region::announce(self.status, message);
	}

	/// Only the buttons that mean something now are shown, so Tab never lands on one that would do nothing. Rescan sits beside Scan Next Page for a page that may not be lined up, without taking Enter from it. The focus moves only when the button it was on is hidden, or nothing in the window has it (`force` moves it anyway), since a screen reader announcing a focus change cuts off what it was saying.
	pub fn show_buttons(&self, state: Buttons, force: bool) {
		let Buttons { scanning, asking_blank, can_rescan } = state;
		let focused_before = self.focused_button();
		self.stop.show(scanning);
		self.next.show(!scanning && !asking_blank);
		self.rescan.show(!scanning && (asking_blank || can_rescan));
		self.skip.show(!scanning && asking_blank);
		let focus = if scanning {
			self.stop
		} else if asking_blank {
			self.rescan
		} else {
			self.next
		};
		focus.set_default();
		self.dialog.layout();
		let still_there = focused_before.is_some_and(|button| button.is_shown());
		if force || !still_there {
			focus.set_focus();
		}
	}

	pub fn focused_button(&self) -> Option<Button> {
		[self.next, self.rescan, self.skip, self.stop, self.done].into_iter().find(WxWidget::has_focus)
	}
}

/// What the Scanning window's buttons are for right now.
#[derive(Debug, Clone, Copy)]
pub struct Buttons {
	pub scanning: bool,
	/// A flatbed page came out blank: Rescan or Skip.
	pub asking_blank: bool,
	/// A flatbed page that may not be lined up: Rescan beside Scan Next Page.
	pub can_rescan: bool,
}

/// What to do with a scan in progress that is being closed.
pub enum CloseChoice {
	Save,
	Discard,
	Cancel,
}

const ID_DISCARD: i32 = ID_HIGHEST + 6;

pub fn ask_to_save_scan(parent: &dyn WxWidget, title: &str) -> CloseChoice {
	// TRANSLATORS: Title of the question asked when a scanned document that hasn't been saved is closed
	let dialog = Dialog::builder(parent, &t("Save Scan")).build();
	let _destroy = super::DestroyOnDrop(dialog);
	let padding = dialog_padding(&dialog);
	// TRANSLATORS: Asked when a scanned document that hasn't been saved is closed; %s is its title. Closing without saving deletes it.
	let message =
		t("%s hasn't been saved. Save it before closing? If you don't, the scan is deleted.").replacen("%s", title, 1);
	let label = StaticText::builder(&dialog).with_label(&message).build();
	label.wrap(420);
	// TRANSLATORS: Button that saves a scanned document before closing it
	let save = Button::builder(&dialog).with_id(ID_OK).with_label(&t("&Save...")).build();
	// TRANSLATORS: Button that closes a scanned document without saving it, deleting it
	let discard = Button::builder(&dialog).with_id(ID_DISCARD).with_label(&t("&Don't Save")).build();
	// TRANSLATORS: Label for the cancellation button
	let cancel = Button::builder(&dialog).with_id(ID_CANCEL).with_label(&t("Cancel")).build();
	discard.on_click(move |_| dialog.end_modal(ID_DISCARD));
	dialog.set_escape_id(ID_CANCEL);
	dialog.set_affirmative_id(ID_OK);
	save.set_default();
	let buttons = BoxSizer::builder(Orientation::Horizontal).build();
	for button in [save, discard, cancel] {
		buttons.add(&button, 0, SizerFlag::Right, padding / 2);
	}
	let content = BoxSizer::builder(Orientation::Vertical).build();
	content.add(&label, 0, SizerFlag::All, padding);
	content.add_sizer(&buttons, 0, SizerFlag::All | SizerFlag::AlignRight, padding);
	dialog.set_sizer_and_fit(content, true);
	dialog.centre();
	save.set_focus();
	match dialog.show_modal() {
		ID_OK => CloseChoice::Save,
		ID_DISCARD => CloseChoice::Discard,
		_ => CloseChoice::Cancel,
	}
}

/// Where to save a scan, as a comic book archive, which is what it is.
pub fn choose_save_path(parent: &dyn WxWidget, title: &str) -> Option<std::path::PathBuf> {
	// TRANSLATORS: Title of the file picker for saving a scanned document
	let caption = t("Save Scan As");
	// TRANSLATORS: File type in the Save Scan As file picker; keep the (*.cbz) and the |*.cbz after it as they are
	let wildcard = t("Comic book archives (*.cbz)|*.cbz");
	let dialog = FileDialog::builder(parent)
		.with_message(&caption)
		.with_default_file(&format!("{title}.cbz"))
		.with_wildcard(&wildcard)
		.with_style(FileDialogStyle::Save | FileDialogStyle::OverwritePrompt)
		.build();
	if dialog.show_modal() != ID_OK {
		return None;
	}
	let chosen = std::path::PathBuf::from(dialog.get_path()?);
	let path = with_cbz_extension(chosen.clone());
	// The picker asked about overwriting the name as typed; a name it never saw gets asked about here.
	if path != chosen && path.exists() {
		// TRANSLATORS: Asked when the name chosen for a saved scan, with .cbz added, is already taken; %s is the file name
		let question = t("%s already exists. Replace it?").replacen(
			"%s",
			&path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
			1,
		);
		if !wx_utils::confirm(parent, question, &caption) {
			return None;
		}
	}
	Some(path)
}

/// The archive is a comic book archive whatever the reader typed, so Paperback and other readers know it. `.cbz` is added to the name, never put in place of what follows its last dot: "Dr. Smith letter" stays itself.
fn with_cbz_extension(path: std::path::PathBuf) -> std::path::PathBuf {
	if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("cbz")) {
		return path;
	}
	let mut name = path.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_default();
	name.push(".cbz");
	path.with_file_name(name)
}

/// What to do with scans in progress left from last time.
pub enum UnsavedChoice {
	Open,
	Delete,
	Later,
}

const ID_DELETE: i32 = ID_HIGHEST + 7;

/// Asked at start when scans in progress were left by quitting or a crash and nothing reopened them. Nothing is deleted unless the reader chooses Delete and confirms it. `unreadable` couldn't be opened just now (another program may be using one); Open leaves them for next time.
pub fn ask_about_unsaved_scans(
	parent: &dyn WxWidget,
	count: usize,
	recovered: usize,
	unreadable: usize,
) -> UnsavedChoice {
	// TRANSLATORS: Title of the question asked at start about scanned documents that were never saved
	let dialog = Dialog::builder(parent, &t("Unsaved Scans")).build();
	let _destroy = super::DestroyOnDrop(dialog);
	let padding = dialog_padding(&dialog);
	let count_u64 = u64::try_from(count).unwrap_or(u64::MAX);
	// TRANSLATORS: Asked at start; %d is the number of scanned documents left unsaved when Paperback last closed
	let mut message = patois::nt(
		"Paperback found %d unsaved scan from last time.",
		"Paperback found %d unsaved scans from last time.",
		count_u64,
	)
	.replacen("%d", &count.to_string(), 1);
	if recovered > 0 {
		let recovered_u64 = u64::try_from(recovered).unwrap_or(u64::MAX);
		// TRANSLATORS: Part of the question asked at start; %d is how many of the unsaved scans were cut short by Paperback closing unexpectedly, and repaired
		let note = patois::nt(
			"%d of them was cut short when Paperback closed unexpectedly, and its complete pages have been recovered.",
			"%d of them were cut short when Paperback closed unexpectedly, and their complete pages have been recovered.",
			recovered_u64,
		)
		.replacen("%d", &recovered.to_string(), 1);
		message = format!("{message} {note}");
	}
	if unreadable > 0 {
		let unreadable_u64 = u64::try_from(unreadable).unwrap_or(u64::MAX);
		// TRANSLATORS: Part of the question asked at start; %d is how many of the unsaved scans couldn't be opened just now (another program may be using them). They're kept and offered again next time.
		let note = patois::nt(
			"%d of them could not be opened just now; it is kept as it is.",
			"%d of them could not be opened just now; they are kept as they are.",
			unreadable_u64,
		)
		.replacen("%d", &unreadable.to_string(), 1);
		message = format!("{message} {note}");
	}
	// TRANSLATORS: The end of the question asked at start about unsaved scans; Open, Delete and Keep for Later are its buttons
	message = format!("{message} {}", t("Open them now? Keep for Later asks again next time."));
	let label = StaticText::builder(&dialog).with_label(&message).build();
	label.wrap(460);
	// TRANSLATORS: Button that opens the unsaved scans found at start
	let open = Button::builder(&dialog).with_id(ID_OK).with_label(&t("&Open")).build();
	// TRANSLATORS: Button that deletes the unsaved scans found at start, after asking to confirm
	let delete = Button::builder(&dialog).with_id(ID_DELETE).with_label(&t("&Delete...")).build();
	// TRANSLATORS: Button that leaves the unsaved scans found at start for next time
	let later = Button::builder(&dialog).with_id(ID_CANCEL).with_label(&t("&Keep for Later")).build();
	delete.on_click(move |_| dialog.end_modal(ID_DELETE));
	dialog.set_escape_id(ID_CANCEL);
	dialog.set_affirmative_id(ID_OK);
	open.set_default();
	let buttons = BoxSizer::builder(Orientation::Horizontal).build();
	for button in [open, delete, later] {
		buttons.add(&button, 0, SizerFlag::Right, padding / 2);
	}
	let content = BoxSizer::builder(Orientation::Vertical).build();
	content.add(&label, 0, SizerFlag::All, padding);
	content.add_sizer(&buttons, 0, SizerFlag::All | SizerFlag::AlignRight, padding);
	dialog.set_sizer_and_fit(content, true);
	dialog.centre();
	open.set_focus();
	match dialog.show_modal() {
		ID_OK => UnsavedChoice::Open,
		ID_DELETE => {
			// TRANSLATORS: Confirmation before deleting the unsaved scans found at start; %d is how many
			let confirm = patois::nt(
				"Delete %d unsaved scan? This can't be undone.",
				"Delete %d unsaved scans? This can't be undone.",
				count_u64,
			)
			.replacen("%d", &count.to_string(), 1);
			if wx_utils::confirm(parent, confirm, &t("Unsaved Scans")) {
				UnsavedChoice::Delete
			} else {
				UnsavedChoice::Later
			}
		}
		_ => UnsavedChoice::Later,
	}
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use super::with_cbz_extension;

	#[test]
	fn cbz_is_added_to_the_name_not_put_in_place_of_its_last_dot() {
		let named = |name: &str| with_cbz_extension(PathBuf::from("/saved").join(name));
		assert_eq!(named("Dr. Smith letter"), PathBuf::from("/saved/Dr. Smith letter.cbz"));
		assert_eq!(named("Report v1.2"), PathBuf::from("/saved/Report v1.2.cbz"));
		assert_eq!(named("Bank letter.cbz"), PathBuf::from("/saved/Bank letter.cbz"));
		assert_eq!(named("Bank letter.CBZ"), PathBuf::from("/saved/Bank letter.CBZ"));
		assert_eq!(named("notes.txt"), PathBuf::from("/saved/notes.txt.cbz"));
	}
}
