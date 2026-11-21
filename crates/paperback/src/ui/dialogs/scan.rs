//! The two windows of File > Scan document: the settings, and the Scanning window that stays open while pages come in. Neither shows anything of the scanner's own; every step is spoken instead.

use std::{cell::RefCell, rc::Rc};

use paperback_scan::{ColorMode, RESOLUTIONS, ScanSettings, ScannerInfo, Source};
use patois::t;
use wxdragon::prelude::*;

use super::{add_ok_cancel_footer, build_ok_cancel_buttons, dialog_padding};
use crate::scanning::{messages, remembered::Remembered};

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
}

const ID_SCAN_NEXT: i32 = ID_HIGHEST + 2;
const ID_RESCAN: i32 = ID_HIGHEST + 3;
const ID_SKIP: i32 = ID_HIGHEST + 4;
const ID_STOP: i32 = ID_HIGHEST + 5;

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
		let done = Button::builder(&dialog).with_id(ID_CANCEL).with_label(&t("&Done")).build();
		dialog.set_escape_id(ID_CANCEL);
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
		Self { dialog, status, next, rescan, skip, stop }
	}

	pub fn say(&self, message: &str) {
		self.status.set_label(message);
		self.status.wrap(480);
		self.dialog.layout();
		live_region::announce(self.status, message);
	}

	/// Only the buttons that mean something now are shown, so Tab never lands on one that would do nothing. Rescan sits beside Scan Next Page for a page that may not be lined up, without taking Enter from it.
	pub fn show_buttons(&self, scanning: bool, asking_blank: bool, can_rescan: bool) {
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
		focus.set_focus();
	}
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
	let path = std::path::PathBuf::from(dialog.get_path()?);
	// The archive is a comic book archive whatever the reader typed, so Paperback and other readers know it.
	Some(if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("cbz")) {
		path
	} else {
		path.with_extension("cbz")
	})
}
