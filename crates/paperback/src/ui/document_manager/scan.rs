//! A scanned document grows while it's open, so after each page its tab is re-read rather than reopened.

use std::{path::Path, rc::Rc, sync::Mutex};

use super::{
	DocumentManager,
	reload::{parse_settings, reparse_tab_in_place},
};
use crate::ui::readability::readability_style;

impl DocumentManager {
	/// Opens the scan the first time and re-reads it after that; false if it could be neither.
	pub fn show_scan(&mut self, self_rc: &Rc<Mutex<Self>>, path: &Path) -> bool {
		let Some(index) = self.find_tab_by_path(path) else {
			return self.open_file(self_rc, path);
		};
		let (settings, style) = {
			let config = self.config.lock().unwrap();
			(parse_settings(&config), readability_style(&config))
		};
		reparse_tab_in_place(&mut self.tabs[index], "", "", settings, &style).is_ok()
	}
}
