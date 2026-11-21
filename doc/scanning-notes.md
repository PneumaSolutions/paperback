# Scanning: developer notes

Notes for reviewing File > Scan Document: how it's built, where it departs from the spec and why, how Quin's answers of 2026-11-21 were applied, a security review, and what was tested. The user guide section is "Scanning documents" in `readme.md`. (This file isn't a user guide: the docs build only picks up `readme*.md`.)

## How it's built

* `crates/paperback-scan`: the scanner interface, a thin layer over [pagescan](https://github.com/PneumaSolutions/pagescan) on all three platforms. `Scanner` lists scanners with their sources, scans pages as images (each page handed over as soon as it's scanned), and cancels. `SystemScanner` is pagescan; `fake::FakeScanner` hands back scripted pages for tests. Sources are what the reader picks: flatbed, feeder (one side), feeder (both sides).
* pagescan (Pneuma's scanner library, MIT): WIA 2.0 with a WIA 1.0 fallback on Windows, ImageCaptureCore on macOS, SANE on Linux, loaded at run time with `libloading`. Paperback uses `scan_with_pages` and `ErrorKind::SaneMissing`, both added for Paperback on pagescan's `paperback-api` branch. `scan_with_pages` streams: each page is handed over by value as soon as it's decoded and isn't kept in the outcome, so a long run holds one page at a time in the library, and its 4 GB cap counts only pages not yet handed over.
* `paperback-core`:
  * `scan::archive`: the scanned document, a comic book archive (`.cbz`) of PNG pages with a `ComicInfo.xml` that names it and marks it with `<ScanInformation>Paperback</ScanInformation>`. Every change writes a new archive beside the old one and renames it over it. `is_scan` tells Paperback's scans from other comics, so their OCR lines are joined into paragraphs like a PDF's.
  * The existing comic parser and renderer read it: every page is an OCR placeholder with the `ImageOnlyPage` marker, so Recognize Current Page, Batch OCR and the saved-pages store work on it unchanged. The comic parser now gives each page an anchor, so a reload of an archive that grew keeps the caret on its page.
  * `scan::check`: the feedback checks. Blank (share of ink, ignoring a 2% border where the lid and paper edges leave shadows), plausible words (to tell a page that reads from one upside down or sideways), the best turn, little text, and words against an edge.
* The desktop app:
  * `scanning/`: what is remembered per scanner, what happens to a page (`pages::read`: OCR, then the other orientations when the page reads poorly), every message the run says, and `store` (the scans-in-progress folder, Save Scan As, discarding, and the sweep at start).
  * `ui/dialogs/scan.rs`: the Scan Document dialog and the Scanning window.
  * `ui/scan_session.rs`: the run itself. A scanner thread hands pages to an OCR thread, which checks and reads them in order and posts results to the UI thread with `when_unlocked`. The UI thread owns the archive and the tab.
  * `ui/document_manager/scan.rs`: opens the scan after its first page, and re-reads it after each change.

### The flow

1. File > Scan Document. The first time, the scanners are listed behind a cancellable progress dialog. After that, the last scanner is offered without asking the system, with Find Other Scanners in the dialog.
2. Source, color and resolution default to the flatbed (or the one-sided feeder on a scanner without one), grayscale, 300 DPI. They're remembered per scanner, as one JSON value under the app setting `scanners`, because scanner IDs contain backslashes and braces.
3. The Scanning window stays open: a status line that's announced (a UIA notification on Windows), and only the buttons that apply right now.
4. Flatbed: scan, open the document after the first page, read the page and say "Page N: W words." Then Enter scans the next page and Escape finishes. A page that may not be lined up is announced and gets a Rescan button beside Scan Next Page, which keeps Enter; a blank page stops with Rescan or Skip. Feeder: every page is announced as its text is read, then a summary, then Enter scans more into the same document.
5. A page is read (and turned if need be) before it goes into the archive, so it's added once, with its text. The document opens after the first page has been read, and later pages appear as they're read: within a second or so while the Scanning window is open, and all at once at the end of the run after it's closed (see departure 5).

## Departures from the spec, and why

1. **WIA through classic COM, not Windows.Devices.Scanners.** pagescan uses WIA 2.0 (`IWiaDevMgr2`) with a WIA 1.0 fallback. On our test laptop, the HP Color LaserJet Pro MFP 3301's network eSCL device only appeared through WIA 1.0, which the WinRT API can't reach. This was already in Mike's reply to Quin.
2. **SANE lives in pagescan.** That was Quin's answer. paperback-scan has no platform code of its own.
3. **Scans are comic book archives.** Quin's answer. `paperback-formats` and the parsers are untouched; the archive is written with a `ComicInfo.xml` (title, and `ScanInformation` = Paperback), which other comic readers show and which tells Paperback to join OCR lines into paragraphs (`archive::is_scan`, checked in `joins_lines`). The OCR text persists through Paperback's existing saved-pages store, keyed by the archive's contents, rather than a sidecar in the zip: that's the code every OCR source already uses to come back without OCR, it needs no new reading code, and a copy of the file finds the same text. Save Scan As copies the text across, since the new title changes the contents. A shared .cbz travels without its text (the recipient can Batch OCR it); a sidecar entry could be added later if that matters.
4. **Scans in progress, Save Scan As, and clearing.** Quin's answer: the `scans` folder (in Paperback's settings folder, 0700 on Unix) is for work in progress, named "Scan", "Scan 2", and so on. File > Save Scan As (no shortcut) writes a copy where the reader chooses, titled by its file name, with its text, and the saved copy replaces the in-progress one in its tab at the same position. What I chose for old ones: closing an unsaved scan asks Save / Don't Save / Cancel, and Don't Save deletes the archive, its text and its Recent Documents entry. A scan left open at exit comes back with "reopen documents"; at the next start, any scan in progress that isn't open again (restore off, or a crash before its first page) is deleted, with any stray temporary files. A run still delivering pages to a scan that was just discarded stops rather than bringing it back.
5. **Writing and re-reading, sized for long runs.** A page is OCR'd (and turned) before it's added, so each page reaches the archive once, already the right way up, with its text: one append in place (`ZipWriter::new_append`, the page plus a new central directory) and one fsync. Only the first page, a rescan's replacement and Save Scan As write a whole archive (to a temporary file, renamed over). The run keeps its own page count, so adding a page reads nothing. The tab shows new pages by re-parsing in place (the same code as F5), with the text coming back from the saved-pages store, which the run rewrites under the archive's new digest (the store is keyed by contents) and whose previous file it removes. That re-read is coalesced: the first page opens the document at once; while the Scanning window is open, pages arriving within 0.8 s share one re-read; once the window is closed (the reader may be reading), pages still being read are added in a single re-read when the run ends, so the text under the caret isn't replaced mid-reading. Each page has an anchor, so a re-read keeps the caret on its page. A crash mid-append can leave the scan in progress unreadable; a saved scan is never appended to. Pages travel from the scanner thread to the OCR thread as PNG (encoded in the scanner library's callback, while the driver goes on in its own thread on Windows, or its own buffers under SANE), so a feeder faster than OCR queues compressed pages, not 8 MB decoded ones; that PNG is what's stored unless the page was turned.
6. **OCR for scanned pages doesn't go through the Batch OCR pool.** The run needs each page's result in order, as it arrives, for the announcements and checks. The pool works from a file and a list of page numbers. The run reuses the same engine call (`crate::ocr::recognize_lines`), the same saved-pages store, and the same restore path. Batch OCR and Recognize Current Page work on a scan afterwards.
7. **Rescan without stopping, for pages out of line.** Quin's answer. A page that's upside down or sideways is turned and kept automatically, with a brief note ("Page 3 was upside down; corrected."). A page with very little text or text touching an edge is announced, and on the flatbed a Rescan button appears beside Scan Next Page: Enter still goes on, and Rescan scans the page again in its place. A blank flatbed page (probably face down) still stops with Rescan or Skip, for now, as Quin asked. A feeder can't take one page back, so there these are announced only. From the feeder, blank pages follow the same opt-in Skip blank pages setting as two-sided scanning (decided after Quin's review): off by default, a blank page is kept and announced as looking blank, since a page with only a faint signature could otherwise be lost; on, pages without text are left out and counted.
8. **No default shortcuts** for Scan Document or Save Scan As, as Quin asked. They can be set in Customize Keyboard Shortcuts.
9. **macOS isn't tested.** ImageCaptureCore delivers on the main thread, so there the scan runs on the UI thread (`Scanner::needs_main_thread`), and pagescan pumps the run loop. Pages are still read on the OCR thread, but the document only updates when the scan returns. pagescan's macOS backend itself hasn't run on hardware yet.
10. **Searchable PDF is a follow-up,** as agreed. Plain text and Markdown export work now.

### Skipping blank pages is opt-in

Mike proposed skipping blank sides for two-sided scanning; Quin made it opt-in, since OCR finds no words in photos, signatures or handwriting, and after his review it covers the one-sided feeder too.

* "Document feeder, one side" (listed first, the feeder's default) and "Document feeder, both sides" keep every page or side by default. The one-sided feeder announces a blank page as looking blank, since the stack may be face down.
* "Skip blank pages" (a checkbox for either feeder source, off unless chosen, remembered per scanner) keeps only the pages or sides where OCR finds real text: at least 3 plausible words or characters (see below), after trying the page the other way up, so show-through doesn't count. A page with no ink at all is dropped without OCR; with no OCR engine, the ink check alone decides. A face-down sheet's printed side arrives second and upside down, and is turned. Only kept pages are announced; the end sums up ("5 sheets, 7 pages, 3 blank sides skipped", or for one side "Scanned 7 pages. 2 blank pages left out...").
* On the flatbed nothing changes: a blank page means "may be face down; turn it over and Rescan".

Code: `pages::Mode`, `pages::side_has_text`, `scan_session::sort_pages`. Tests run fake feeders through each mode (`ui::scan_session::tests`).

### Scripts written without spaces

Windows OCR and Tesseract report each Chinese or Japanese character (and each Thai, Lao, Khmer or Burmese one) as its own word, so counting words with two or more letters scored such pages zero: every page tried all four orientations, a turn was never chosen, skipping dropped every side, and the count announced was of characters called words. `scan::check` now counts characters of Han, Hiragana, Katakana, Thai, Lao, Khmer and Myanmar one by one as plausible text, and the announcement says "Page 3: 512 characters." when such characters outnumber words (`TextAmount`). Korean is written with spaces and counts by the word. The thresholds (25 to try turning, 40 to stop trying, 3 for a side with text, 15 for little text) apply to words and characters alike.

## Security review

Scope: paperback-scan, the new paperback-core modules (`scan::*`, `parser::scan`), the desktop scanning code, and pagescan's Linux backend and `scan_with_pages`. pagescan's own review is in its `SECURITY_REVIEW.md` (findings 24 and 25 cover the SANE backend).

* **Image data from drivers is untrusted.** pagescan bounds every page before decoding it: 1 GB of driver data, 250 megapixels, 500 pages, 4 GB per scan. Dimensions are checked before pixels are allocated, on all three backends. SANE frame parameters are validated before allocation, and `sane_read` lengths are clamped. Paperback receives decoded `DynamicImage`s, so it never decodes driver bytes itself. The checks shrink a page to 1000 pixels before looking at it. OCR gets at most the engine's own maximum dimension.
* **Opening a scan from elsewhere** is opening a CBZ, through the existing comic path. The parser reads only entry names. Pages are decoded when they're OCR'd, by `image::load_from_memory`, with the image crate's default limits. That's an existing property of the comic path, not new here. A tighter limit on decoded page size for both would be a reasonable follow-up.
* **Temporary and saved files.** Archive rewrites use `tempfile` in the archive's own folder (created 0600 on Unix), are synced, then renamed over the archive. A crash leaves at most a `.scan-*.tmp` file, never a half-written archive. The scans folder is set to 0700 on Unix. On Windows it inherits the per-user ACL of the settings folder. Entry names are fixed (`page-0001.png`, `ComicInfo.xml`, whose title is XML-escaped). `is_scan` reads at most 64 KB of a `ComicInfo.xml`. The archive's file name comes from a translated string, with characters a file system refuses replaced, and never contains a path separator. Existing files in the scans folder are never overwritten: "Scan 2", "Scan 3" and so on. Save Scan As overwrites only after the file picker's own confirmation. Deleting is limited to files directly in the scans folder (compared after canonicalizing), so a saved scan elsewhere is never deleted. The OCR text store is written only on the UI thread, so the run and Batch OCR never write it at once.
* **SANE through `libloading`** (in pagescan): only `libsane.so.1`, then `libsane.so`, found by the dynamic linker. It must export all eleven SANE entry points. It's never unloaded, because backends such as sane-airscan keep threads. All calls but `sane_cancel` are serialized under one lock. `sane_cancel` is allowed asynchronously by the SANE standard, and it's never called on a closed handle. Strings and lists from backends are bounded and copied out.
* **No network calls.** Neither Paperback's scanning code nor pagescan opens a socket. Network scanners are reached by the operating system's own drivers (WIA's eSCL driver, ImageCaptureCore, sane-airscan), as configured by the system. The simulated eSCL scanner in pagescan's CI binds to 127.0.0.1 and isn't shipped.
* **Settings.** The remembered-scanner JSON is parsed defensively. Unknown sources, out-of-range resolutions and damaged JSON fall back to defaults, and a missing last scanner means picking again.
* **Threads.** Only owned data crosses threads: images, PNG bytes, recognized lines. No wx object leaves the UI thread. A run that outlives its window keeps posting to the document. If the reader closed the tab, the next page reopens it.

## Testing

* Unit tests:
  * the archive (order, replace, unique names, no leftover temp files, private permissions)
  * the parser
  * the checks (blank paper, show-through, dust, lid shadows, one short line, plausible words in several scripts, turns, edges)
  * per-page reading with a stand-in OCR engine (upright, upside down, sideways, unreadable, engine failure)
  * remembered settings (round trip, defaults, damaged JSON)
  * the messages
  * the scan pipeline driven by `FakeScanner` (flatbed, one-sided feeder, both sides smart and keep-all, a face-down sheet)
  * paperback-scan's mapping of pagescan types
* Linux CI: Paperback's `test-linux` job installs SANE and enables only SANE's `test` backend (`PAPERBACK_SANE_TEST=1`). `paperback-scan/tests/sane_test_backend.rs` lists it, scans the flatbed, and scans the 10-sheet feeder through pagescan. pagescan's own CI also runs a simulated eSCL network scanner through sane-airscan (flatbed, plus a duplex feeder giving 6 pages from 3 sheets), and checks the `sane_missing` path on a runner without SANE.
* Windows UI test: `tests/scans_document.rs` drives File > Scan Document with UI Automation against a real scanner. It's ignored, and it only runs with `PAPERBACK_SCANNER_TEST=1`, so it isn't in CI's list. The harness now honors `PAPERBACK_TEST_EXE`, so a test binary built elsewhere can run against that machine's copy of the app.
* Real hardware, 2026-11-21: a Windows 11 laptop with an HP Color LaserJet Pro MFP 3301 over the network (eSCL, through WIA). The app was cross-built with mingw. The UI test passed, before and after the CBZ change: scan, announcement, document opened with its text, archive saved. By hand through UI Automation:
  * The first-time list, remembering the scanner, and Find Other Scanners.
  * Flatbed scans: about 10 to 25 seconds a page, with OCR under a second.
  * The cut-off check correctly flagged a page lying off the left edge of the glass (words starting at x = 0).
  * After Quin's answers (CBZ build): a page out of line was announced with Scan Next Page still focused and Rescan beside it; Rescan replaced page 1 without a prompt, Enter went on to page 2, and Escape went to the document.
  * Closing the unsaved scan asked Save / Don't Save; Don't Save deleted the archive and its text.
  * Save Scan As wrote "Bank letter.cbz" with its text; its tab replaced the scan in progress (titled "Bank letter", text present), the scan in progress was deleted, and closing the saved one asked nothing.
  * The text came back from the saved-pages store on reopening.
  * After Quin's code review (appending, one event per page, PNG queue, streaming pagescan): flatbed pages 1 and 2 were appended in place, Rescan replaced page 2, the archive held 2 pages, and both pages' text came back in the tab. Two runs of the UI test then failed on focus (the first time before scanning even started) because someone had started using the laptop, so testing stopped there.
  * With the feeder empty and "both sides" chosen, this HP scanned the glass instead and reported one page. The two-sided logic counted it as "1 sheet, 1 page".
  * Not tested on hardware: blank pages, turned pages and a real feeder run (the feeder was empty); USB flatbeds; macOS; Linux with a real scanner.

## Building

* pagescan is a git dependency at a fixed revision, because it isn't on crates.io yet. **Its repository is private for now, so CI on the public fork can't fetch it, and every job will fail at dependency resolution until it's public.** Locally, either let cargo fetch it with your GitHub credentials (`CARGO_NET_GIT_FETCH_WITH_CLI=true` with `gh auth setup-git`), or point it at a checkout from a `.cargo/config.toml` that you don't commit:

  ```toml
  [patch."https://github.com/PneumaSolutions/pagescan"]
  pagescan = { path = "../pagescan" }
  ```

* TODO: switch to the crates.io release of pagescan once it's published, and drop the note above.

## Quin's code review (2026-11-21) and what changed

1. Quadratic disk writes: pages are appended in place, each written once; the page count is kept in memory (departure 5).
2. Refreshes: one event and one archive change per page; re-reads coalesced while the window is open and deferred to the end of the run once it's closed (departure 5).
3. The scanner-to-OCR channel carries PNG, not decoded pages (departure 5).
4. CJK and other scripts without spaces count by the character (above).
5. pagescan's `scan_with_pages` streams and keeps nothing it hands over; only undelivered pages count toward the cap.
6. Pages are passed to Paperback by value, not cloned.
Decided with it: the one-sided feeder follows the same opt-in Skip blank pages setting.

## Quin's answers (2026-11-21) and what changed

1. CBZ instead of `.pbscan`: done, with the text in the existing saved-pages store (see departure 3).
2. The scans folder is for work in progress, plus Save Scan As; unsaved scans are offered for saving when closed and deleted otherwise (see departure 4).
3. No default shortcut: kept.
4. Blank-side skipping is opt-in; plain two-sided keeps both sides.
5. The Scanning window stays.
6. Pages out of line: announced, with Rescan offered in the window without stopping. Blank pages keep the prompt for now.
