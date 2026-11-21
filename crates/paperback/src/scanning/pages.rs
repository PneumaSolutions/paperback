//! What happens to one scanned page between the scanner and the document: the blank check, OCR with a retry in the other orientations, and the checks for a page that isn't lined up. The OCR engine is passed in, so tests run these with a stand-in.

use std::borrow::Cow;

use paperback_core::{
	ocr::RecognizedLine,
	scan::check::{self, Turn},
};
use paperback_scan::DynamicImage;

use crate::ocr::{self, OcrError};

/// Lines read, and the size of the image they were read from, which their word boxes are measured in.
pub type Reading = (Vec<RecognizedLine>, u32, u32);

#[derive(Debug, Default)]
pub struct ReadPage {
	pub lines: Vec<RecognizedLine>,
	/// Words, or characters in scripts written without spaces.
	pub amount: check::TextAmount,
	/// Text that looks like text, which bleed-through from the other side and specks don't make.
	pub plausible: usize,
	pub turn: Option<Turn>,
	/// The page turned right way up, to replace the scanned one.
	pub turned: Option<DynamicImage>,
	pub little_text: bool,
	pub cut_off: bool,
	pub error: Option<OcrError>,
}

#[must_use]
pub fn is_blank(page: &DynamicImage) -> bool {
	check::is_blank(check::ink_share(page))
}

/// How a run treats each page it's handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
	/// A blank page is asked about: it's probably face down.
	Flatbed,
	/// One side of each sheet, every page kept; a blank one is said to look blank, since it may be face down.
	Feeder,
	/// One side of each sheet, leaving out pages with no text (opt-in, since OCR finds none in a photo or a signature).
	FeederSkip,
	/// Both sides of each sheet, keeping only the sides with real text, so a one-sided stack, face up or face down, comes out as its printed pages (opt-in).
	BothSidesSmart,
	/// Both sides of each sheet, every one kept: the default for two-sided scanning.
	BothSidesAll,
}

/// Fewer plausible words than this and a side of a two-sided scan is taken to be blank: a page number, a speck, or show-through from the printed side.
const SIDE_TEXT_WORDS: usize = 3;

/// Decided on what OCR read rather than on how dark the side is, since bleed-through can be as dark as faint print. With no OCR engine the darkness is all there is.
#[must_use]
pub fn side_has_text(read: &ReadPage, page: &DynamicImage) -> bool {
	if read.error.is_some() { !is_blank(page) } else { read.plausible >= SIDE_TEXT_WORDS }
}

/// `recognize` reads one image; it's called again for each other orientation tried, only when the page as scanned reads poorly.
pub fn read(page: &DynamicImage, recognize: &mut dyn FnMut(&DynamicImage) -> Result<Reading, OcrError>) -> ReadPage {
	let (mut lines, mut width, mut height) = match recognize(page) {
		Ok(reading) => reading,
		Err(error) => return ReadPage { error: Some(error), ..ReadPage::default() },
	};
	let mut result = ReadPage::default();
	let upright = check::plausible_text(&lines);
	if check::worth_turning(upright) {
		let mut tried = vec![(Turn::None, upright)];
		let mut best: Option<(Turn, DynamicImage, Reading)> = None;
		for turn in Turn::OTHERS {
			let turned = turn.apply(page);
			let Ok(reading) = recognize(&turned) else { continue };
			let words = check::plausible_text(&reading.0);
			let beats_best = tried.iter().skip(1).all(|(_, other)| words > *other);
			tried.push((turn, words));
			if beats_best {
				best = Some((turn, turned, reading));
			}
			// Clearly readable now, so the remaining turns would only cost time.
			if check::reads_clearly(words) {
				break;
			}
		}
		if let Some((turn, turned, reading)) = best.filter(|(turn, ..)| check::best_turn(&tried) == *turn) {
			(lines, width, height) = reading;
			result.turn = Some(turn);
			result.turned = Some(turned);
		}
	}
	result.amount = check::text_amount(&lines);
	result.plausible = check::plausible_text(&lines);
	result.little_text = check::little_text(result.amount);
	result.cut_off = check::cut_off_edges(&lines, width, height).any();
	result.lines = lines;
	result
}

/// The app's own OCR engine, given the page shrunk to what the engine takes.
pub fn recognize_with_engine(page: &DynamicImage) -> Result<Reading, OcrError> {
	let max = ocr::max_image_dimension();
	let image: Cow<'_, DynamicImage> = if max > 0 && page.width().max(page.height()) > max {
		Cow::Owned(page.thumbnail(max, max))
	} else {
		Cow::Borrowed(page)
	};
	let rgba = image.to_rgba8();
	let (width, height) = rgba.dimensions();
	Ok((ocr::recognize_lines(rgba.as_raw(), width, height)?, width, height))
}

#[cfg(test)]
mod tests {
	use image::{GrayImage, Luma};
	use paperback_core::ocr::RecognizedWord;

	use super::*;

	/// A page whose top-left pixel is dark only when it's right way up; the stand-in engine reads a paragraph then, and scraps otherwise.
	fn page_turned(turn: Turn) -> DynamicImage {
		let mut image = GrayImage::from_pixel(40, 60, Luma([255]));
		image.put_pixel(0, 0, Luma([0]));
		let upright = DynamicImage::ImageLuma8(image);
		// Undo the turn that would put it right, so applying `turn` restores it.
		match turn {
			Turn::None => upright,
			Turn::Half => upright.rotate180(),
			Turn::Quarter => upright.rotate270(),
			Turn::ThreeQuarters => upright.rotate90(),
		}
	}

	fn words(text: &str, width: f32) -> Vec<RecognizedLine> {
		text.split(". ")
			.enumerate()
			.map(|(row, line)| RecognizedLine {
				text: line.to_string(),
				words: line
					.split(' ')
					.enumerate()
					.map(|(i, word)| RecognizedWord {
						text: word.to_string(),
						x: (i as f32).mul_add(3.0, 2.0),
						y: (row as f32).mul_add(4.0, 5.0),
						width: width.min(2.5),
						height: 2.0,
					})
					.collect(),
			})
			.collect()
	}

	const PARAGRAPH: &str = "The committee met on Tuesday to review the proposal. Members agreed that the plan should move forward after small changes. A final vote will be held next month once the budget is ready. Everyone thanked the staff for preparing the report so quickly";

	fn stand_in(calls: &mut usize) -> impl FnMut(&DynamicImage) -> Result<Reading, OcrError> + '_ {
		move |image| {
			*calls += 1;
			let readable = image.to_luma8().get_pixel(0, 0).0[0] == 0;
			let text = if readable { PARAGRAPH } else { "' . ~ l1 ," };
			Ok((words(text, 2.0), image.width(), image.height()))
		}
	}

	#[test]
	fn an_upright_page_is_read_once() {
		let mut calls = 0;
		let read = read(&page_turned(Turn::None), &mut stand_in(&mut calls));
		assert_eq!(calls, 1);
		assert_eq!(read.turn, None);
		assert!(read.turned.is_none());
		assert!(read.amount.words > 30);
		assert!(!read.little_text);
	}

	#[test]
	fn an_upside_down_page_is_turned_and_read_the_right_way() {
		let mut calls = 0;
		let read = read(&page_turned(Turn::Half), &mut stand_in(&mut calls));
		assert_eq!(read.turn, Some(Turn::Half));
		assert_eq!(read.turned.as_ref().map(|page| (page.width(), page.height())), Some((40, 60)));
		assert!(read.amount.words > 30);
		// Upside down is tried first, and reads clearly, so sideways is never tried.
		assert_eq!(calls, 2);
	}

	#[test]
	fn an_upside_down_page_of_chinese_is_turned_too() {
		// The engine gives each character as its own word, as Windows OCR and Tesseract do.
		let mut engine = |image: &DynamicImage| {
			let text = if image.to_luma8().get_pixel(0, 0).0[0] == 0 {
				"委员会星期二开会审查了这项提案成员们同意在小的修改之后推进这个计划"
			} else {
				"' ~ 1l"
			};
			let spaced = text.chars().map(String::from).collect::<Vec<_>>().join(" ");
			Ok((vec![RecognizedLine { text: spaced, words: vec![] }], image.width(), image.height()))
		};
		let read = read(&page_turned(Turn::Half), &mut engine);
		assert_eq!(read.turn, Some(Turn::Half));
		assert!(read.amount.in_characters());
		assert!(read.amount.characters > 30);
		assert!(!read.little_text);
	}

	#[test]
	fn a_sideways_page_is_turned_a_quarter() {
		let mut calls = 0;
		let read = read(&page_turned(Turn::Quarter), &mut stand_in(&mut calls));
		assert_eq!(read.turn, Some(Turn::Quarter));
		assert_eq!(read.turned.unwrap().height(), 60);
	}

	#[test]
	fn a_page_that_reads_badly_every_way_keeps_its_orientation_and_says_little_text() {
		let mut calls = 0;
		let mut engine = |image: &DynamicImage| {
			calls += 1;
			Ok((words("Figure 3", 2.0), image.width(), image.height()))
		};
		let read = read(&page_turned(Turn::None), &mut engine);
		assert_eq!(calls, 4);
		assert_eq!(read.turn, None);
		assert!(read.little_text);
	}

	#[test]
	fn text_against_the_edge_is_flagged() {
		let mut engine = |image: &DynamicImage| {
			let mut lines = words(PARAGRAPH, 2.0);
			lines[0].words[0].x = 0.0;
			Ok((lines, image.width(), image.height()))
		};
		assert!(read(&page_turned(Turn::None), &mut engine).cut_off);
	}

	#[test]
	fn an_engine_failure_is_kept_rather_than_read_as_no_text() {
		let mut engine = |_: &DynamicImage| Err(OcrError::Unavailable);
		let read = read(&page_turned(Turn::None), &mut engine);
		assert!(matches!(read.error, Some(OcrError::Unavailable)));
		assert_eq!(read.amount.units(), 0);
	}

	#[test]
	fn a_side_with_only_scraps_or_a_page_number_has_no_text() {
		let page = page_turned(Turn::None);
		let mut scraps = |image: &DynamicImage| Ok((words("' 12 ~ l1", 2.0), image.width(), image.height()));
		assert!(!side_has_text(&read(&page, &mut scraps), &page));
		let mut calls = 0;
		assert!(side_has_text(&read(&page, &mut stand_in(&mut calls)), &page));
	}

	#[test]
	fn a_face_down_back_side_reads_once_turned_over() {
		let page = page_turned(Turn::Half);
		let mut calls = 0;
		let read = read(&page, &mut stand_in(&mut calls));
		assert!(side_has_text(&read, &page));
		assert_eq!(read.turn, Some(Turn::Half));
	}

	#[test]
	fn without_an_ocr_engine_a_side_is_judged_by_its_ink() {
		let mut engine = |_: &DynamicImage| Err(OcrError::Unavailable);
		let blank = DynamicImage::ImageLuma8(GrayImage::from_pixel(850, 1100, Luma([240])));
		assert!(!side_has_text(&read(&blank, &mut engine), &blank));
	}

	#[test]
	fn blank_paper_is_blank_and_a_printed_page_is_not() {
		assert!(is_blank(&DynamicImage::ImageLuma8(GrayImage::from_pixel(850, 1100, Luma([240])))));
		let mut printed = GrayImage::from_pixel(850, 1100, Luma([240]));
		for y in (100..1000).step_by(30) {
			for x in 100..750 {
				for dy in 0..10 {
					printed.put_pixel(x, y + dy, Luma([20]));
				}
			}
		}
		assert!(!is_blank(&DynamicImage::ImageLuma8(printed)));
	}
}
