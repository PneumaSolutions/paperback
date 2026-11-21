//! What a sighted person would notice glancing at a scan: nothing on the page, writing upside down or sideways, or text running off an edge. Each check is a guess, so the app words what it says as "might".

use image::{DynamicImage, GrayImage, imageops::FilterType};

use crate::ocr::RecognizedLine;

/// Large enough to keep a line of small type visible, small enough that looking costs nothing next to OCR.
const LOOK_SIZE: u32 = 1000;

/// Darker than the paper by this much counts as ink; a page's show-through from the other side is fainter.
const INK_CONTRAST: u8 = 70;

/// The scanner's lid and the edges of the paper leave dark lines along the border, and a page's margins are wider than this.
const BORDER_PERCENT: u32 = 2;

/// A page with only a page number or one short line has more ink than this; dust and the scanner's edge shadows have less.
const BLANK_INK: f32 = 0.0004;

/// Less plausible text than this (words, or characters of scripts without spaces) and the page may be turned; a page of text has hundreds.
const TRY_TURNING_BELOW: usize = 25;

/// A turn reading this many plausible words wins against any page worth turning, so the turns after it needn't be tried.
const CLEARLY_READ: usize = 40;

/// Fewer words than this on a page that isn't blank: a picture, or a page lying askew.
const LITTLE_TEXT_WORDS: usize = 15;

/// A turn wins only by a clear margin, since some text reads as a few words any way up.
const TURN_MARGIN: f32 = 1.5;
const TURN_MINIMUM: usize = 8;

/// A word box this close to an edge, as a share of the page's size, may have been cut by it.
const EDGE_SHARE: f32 = 0.01;

/// Clockwise turns that put a page right way up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
	None,
	Quarter,
	Half,
	ThreeQuarters,
}

impl Turn {
	/// In the order they're tried: upside down is far more common than sideways.
	pub const OTHERS: [Self; 3] = [Self::Half, Self::Quarter, Self::ThreeQuarters];

	#[must_use]
	pub fn apply(self, page: &DynamicImage) -> DynamicImage {
		match self {
			Self::None => page.clone(),
			Self::Quarter => page.rotate90(),
			Self::Half => page.rotate180(),
			Self::ThreeQuarters => page.rotate270(),
		}
	}
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Edges {
	pub top: bool,
	pub bottom: bool,
	pub left: bool,
	pub right: bool,
}

impl Edges {
	#[must_use]
	pub const fn any(self) -> bool {
		self.top || self.bottom || self.left || self.right
	}
}

/// The share of the page that is ink, from 0 to 1.
#[must_use]
pub fn ink_share(page: &DynamicImage) -> f32 {
	let gray = small_gray(page);
	let (width, height) = gray.dimensions();
	let (skip_x, skip_y) = (width * BORDER_PERCENT / 100, height * BORDER_PERCENT / 100);
	let inside = image::imageops::crop_imm(&gray, skip_x, skip_y, width - 2 * skip_x, height - 2 * skip_y).to_image();
	let total = inside.as_raw().len();
	if total == 0 {
		return 0.0;
	}
	let paper = paper_level(&inside);
	let threshold = paper.saturating_sub(INK_CONTRAST);
	let ink = inside.as_raw().iter().filter(|&&value| value < threshold).count();
	ink as f32 / total as f32
}

/// A face-down page on a flatbed scans as the lid, and a face-down stack in a feeder as the backs of the pages: both come out blank.
#[must_use]
pub fn is_blank(ink_share: f32) -> bool {
	ink_share < BLANK_INK
}

/// How much readable text a page holds: words in scripts written with spaces, and characters in scripts written without them (Chinese, Japanese, Thai, Lao, Khmer, Burmese), where OCR engines report every character as its own word.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextAmount {
	pub words: usize,
	pub characters: usize,
}

impl TextAmount {
	/// Words and characters together, each counting as one unit of text, which is what the thresholds here are measured in.
	#[must_use]
	pub const fn units(self) -> usize {
		self.words + self.characters
	}

	/// Mostly characters of scripts without spaces, so "characters" is what to count out loud.
	#[must_use]
	pub const fn in_characters(self) -> bool {
		self.characters > self.words
	}
}

#[must_use]
pub fn text_amount(lines: &[RecognizedLine]) -> TextAmount {
	let mut amount = TextAmount::default();
	for token in lines.iter().flat_map(|line| line.text.split_whitespace()) {
		let unspaced = token.chars().filter(|c| is_unspaced(*c)).count();
		if unspaced > 0 {
			amount.characters += unspaced;
		} else {
			amount.words += 1;
		}
	}
	amount
}

/// Text that looks like text: a word with at least two letters, mostly letters, or one character of a script written without spaces. An upside-down page reads as scattered marks and short fragments. A Han character counts only if it's a common one ([`super::common_han`]): read upside down, a Chinese page comes back as real but rare characters.
#[must_use]
pub fn plausible_text(lines: &[RecognizedLine]) -> usize {
	lines
		.iter()
		.flat_map(|line| line.text.split_whitespace())
		.map(|token| {
			let unspaced = token.chars().filter(|c| is_unspaced(*c)).count();
			if unspaced > 0 {
				token
					.chars()
					.filter(|c| is_unspaced(*c) && (!is_han(*c) || super::common_han::is_common_han(*c)))
					.count()
			} else {
				usize::from(is_plausible(token))
			}
		})
		.sum()
}

fn is_han(c: char) -> bool {
	matches!(u32::from(c), 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F)
}

/// Han (with its compatibility and extension blocks), Hiragana, Katakana, Thai, Lao, Myanmar and Khmer. Korean is written with spaces, so Hangul counts by the word.
fn is_unspaced(c: char) -> bool {
	matches!(
		u32::from(c),
		0x0E00..=0x0EFF // Thai, Lao
			| 0x1000..=0x109F // Myanmar
			| 0x1780..=0x17FF // Khmer
			| 0x19E0..=0x19FF // Khmer symbols
			| 0x3040..=0x30FF // Hiragana, Katakana
			| 0x31F0..=0x31FF // Katakana phonetic extensions
			| 0x3400..=0x4DBF // CJK extension A
			| 0x4E00..=0x9FFF // CJK unified ideographs
			| 0xA9E0..=0xA9FF // Myanmar extended B
			| 0xAA60..=0xAA7F // Myanmar extended A
			| 0xF900..=0xFAFF // CJK compatibility ideographs
			| 0xFF66..=0xFF9F // Halfwidth Katakana
			| 0x20000..=0x3134F // CJK extensions B to G
	) && c.is_alphabetic()
}

#[must_use]
pub fn worth_turning(plausible: usize) -> bool {
	plausible < TRY_TURNING_BELOW
}

#[must_use]
pub fn reads_clearly(plausible: usize) -> bool {
	plausible >= CLEARLY_READ
}

/// `tried` holds each turn with the plausible words it read; the first entry is the page as scanned.
#[must_use]
pub fn best_turn(tried: &[(Turn, usize)]) -> Turn {
	let Some(&(_, upright)) = tried.first() else { return Turn::None };
	let Some(&(turn, best)) = tried.iter().skip(1).max_by_key(|(_, words)| *words) else { return Turn::None };
	if best >= TURN_MINIMUM && best as f32 > upright as f32 * TURN_MARGIN { turn } else { Turn::None }
}

#[must_use]
pub fn little_text(amount: TextAmount) -> bool {
	amount.units() < LITTLE_TEXT_WORDS
}

/// `width` and `height` are of the image the words were read from.
#[must_use]
pub fn cut_off_edges(lines: &[RecognizedLine], width: u32, height: u32) -> Edges {
	let (width, height) = (width as f32, height as f32);
	let margin_x = (width * EDGE_SHARE).max(2.0);
	let margin_y = (height * EDGE_SHARE).max(2.0);
	let mut edges = Edges::default();
	for word in lines.iter().flat_map(|line| &line.words).filter(|word| !word.text.trim().is_empty()) {
		edges.left |= word.x <= margin_x;
		edges.top |= word.y <= margin_y;
		edges.right |= word.x + word.width >= width - margin_x;
		edges.bottom |= word.y + word.height >= height - margin_y;
	}
	edges
}

/// A word with at least two letters, mostly letters (a script's combining marks, such as the Indic virama, count with its letters), or a group of at least three digits, as in a table of figures ("12,340.50"), which has no letters at all.
fn is_plausible(word: &str) -> bool {
	let letters = word.chars().filter(|c| c.is_alphabetic()).count();
	let marks = word.chars().filter(|c| !c.is_alphabetic() && is_mark(*c)).count();
	let digits = word.chars().filter(char::is_ascii_digit).count();
	let total = word.chars().count();
	(letters >= 2 && (letters + marks) * 10 >= total * 7) || (digits >= 3 && digits * 10 >= total * 6)
}

/// Combining marks that Unicode doesn't call alphabetic but that are part of a word: the virama, nukta and similar signs of the Indic scripts, Hebrew points, Arabic harakat, and the general combining blocks.
fn is_mark(c: char) -> bool {
	let code = u32::from(c);
	matches!(
		code,
		0x0300..=0x036F // combining diacritical marks
			| 0x0483..=0x0489 // Cyrillic
			| 0x0591..=0x05C7 // Hebrew points and accents
			| 0x0610..=0x061A | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06ED // Arabic
			| 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F
	) || ((0x0900..=0x0DFF).contains(&code) // Devanagari to Sinhala: signs that aren't letters, digits or danda
		&& !c.is_alphanumeric()
		&& !matches!(code, 0x0964 | 0x0965 | 0x0970))
}

fn small_gray(page: &DynamicImage) -> GrayImage {
	let longest = page.width().max(page.height());
	if longest > LOOK_SIZE {
		page.resize(LOOK_SIZE, LOOK_SIZE, FilterType::Triangle).into_luma8()
	} else {
		page.to_luma8()
	}
}

/// The paper is what most of a page is, so a high percentile finds it without being thrown by a dark border from the scanner's lid.
fn paper_level(gray: &GrayImage) -> u8 {
	let mut histogram = [0usize; 256];
	for &value in gray.as_raw() {
		histogram[usize::from(value)] += 1;
	}
	let target = gray.as_raw().len() * 9 / 10;
	let mut seen = 0;
	for (level, count) in histogram.iter().enumerate() {
		seen += count;
		if seen >= target {
			return u8::try_from(level).unwrap_or(u8::MAX);
		}
	}
	u8::MAX
}

#[cfg(test)]
mod tests {
	use image::Luma;

	use super::*;
	use crate::ocr::RecognizedWord;

	fn paper(width: u32, height: u32) -> GrayImage {
		GrayImage::from_pixel(width, height, Luma([235]))
	}

	fn ink(image: &mut GrayImage, x: u32, y: u32, width: u32, height: u32) {
		for py in y..y + height {
			for px in x..x + width {
				image.put_pixel(px, py, Luma([30]));
			}
		}
	}

	fn line(words: &[(&str, f32, f32)]) -> RecognizedLine {
		let words: Vec<RecognizedWord> = words
			.iter()
			.map(|(text, x, y)| RecognizedWord { text: (*text).to_string(), x: *x, y: *y, width: 40.0, height: 12.0 })
			.collect();
		RecognizedLine { text: words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "), words }
	}

	#[test]
	fn plain_paper_is_blank() {
		assert!(is_blank(ink_share(&DynamicImage::ImageLuma8(paper(2550, 3300)))));
	}

	#[test]
	fn faint_show_through_from_the_other_side_is_still_blank() {
		let mut page = paper(2550, 3300);
		for row in 0..40 {
			for py in 300 + row * 70..300 + row * 70 + 25 {
				for px in 300..2200 {
					page.put_pixel(px, py, Luma([200]));
				}
			}
		}
		assert!(is_blank(ink_share(&DynamicImage::ImageLuma8(page))));
	}

	#[test]
	fn a_few_specks_of_dust_are_still_blank() {
		let mut page = paper(2550, 3300);
		for i in 0..30 {
			ink(&mut page, 100 + i * 80, 200 + i * 90, 3, 3);
		}
		assert!(is_blank(ink_share(&DynamicImage::ImageLuma8(page))));
	}

	#[test]
	fn a_page_with_one_short_line_is_not_blank() {
		let mut page = paper(2550, 3300);
		// About a page number's worth at 300 DPI: three characters, strokes 4 pixels wide.
		for c in 0..3 {
			ink(&mut page, 1200 + c * 30, 3000, 4, 32);
			ink(&mut page, 1200 + c * 30, 3000, 20, 4);
			ink(&mut page, 1200 + c * 30, 3028, 20, 4);
		}
		ink(&mut page, 300, 300, 1500, 6);
		assert!(!is_blank(ink_share(&DynamicImage::ImageLuma8(page))));
	}

	#[test]
	fn a_page_of_text_is_not_blank() {
		let mut page = paper(2550, 3300);
		for row in 0..40 {
			ink(&mut page, 300, 300 + row * 70, 1900, 25);
		}
		let share = ink_share(&DynamicImage::ImageLuma8(page));
		assert!(!is_blank(share));
		assert!(share > 0.1 && share < 0.3, "{share}");
	}

	#[test]
	fn a_dark_border_from_the_lid_does_not_hide_a_blank_page() {
		let mut page = paper(2550, 3300);
		ink(&mut page, 0, 0, 2550, 8);
		assert!(is_blank(ink_share(&DynamicImage::ImageLuma8(page))));
	}

	#[test]
	fn words_with_letters_are_plausible_and_fragments_are_not() {
		let lines = [
			line(&[("The", 0.0, 0.0), ("quick,", 0.0, 0.0), ("brown", 0.0, 0.0)]),
			line(&[("·", 0.0, 0.0), ("1l", 0.0, 0.0), ("'", 0.0, 0.0), ("z", 0.0, 0.0), ("~^", 0.0, 0.0)]),
		];
		assert_eq!(plausible_text(&lines), 3);
		assert_eq!(text_amount(&lines), TextAmount { words: 8, characters: 0 });
	}

	#[test]
	fn letters_in_any_script_count() {
		assert!(is_plausible("Привет"));
		assert!(is_plausible("שלום"));
		assert!(!is_plausible("12"), "a page number alone isn't text");
		assert!(!is_plausible("1l"));
	}

	#[test]
	fn words_with_combining_marks_count() {
		// Tamil and Hindi conjuncts: the virama (pulli) isn't alphabetic in Unicode.
		assert!(is_plausible("மக்கள்"));
		assert!(is_plausible("स्त्री"));
		assert!(is_plausible("क़िला"), "with a nukta");
		assert!(is_plausible("ಕನ್ನಡ"));
		assert!(is_plausible("മലയാളം"));
		assert!(is_plausible("తెలుగు"));
		// Arabic and Hebrew with their vowel marks.
		assert!(is_plausible("كَتَبَ"));
		assert!(is_plausible("שָׁלוֹם"));
		// Marks alone don't make a word.
		assert!(!is_plausible("्ं"));
	}

	#[test]
	fn groups_of_figures_count_so_a_table_of_numbers_has_text() {
		assert!(is_plausible("12,340.50"));
		assert!(is_plausible("$1,200"));
		assert!(is_plausible("555-1234"));
		assert!(is_plausible("2026"));
		let table = [line(&[("12,340.50", 0.0, 0.0), ("9,812.00", 0.0, 0.0), ("1,003", 0.0, 0.0), ("7", 0.0, 0.0)])];
		assert_eq!(plausible_text(&table), 3);
	}

	#[test]
	fn scripts_without_spaces_count_by_the_character() {
		// OCR engines give each CJK or Thai character as its own word.
		let japanese =
			line(&[("今", 0.0, 0.0), ("日", 0.0, 0.0), ("は", 0.0, 0.0), ("晴", 0.0, 0.0), ("れ", 0.0, 0.0)]);
		// 晴 isn't among the common characters, which only lowers the vote; kana always count.
		assert_eq!(plausible_text(std::slice::from_ref(&japanese)), 4);
		let amount = text_amount(std::slice::from_ref(&japanese));
		assert_eq!(amount, TextAmount { words: 0, characters: 5 });
		assert!(amount.in_characters());
		let run_together = RecognizedLine { text: "ภาษาไทย 中文".to_string(), words: vec![] };
		assert_eq!(text_amount(std::slice::from_ref(&run_together)).characters, 9);
		let korean = RecognizedLine { text: "안녕하세요 세계".to_string(), words: vec![] };
		assert_eq!(text_amount(std::slice::from_ref(&korean)), TextAmount { words: 2, characters: 0 });
		assert!(!is_unspaced('。'), "punctuation is not text");
	}

	/// What Windows OCR actually returned for one page of Chinese, upside down and the right way up (Windows 11, zh-Hans-CN recognizer, at the scanner's resolution).
	#[test]
	fn real_windows_ocr_of_upside_down_chinese_asks_for_a_turn_and_gets_it() {
		let spaced = |text: &str| {
			[RecognizedLine {
				text: text.chars().filter(|c| !c.is_whitespace()).map(String::from).collect::<Vec<_>>().join(" "),
				words: vec![],
			}]
		};
		let upside_down = spaced("圭園寻腰也'孑也杲。±土兽Y工¥。身畀土±k碧。\\「／回兽'乯申二鞠旱。鞠鞋鞠旱壬¥");
		let upright = spaced(
			"关于提供语言协助和辅助服务的通知。我们免费为您提供语言协助服务，包括口译和笔译。如果您需要这些服务，请拨打会员服务电话。我们也提供其他格式的资料，例如大字体、盲文和音频。委员会星期二开会审查了这项提案，成员们同意在小的修改之后推进这个计划。",
		);
		let first = plausible_text(&upside_down);
		assert!(worth_turning(first), "upside down read as {first} plausible characters");
		let turned = plausible_text(&upright);
		assert!(reads_clearly(turned), "right way up read as {turned}");
		assert_eq!(best_turn(&[(Turn::None, first), (Turn::Half, turned)]), Turn::Half);
	}

	#[test]
	fn a_page_of_chinese_reads_clearly_and_is_not_little_text() {
		let text = "我们今天讨论了这个计划并同意在小的修改之后继续推进预算部分也已经准备好了大家都感谢工作人员";
		let lines =
			[RecognizedLine { text: text.chars().map(String::from).collect::<Vec<_>>().join(" "), words: vec![] }];
		assert!(reads_clearly(plausible_text(&lines)));
		assert!(!worth_turning(plausible_text(&lines)));
		assert!(!little_text(text_amount(&lines)));
	}

	#[test]
	fn a_turn_must_read_clearly_better_to_win() {
		assert_eq!(best_turn(&[(Turn::None, 3), (Turn::Half, 180), (Turn::Quarter, 5)]), Turn::Half);
		assert_eq!(best_turn(&[(Turn::None, 3), (Turn::Half, 4), (Turn::Quarter, 2)]), Turn::None);
		assert_eq!(best_turn(&[(Turn::None, 20), (Turn::Half, 24)]), Turn::None);
		assert_eq!(best_turn(&[(Turn::None, 0), (Turn::Half, 2), (Turn::ThreeQuarters, 60)]), Turn::ThreeQuarters);
		assert_eq!(best_turn(&[(Turn::None, 3)]), Turn::None);
		assert_eq!(best_turn(&[]), Turn::None);
	}

	#[test]
	fn a_clear_reading_beats_any_page_worth_turning() {
		let worst_upright = TRY_TURNING_BELOW - 1;
		assert!(reads_clearly(CLEARLY_READ));
		assert_eq!(best_turn(&[(Turn::None, worst_upright), (Turn::Half, CLEARLY_READ)]), Turn::Half);
	}

	#[test]
	fn turning_puts_the_page_s_width_where_it_belongs() {
		let page = DynamicImage::new_luma8(4, 2);
		assert_eq!(Turn::Quarter.apply(&page).width(), 2);
		assert_eq!(Turn::Half.apply(&page).width(), 4);
		assert_eq!(Turn::ThreeQuarters.apply(&page).height(), 4);
	}

	#[test]
	fn words_against_an_edge_are_flagged_on_that_edge() {
		let lines = [line(&[("Left", 1.0, 500.0), ("middle", 400.0, 500.0)]), line(&[("bottom", 400.0, 988.0)])];
		let edges = cut_off_edges(&lines, 1000, 1000);
		assert!(edges.left && edges.bottom);
		assert!(!edges.top && !edges.right);
		assert!(edges.any());
	}

	#[test]
	fn words_inside_the_margins_are_not_cut_off() {
		let lines = [line(&[("Well", 100.0, 100.0), ("inside", 800.0, 850.0)])];
		assert!(!cut_off_edges(&lines, 1000, 1000).any());
	}
}
