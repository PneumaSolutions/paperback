//! A File > Scan document run, from choosing the scanner to the last page's text.
//!
//! Three threads per run: the scanner's, which hands over pages as they come; an OCR thread, which checks and reads each page in order and adds it to the archive; and the UI thread, which owns the tab and speaks. The OCR thread posts what it did with `when_unlocked`, so results land in order and the UI thread never waits on a scanner, the OCR engine or the disk. Pages travel from the scanner to the OCR thread through a queue a few pages deep: when OCR falls behind, the scanner waits (pagescan makes the driver wait too), so memory stays bounded however long the feeder run. The document opens after the first page; later re-reads are parsed off the UI thread and only installed on it.
//!
//! A run is over only when the OCR thread has finished with every page the scanner handed over. Until then the archive belongs to the run: Save Scan As and closing the tab are refused with a spoken reason, and quitting asks first.

use std::{
	cell::RefCell,
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
	rc::Rc,
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, AtomicUsize, Ordering},
		mpsc,
	},
	thread,
	time::{Duration, Instant},
};

use paperback_core::{
	config::ConfigManager,
	document::DocPos,
	ocr::{RecognizedLine, encode_pages, load_pages, save_pages, saved_pages_file},
	scan::{
		archive::{ScanArchive, encode_png},
		check::{TextAmount, Turn},
	},
	session::DocumentSession,
};
use patois::t;
use wx_utils::{
	progress::{Ended, run_without_estimate},
	show_error, show_info,
};
use wxdragon::prelude::*;

use crate::{
	ocr::{self, OcrError, saved_pages_dir},
	scanning::{
		messages,
		pages::{self, Mode},
		remembered::Remembered,
		scanner::{
			DynamicImage, ErrorKind, ScanError, ScanSettings, ScanSummary, Scanner, ScannerInfo, Source, SystemScanner,
		},
		store::{self, Recovery},
	},
	ui::{
		dialogs::scan::{
			Buttons, ScanningWindow, SettingsChoice, UnsavedChoice, ask_about_unsaved_scans, choose_save_path,
			show_scan_settings_dialog,
		},
		document_manager::{DocumentManager, display_title, parse_scan, scan_parse_settings},
		main_window::update_title_from_manager,
		menu,
		navigation::{announce, announce_for_command},
	},
};

thread_local! {
	static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// Pages waiting between the scanner and OCR. Enough to keep a feeder moving through a slow page; past it the scanner waits.
const QUEUE_DEPTH: usize = 3;

/// A feeder's pages aren't each announced; how often "N pages so far" is said instead.
const PROGRESS_EVERY: Duration = Duration::from_secs(5);

/// Pages in a row with ink but no readable text before the OCR language is mentioned.
const NO_TEXT_STREAK: usize = 3;

/// A Stop this soon after a run starts is a second Enter pressed too early, not a decision to stop.
const STOP_GRACE: Duration = Duration::from_secs(1);

/// After Stop or Done, how long the scanner thread has to return before the run is ended without it. pagescan gives up on a driver 30 seconds after a cancel, so this is only for a scan that never returns at all.
const GIVE_UP_AFTER_CANCEL: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct Shared {
	frame: Frame,
	dm: Rc<Mutex<DocumentManager>>,
	config: Rc<Mutex<ConfigManager>>,
	label: StaticText,
	scanner: Arc<dyn Scanner>,
}

/// What a run's threads share with the UI thread.
#[derive(Default)]
struct Run {
	/// Pages the scanner handed over.
	scanned: AtomicUsize,
	/// When Stop or Done was chosen.
	cancelled_at: Mutex<Option<Instant>>,
	/// The scanner thread has returned.
	scanner_ended: AtomicBool,
	/// When the scanner last handed over a page or got one back, or the run started.
	last_heard: Mutex<Option<Instant>>,
	/// A page is with Paperback, which may wait on OCR: the scanner can't be blamed for that time.
	in_page: AtomicBool,
	/// The watchdog gave up on a scanner thread that never returned.
	abandoned: AtomicBool,
}

impl Run {
	fn cancel(&self) {
		self.cancelled_at.lock().unwrap().get_or_insert_with(Instant::now);
	}

	fn heard(&self) {
		*self.last_heard.lock().unwrap() = Some(Instant::now());
	}

	/// Whether the run should be ended without its scanner thread: it hasn't returned long after a cancel, or the scanner has been silent, outside a page Paperback is holding, long past pagescan's own inactivity timeout.
	fn overdue(&self) -> bool {
		let silent = self
			.last_heard
			.lock()
			.unwrap()
			.is_some_and(|at| at.elapsed() > crate::scanning::scanner::TIMEOUT + GIVE_UP_AFTER_CANCEL);
		!self.scanner_ended.load(Ordering::Acquire)
			&& (self.cancelled_at.lock().unwrap().is_some_and(|at| at.elapsed() > GIVE_UP_AFTER_CANCEL)
				|| (!self.in_page.load(Ordering::Acquire) && silent))
	}
}

struct Session {
	shared: Shared,
	window: Option<ScanningWindow>,
	scanner_id: String,
	settings: ScanSettings,
	mode: Mode,
	/// Written by each run's OCR thread; locked by the UI thread only to read it whole.
	archive: Arc<Mutex<ScanArchive>>,
	path: PathBuf,
	pages: usize,
	/// The run going on, until the OCR thread has finished with its last page.
	run: Option<Arc<Run>>,
	run_started: Instant,
	/// The flatbed page came out blank, probably face down, and the window waits for Rescan or Skip.
	asking_blank: bool,
	/// A flatbed page that may not be lined up, offered for Rescan; scanning the next page goes on without it.
	rescan: Option<usize>,
	/// The page a rescan stands in for. Kept until a page takes its place or the reader moves on, so a blank rescan doesn't forget it.
	replacing: Option<usize>,
	run_pages: usize,
	run_dropped: usize,
	run_unreadable: usize,
	/// A feeder's notes about particular pages (turned, blank, out of line), said together at the end.
	notes: Vec<String>,
	no_text_streak: usize,
	told_language: bool,
	last_spoken: Instant,
	/// A flatbed page's result, said once the buttons have changed, so the focus moving to Scan Next Page doesn't cut it off.
	held: Option<String>,
	told_ocr_problem: bool,
	storage_failed: bool,
	opened: bool,
	/// Pages or text not yet shown in the tab. Re-reading the tab is put off a moment so a run's pages share one re-read, and, once the Scanning window is closed and the reader may be reading, until the run ends.
	dirty: bool,
	flush_scheduled: bool,
	/// A re-read is being parsed off the UI thread.
	parsing: bool,
}

/// What the OCR thread did with a page it added.
struct Stored {
	number: usize,
	replaced: bool,
	amount: TextAmount,
	turn: Option<Turn>,
	/// Whether the page may not be lined up, and if so whether text touches an edge.
	out_of_line: Option<bool>,
	/// A one-sided feeder page with nothing on it, kept because skipping wasn't asked for.
	looks_blank: bool,
	/// Ink, but no text OCR could read: kept when skipping blank pages, since it isn't blank.
	no_text: bool,
	error: Option<OcrError>,
	/// Text recognized on the scan since it was opened is kept by the archive's contents, which the page just changed: from where to where it moves.
	moved_text: Option<(PathBuf, PathBuf)>,
}

enum Event {
	Stored(Box<Stored>),
	/// A blank flatbed page, which the window asks about.
	Blank,
	/// A page or side left out because skipping blank pages was asked for.
	Dropped,
	/// A page the scanner handed over whose image couldn't be encoded or decoded.
	Unreadable,
	/// A page couldn't be added; the run stops there.
	StoreFailed {
		number: usize,
		reason: String,
		earlier: usize,
	},
	/// Time to show what's arrived in the tab.
	Flush,
	/// A re-read parsed off the UI thread, to put in the tab.
	Parsed(Box<Result<DocumentSession, String>>),
	Ended(ScanSummary),
}

/// Pages travel to the OCR thread as PNG, a fraction of their decoded size.
enum ToOcr {
	Page(Vec<u8>),
	Unreadable,
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

/// Long enough for a feeder's next page to share the re-read, short enough that the tab keeps up. A re-read costs more as the scan grows, so it waits longer too.
fn flush_delay(pages: usize) -> Duration {
	Duration::from_millis(800 + 20 * u64::try_from(pages).unwrap_or(u64::MAX).min(200))
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
	// Skipping is opt-in for both feeder modes, and even then a page with ink that OCR can't read is kept.
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
				messages::scan_failed(&ScanError { kind: ErrorKind::Other, message: error.to_string() }),
				&title(),
			);
			return;
		}
	};
	let window = ScanningWindow::build(&shared.frame, settings.source.is_feeder());
	bind_buttons(window);
	let now = Instant::now();
	SESSION.set(Some(Session {
		shared,
		window: Some(window),
		scanner_id: scanner.id.clone(),
		settings,
		mode,
		path: archive.path().to_path_buf(),
		archive: Arc::new(Mutex::new(archive)),
		pages: 0,
		run: None,
		run_started: now,
		asking_blank: false,
		rescan: None,
		replacing: None,
		run_pages: 0,
		run_dropped: 0,
		run_unreadable: 0,
		notes: Vec::new(),
		no_text_streak: 0,
		told_language: false,
		last_spoken: now,
		held: None,
		told_ocr_problem: false,
		storage_failed: false,
		opened: false,
		dirty: false,
		flush_scheduled: false,
		parsing: false,
	}));
	start_run();
	window.dialog.show_modal();
	window.dialog.destroy();
	finish();
}

fn bind_buttons(window: ScanningWindow) {
	// A hidden button's mnemonic can still reach it, so each acts only when it's shown.
	window.rescan.on_click(move |_| {
		let go = with_session(|session| {
			if session.run.is_some() {
				return false;
			}
			if std::mem::take(&mut session.asking_blank) {
				// The blank page was a rescan's? Then it's still that page being replaced.
				return true;
			}
			match session.rescan.take() {
				Some(page) => {
					session.replacing = Some(page);
					true
				}
				None => false,
			}
		});
		if go == Some(true) {
			start_run();
		}
	});
	window.skip.on_click(move |_| {
		with_session(|session| {
			if !std::mem::take(&mut session.asking_blank) {
				return;
			}
			// Moving on: a page being replaced stays as it was.
			session.replacing = None;
			session.show_buttons();
			if let Some(window) = session.window {
				window.say(&messages::flatbed_prompt());
			}
		});
	});
	window.next.on_click(move |_| {
		let go = with_session(|session| {
			let go = session.run.is_none() && !session.asking_blank;
			if go {
				session.rescan = None;
				session.replacing = None;
			}
			go
		});
		if go == Some(true) {
			start_run();
		}
	});
	window.stop.on_click(move |_| {
		with_session(|session| {
			if session.run_started.elapsed() >= STOP_GRACE {
				session.cancel_run();
			}
		});
	});
	// Escape presses Done. A feeder still pulling paper is stopped only once the reader confirms, since Escape is also how a screen reader user backs out of things.
	window.done.on_click(move |event| {
		if may_close(window) {
			window.dialog.end_modal(ID_CANCEL);
		}
		// Handled here, so the dialog's own Escape handling doesn't close it regardless.
		event.event.skip(false);
	});
	window.dialog.on_close(move |event| {
		if may_close(window) {
			window.dialog.end_modal(ID_CANCEL);
		}
		event.skip(false);
	});
}

fn may_close(window: ScanningWindow) -> bool {
	let feeding = with_session(|session| session.run.is_some() && session.feeder()).unwrap_or(false);
	!feeding || wx_utils::confirm(&window.dialog, messages::confirm_stop(), &title())
}

/// Whether a scan is running or its Scanning window is open.
pub fn is_running() -> bool {
	SESSION.with(|slot| slot.try_borrow().map_or(true, |session| session.is_some()))
}

/// Before quitting while pages are still being scanned or read: whether to go ahead. The pages already added are kept either way.
pub fn confirm_quit(parent: &dyn WxWidget) -> bool {
	!is_busy() || wx_utils::confirm(parent, messages::quit_while_scanning(), &title())
}

/// Quitting: the scanner is told to stop, so a driver isn't left feeding pages to nothing.
pub fn stop_for_quit() {
	with_session(|session| session.cancel_run());
}

/// Whether a run still owns an archive: pages are being scanned or read into it.
pub fn is_busy() -> bool {
	SESSION.with(|slot| slot.try_borrow().map_or(true, |session| session.as_ref().is_some_and(|s| s.run.is_some())))
}

/// Whether a run still owns `path`, so it mustn't be saved over, closed or deleted yet.
pub fn is_busy_with(path: &Path) -> bool {
	SESSION.with(|slot| {
		slot.try_borrow()
			.is_ok_and(|session| session.as_ref().is_some_and(|s| s.run.is_some() && store::same_file(&s.path, path)))
	})
}

fn with_session<T>(action: impl FnOnce(&mut Session) -> T) -> Option<T> {
	SESSION.with_borrow_mut(|session| session.as_mut().map(action))
}

/// Starts a run: pages are added at the end, or, for a rescan, the first one kept takes the place of `Session::replacing`.
fn start_run() {
	let Some((scanner, scanner_id, settings, mode, archive, replace, run)) = with_session(|session| {
		if session.run.is_some() {
			return None;
		}
		let run = Arc::new(Run::default());
		session.run = Some(Arc::clone(&run));
		session.run_started = Instant::now();
		session.last_spoken = session.run_started;
		session.asking_blank = false;
		session.rescan = None;
		session.run_pages = 0;
		session.run_dropped = 0;
		session.run_unreadable = 0;
		session.notes.clear();
		session.no_text_streak = 0;
		session.held = None;
		session.storage_failed = false;
		session.show_buttons();
		if let Some(window) = session.window {
			window.say(&messages::scanning());
		}
		Some((
			Arc::clone(&session.shared.scanner),
			session.scanner_id.clone(),
			session.settings,
			session.mode,
			Arc::clone(&session.archive),
			session.replacing,
			run,
		))
	})
	.flatten() else {
		return;
	};
	// Before the scanner thread exists, so a Stop from now on reaches this scan however early it comes.
	scanner.prepare();
	let (to_ocr, from_scanner) = mpsc::sync_channel(QUEUE_DEPTH);
	let reader = {
		let (run, scanner) = (Arc::clone(&run), Arc::clone(&scanner));
		thread::Builder::new().name("paperback-scan-ocr".into()).spawn(move || {
			let mut job = OcrJob {
				run: &run,
				mode,
				archive: &archive,
				replace,
				text_store: Some(saved_pages_dir()),
				cancel: &|| scanner.cancel(),
			};
			job.read_pages(&from_scanner, &mut pages::recognize_with_engine, &mut post);
		})
	};
	if let Err(error) = reader {
		tracing::warn!(%error, "could not start the scan's ocr thread");
		post(Event::Ended(ScanSummary {
			pages: 0,
			error: Some(ScanError { kind: ErrorKind::Other, message: error.to_string() }),
		}));
		return;
	}
	{
		let run = Arc::clone(&run);
		run.heard();
		let watchdog = thread::Builder::new().name("paperback-scan-watchdog".into()).spawn(move || {
			while !run.scanner_ended.load(Ordering::Acquire) {
				thread::sleep(Duration::from_secs(1));
				if run.overdue() {
					tracing::warn!("the scanner never returned; ending the run without it");
					run.abandoned.store(true, Ordering::Release);
					return;
				}
			}
		});
		if let Err(error) = watchdog {
			tracing::warn!(%error, "could not start the scan's watchdog");
		}
	}
	let main_thread = scanner.needs_main_thread();
	let scan = move || {
		let summary = scanner.scan(&scanner_id, settings, &mut |page| {
			run.in_page.store(true, Ordering::Release);
			run.scanned.fetch_add(1, Ordering::AcqRel);
			// Encoded here, while the driver goes on with the next page in its own thread (or its own buffers, for SANE).
			let message = match encode_png(&page) {
				Ok(png) => ToOcr::Page(png),
				Err(error) => {
					tracing::warn!(%error, "a scanned page could not be encoded");
					ToOcr::Unreadable
				}
			};
			drop(page);
			// Waits while OCR is behind, which makes the scanner wait too.
			let _ = to_ocr.send(message);
			run.heard();
			run.in_page.store(false, Ordering::Release);
		});
		run.scanner_ended.store(true, Ordering::Release);
		let _ = to_ocr.send(ToOcr::End(summary));
	};
	if main_thread {
		// macOS: ImageCaptureCore calls back only on the main thread, so the scan runs there, after the window is up; pagescan keeps the window answering meanwhile, so Stop works.
		wx_utils::post_to_ui(scan);
	} else if let Err(error) = thread::Builder::new().name("paperback-scan".into()).spawn(scan) {
		tracing::warn!(%error, "could not start the scan thread");
	}
}

type Recognize<'a> = dyn FnMut(&DynamicImage) -> Result<pages::Reading, OcrError> + 'a;

/// What becomes of one page.
enum Sorted {
	Keep(Box<Kept>),
	/// A blank flatbed page.
	Blank,
	/// A blank side left out, as asked.
	Dropped,
}

struct Kept {
	png: Vec<u8>,
	lines: Vec<RecognizedLine>,
	amount: TextAmount,
	turn: Option<Turn>,
	out_of_line: Option<bool>,
	looks_blank: bool,
	no_text: bool,
	error: Option<OcrError>,
}

/// A page is read (and turned if need be) before it's added, so it goes into the document once, already the right way up, with its text.
fn sort_page(page: &DynamicImage, png: Vec<u8>, mode: Mode, recognize: &mut Recognize<'_>) -> Sorted {
	match mode {
		Mode::Flatbed if pages::is_blank(page) => Sorted::Blank,
		Mode::Flatbed => keep(png, pages::read(page, recognize), false, false),
		Mode::Feeder | Mode::BothSidesAll => {
			let blank = pages::is_blank(page);
			let read = if blank { pages::ReadPage::default() } else { pages::read(page, recognize) };
			keep(png, read, blank && mode == Mode::Feeder, false)
		}
		Mode::FeederSkip | Mode::BothSidesSmart => {
			if pages::is_blank(page) {
				return Sorted::Dropped;
			}
			let read = pages::read(page, recognize);
			if pages::is_blank_side(&read, page, recognize) {
				return Sorted::Dropped;
			}
			let no_text = read.error.is_none() && !pages::side_has_text(&read, page);
			keep(png, read, false, no_text)
		}
	}
}

fn keep(png: Vec<u8>, read: pages::ReadPage, looks_blank: bool, no_text: bool) -> Sorted {
	let png = match read.turned.as_ref().map(encode_png) {
		Some(Ok(turned)) => turned,
		Some(Err(error)) => {
			tracing::warn!(%error, "a turned page could not be encoded; keeping it as scanned");
			png
		}
		None => png,
	};
	// An engine that failed says nothing about how the page lies, and a page with no text has no lines to judge by.
	let out_of_line = (read.error.is_none() && !looks_blank && !no_text && (read.little_text || read.cut_off))
		.then_some(read.cut_off);
	Sorted::Keep(Box::new(Kept {
		png,
		lines: read.lines,
		amount: read.amount,
		turn: read.turn,
		out_of_line,
		looks_blank,
		no_text,
		error: read.error,
	}))
}

/// The OCR thread's work for one run.
struct OcrJob<'a> {
	run: &'a Run,
	mode: Mode,
	archive: &'a Mutex<ScanArchive>,
	/// The page the run's first kept page replaces (a rescan).
	replace: Option<usize>,
	/// The saved-pages store, whose file for a scan in progress follows the archive's contents.
	text_store: Option<PathBuf>,
	/// Stops the scanner, after a page couldn't be saved.
	cancel: &'a dyn Fn(),
}

impl OcrJob<'_> {
	/// Each page in the order scanned, until the scanner says it's done (or never will).
	fn read_pages(
		&mut self,
		from_scanner: &mpsc::Receiver<ToOcr>,
		recognize: &mut Recognize<'_>,
		post: &mut dyn FnMut(Event),
	) {
		let mut failed = false;
		loop {
			let message = match from_scanner.recv_timeout(Duration::from_millis(250)) {
				Ok(message) => message,
				Err(mpsc::RecvTimeoutError::Timeout) => {
					if self.run.abandoned.load(Ordering::Acquire) {
						post(Event::Ended(ScanSummary {
							pages: self.run.scanned.load(Ordering::Acquire),
							error: Some(ScanError {
								kind: ErrorKind::Other,
								message: "The scanner stopped responding.".into(),
							}),
						}));
						return;
					}
					continue;
				}
				Err(mpsc::RecvTimeoutError::Disconnected) => {
					post(Event::Ended(ScanSummary {
						pages: self.run.scanned.load(Ordering::Acquire),
						error: Some(ScanError {
							kind: ErrorKind::Other,
							message: "The scanner stopped unexpectedly.".into(),
						}),
					}));
					return;
				}
			};
			match message {
				ToOcr::End(summary) => {
					post(Event::Ended(summary));
					return;
				}
				// After a page couldn't be saved nothing more is added, so nothing lands out of order; the reader has been told once.
				ToOcr::Page(_) | ToOcr::Unreadable if failed => {}
				ToOcr::Unreadable => post(Event::Unreadable),
				ToOcr::Page(png) => match image::load_from_memory(&png) {
					Err(error) => {
						tracing::warn!(%error, "a scanned page could not be decoded again");
						post(Event::Unreadable);
					}
					Ok(page) => match sort_page(&page, png, self.mode, recognize) {
						Sorted::Blank => post(Event::Blank),
						Sorted::Dropped => post(Event::Dropped),
						Sorted::Keep(kept) => match self.store(*kept) {
							Ok(stored) => post(Event::Stored(Box::new(stored))),
							Err(event) => {
								failed = true;
								(self.cancel)();
								post(event);
							}
						},
					},
				},
			}
		}
	}

	/// Adds the page (or puts it in place of the one being replaced) with its text, numbered by the archive itself, so text and image always share a number.
	fn store(&mut self, kept: Kept) -> Result<Stored, Event> {
		let mut archive = self.archive.lock().unwrap();
		let text_before = self
			.text_store
			.as_ref()
			.filter(|_| archive.path().exists())
			.map(|dir| saved_pages_file(dir, archive.path()));
		let (number, replaced) = match self.replace {
			Some(page) => (page, true),
			None => (archive.page_count() + 1, false),
		};
		let text = encode_pages(&[(i32::try_from(number).unwrap_or(i32::MAX), kept.lines)]).unwrap_or_default();
		let result = if replaced {
			archive.replace(number, &kept.png, &text)
		} else {
			archive.append(&kept.png, &text).map(|_| ())
		};
		if let Err(error) = result {
			tracing::warn!(error = format!("{error:#}"), "a scanned page could not be saved");
			return Err(Event::StoreFailed {
				number,
				reason: error.root_cause().to_string(),
				earlier: archive.page_count(),
			});
		}
		if replaced {
			self.replace = None;
		}
		let text_after = self.text_store.as_ref().map(|dir| saved_pages_file(dir, archive.path()));
		let moved_text = text_before.zip(text_after).filter(|(before, after)| before != after && before.exists());
		Ok(Stored {
			number,
			replaced,
			amount: kept.amount,
			turn: kept.turn,
			out_of_line: kept.out_of_line,
			looks_blank: kept.looks_blank,
			no_text: kept.no_text,
			error: kept.error,
			moved_text,
		})
	}
}

/// Recognize Current Page or Batch OCR on a scan in progress saved its text under the archive's contents as they were; the page just added changed them.
fn carry_text_over(before: &Path, after: &Path) {
	let mut pages: BTreeMap<i32, Vec<RecognizedLine>> = load_pages(before).into_iter().collect();
	pages.extend(load_pages(after));
	let pages: Vec<_> = pages.into_iter().collect();
	let _ = fs::remove_file(after);
	match save_pages(after, &pages) {
		Ok(()) => {
			let _ = fs::remove_file(before);
		}
		Err(error) => tracing::warn!(%error, "could not move recognized text along with a scan in progress"),
	}
}

fn post(event: Event) {
	crate::ui::app::when_unlocked(move |_| handle(event));
}

fn handle(event: Event) {
	let over = SESSION.with_borrow_mut(|slot| {
		let Some(session) = slot.as_mut() else { return false };
		session.apply(event);
		session.is_over()
	});
	if over {
		SESSION.set(None);
	}
}

impl Session {
	fn feeder(&self) -> bool {
		self.settings.source.is_feeder()
	}

	/// The window is closed, the last run has ended and the tab has its last re-read.
	fn is_over(&self) -> bool {
		self.window.is_none() && self.run.is_none() && !self.parsing
	}

	fn show_buttons(&self) {
		self.show_buttons_focusing(false);
	}

	fn show_buttons_focusing(&self, force: bool) {
		if let Some(window) = self.window {
			window.show_buttons(
				Buttons {
					scanning: self.run.is_some(),
					asking_blank: self.asking_blank,
					can_rescan: self.rescan.is_some(),
				},
				force,
			);
		}
	}

	fn cancel_run(&self) {
		if let Some(run) = &self.run {
			run.cancel();
			self.shared.scanner.cancel();
		}
	}

	fn apply(&mut self, event: Event) {
		match event {
			Event::Stored(stored) => {
				self.pages = self.pages.max(stored.number);
				if stored.replaced {
					self.replacing = None;
				} else {
					self.run_pages += 1;
				}
				if let Some((before, after)) = &stored.moved_text {
					carry_text_over(before, after);
				}
				self.mark_dirty();
				self.announce_page(&stored);
			}
			Event::Dropped => self.run_dropped += 1,
			Event::Blank => {
				self.asking_blank = true;
				// Said when the run ends, once the focus has moved to Rescan.
				self.held = Some(messages::blank_on_flatbed(self.replacing.unwrap_or(self.pages + 1)));
			}
			Event::Unreadable => {
				self.run_unreadable += 1;
				if self.feeder() {
					self.notes.push(messages::page_unreadable());
				} else {
					self.held = Some(messages::page_unreadable());
				}
			}
			Event::StoreFailed { number, reason, earlier } => {
				self.storage_failed = true;
				// Said even after Done: pages are being lost.
				let message = messages::page_not_saved(number, &reason, earlier);
				match self.window {
					// First thing said when the run ends, which it does as soon as the scanner stops.
					Some(_) => self.held = Some(message),
					None => announce(self.shared.label, &message),
				}
			}
			Event::Flush => {
				self.flush_scheduled = false;
				// Once the window is closed the reader may be reading, so the tab waits for the run to end.
				if self.dirty && self.window.is_some() {
					self.flush();
				}
			}
			Event::Parsed(parsed) => {
				self.parsing = false;
				match *parsed {
					Ok(document) => {
						let dm = Rc::clone(&self.shared.dm);
						self.keeping_focus(|| dm.lock().unwrap().show_parsed_scan(&self.path, document));
					}
					Err(error) => tracing::warn!(%error, "could not re-read the scan"),
				}
				if self.dirty && (self.window.is_some() || self.run.is_none()) {
					self.flush();
				}
			}
			Event::Ended(summary) => {
				tracing::info!(pages = summary.pages, error = ?summary.error, "a scan run ended");
				self.run = None;
				if self.dirty {
					self.flush();
				}
				// The buttons first: the focus moves, and what's said next isn't cut off by it.
				self.show_buttons();
				self.summarize(summary);
			}
		}
	}

	/// A flatbed page is said in full once the run ends; a feeder's first page is said in full, then a count every few seconds, with notes about particular pages kept for the end. After Done nothing is said until the end.
	fn announce_page(&mut self, stored: &Stored) {
		if self.window.is_none() {
			return;
		}
		self.no_text_streak = if stored.no_text { self.no_text_streak + 1 } else { 0 };
		let (message, noted) = self.page_message(stored);
		if !self.feeder() {
			self.held = Some(message);
			return;
		}
		if self.run_pages == 1 {
			self.say(&message, true);
			self.last_spoken = Instant::now();
		} else {
			if noted {
				self.notes.push(message);
			}
			if self.no_text_streak >= NO_TEXT_STREAK && !self.told_language {
				self.told_language = true;
				self.say(&messages::ocr_language_hint(), true);
				self.last_spoken = Instant::now();
			} else if self.last_spoken.elapsed() >= PROGRESS_EVERY {
				self.say(&messages::pages_so_far(self.run_pages), true);
				self.last_spoken = Instant::now();
			}
		}
	}

	/// What's said about a page, and whether there's anything about it beyond its word count.
	fn page_message(&mut self, stored: &Stored) -> (String, bool) {
		let number = stored.number;
		let mut message = messages::page_read(number, stored.amount, stored.turn);
		let mut noted = stored.turn.is_some_and(|turn| turn != Turn::None) || stored.no_text;
		if let Some(error) = &stored.error
			&& !self.told_ocr_problem
		{
			self.told_ocr_problem = true;
			noted = true;
			message = format!("{message} {}", ocr::message(error));
		}
		if stored.looks_blank {
			noted = true;
			message = format!("{message} {}", messages::looks_blank());
		}
		if let Some(cut_off) = stored.out_of_line {
			noted = true;
			// Said without stopping: Enter still scans the next page, and Rescan is there for this one.
			let can_rescan = !self.feeder() && self.window.is_some();
			message = format!("{message} {}", messages::not_lined_up(cut_off, can_rescan));
			if can_rescan {
				self.rescan = Some(number);
			}
		} else if !self.feeder() && self.pages == 1 && !stored.replaced {
			message = format!("{message} {}", messages::flatbed_prompt());
		}
		(message, noted)
	}

	fn summarize(&mut self, summary: ScanSummary) {
		if self.window.is_none() {
			// After Done: one message, with the final count.
			let failure = summary
				.error
				.filter(|error| error.kind != ErrorKind::Cancelled && !self.storage_failed)
				.map(|error| messages::scan_failed(&error));
			let finished = (self.pages > 0).then(|| messages::finished(self.pages));
			let message = [failure, finished].into_iter().flatten().collect::<Vec<_>>().join(" ");
			if !message.is_empty() {
				announce(self.shared.label, message);
			}
			return;
		}
		let held = self.held.take();
		if !self.feeder() {
			let failure = summary
				.error
				.filter(|error| !(self.storage_failed && error.kind == ErrorKind::Cancelled))
				.map(|error| messages::scan_failed(&error));
			let message = [held, failure].into_iter().flatten().collect::<Vec<_>>().join(" ");
			if !message.is_empty() {
				self.say(&message, false);
			}
			return;
		}
		let skipping = matches!(self.mode, Mode::FeederSkip | Mode::BothSidesSmart);
		let finished = match self.mode {
			Mode::BothSidesSmart => {
				let sheets = (self.run_pages + self.run_dropped + self.run_unreadable).div_ceil(2);
				messages::both_sides_finished(sheets, self.run_pages, self.run_dropped)
			}
			_ => messages::feeder_finished(self.run_pages, if skipping { self.run_dropped } else { 0 }),
		};
		let notes = (!self.notes.is_empty()).then(|| messages::page_notes(&self.notes));
		let message = match summary.error {
			// The reader was told why when the page failed.
			_ if self.storage_failed => None,
			Some(error) if error.kind != ErrorKind::SaneMissing && self.run_pages > 0 => {
				// A feeder that jams after some pages keeps them; say both.
				Some(format!("{} {finished}", messages::scan_failed(&error)))
			}
			Some(error) => Some(messages::scan_failed(&error)),
			None if skipping && self.run_pages == 0 && self.run_dropped > 0 => Some(messages::all_blank()),
			None => Some(finished),
		};
		let message = [held, message, notes].into_iter().flatten().collect::<Vec<_>>().join(" ");
		if !message.is_empty() {
			self.say(&message, true);
		}
	}

	fn mark_dirty(&mut self) {
		self.dirty = true;
		match when_to_refresh(self.opened, self.window.is_some(), self.flush_scheduled) {
			Refresh::Now => self.flush(),
			Refresh::Soon => {
				self.flush_scheduled = true;
				let delay = flush_delay(self.pages);
				let spawned = thread::Builder::new().name("paperback-scan-flush".into()).spawn(move || {
					thread::sleep(delay);
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

	/// In the Scanning window while it's open, else through the main window's announcements. `queue` waits for what's being said, as a feeder's should.
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

	/// Runs `change`, which can take the focus from the Scanning window by opening or re-reading the scan's tab, and gives it back to the button that had it. Focus that never left the window, or was already elsewhere, stays put, since a screen reader announces every focus change.
	fn keeping_focus<T>(&self, change: impl FnOnce() -> T) -> T {
		let before = self.window.and_then(|window| window.focused_button());
		let result = change();
		if let (Some(window), Some(button)) = (self.window, before)
			&& window.focused_button().is_none()
		{
			window.dialog.raise();
			if button.is_shown() {
				button.set_focus();
			} else {
				self.show_buttons_focusing(true);
			}
		}
		result
	}

	/// The tab shows what's arrived. The first page opens it here; after that the re-read is parsed on a thread of its own and put in the tab when it's ready ([`Event::Parsed`]).
	fn flush(&mut self) {
		if !self.path.exists() {
			self.dirty = false;
			return;
		}
		if !self.opened {
			self.dirty = false;
			let dm = Rc::clone(&self.shared.dm);
			// Not while the OCR thread is part way through adding a page.
			let shown = self.keeping_focus(|| {
				let _writing = self.archive.lock().unwrap();
				dm.lock().unwrap().open_scan(&dm, &self.path)
			});
			if shown {
				self.opened = true;
				let manager = dm.lock().unwrap();
				update_title_from_manager(&self.shared.frame, &manager);
				drop(manager);
				let menu_bar = menu::create_menu_bar(&self.shared.config.lock().unwrap());
				self.shared.frame.set_menu_bar(menu_bar);
				menu::update_menu_item_states(&self.shared.frame, true);
			}
			return;
		}
		if self.parsing {
			// Picked up when the re-read in hand is done.
			return;
		}
		self.dirty = false;
		self.parsing = true;
		let settings = scan_parse_settings(&self.shared.config.lock().unwrap());
		let (archive, path) = (Arc::clone(&self.archive), self.path.clone());
		let spawned = thread::Builder::new().name("paperback-scan-reread".into()).spawn(move || {
			let parsed = {
				let _writing = archive.lock().unwrap();
				parse_scan(&path, settings)
			};
			post(Event::Parsed(Box::new(parsed)));
		});
		if let Err(error) = spawned {
			tracing::warn!(%error, "could not start re-reading the scan");
			self.parsing = false;
			self.dirty = true;
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
	// Pages still on their way would be left behind, and the run would have nowhere to put them.
	if is_busy_with(&source) {
		announce_for_command(label, from_keyboard, messages::still_reading());
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
	if store::same_file(source, destination) && !store::is_in_progress(source) {
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

/// At start, once the documents left open are back (or straight away, when Paperback isn't set to reopen them): scans in progress that nothing reopened are offered, never deleted unasked. A scan a crash cut short has its complete pages recovered first, behind a progress window, since that copies the whole archive.
pub(crate) fn offer_unsaved_scans(frame: Frame, dm: &Rc<Mutex<DocumentManager>>, config: &Rc<Mutex<ConfigManager>>) {
	if is_running() {
		return;
	}
	let open: Vec<PathBuf> = dm.lock().unwrap().open_paths().into_iter().map(PathBuf::from).collect();
	let found = store::scans_left(&open);
	if found.is_empty() {
		return;
	}
	let (dm, config) = (Rc::clone(dm), Rc::clone(config));
	if found.iter().all(|(_, opens)| *opens) {
		let unsaved: Vec<_> =
			found.into_iter().map(|(path, _)| store::Unsaved { path, state: Recovery::Intact }).collect();
		ask_about(frame, &dm, &config, &unsaved);
		return;
	}
	run_without_estimate(
		&frame,
		&title(),
		// TRANSLATORS: Shown at start while scanned documents that were cut short by Paperback closing unexpectedly are repaired
		&t("Recovering scans that were cut short..."),
		move |_| {
			found
				.into_iter()
				.map(|(path, opens)| {
					let state = if opens { Recovery::Intact } else { store::recover(&path) };
					store::Unsaved { path, state }
				})
				.collect::<Vec<_>>()
		},
		move |checked, _| {
			let config_guard = config.lock().unwrap();
			let unsaved = store::clear_empty(checked, &config_guard);
			drop(config_guard);
			if !unsaved.is_empty() {
				ask_about(frame, &dm, &config, &unsaved);
			}
		},
	);
}

fn ask_about(
	frame: Frame,
	dm: &Rc<Mutex<DocumentManager>>,
	config: &Rc<Mutex<ConfigManager>>,
	unsaved: &[store::Unsaved],
) {
	let count = |state: Recovery| unsaved.iter().filter(|scan| scan.state == state).count();
	match ask_about_unsaved_scans(&frame, unsaved.len(), count(Recovery::Repaired), count(Recovery::Unreadable)) {
		UnsavedChoice::Open => {
			let mut manager = dm.lock().unwrap();
			for scan in unsaved.iter().filter(|scan| scan.state != Recovery::Unreadable) {
				manager.open_file(dm, &scan.path);
			}
			update_title_from_manager(&frame, &manager);
			manager.focus_document_text();
			drop(manager);
			let menu_bar = menu::create_menu_bar(&config.lock().unwrap());
			frame.set_menu_bar(menu_bar);
			menu::update_menu_item_states(&frame, true);
		}
		UnsavedChoice::Delete => {
			let config = config.lock().unwrap();
			for scan in unsaved {
				store::discard(&scan.path, &config);
			}
		}
		UnsavedChoice::Later => {}
	}
}

/// A scan being reopened with the other documents is repaired first if a crash cut it short; false if it can't be opened (it's kept either way, unless it holds not one complete page).
pub(crate) fn prepare_to_reopen(path: &Path, label: StaticText) -> bool {
	if !store::is_in_progress(path) {
		return true;
	}
	let title = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
	match store::recover(path) {
		Recovery::Repaired => {
			// TRANSLATORS: Announced at start when a scanned document that was cut short by Paperback closing unexpectedly has been repaired; %s is its title
			announce(
				label,
				t("%s was cut short when Paperback closed; its complete pages have been recovered.")
					.replacen("%s", &title, 1),
			);
			true
		}
		Recovery::Intact => true,
		Recovery::Unreadable => {
			// TRANSLATORS: Announced at start when a scanned document left open couldn't be opened just now (another program may be using it); %s is its title. It's kept, and offered again next time.
			announce(label, t("%s could not be opened just now. It has been kept.").replacen("%s", &title, 1));
			false
		}
		Recovery::Empty => false,
	}
}

/// The Scanning window has closed: a scan still running is stopped, and the reader is taken to the document. If pages are still being read, the final count is said when they're done.
fn finish() {
	let Some((dm, label, pages, running, over)) = with_session(|session| {
		session.window = None;
		let running = session.run.is_some();
		if running {
			session.cancel_run();
		} else if session.dirty {
			session.flush();
		}
		(Rc::clone(&session.shared.dm), session.shared.label, session.pages, running, session.is_over())
	}) else {
		return;
	};
	if over {
		SESSION.set(None);
	}
	if pages > 0 {
		dm.lock().unwrap().focus_document_text();
	}
	if running {
		announce(label, messages::finishing());
	} else if pages > 0 {
		announce(label, messages::finished(pages));
	}
}

#[cfg(test)]
mod tests {
	use image::{GrayImage, Luma};
	use paperback_core::ocr::RecognizedWord;

	use super::*;
	use crate::scanning::scanner::{
		ScanSettings, Scanner, ScannerInfo, Source,
		fake::{FakeScanner, Script},
	};

	const TEXT: &str = "The committee met on Tuesday to review the proposal and agreed that the plan should move forward after small changes were made to the budget section";

	/// Ink lines, and a two-pixel marker at the top left that the stand-in engine reads as text; only a page with the marker there in that order reads.
	fn sheet(marked_at_left: bool, marked_at_right: bool, ink: bool) -> GrayImage {
		let mut image = GrayImage::from_pixel(200, 300, Luma([240]));
		if ink {
			for y in (60..240).step_by(12) {
				for x in 30..170 {
					for dy in 0..5 {
						image.put_pixel(x, y + dy, Luma([25]));
					}
				}
			}
		}
		if marked_at_left {
			image.put_pixel(0, 0, Luma([0]));
			image.put_pixel(1, 0, Luma([100]));
		}
		if marked_at_right {
			image.put_pixel(199, 0, Luma([0]));
			image.put_pixel(198, 0, Luma([100]));
		}
		image
	}

	fn printed() -> DynamicImage {
		DynamicImage::ImageLuma8(sheet(true, false, true))
	}

	/// A printed page upside down: reads only once turned.
	fn upside_down() -> DynamicImage {
		printed().rotate180()
	}

	fn blank() -> DynamicImage {
		DynamicImage::ImageLuma8(sheet(false, false, false))
	}

	/// Ink that reads only mirrored: the printed side seen through the paper.
	fn show_through() -> DynamicImage {
		DynamicImage::ImageLuma8(sheet(false, true, true))
	}

	/// Ink that reads as nothing any way: a photo, handwriting, or a script the OCR language doesn't cover.
	fn photo() -> DynamicImage {
		DynamicImage::ImageLuma8(sheet(false, false, true))
	}

	#[allow(clippy::unnecessary_wraps, reason = "stands in for the OCR engine, which can fail")]
	fn engine(image: &DynamicImage) -> Result<pages::Reading, OcrError> {
		let gray = image.to_luma8();
		let readable = gray.get_pixel(0, 0).0[0] == 0 && gray.get_pixel(1, 0).0[0] == 100;
		let text = if readable { TEXT } else { "' ~ 1l ." };
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

	struct Outcome {
		events: Vec<Event>,
		archive: ScanArchive,
		cancelled: usize,
		_dir: tempfile::TempDir,
	}

	/// Runs the fake scanner's pages through the OCR thread's job, as a real run does (encoded to PNG on the way, through the bounded queue), into a real archive, and returns what it would post.
	fn run_into(
		mode: Mode,
		sides: Vec<DynamicImage>,
		prepare: impl FnOnce(&mut ScanArchive),
		replace: Option<usize>,
	) -> Outcome {
		let info = ScannerInfo {
			id: "fake".into(),
			name: "Fake".into(),
			sources: vec![Source::Feeder, Source::FeederDuplex],
			problem: None,
		};
		let scanner = FakeScanner::new(vec![info]).with_script(Script::pages(sides));
		let dir = tempfile::tempdir().unwrap();
		let mut archive = ScanArchive::create(dir.path(), "Scan").unwrap();
		prepare(&mut archive);
		let archive = Mutex::new(archive);
		let run = Run::default();
		let (to_ocr, from_scanner) = mpsc::sync_channel(QUEUE_DEPTH);
		let settings = ScanSettings { source: Source::FeederDuplex, ..ScanSettings::default() };
		let cancelled = AtomicUsize::new(0);
		let mut events = Vec::new();
		thread::scope(|scope| {
			scope.spawn(|| {
				let summary = scanner.scan("fake", settings, &mut |page| {
					run.scanned.fetch_add(1, Ordering::AcqRel);
					to_ocr.send(ToOcr::Page(encode_png(&page).unwrap())).unwrap();
				});
				to_ocr.send(ToOcr::End(summary)).unwrap();
			});
			let cancel = || {
				cancelled.fetch_add(1, Ordering::AcqRel);
			};
			let mut job = OcrJob { run: &run, mode, archive: &archive, replace, text_store: None, cancel: &cancel };
			job.read_pages(&from_scanner, &mut engine, &mut |event| events.push(event));
		});
		Outcome { events, archive: archive.into_inner().unwrap(), cancelled: cancelled.into_inner(), _dir: dir }
	}

	fn run(mode: Mode, sides: Vec<DynamicImage>) -> Vec<Event> {
		run_into(mode, sides, |_| {}, None).events
	}

	fn stored(events: &[Event]) -> Vec<&Stored> {
		events
			.iter()
			.filter_map(|event| if let Event::Stored(stored) = event { Some(&**stored) } else { None })
			.collect()
	}

	fn added(events: &[Event]) -> Vec<usize> {
		stored(events).iter().map(|stored| stored.number).collect()
	}

	fn count(events: &[Event], matches: fn(&Event) -> bool) -> usize {
		events.iter().filter(|event| matches(event)).count()
	}

	/// Three sheets: printed front with a blank back; a sheet in face down, whose printed side comes second and upside down, after its show-through; and one printed on both sides.
	fn three_sheets() -> Vec<DynamicImage> {
		vec![printed(), blank(), show_through(), upside_down(), printed(), printed()]
	}

	#[test]
	fn the_tab_is_re_read_once_for_pages_arriving_together_and_not_while_the_reader_reads() {
		assert_eq!(when_to_refresh(false, true, false), Refresh::Now, "the document opens after the first page");
		assert_eq!(when_to_refresh(true, true, false), Refresh::Soon);
		assert_eq!(when_to_refresh(true, true, true), Refresh::AlreadyDue, "a second page shares the pending re-read");
		assert_eq!(when_to_refresh(true, false, false), Refresh::AtEnd, "after Done, once, when the run ends");
		assert!(flush_delay(200) > flush_delay(1), "a bigger scan waits longer between re-reads");
		assert!(flush_delay(100_000) <= Duration::from_secs(5));
	}

	#[test]
	fn skipping_blank_sides_keeps_only_the_sides_that_are_not_blank() {
		let outcome = run_into(Mode::BothSidesSmart, three_sheets(), |_| {}, None);
		assert_eq!(added(&outcome.events), vec![1, 2, 3, 4]);
		assert_eq!(
			count(&outcome.events, |event| matches!(event, Event::Dropped)),
			2,
			"the blank back and the show-through"
		);
		assert_eq!(
			count(&outcome.events, |event| matches!(event, Event::Blank)),
			0,
			"nothing is announced for a dropped side"
		);
		assert!(matches!(outcome.events.last(), Some(Event::Ended(_))));
		assert_eq!(outcome.archive.page_count(), 4);
	}

	#[test]
	fn skipping_blank_pages_keeps_a_page_with_ink_that_ocr_cannot_read() {
		let events = run(Mode::FeederSkip, vec![printed(), photo(), blank(), show_through()]);
		assert_eq!(added(&events), vec![1, 2]);
		let flags: Vec<bool> = stored(&events).iter().map(|stored| stored.no_text).collect();
		assert_eq!(flags, vec![false, true], "kept, and said to have no text");
		assert_eq!(count(&events, |event| matches!(event, Event::Dropped)), 2);
	}

	#[test]
	fn a_face_down_sheet_s_printed_side_is_kept_the_right_way_up() {
		let outcome = run_into(Mode::BothSidesSmart, three_sheets(), |_| {}, None);
		let turns: Vec<Option<Turn>> = stored(&outcome.events).iter().map(|stored| stored.turn).collect();
		assert_eq!(turns, vec![None, Some(Turn::Half), None, None]);
		let path = outcome.archive.path().to_string_lossy().into_owned();
		let session = DocumentSession::new(&path, "", "", paperback_core::document::ParseSettings::default()).unwrap();
		assert_eq!(session.page_count(), 4);
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
		let flagged: Vec<bool> = stored(&events).iter().map(|stored| stored.looks_blank).collect();
		assert_eq!(flagged, vec![false, true, false]);
	}

	#[test]
	fn a_page_arrives_once_with_its_text_and_the_right_way_up() {
		let outcome = run_into(Mode::Feeder, vec![upside_down()], |_| {}, None);
		assert_eq!(outcome.events.len(), 2, "one page event and the end");
		assert!(
			matches!(&outcome.events[0], Event::Stored(stored) if stored.number == 1 && stored.turn == Some(Turn::Half))
		);
		let text = paperback_core::scan::archive::recognized_pages(outcome.archive.path());
		assert_eq!(text.len(), 1);
		assert_eq!(text[0].0, 1, "the text is stored under its page number");
		assert!(text[0].1[0].text.starts_with("The committee"), "{:?}", text[0].1);
	}

	#[test]
	fn pages_continue_the_archive_s_numbering_and_text_stays_with_its_page() {
		let outcome = run_into(
			Mode::Feeder,
			vec![printed(), printed()],
			|archive| {
				archive.append_page(&blank()).unwrap();
				archive.append_page(&blank()).unwrap();
			},
			None,
		);
		assert_eq!(added(&outcome.events), vec![3, 4]);
		let numbers: Vec<i32> =
			paperback_core::scan::archive::recognized_pages(outcome.archive.path()).iter().map(|page| page.0).collect();
		assert_eq!(numbers, vec![3, 4]);
	}

	#[test]
	fn a_rescan_replaces_its_page_and_a_blank_one_replaces_nothing() {
		let three = |archive: &mut ScanArchive| {
			for _ in 0..3 {
				archive.append_page(&blank()).unwrap();
			}
		};
		// The rescan came out blank: nothing replaced, and the window asks again.
		let outcome = run_into(Mode::Flatbed, vec![blank()], three, Some(2));
		assert!(matches!(outcome.events[0], Event::Blank));
		assert_eq!(outcome.archive.page_count(), 3);
		// Rescanned again, the right way: page 2 is replaced, not a page added.
		let outcome = run_into(Mode::Flatbed, vec![printed()], three, Some(2));
		assert!(matches!(&outcome.events[0], Event::Stored(stored) if stored.number == 2 && stored.replaced));
		assert_eq!(outcome.archive.page_count(), 3);
		let numbers: Vec<i32> =
			paperback_core::scan::archive::recognized_pages(outcome.archive.path()).iter().map(|page| page.0).collect();
		assert_eq!(numbers, vec![2]);
	}

	#[test]
	fn a_page_that_cannot_be_saved_stops_the_scanner_and_is_said_once() {
		let outcome = run_into(
			Mode::Feeder,
			vec![printed(), printed(), printed()],
			|archive| {
				archive.append_page(&blank()).unwrap();
				// Deleted under the run, as a page that can't be written: every later page would fail the same way.
				fs::remove_file(archive.path()).unwrap();
			},
			None,
		);
		let failures: Vec<_> = outcome
			.events
			.iter()
			.filter_map(|event| {
				if let Event::StoreFailed { number, earlier, .. } = event { Some((*number, *earlier)) } else { None }
			})
			.collect();
		assert_eq!(failures, vec![(2, 1)], "said once, for the first page that failed");
		assert_eq!(outcome.cancelled, 1, "the scanner is stopped");
		assert!(stored(&outcome.events).is_empty());
		assert!(matches!(outcome.events.last(), Some(Event::Ended(_))), "the run still ends");
	}

	#[test]
	fn a_scanner_that_never_returns_is_given_up_on() {
		let dir = tempfile::tempdir().unwrap();
		let archive = Mutex::new(ScanArchive::create(dir.path(), "Scan").unwrap());
		let run = Run::default();
		// The scanner thread keeps the sender and says nothing more.
		let (_to_ocr, from_scanner) = mpsc::sync_channel::<ToOcr>(QUEUE_DEPTH);
		run.abandoned.store(true, Ordering::Release);
		let mut events = Vec::new();
		let mut job = OcrJob {
			run: &run,
			mode: Mode::Feeder,
			archive: &archive,
			replace: None,
			text_store: None,
			cancel: &|| {},
		};
		job.read_pages(&from_scanner, &mut engine, &mut |event| events.push(event));
		assert!(matches!(&events[..], [Event::Ended(summary)] if summary.error.is_some()));
	}

	#[test]
	fn the_watchdog_waits_for_a_cancel_or_the_timeout() {
		let run = Run::default();
		run.heard();
		assert!(!run.overdue());
		*run.cancelled_at.lock().unwrap() = Instant::now().checked_sub(GIVE_UP_AFTER_CANCEL + Duration::from_secs(1));
		assert!(run.overdue());
		run.scanner_ended.store(true, Ordering::Release);
		assert!(!run.overdue(), "a scanner that returned is never given up on");
	}

	#[test]
	fn the_timeout_counts_silence_from_the_scanner_not_the_length_of_the_run() {
		let run = Run::default();
		let long_ago = Instant::now()
			.checked_sub(crate::scanning::scanner::TIMEOUT + GIVE_UP_AFTER_CANCEL + Duration::from_secs(1));
		*run.last_heard.lock().unwrap() = long_ago;
		run.in_page.store(true, Ordering::Release);
		assert!(!run.overdue(), "time a page spends waiting on OCR isn't the scanner's");
		run.in_page.store(false, Ordering::Release);
		assert!(run.overdue(), "a scanner silent past the timeout is given up on");
		run.heard();
		assert!(!run.overdue(), "a page just handed over starts the count again");
	}

	#[test]
	fn text_recognized_on_a_scan_in_progress_follows_it_as_pages_are_added() {
		let dir = tempfile::tempdir().unwrap();
		let line = |text: &str| vec![RecognizedLine { text: text.into(), words: vec![] }];
		let before = dir.path().join("old.ocr");
		let after = dir.path().join("new.ocr");
		save_pages(&before, &[(1, line("read on page one")), (2, line("old two"))]).unwrap();
		save_pages(&after, &[(2, line("new two"))]).unwrap();
		carry_text_over(&before, &after);
		assert!(!before.exists());
		let pages: Vec<(i32, String)> =
			load_pages(&after).into_iter().map(|(page, lines)| (page, lines[0].text.clone())).collect();
		assert_eq!(pages, vec![(1, "read on page one".to_string()), (2, "new two".to_string())]);
	}

	#[test]
	fn the_queue_to_ocr_holds_only_a_few_pages() {
		let (to_ocr, _from_scanner) = mpsc::sync_channel::<ToOcr>(QUEUE_DEPTH);
		for _ in 0..QUEUE_DEPTH {
			to_ocr.try_send(ToOcr::Unreadable).unwrap();
		}
		assert!(to_ocr.try_send(ToOcr::Unreadable).is_err(), "the scanner waits once the queue is full");
	}
}
