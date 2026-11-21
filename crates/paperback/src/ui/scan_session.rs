//! A File > Scan document run, from choosing the scanner to the last page's text.
//!
//! Three threads: the scanner's, which hands over pages as they come; an OCR thread, which checks and reads each page in order; and the UI thread, which owns the scan's archive and its tab. The OCR thread posts its results with `when_unlocked`, so they land in order and the UI thread never waits on a scanner or the OCR engine. The document opens after the first page and each page's text appears as its OCR finishes, while the Scanning window is still open or after it has closed.

use std::{
	cell::RefCell,
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
	rc::Rc,
	sync::{Arc, Mutex, mpsc},
	thread,
	time::Duration,
};

use paperback_core::{
	config::ConfigManager,
	document::DocPos,
	ocr::{RecognizedLine, save_pages, saved_pages_file},
	scan::{
		archive::{ScanArchive, encode_png},
		check::{TextAmount, Turn},
	},
};
use paperback_scan::{DynamicImage, ErrorKind, ScanSettings, ScanSummary, Scanner, ScannerInfo, Source, SystemScanner};
use patois::t;
use wx_utils::{
	progress::{Ended, run_without_estimate},
	show_error, show_info,
};
use wxdragon::prelude::*;

use crate::{
	ocr::{self, OcrError},
	scanning::{
		messages,
		pages::{self, Mode},
		remembered::Remembered,
		store,
	},
	ui::{
		dialogs::scan::{ScanningWindow, SettingsChoice, choose_save_path, show_scan_settings_dialog},
		document_manager::{DocumentManager, display_title},
		main_window::update_title_from_manager,
		menu,
		navigation::{announce, announce_for_command},
	},
};

thread_local! {
	static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct Shared {
	frame: Frame,
	dm: Rc<Mutex<DocumentManager>>,
	config: Rc<Mutex<ConfigManager>>,
	label: StaticText,
	scanner: Arc<dyn Scanner>,
}

struct Session {
	shared: Shared,
	window: Option<ScanningWindow>,
	scanner_id: String,
	settings: ScanSettings,
	mode: Mode,
	archive: ScanArchive,
	pages: usize,
	recognized: BTreeMap<usize, Vec<RecognizedLine>>,
	/// Named for the archive's contents, so it changes with every page; the old one is removed.
	cache: Option<PathBuf>,
	running: bool,
	/// The flatbed page came out blank, probably face down, and the window waits for Rescan or Skip.
	asking_blank: bool,
	/// A flatbed page that may not be lined up. Rescan replaces it; scanning the next page goes on without it.
	rescan: Option<usize>,
	run_pages: usize,
	run_dropped: usize,
	told_ocr_problem: bool,
	opened: bool,
	/// Pages or text not yet shown in the tab. Re-reading the tab is put off a moment so a run's pages share one re-read, and, once the Scanning window is closed and the reader may be reading, until the run ends.
	dirty: bool,
	flush_scheduled: bool,
}

enum Event {
	/// A page to add (or, for a rescan, to put in place of `number`), already the right way up, with its text.
	Page {
		number: usize,
		png: Vec<u8>,
		replaces: bool,
		lines: Vec<RecognizedLine>,
		amount: TextAmount,
		turn: Option<Turn>,
		/// Whether the page may not be lined up, and if so whether text touches an edge.
		out_of_line: Option<bool>,
		/// A one-sided feeder page with nothing on it, kept because skipping wasn't asked for.
		looks_blank: bool,
		error: Option<OcrError>,
	},
	/// A blank flatbed page, which the window asks about.
	Blank,
	/// A page or side left out because skipping blank pages was asked for.
	Dropped,
	/// Time to show what's arrived in the tab.
	Flush,
	Ended(ScanSummary),
}

/// Pages travel to the OCR thread as PNG, a fraction of their decoded size, so a fast feeder ahead of OCR doesn't pile up decoded pages.
enum ToOcr {
	Page(Vec<u8>),
	End(ScanSummary),
}

#[derive(Debug, PartialEq, Eq)]
enum Refresh {
	Now,
	Soon,
	AlreadyDue,
	AtEnd,
}

/// The first page opens the document at once. While the Scanning window is open, re-reading waits a moment so pages arriving together share it. Once it's closed the reader may be reading in the tab, and re-reading the text under the caret would interrupt the screen reader, so it waits for the run to end and happens once.
fn when_to_refresh(opened: bool, window_open: bool, scheduled: bool) -> Refresh {
	if !opened {
		Refresh::Now
	} else if !window_open {
		Refresh::AtEnd
	} else if scheduled {
		Refresh::AlreadyDue
	} else {
		Refresh::Soon
	}
}

/// Long enough for a feeder's next page to share the re-read, short enough that the tab keeps up.
const FLUSH_DELAY: Duration = Duration::from_millis(800);

/// File > Scan document.
pub fn scan_document(
	frame: Frame,
	dm: &Rc<Mutex<DocumentManager>>,
	config: &Rc<Mutex<ConfigManager>>,
	label: StaticText,
) {
	if SESSION.with_borrow(Option::is_some) {
		announce(label, messages::already_scanning());
		return;
	}
	let shared =
		Shared { frame, dm: Rc::clone(dm), config: Rc::clone(config), label, scanner: Arc::new(SystemScanner::new()) };
	let remembered = Remembered::load(&config.lock().unwrap());
	match remembered.last_scanner() {
		Some(last) => choose(&shared, &[last], &remembered, true),
		None => find_scanners(shared),
	}
}

fn title() -> String {
	// TRANSLATORS: Title of error and information messages from scanning
	t("Scan Document")
}

fn find_scanners(shared: Shared) {
	let scanner = Arc::clone(&shared.scanner);
	let frame = shared.frame;
	run_without_estimate(
		&frame,
		&title(),
		&messages::looking_for_scanners(),
		move |_| scanner.list(),
		move |listed, ended| {
			if ended == Ended::Cancelled {
				return;
			}
			match listed {
				Err(error) if error.kind == ErrorKind::SaneMissing => {
					show_error(&shared.frame, messages::sane_missing(), &title());
				}
				Err(error) => show_error(&shared.frame, messages::scan_failed(&error), &title()),
				Ok(list) if list.is_empty() => show_info(&shared.frame, messages::no_scanners(), &title()),
				Ok(list) => {
					let remembered = Remembered::load(&shared.config.lock().unwrap());
					choose(&shared, &list, &remembered, false);
				}
			}
		},
	);
}

fn choose(shared: &Shared, scanners: &[ScannerInfo], remembered: &Remembered, offer_find: bool) {
	match show_scan_settings_dialog(&shared.frame, scanners, remembered, offer_find) {
		SettingsChoice::Scan { scanner, settings, skip_blank_sides } => {
			begin(shared.clone(), &scanner, settings, skip_blank_sides);
		}
		SettingsChoice::FindScanners => find_scanners(shared.clone()),
		SettingsChoice::Cancel => {}
	}
}

fn begin(shared: Shared, scanner: &ScannerInfo, settings: ScanSettings, skip_blank_sides: bool) {
	{
		let config = shared.config.lock().unwrap();
		let mut remembered = Remembered::load(&config);
		remembered.remember(scanner, settings, skip_blank_sides);
		remembered.save(&config);
	}
	// Skipping is opt-in for both feeder modes: OCR finds no words in a photo, a signature or handwriting, and those pages are worth keeping.
	let mode = match settings.source {
		Source::Flatbed => Mode::Flatbed,
		Source::Feeder if skip_blank_sides => Mode::FeederSkip,
		Source::Feeder => Mode::Feeder,
		Source::FeederDuplex if skip_blank_sides => Mode::BothSidesSmart,
		Source::FeederDuplex => Mode::BothSidesAll,
	};
	// TRANSLATORS: The name a new scanned document is saved under, before a number when there are several
	let archive = match ScanArchive::create(&store::scans_dir(), &t("Scan")) {
		Ok(archive) => archive,
		Err(error) => {
			tracing::warn!(%error, "could not make the folder for scanned documents");
			show_error(
				&shared.frame,
				messages::scan_failed(&paperback_scan::ScanError {
					kind: ErrorKind::Other,
					message: error.to_string(),
				}),
				&title(),
			);
			return;
		}
	};
	let window = ScanningWindow::build(&shared.frame, settings.source.is_feeder());
	bind_buttons(window);
	SESSION.set(Some(Session {
		shared,
		window: Some(window),
		scanner_id: scanner.id.clone(),
		settings,
		mode,
		archive,
		pages: 0,
		recognized: BTreeMap::new(),
		cache: None,
		running: false,
		asking_blank: false,
		rescan: None,
		run_pages: 0,
		run_dropped: 0,
		told_ocr_problem: false,
		opened: false,
		dirty: false,
		flush_scheduled: false,
	}));
	start_run(None);
	window.dialog.show_modal();
	window.dialog.destroy();
	finish();
}

fn bind_buttons(window: ScanningWindow) {
	// A hidden button's mnemonic can still reach it, so each acts only when it's shown.
	window.rescan.on_click(move |_| {
		let target = with_session(|session| {
			if session.running {
				return None;
			}
			if std::mem::take(&mut session.asking_blank) {
				return Some(None);
			}
			session.rescan.take().map(Some)
		});
		if let Some(Some(replace)) = target {
			start_run(replace);
		}
	});
	window.skip.on_click(move |_| {
		with_session(|session| {
			if !std::mem::take(&mut session.asking_blank) {
				return;
			}
			session.show_buttons();
			if let Some(window) = session.window {
				window.say(&messages::flatbed_prompt());
			}
		});
	});
	window.next.on_click(move |_| {
		let go = with_session(|session| {
			let go = !session.running && !session.asking_blank;
			if go {
				session.rescan = None;
			}
			go
		});
		if go == Some(true) {
			start_run(None);
		}
	});
	window.stop.on_click(move |_| {
		with_session(|session| session.shared.scanner.cancel());
	});
}

/// Whether a scan is running or its Scanning window is open.
pub fn is_running() -> bool {
	SESSION.with_borrow(Option::is_some)
}

fn with_session<T>(action: impl FnOnce(&mut Session) -> T) -> Option<T> {
	SESSION.with_borrow_mut(|session| session.as_mut().map(action))
}

/// `replace` is the page a rescan stands in for; otherwise pages are added at the end.
fn start_run(replace: Option<usize>) {
	let Some((scanner, scanner_id, settings, mode, first)) = with_session(|session| {
		if session.running {
			return None;
		}
		session.running = true;
		session.asking_blank = false;
		session.rescan = None;
		session.run_pages = 0;
		session.run_dropped = 0;
		session.show_buttons();
		if let Some(window) = session.window {
			window.say(&messages::scanning());
		}
		Some((
			Arc::clone(&session.shared.scanner),
			session.scanner_id.clone(),
			session.settings,
			session.mode,
			replace.unwrap_or(session.pages + 1),
		))
	})
	.flatten() else {
		return;
	};
	let (to_ocr, from_scanner) = mpsc::channel();
	let reader = thread::Builder::new()
		.name("paperback-scan-ocr".into())
		.spawn(move || read_pages(&from_scanner, first, replace.is_some(), mode));
	if let Err(error) = reader {
		tracing::warn!(%error, "could not start the scan's ocr thread");
		post(Event::Ended(ScanSummary {
			pages: 0,
			error: Some(paperback_scan::ScanError { kind: ErrorKind::Other, message: error.to_string() }),
		}));
		return;
	}
	let run = move || {
		let summary = scanner.scan(&scanner_id, &settings, &mut |page| {
			// Encoded here, while the driver goes on with the next page in its own thread (or its own buffers, for SANE).
			match encode_png(&page) {
				Ok(png) => {
					drop(page);
					let _ = to_ocr.send(ToOcr::Page(png));
				}
				Err(error) => tracing::warn!(%error, "a scanned page could not be encoded"),
			}
		});
		let _ = to_ocr.send(ToOcr::End(summary));
	};
	if with_session(|session| session.shared.scanner.needs_main_thread()).unwrap_or(false) {
		// After the window is up, since a main-thread scan holds the event loop until it ends.
		wx_utils::post_to_ui(run);
	} else if let Err(error) = thread::Builder::new().name("paperback-scan".into()).spawn(run) {
		tracing::warn!(%error, "could not start the scan thread");
	}
}

type Recognize<'a> = dyn FnMut(&DynamicImage) -> Result<pages::Reading, OcrError> + 'a;

/// On the OCR thread: each page in the order scanned, numbered as the UI thread will number it.
fn read_pages(from_scanner: &mpsc::Receiver<ToOcr>, first: usize, replaces: bool, mode: Mode) {
	sort_pages(from_scanner, first, replaces, mode, &mut pages::recognize_with_engine, &mut post);
}

/// [`read_pages`] with the OCR engine and the way out passed in, so tests can run a fake scanner through it. A page is read before it's added, so it goes into the document once, already the right way up, with its text.
fn sort_pages(
	from_scanner: &mpsc::Receiver<ToOcr>,
	first: usize,
	replaces: bool,
	mode: Mode,
	recognize: &mut Recognize<'_>,
	post: &mut dyn FnMut(Event),
) {
	let mut number = first;
	for message in from_scanner {
		let png = match message {
			ToOcr::Page(png) => png,
			ToOcr::End(summary) => {
				post(Event::Ended(summary));
				return;
			}
		};
		// Our own encoding of a page the scanner library already checked.
		let page = match image::load_from_memory(&png) {
			Ok(page) => page,
			Err(error) => {
				tracing::warn!(%error, "a scanned page could not be decoded again");
				continue;
			}
		};
		let kept = match mode {
			Mode::Flatbed if pages::is_blank(&page) => {
				post(Event::Blank);
				false
			}
			Mode::Flatbed => {
				post(page_event(number, png, replaces, pages::read(&page, recognize), false));
				true
			}
			Mode::Feeder | Mode::BothSidesAll => {
				let blank = pages::is_blank(&page);
				let read = if blank { pages::ReadPage::default() } else { pages::read(&page, recognize) };
				post(page_event(number, png, false, read, blank && mode == Mode::Feeder));
				true
			}
			Mode::FeederSkip | Mode::BothSidesSmart => {
				let read = (!pages::is_blank(&page)).then(|| pages::read(&page, recognize));
				if let Some(read) = read.filter(|read| pages::side_has_text(read, &page)) {
					post(page_event(number, png, false, read, false));
					true
				} else {
					post(Event::Dropped);
					false
				}
			}
		};
		// A rescan is one flatbed page, so only pages added at the end move the count on.
		if kept && !replaces {
			number += 1;
		}
	}
}

fn page_event(number: usize, png: Vec<u8>, replaces: bool, read: pages::ReadPage, looks_blank: bool) -> Event {
	let png = match read.turned.as_ref().map(encode_png) {
		Some(Ok(turned)) => turned,
		Some(Err(error)) => {
			tracing::warn!(%error, "a turned page could not be encoded; keeping it as scanned");
			png
		}
		None => png,
	};
	// An engine that failed says nothing about how the page lies.
	let out_of_line =
		(read.error.is_none() && !looks_blank && (read.little_text || read.cut_off)).then_some(read.cut_off);
	Event::Page {
		number,
		png,
		replaces,
		lines: read.lines,
		amount: read.amount,
		turn: read.turn,
		out_of_line,
		looks_blank,
		error: read.error,
	}
}

fn post(event: Event) {
	crate::ui::app::when_unlocked(move |_| handle(event));
}

fn handle(event: Event) {
	let ended_and_closed = SESSION.with_borrow_mut(|slot| {
		let Some(session) = slot.as_mut() else { return false };
		session.apply(event);
		!session.running && session.window.is_none()
	});
	if ended_and_closed {
		SESSION.set(None);
	}
}

impl Session {
	fn feeder(&self) -> bool {
		self.settings.source.is_feeder()
	}

	fn show_buttons(&self) {
		if let Some(window) = self.window {
			window.show_buttons(self.running, self.asking_blank, self.rescan.is_some());
		}
	}

	fn apply(&mut self, event: Event) {
		// Closed without saving, and so deleted: what's still coming has nowhere to go, and must not bring the scan back.
		if self.pages > 0 && !self.archive.path().exists() {
			return;
		}
		match event {
			Event::Page { number, png, replaces, lines, amount, turn, out_of_line, looks_blank, error } => {
				let stored = if replaces {
					self.archive.replace_png(number, &png)
				} else {
					self.archive.append_png(&png).map(|_| ())
				};
				if let Err(error) = stored {
					tracing::warn!(%error, "a scanned page could not be saved");
					let error = paperback_scan::ScanError { kind: ErrorKind::Other, message: error.to_string() };
					self.say(&messages::scan_failed(&error), false);
					return;
				}
				if !replaces {
					self.pages += 1;
					self.run_pages += 1;
				}
				self.recognized.insert(number, lines);
				self.mark_dirty();
				let message = self.page_message(number, amount, turn, out_of_line, looks_blank, error);
				self.say(&message, self.feeder());
			}
			Event::Dropped => self.run_dropped += 1,
			Event::Blank => {
				self.asking_blank = true;
				self.show_buttons();
				self.say(&messages::blank_on_flatbed(self.pages + 1), false);
			}
			Event::Flush => {
				self.flush_scheduled = false;
				// Once the window is closed the reader may be reading, so the tab waits for the run to end.
				if self.dirty && self.window.is_some() {
					self.flush();
				}
			}
			Event::Ended(summary) => {
				self.running = false;
				if self.dirty {
					self.flush();
				}
				self.show_buttons();
				self.summarize(summary);
			}
		}
	}

	fn page_message(
		&mut self,
		number: usize,
		amount: TextAmount,
		turn: Option<Turn>,
		out_of_line: Option<bool>,
		looks_blank: bool,
		error: Option<OcrError>,
	) -> String {
		let mut message = messages::page_read(number, amount, turn);
		if let Some(error) = error
			&& !self.told_ocr_problem
		{
			self.told_ocr_problem = true;
			message = format!("{message} {}", ocr::message(&error));
		}
		if looks_blank {
			message = format!("{message} {}", messages::looks_blank());
		}
		if let Some(cut_off) = out_of_line {
			// Said without stopping: Enter still scans the next page, and Rescan is there for this one.
			let can_rescan = !self.feeder() && self.window.is_some();
			message = format!("{message} {}", messages::not_lined_up(cut_off, can_rescan));
			if can_rescan {
				self.rescan = Some(number);
				self.show_buttons();
			}
		} else if !self.feeder() && self.pages == 1 {
			message = format!("{message} {}", messages::flatbed_prompt());
		}
		message
	}

	fn summarize(&self, summary: ScanSummary) {
		let skipping = matches!(self.mode, Mode::FeederSkip | Mode::BothSidesSmart);
		let finished = || match self.mode {
			Mode::BothSidesSmart => {
				let sheets = (self.run_pages + self.run_dropped).div_ceil(2);
				messages::both_sides_finished(sheets, self.run_pages, self.run_dropped)
			}
			_ => messages::feeder_finished(self.run_pages, if skipping { self.run_dropped } else { 0 }),
		};
		match summary.error {
			Some(error) if self.feeder() && error.kind == ErrorKind::Other && self.run_pages > 0 => {
				// A feeder that jams after some pages keeps them; say both.
				self.say(&format!("{} {}", messages::scan_failed(&error), finished()), true);
			}
			Some(error) => self.say(&messages::scan_failed(&error), false),
			None if skipping && self.run_pages == 0 && self.run_dropped > 0 => self.say(&messages::all_blank(), true),
			None if self.feeder() => self.say(&finished(), true),
			None => {}
		}
	}

	fn mark_dirty(&mut self) {
		self.dirty = true;
		match when_to_refresh(self.opened, self.window.is_some(), self.flush_scheduled) {
			Refresh::Now => self.flush(),
			Refresh::Soon => {
				self.flush_scheduled = true;
				let spawned = thread::Builder::new().name("paperback-scan-flush".into()).spawn(|| {
					thread::sleep(FLUSH_DELAY);
					post(Event::Flush);
				});
				if spawned.is_err() {
					self.flush_scheduled = false;
					self.flush();
				}
			}
			Refresh::AlreadyDue | Refresh::AtEnd => {}
		}
	}

	/// In the Scanning window while it's open, else through the main window's announcements. `queue` waits for what's being said, as a feeder's pages should.
	fn say(&self, message: &str, queue: bool) {
		match self.window {
			Some(window) if queue => {
				window.status.set_label(message);
				live_region::announce_with_priority(window.status, message, live_region::Priority::Low);
			}
			Some(window) => window.say(message),
			None => announce(self.shared.label, message),
		}
	}

	/// The recognized text is kept where reopening the document looks for it, under the archive's current contents, and the tab re-read once for everything that arrived since the last time.
	fn flush(&mut self) {
		self.dirty = false;
		let path = self.archive.path().to_path_buf();
		if !path.exists() {
			return;
		}
		let cache = saved_pages_file(&ocr::saved_pages_dir(), &path);
		if let Some(old) = self.cache.replace(cache.clone()) {
			let _ = fs::remove_file(old);
		}
		let _ = fs::remove_file(&cache);
		let pages: Vec<(i32, Vec<RecognizedLine>)> = self
			.recognized
			.iter()
			.filter_map(|(page, lines)| Some((i32::try_from(*page).ok()?, lines.clone())))
			.collect();
		if let Err(error) = save_pages(&cache, &pages) {
			tracing::warn!(%error, "could not keep a scanned document's text");
		}
		let dm = Rc::clone(&self.shared.dm);
		let shown = dm.lock().unwrap().show_scan(&dm, &path);
		if shown && !self.opened {
			self.opened = true;
			let manager = dm.lock().unwrap();
			update_title_from_manager(&self.shared.frame, &manager);
			drop(manager);
			let menu_bar = menu::create_menu_bar(&self.shared.config.lock().unwrap());
			self.shared.frame.set_menu_bar(menu_bar);
			menu::update_menu_item_states(&self.shared.frame, true);
		}
		// Opening a tab can take the focus from the Scanning window, which still has the reader's next step.
		if let Some(window) = self.window {
			window.dialog.raise();
		}
	}
}

/// File > Save Scan As. A scan in progress is replaced by the saved copy, which then opens where the reader was; a scan saved before is copied.
pub fn save_scan_as(frame: Frame, dm: &Rc<Mutex<DocumentManager>>, label: StaticText, from_keyboard: bool) {
	let Some((source, document_title, position)) = dm.lock().unwrap().active_tab().map(|tab| {
		(tab.local_path().to_path_buf(), display_title(tab), tab.window.to_doc(tab.text_ctrl.get_insertion_point()))
	}) else {
		return;
	};
	if !paperback_core::scan::archive::is_scan(&source) {
		// TRANSLATORS: Announced when Save Scan As is chosen in a document that wasn't scanned with Paperback
		announce_for_command(label, from_keyboard, t("Save Scan As is for scanned documents."));
		return;
	}
	let Some(destination) = choose_save_path(&frame, &document_title) else { return };
	if let Err(error) = save_to(dm, &source, &destination, position) {
		show_error(&frame, error, &title());
		return;
	}
	update_title_from_manager(&frame, &dm.lock().unwrap());
	let name = destination.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
	// TRANSLATORS: Announced after a scanned document is saved; %s is the file name it was saved as
	announce(label, t("Saved as %s.").replacen("%s", &name, 1));
}

/// Saves `source` as `destination`; a scan in progress is then discarded and the saved copy opened in its place, at `position`.
pub(crate) fn save_to(
	dm: &Rc<Mutex<DocumentManager>>,
	source: &Path,
	destination: &Path,
	position: DocPos,
) -> Result<(), String> {
	if store::same_file(source, destination) {
		return Ok(());
	}
	store::save_copy(source, destination).map_err(|error| {
		tracing::warn!(%error, "could not save a scan");
		// TRANSLATORS: Shown when a scanned document can't be saved; %s is the reason
		t("The scan could not be saved: %s").replacen("%s", &error.to_string(), 1)
	})?;
	if !store::is_in_progress(source) {
		return Ok(());
	}
	let mut manager = dm.lock().unwrap();
	if let Some(index) = manager.find_tab_by_path(source) {
		manager.close_document_without_asking(index, false);
	}
	store::discard(source, &manager.config.lock().unwrap());
	if manager.open_file(dm, destination) {
		manager.place_caret(position);
	}
	Ok(())
}

/// The Scanning window has closed: a scan still running is stopped, and the reader is taken to the document.
fn finish() {
	let Some((dm, label, pages, running)) = with_session(|session| {
		session.window = None;
		if session.running {
			session.shared.scanner.cancel();
		} else if session.dirty {
			session.flush();
		}
		(Rc::clone(&session.shared.dm), session.shared.label, session.pages, session.running)
	}) else {
		return;
	};
	if !running {
		SESSION.set(None);
	}
	if pages == 0 {
		return;
	}
	let manager = dm.lock().unwrap();
	manager.focus_document_text();
	drop(manager);
	announce(label, messages::finished(pages));
}

#[cfg(test)]
mod tests {
	use image::{GrayImage, Luma};
	use paperback_core::ocr::RecognizedWord;
	use paperback_scan::{
		ScanSettings, Scanner, ScannerInfo, Source,
		fake::{FakeScanner, Script},
	};

	use super::*;

	const TEXT: &str = "The committee met on Tuesday to review the proposal and agreed that the plan should move forward after small changes were made to the budget section";

	/// A printed page reads as [`TEXT`] when its top-left pixel is black; any other way up, or a page marked gray, reads as scraps.
	fn sheet_side(marker: u8, turned_over: bool) -> DynamicImage {
		let mut image = GrayImage::from_pixel(200, 300, Luma([240]));
		if marker < 240 {
			for y in (60..240).step_by(12) {
				for x in 30..170 {
					for dy in 0..5 {
						image.put_pixel(x, y + dy, Luma([25]));
					}
				}
			}
		}
		image.put_pixel(0, 0, Luma([marker]));
		let page = DynamicImage::ImageLuma8(image);
		if turned_over { page.rotate180() } else { page }
	}

	fn printed() -> DynamicImage {
		sheet_side(0, false)
	}

	fn blank() -> DynamicImage {
		sheet_side(240, false)
	}

	/// Ink on the page, but nothing that reads as words: show-through from the printed side.
	fn show_through() -> DynamicImage {
		sheet_side(128, false)
	}

	#[allow(clippy::unnecessary_wraps, reason = "stands in for the OCR engine, which can fail")]
	fn engine(image: &DynamicImage) -> Result<pages::Reading, OcrError> {
		let text = if image.to_luma8().get_pixel(0, 0).0[0] == 0 { TEXT } else { "' ~ 1l ." };
		let words = text
			.split(' ')
			.enumerate()
			.map(|(i, word)| RecognizedWord {
				text: word.into(),
				x: ((i % 10) as f32).mul_add(14.0, 30.0),
				y: ((i / 10) as f32).mul_add(12.0, 60.0),
				width: 12.0,
				height: 6.0,
			})
			.collect();
		Ok((vec![RecognizedLine { text: text.into(), words }], image.width(), image.height()))
	}

	/// Runs the fake scanner's pages through the OCR thread's sorting, as a real run does (encoded to PNG on the way), and returns what it would post.
	fn run(mode: Mode, sides: Vec<DynamicImage>) -> Vec<Event> {
		let info = ScannerInfo {
			id: "fake".into(),
			name: "Fake".into(),
			sources: vec![Source::Feeder, Source::FeederDuplex],
			problem: None,
		};
		let scanner = FakeScanner::new(vec![info]).with_script(Script::pages(sides));
		let (to_ocr, from_scanner) = mpsc::channel();
		let settings = ScanSettings { source: Source::FeederDuplex, ..ScanSettings::default() };
		let summary =
			scanner.scan("fake", &settings, &mut |page| to_ocr.send(ToOcr::Page(encode_png(&page).unwrap())).unwrap());
		to_ocr.send(ToOcr::End(summary)).unwrap();
		let mut events = Vec::new();
		sort_pages(&from_scanner, 1, false, mode, &mut engine, &mut |event| events.push(event));
		events
	}

	fn added(events: &[Event]) -> Vec<usize> {
		events
			.iter()
			.filter_map(|event| if let Event::Page { number, .. } = event { Some(*number) } else { None })
			.collect()
	}

	fn count(events: &[Event], matches: fn(&Event) -> bool) -> usize {
		events.iter().filter(|event| matches(event)).count()
	}

	/// Three sheets: printed front with a blank back; a sheet in face down, whose printed side comes second and upside down, after show-through; and one printed on both sides.
	fn three_sheets() -> Vec<DynamicImage> {
		vec![printed(), blank(), show_through(), sheet_side(0, true), printed(), printed()]
	}

	#[test]
	fn the_tab_is_re_read_once_for_pages_arriving_together_and_not_while_the_reader_reads() {
		assert_eq!(when_to_refresh(false, true, false), Refresh::Now, "the document opens after the first page");
		assert_eq!(when_to_refresh(true, true, false), Refresh::Soon);
		assert_eq!(when_to_refresh(true, true, true), Refresh::AlreadyDue, "a second page shares the pending re-read");
		assert_eq!(when_to_refresh(true, false, false), Refresh::AtEnd, "after Done, once, when the run ends");
	}

	#[test]
	fn skipping_blank_sides_keeps_only_the_sides_with_text() {
		let events = run(Mode::BothSidesSmart, three_sheets());
		assert_eq!(added(&events), vec![1, 2, 3, 4]);
		assert_eq!(count(&events, |event| matches!(event, Event::Dropped)), 2);
		assert_eq!(count(&events, |event| matches!(event, Event::Blank)), 0, "nothing is announced for a dropped side");
		assert!(matches!(events.last(), Some(Event::Ended(_))));
	}

	#[test]
	fn a_face_down_sheet_s_printed_side_is_kept_the_right_way_up() {
		let events = run(Mode::BothSidesSmart, three_sheets());
		let turns: Vec<Option<Turn>> = events
			.iter()
			.filter_map(|event| if let Event::Page { turn, .. } = event { Some(*turn) } else { None })
			.collect();
		assert_eq!(turns, vec![None, Some(Turn::Half), None, None]);
		let Some(Event::Page { png, .. }) = events.iter().filter(|event| matches!(event, Event::Page { .. })).nth(1)
		else {
			panic!("no second page")
		};
		let stored = image::load_from_memory(png).unwrap();
		assert_eq!(stored.to_luma8().get_pixel(0, 0).0[0], 0, "the page is stored turned the right way up");
	}

	#[test]
	fn two_sided_scanning_keeps_every_side_by_default() {
		let events = run(Mode::BothSidesAll, three_sheets());
		assert_eq!(added(&events), vec![1, 2, 3, 4, 5, 6]);
		assert_eq!(count(&events, |event| matches!(event, Event::Blank | Event::Dropped)), 0);
	}

	#[test]
	fn one_sided_feeding_keeps_a_blank_page_by_default_and_says_it_looks_blank() {
		let events = run(Mode::Feeder, vec![printed(), blank(), printed()]);
		assert_eq!(added(&events), vec![1, 2, 3]);
		let flagged: Vec<bool> = events
			.iter()
			.filter_map(|event| if let Event::Page { looks_blank, .. } = event { Some(*looks_blank) } else { None })
			.collect();
		assert_eq!(flagged, vec![false, true, false]);
	}

	#[test]
	fn one_sided_feeding_with_skipping_leaves_out_pages_without_text() {
		let events = run(Mode::FeederSkip, vec![printed(), blank(), show_through(), printed()]);
		assert_eq!(added(&events), vec![1, 2]);
		assert_eq!(count(&events, |event| matches!(event, Event::Dropped)), 2);
	}

	#[test]
	fn a_page_arrives_once_with_its_text_and_the_right_way_up() {
		let events = run(Mode::Feeder, vec![sheet_side(0, true)]);
		assert_eq!(events.len(), 2, "one page event and the end");
		assert!(matches!(events[0], Event::Page { number: 1, turn: Some(Turn::Half), .. }));
		assert!(matches!(events[1], Event::Ended(_)));
	}
}
