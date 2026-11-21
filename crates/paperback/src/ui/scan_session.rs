//! A File > Scan document run, from choosing the scanner to the last page's text.
//!
//! Three threads: the scanner's, which hands over pages as they come; an OCR thread, which checks and reads each page in order; and the UI thread, which owns the scan's archive and its tab. The OCR thread posts its results with `when_unlocked`, so they land in order and the UI thread never waits on a scanner or the OCR engine. The document opens after the first page and each page's text appears as its OCR finishes, while the Scanning window is still open or after it has closed.

use std::{
	cell::RefCell,
	collections::BTreeMap,
	fs,
	path::PathBuf,
	rc::Rc,
	sync::{Arc, Mutex, mpsc},
	thread,
};

use paperback_core::{
	config::ConfigManager,
	ocr::{RecognizedLine, save_pages, saved_pages_file},
	scan::{
		archive::{ScanArchive, encode_png},
		check::Turn,
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
	config_ext::config_dir,
	ocr::{self, OcrError},
	scanning::{
		messages,
		pages::{self, Mode},
		remembered::Remembered,
	},
	ui::{
		dialogs::scan::{ScanningWindow, SettingsChoice, show_scan_settings_dialog},
		document_manager::DocumentManager,
		main_window::{MainWindow, update_title_from_manager},
		menu,
		navigation::announce,
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

/// A question the Scanning window is waiting on, about the page scanned last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asking {
	/// Not added: probably face down.
	Blank,
	/// Added, but it may be out of line; Rescan replaces it.
	Alignment(usize),
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
	asking: Option<Asking>,
	run_pages: usize,
	run_blanks: usize,
	run_dropped: usize,
	told_ocr_problem: bool,
	opened: bool,
}

enum Event {
	Added {
		number: usize,
		png: Vec<u8>,
		replaces: bool,
	},
	Turned {
		number: usize,
		png: Vec<u8>,
	},
	Read {
		number: usize,
		lines: Vec<RecognizedLine>,
		words: usize,
		turn: Option<Turn>,
		out_of_line: Option<bool>,
		error: Option<OcrError>,
	},
	Blank,
	/// A side of a two-sided scan with no text on it.
	Dropped,
	Ended(ScanSummary),
}

enum ToOcr {
	Page(DynamicImage),
	End(ScanSummary),
}

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
		SettingsChoice::Scan { scanner, settings, keep_all_sides } => {
			begin(shared.clone(), &scanner, settings, keep_all_sides);
		}
		SettingsChoice::FindScanners => find_scanners(shared.clone()),
		SettingsChoice::Cancel => {}
	}
}

fn begin(shared: Shared, scanner: &ScannerInfo, settings: ScanSettings, keep_all_sides: bool) {
	{
		let config = shared.config.lock().unwrap();
		let mut remembered = Remembered::load(&config);
		remembered.remember(scanner, settings, keep_all_sides);
		remembered.save(&config);
	}
	let mode = match settings.source {
		Source::Flatbed => Mode::Flatbed,
		Source::Feeder => Mode::Feeder,
		Source::FeederDuplex if keep_all_sides => Mode::BothSidesAll,
		Source::FeederDuplex => Mode::BothSidesSmart,
	};
	// TRANSLATORS: The name a new scanned document is saved under, before a number when there are several
	let archive = match ScanArchive::create(&config_dir().join("scans"), &t("Scan")) {
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
		asking: None,
		run_pages: 0,
		run_blanks: 0,
		run_dropped: 0,
		told_ocr_problem: false,
		opened: false,
	}));
	start_run(None);
	window.dialog.show_modal();
	window.dialog.destroy();
	finish();
}

fn bind_buttons(window: ScanningWindow) {
	// A hidden button's mnemonic can still reach it, so each acts only while its question is open.
	window.rescan.on_click(move |_| {
		let Some(asking) = with_session(|session| session.asking.take()).flatten() else { return };
		start_run(match asking {
			Asking::Alignment(number) => Some(number),
			Asking::Blank => None,
		});
	});
	window.skip.on_click(move |_| {
		with_session(|session| {
			if session.asking.take().is_none() {
				return;
			}
			if let Some(window) = session.window {
				window.show_buttons(false, false);
				window.say(&messages::flatbed_prompt());
			}
		});
	});
	window.next.on_click(move |_| {
		if with_session(|session| session.asking.is_none()).unwrap_or(false) {
			start_run(None);
		}
	});
	window.stop.on_click(move |_| {
		with_session(|session| session.shared.scanner.cancel());
	});
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
		session.asking = None;
		session.run_pages = 0;
		session.run_blanks = 0;
		session.run_dropped = 0;
		if let Some(window) = session.window {
			window.show_buttons(true, false);
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
			let _ = to_ocr.send(ToOcr::Page(page));
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

/// [`read_pages`] with the OCR engine and the way out passed in, so tests can run a fake scanner through it.
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
		let page = match message {
			ToOcr::Page(page) => page,
			ToOcr::End(summary) => {
				post(Event::Ended(summary));
				return;
			}
		};
		let kept = match mode {
			Mode::Flatbed | Mode::Feeder => read_then_add(&page, number, replaces, false, recognize, post),
			Mode::BothSidesAll => read_then_add(&page, number, false, true, recognize, post),
			Mode::BothSidesSmart => add_if_it_has_text(&page, number, recognize, post),
		};
		// A rescan is one flatbed page, so only pages added at the end move the count on.
		if kept && (!replaces || mode != Mode::Flatbed) {
			number += 1;
		}
	}
}

/// The page goes into the document before OCR, so it can be read as an image while its text is coming. Returns whether it was added.
fn read_then_add(
	page: &DynamicImage,
	number: usize,
	replaces: bool,
	keep_blank: bool,
	recognize: &mut Recognize<'_>,
	post: &mut dyn FnMut(Event),
) -> bool {
	if !keep_blank && pages::is_blank(page) {
		post(Event::Blank);
		return false;
	}
	let Some(png) = png_of(page) else { return false };
	post(Event::Added { number, png, replaces });
	let read = pages::read(page, recognize);
	if let Some(png) = read.turned.as_ref().and_then(png_of) {
		post(Event::Turned { number, png });
	}
	post(read_event(number, read));
	true
}

/// One side of a two-sided scan, kept only if OCR finds real words on it, the right way up.
fn add_if_it_has_text(
	page: &DynamicImage,
	number: usize,
	recognize: &mut Recognize<'_>,
	post: &mut dyn FnMut(Event),
) -> bool {
	if pages::is_blank(page) {
		post(Event::Dropped);
		return false;
	}
	let read = pages::read(page, recognize);
	if !pages::side_has_text(&read, page) {
		post(Event::Dropped);
		return false;
	}
	let Some(png) = png_of(read.turned.as_ref().unwrap_or(page)) else { return false };
	post(Event::Added { number, png, replaces: false });
	post(read_event(number, read));
	true
}

fn png_of(page: &DynamicImage) -> Option<Vec<u8>> {
	encode_png(page).map_err(|error| tracing::warn!(%error, "a scanned page could not be encoded")).ok()
}

fn read_event(number: usize, read: pages::ReadPage) -> Event {
	// An engine that failed says nothing about how the page lies.
	let out_of_line = (read.error.is_none() && (read.little_text || read.cut_off)).then_some(read.cut_off);
	Event::Read { number, lines: read.lines, words: read.words, turn: read.turn, out_of_line, error: read.error }
}

fn post(event: Event) {
	crate::ui::app::when_unlocked(move |window| handle(window, event));
}

fn handle(window: &'static MainWindow, event: Event) {
	let ended_and_closed = SESSION.with_borrow_mut(|slot| {
		let Some(session) = slot.as_mut() else { return false };
		session.apply(window, event);
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

	fn apply(&mut self, window: &'static MainWindow, event: Event) {
		match event {
			Event::Added { number, png, replaces } => {
				let stored = if replaces {
					self.archive.replace_png(number, &png)
				} else {
					self.archive.append_png(&png).map(|_| ())
				};
				if let Err(error) = stored {
					tracing::warn!(%error, "a scanned page could not be saved");
					self.say(
						&messages::scan_failed(&paperback_scan::ScanError {
							kind: ErrorKind::Other,
							message: error.to_string(),
						}),
						false,
					);
					return;
				}
				if replaces {
					self.recognized.remove(&number);
				} else {
					self.pages += 1;
					self.run_pages += 1;
				}
				self.refresh(window);
			}
			Event::Turned { number, png } => {
				if let Err(error) = self.archive.replace_png(number, &png) {
					tracing::warn!(%error, "a turned page could not be saved");
				}
				self.refresh(window);
			}
			Event::Read { number, lines, words, turn, out_of_line, error } => {
				self.recognized.insert(number, lines);
				self.refresh(window);
				let mut message = messages::page_read(number, words, turn);
				if let Some(error) = error
					&& !self.told_ocr_problem
				{
					self.told_ocr_problem = true;
					message = format!("{message} {}", ocr::message(&error));
				}
				if let Some(cut_off) = out_of_line {
					let can_rescan = !self.feeder() && self.window.is_some();
					message = format!("{message} {}", messages::not_lined_up(cut_off, can_rescan));
					if can_rescan {
						self.asking = Some(Asking::Alignment(number));
					}
				} else if !self.feeder() && self.pages == 1 {
					message = format!("{message} {}", messages::flatbed_prompt());
				}
				self.say(&message, self.feeder());
			}
			Event::Dropped => self.run_dropped += 1,
			Event::Blank => {
				if self.feeder() {
					self.run_blanks += 1;
					self.say(&messages::blank_left_out(), true);
				} else {
					self.asking = Some(Asking::Blank);
					self.say(&messages::blank_on_flatbed(self.pages + 1), false);
				}
			}
			Event::Ended(summary) => {
				self.running = false;
				let asking = self.asking.is_some();
				if let Some(window) = self.window {
					window.show_buttons(false, asking);
				}
				match summary.error {
					Some(error) if self.feeder() && error.kind == ErrorKind::Other && self.run_pages > 0 => {
						// A feeder that jams after some pages keeps them; say both.
						let message = format!(
							"{} {}",
							messages::scan_failed(&error),
							messages::feeder_finished(self.run_pages, self.run_blanks)
						);
						self.say(&message, true);
					}
					Some(error) => self.say(&messages::scan_failed(&error), false),
					None if self.mode == Mode::BothSidesSmart => {
						let sheets = (self.run_pages + self.run_dropped).div_ceil(2);
						self.say(&messages::both_sides_finished(sheets, self.run_pages, self.run_dropped), true);
					}
					None if self.feeder() && self.run_pages == 0 && self.run_blanks > 0 => {
						self.say(&messages::all_blank(), true);
					}
					None if self.feeder() => {
						self.say(&messages::feeder_finished(self.run_pages, self.run_blanks), true);
					}
					None => {}
				}
			}
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

	/// The recognized text is kept where reopening the document looks for it, under the archive's current contents, and the tab re-read.
	fn refresh(&mut self, window: &'static MainWindow) {
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
		let dm = window.document_manager();
		let shown = dm.lock().unwrap().show_scan(dm, &path);
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

/// The Scanning window has closed: a scan still running is stopped, and the reader is taken to the document.
fn finish() {
	let Some((dm, label, pages, running)) = with_session(|session| {
		session.window = None;
		if session.running {
			session.shared.scanner.cancel();
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

	/// Runs the fake scanner's pages through the OCR thread's sorting, as a real run does, and returns what it would post.
	fn run(mode: Mode, sides: Vec<DynamicImage>) -> Vec<Event> {
		let info = ScannerInfo {
			id: "fake".into(),
			name: "Fake".into(),
			sources: vec![Source::FeederDuplex, Source::Feeder],
			problem: None,
		};
		let scanner = FakeScanner::new(vec![info]).with_script(Script::pages(sides));
		let (to_ocr, from_scanner) = mpsc::channel();
		let settings = ScanSettings { source: Source::FeederDuplex, ..ScanSettings::default() };
		let summary = scanner.scan("fake", &settings, &mut |page| to_ocr.send(ToOcr::Page(page)).unwrap());
		to_ocr.send(ToOcr::End(summary)).unwrap();
		let mut events = Vec::new();
		sort_pages(&from_scanner, 1, false, mode, &mut engine, &mut |event| events.push(event));
		events
	}

	fn added(events: &[Event]) -> Vec<usize> {
		events
			.iter()
			.filter_map(|event| if let Event::Added { number, .. } = event { Some(*number) } else { None })
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
	fn both_sides_keeps_only_the_sides_with_text() {
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
			.filter_map(|event| if let Event::Read { turn, .. } = event { Some(*turn) } else { None })
			.collect();
		assert_eq!(turns, vec![None, Some(Turn::Half), None, None]);
		// Added already turned, so nothing has to be replaced afterwards.
		assert_eq!(count(&events, |event| matches!(event, Event::Turned { .. })), 0);
	}

	#[test]
	fn keeping_all_sides_keeps_blank_ones_too() {
		let events = run(Mode::BothSidesAll, three_sheets());
		assert_eq!(added(&events), vec![1, 2, 3, 4, 5, 6]);
		assert_eq!(count(&events, |event| matches!(event, Event::Blank | Event::Dropped)), 0);
	}

	#[test]
	fn one_sided_feeding_leaves_out_blank_pages_and_says_so() {
		let events = run(Mode::Feeder, vec![printed(), blank(), printed()]);
		assert_eq!(added(&events), vec![1, 2]);
		assert_eq!(count(&events, |event| matches!(event, Event::Blank)), 1);
	}

	#[test]
	fn a_page_is_in_the_document_before_its_text_arrives() {
		let events = run(Mode::Feeder, vec![sheet_side(0, true)]);
		assert!(matches!(events[0], Event::Added { number: 1, .. }));
		assert!(matches!(events[1], Event::Turned { number: 1, .. }));
		assert!(matches!(events[2], Event::Read { number: 1, turn: Some(Turn::Half), .. }));
	}
}
