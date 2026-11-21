# Scanning: developer notes

Notes for reviewing File > Scan Document: how it's built, where it departs from the spec and why, a security review, and what was tested. The user guide section is "Scanning documents" in `readme.md`. (This file isn't a user guide: the docs build only picks up `readme*.md`.)

## How it's built

* `crates/paperback-scan`: the scanner interface, a thin layer over [pagescan](https://github.com/PneumaSolutions/pagescan) on all three platforms. `Scanner` lists scanners with their sources, scans pages as images (each page handed over as soon as it's scanned), and cancels. `SystemScanner` is pagescan; `fake::FakeScanner` hands back scripted pages for tests. Sources are what the reader picks: flatbed, feeder (one side), feeder (both sides).
* pagescan (Pneuma's scanner library, MIT): WIA 2.0 with a WIA 1.0 fallback on Windows, ImageCaptureCore on macOS, SANE on Linux, loaded at run time with `libloading`. Paperback uses `scan_with_pages` (per-page callback) and `ErrorKind::SaneMissing`, both added for Paperback on pagescan's `paperback-api` branch.
* `paperback-core`:
  * `scan::archive`: the scanned document, a `.pbscan` zip of PNG pages. Every change writes a new archive beside the old one and renames it over it.
  * `parser::scan`: reads a `.pbscan` like a comic archive. Every page is an OCR placeholder with the `ImageOnlyPage` marker, so the existing OCR code (Recognize Current Page, Batch OCR, the saved-pages store) works on it unchanged. Each page gets an anchor, so a reload keeps the caret on its page.
  * `scan::check`: the feedback checks. Blank (share of ink, ignoring a 2% border where the lid and paper edges leave shadows), plausible words (to tell a page that reads from one upside down or sideways), the best turn, little text, and words against an edge.
  * `ocr::PageRenderer` renders `.pbscan` pages like comic pages, and scanned pages get paragraph joining like PDFs.
* The desktop app:
  * `scanning/`: what is remembered per scanner, what happens to a page (`pages::read`: OCR, then the other orientations when the page reads poorly), and every message the run says.
  * `ui/dialogs/scan.rs`: the Scan Document dialog and the Scanning window.
  * `ui/scan_session.rs`: the run itself. A scanner thread hands pages to an OCR thread, which checks and reads them in order and posts results to the UI thread with `when_unlocked`. The UI thread owns the archive and the tab.
  * `ui/document_manager/scan.rs`: opens the scan after its first page, and re-reads it after each change.

### The flow

1. File > Scan Document. The first time, the scanners are listed behind a cancellable progress dialog. After that, the last scanner is offered without asking the system, with Find Other Scanners in the dialog.
2. Source, color and resolution default to the flatbed (or the feeder, both sides, on a scanner without one), grayscale, 300 DPI. They're remembered per scanner, as one JSON value under the app setting `scanners`, because scanner IDs contain backslashes and braces.
3. The Scanning window stays open: a status line that's announced (a UIA notification on Windows), and only the buttons that apply right now.
4. Flatbed: scan, open the document after the first page, read the page and say "Page N: W words." Then Enter scans the next page and Escape finishes. Feeder: every page is announced as its text is read, then a summary, then Enter scans more into the same document.
5. A page goes into the archive before its OCR, so the document shows it as a placeholder and the text replaces the placeholder when OCR finishes. That's true while the window is open and after it's closed.

## Departures from the spec, and why

1. **WIA through classic COM, not Windows.Devices.Scanners.** pagescan uses WIA 2.0 (`IWiaDevMgr2`) with a WIA 1.0 fallback. On our test laptop, the HP Color LaserJet Pro MFP 3301's network eSCL device only appeared through WIA 1.0, which the WinRT API can't reach. This was already in Mike's reply to Quin.
2. **SANE lives in pagescan.** That was Quin's answer. paperback-scan has no platform code of its own.
3. **A new `.pbscan` format.** The spec says "a scanned document type ... holding page images (like comic archives)". A CBZ would work too, but comic pages deliberately don't join OCR lines into paragraphs, and scans should. The format is registered in `paperback-formats` (opt-in in the installer, MIME `application/x-paperback-scan`) so that reopening and Recent Documents work.
4. **Where scans are kept.** In `scans` under Paperback's settings folder (mode 0700 on Unix), named "Scan", "Scan 2", and so on. The name is translatable. The date isn't in the name, because the app has no local-time crate. Reveal File in Folder finds them.
5. **Re-reading rather than appending.** After each page, or a page turned the right way up, the tab is re-parsed in place (the same code as F5). The OCR text comes back from the saved-pages store. That store is keyed by the file's content digest, which changes with every page. So the run rewrites the store's file under the new digest and removes the old one each time. That way a reopened scan, or a reload, always has its text. Re-parsing a zip of page names is cheap; appending to a live session would have needed new buffer surgery.
6. **OCR for scanned pages doesn't go through the Batch OCR pool.** The run needs each page's result in order, as it arrives, for the announcements and checks. The pool works from a file and a list of page numbers. The run reuses the same engine call (`crate::ocr::recognize_lines`), the same saved-pages store, and the same restore path. Batch OCR and Recognize Current Page work on a scan afterwards.
7. **Rescan and Skip, only on the flatbed.** As the spec says, a page that's upside down or sideways is turned and kept automatically, with a brief note ("Page 3 was upside down; corrected."). Rescan or Skip is offered only for a blank page (probably face down; Skip leaves it out) and for a page with very little text or text touching an edge (alignment; Rescan replaces it, Skip keeps it). A feeder can't take one page back, so there these are announced instead: blank pages are left out and counted, and the summary suggests they may have been face down.
8. **Scan Document has no default shortcut.** None of the obvious chords was free on all platforms. The reader can set one in Customize Keyboard Shortcuts.
9. **macOS isn't tested.** ImageCaptureCore delivers on the main thread, so there the scan runs on the UI thread (`Scanner::needs_main_thread`), and pagescan pumps the run loop. Pages are still read on the OCR thread, but the document only updates when the scan returns. pagescan's macOS backend itself hasn't run on hardware yet.
10. **Searchable PDF is a follow-up,** as agreed. Plain text and Markdown export work now.

### Proposal for Quin: a smart two-sided feeder (Mike's addition)

This goes beyond the spec. Mike asked for it, and it's here for Quin to accept, change or drop:

* On a scanner whose feeder can scan both sides, "Document feeder, both sides" is the feeder's default. It's listed before "one side".
* Both sides of every sheet are scanned, and only the sides with real text are kept. That's decided by OCR (at least 3 plausible words on the side, after trying it the other way up), not by brightness, so show-through from thin paper doesn't count. A side with no ink at all is dropped without OCR. If no OCR engine is available, the ink check alone decides.
* So a one-sided stack comes out as just its printed pages, whichever way up it was loaded: a face-down sheet's printed side arrives second and upside down, and is turned. Sheets printed on both sides keep both.
* Only kept pages are announced. The end sums up: "5 sheets, 7 pages, 3 blank sides skipped."
* "Keep blank sides" (a checkbox, remembered per scanner) keeps every side. "Document feeder, one side" is plain one-sided scanning, where a blank page is left out and counted, with the hint that it may have been face down.
* Without duplex hardware nothing changes: on the flatbed, a blank page means "may be face down; turn it over and Rescan".

Code: `pages::Mode`, `pages::side_has_text`, and `scan_session::add_if_it_has_text`. Tests use the fake scanner, a 3-sheet run in each mode (`ui::scan_session::tests`).

## Security review

Scope: paperback-scan, the new paperback-core modules (`scan::*`, `parser::scan`), the desktop scanning code, and pagescan's Linux backend and `scan_with_pages`. pagescan's own review is in its `SECURITY_REVIEW.md` (findings 24 and 25 cover the SANE backend).

* **Image data from drivers is untrusted.** pagescan bounds every page before decoding it: 1 GB of driver data, 250 megapixels, 500 pages, 4 GB per scan. Dimensions are checked before pixels are allocated, on all three backends. SANE frame parameters are validated before allocation, and `sane_read` lengths are clamped. Paperback receives decoded `DynamicImage`s, so it never decodes driver bytes itself. The checks shrink a page to 1000 pixels before looking at it. OCR gets at most the engine's own maximum dimension.
* **Opening a `.pbscan` from elsewhere** goes through the same zip and image-decoding path as a CBZ. The parser reads only entry names. Pages are decoded when they're OCR'd, by `image::load_from_memory`, with the image crate's default limits. That's an existing property of the comic path, not new here. A tighter limit on decoded page size for both would be a reasonable follow-up.
* **Temporary and saved files.** Archive rewrites use `tempfile` in the archive's own folder (created 0600 on Unix), are synced, then renamed over the archive. A crash leaves at most a `.scan-*.tmp` file, never a half-written archive. The scans folder is set to 0700 on Unix. On Windows it inherits the per-user ACL of the settings folder. Entry names are fixed (`page-0001.png`). The archive's file name comes from a translated string, with characters a file system refuses replaced, and never contains a path separator. Existing files are never overwritten: "Scan 2", "Scan 3" and so on. The OCR text store is written only on the UI thread, so the run and Batch OCR never write it at once.
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
* Real hardware, 2026-11-21: a Windows 11 laptop with an HP Color LaserJet Pro MFP 3301 over the network (eSCL, through WIA). The app was cross-built with mingw. The UI test passed: scan, announcement, document opened with its text, archive saved. By hand through UI Automation:
  * The first-time list, remembering the scanner, and Find Other Scanners.
  * Flatbed scans: about 10 to 25 seconds a page, with OCR under a second.
  * The cut-off check correctly flagged a page lying off the left edge of the glass (words starting at x = 0).
  * Rescan replaced page 1, Skip kept the page and gave the prompt, and Escape went to the document.
  * The text came back from the saved-pages store on reopening.
  * With the feeder empty and "both sides" chosen, this HP scanned the glass instead and reported one page. The two-sided logic counted it as "1 sheet, 1 page".
  * Not tested on hardware: blank pages, turned pages and a real feeder run (the feeder was empty); USB flatbeds; macOS; Linux with a real scanner.

## Building

* pagescan is a git dependency at a fixed revision, because it isn't on crates.io yet. **Its repository is private for now, so CI on the public fork can't fetch it, and every job will fail at dependency resolution until it's public.** Locally, either let cargo fetch it with your GitHub credentials (`CARGO_NET_GIT_FETCH_WITH_CLI=true` with `gh auth setup-git`), or point it at a checkout from a `.cargo/config.toml` that you don't commit:

  ```toml
  [patch."https://github.com/PneumaSolutions/pagescan"]
  pagescan = { path = "../pagescan" }
  ```

* TODO: switch to the crates.io release of pagescan once it's published, and drop the note above.

## Questions for Quin

1. Is a new `.pbscan` format right, or would you rather scans be CBZ (or something else) with paragraph joining decided some other way?
2. Where should scans live: the settings folder (as now), Documents, or asking the first time? And should old scans ever be cleaned up?
3. A default shortcut for Scan Document?
4. The two-sided smart feeder above (Mike's proposal): keep it as the default for duplex feeders, make it opt-in, or drop it?
5. The alignment prompt on the flatbed asks "Rescan or Skip" after every page that looks out of line. Is that the right amount of interruption, or should it only speak and let the reader rescan with a key?
6. Is the Scanning window (a small modal window with a spoken status line) the right shape, or would you prefer the scan to run behind the document with keys in the reader?
