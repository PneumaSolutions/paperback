//! Han characters common in running Chinese and Japanese text, for telling a page that reads from one turned the wrong way.
//!
//! Windows OCR reads an upside-down Chinese page as a scatter of real but unrelated characters (on a test page: 圭 園 寻 腰 孑 杲 兽 畀 碧 鞠 旱 鞋 壬 for 195 correct ones the right way up), so counting every Han character as readable never asks for a turn. Function words and everyday vocabulary make up most of any real text, while a misreading lands on them only by chance, so counting only these tells the two apart. Simplified and the most common traditional forms; not a dictionary, just enough to vote.

const COMMON: &str = concat!(
	"一三上下不与且世东两个中为主么之九也书买了事二于些交产人什今从他代以们件价任会但位住体何作你使來例供便保信修個們借億儿先",
	"光免入全公关其内写军农冬决况准减几出分划则利别到制刻前力办加务助動務包化北区医區十午卖南即厂历原去又及友反发受变口只可台",
	"史右号司各同后向听告员周命和咖品响員哥哪商問啡喜單嗎四因国图國團土在地场城報增备夏外多够大天太头女奶好如妈妹始姐委子字学",
	"孩學它完定宜实审家實寫对将將對小少就屋展山工左己已市师希常年并幾床应店府庭建开式弟当影很律後得從心必快怎总您情想意愛愿應",
	"成我或房所手打技把投报择括按据推提改放政教数文料新方无日早时明春昨是時晚更書曾最會月有朋服望木末本术机村束条来東果查校样",
	"根格案桌椅業概構樂樣機次欢歡正此步歲母每比民气水汽没油治法活流济海火灯点為無然照父爷爸牛物特现現理生用由电男當疗病發白百",
	"的目直相看真着知社票秋种科秒程種空窗立笔算米系級結絡經網線總约终经结给统继网美老者而耳職聽肉育能自至與船色花茶药菜萬處號",
	"虽蛋行街表被西要見覺见觉解言記話該認語說誰請論講識議讀變讓计认许论设识话该语说请读调谢贵费资起足路身車輸车较过近还这进选",
	"這通進運過道達還那部都酒里重量金钱银長长門開間關门问间院除際雙電需青非面音響頭題類项须预题风飞饭馬马體高魚鱼鳥鸡麼點",
);

pub(super) fn is_common_han(c: char) -> bool {
	COMMON.contains(c)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn everyday_characters_are_common_and_a_misreading_s_mostly_are_not() {
		assert!("的是我们不在有这个说".chars().all(is_common_han));
		assert!("們這個說國".chars().all(is_common_han), "common traditional forms count too");
		let misread = "圭園寻腰孑杲兽畀碧乯鞠旱鞋壬";
		assert!(misread.chars().filter(|c| is_common_han(*c)).count() <= 1);
		assert!(!is_common_han('a'));
	}
}
