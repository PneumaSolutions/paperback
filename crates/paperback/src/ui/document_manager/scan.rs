//! A scanned document grows while it's open, so after each page its tab is re-read rather than reopened. The re-read is parsed off the UI thread ([`parse_scan`]), since its cost grows with the scan, and only installed here.

use std::{path::Path, rc::Rc, sync::Mutex};

use paperback_core::{config::ConfigManager, document::ParseSettings, session::DocumentSession};

use super::{
	DocumentManager,
	reload::{install_session, parse_settings, parse_with_text},
};
use crate::ui::readability::readability_style;

/// The settings a scan is parsed with, read on the UI thread.
#[must_use]
pub fn scan_parse_settings(config: &ConfigManager) -> ParseSettings {
	parse_settings(config)
}

/// Safe on any thread: the scan's pages and the text stored with them.
pub fn parse_scan(path: &Path, settings: ParseSettings) -> Result<DocumentSession, String> {
	parse_with_text(path, settings)
}

impl DocumentManager {
	/// Opens the scan; false if it couldn't be. Done on the UI thread, once, for the first page.
	pub fn open_scan(&mut self, self_rc: &Rc<Mutex<Self>>, path: &Path) -> bool {
		match self.find_tab_by_path(path) {
			Some(_) => true,
			None => self.open_file(self_rc, path),
		}
	}

	/// Puts a scan parsed by [`parse_scan`] in its tab; false if the tab has been closed meanwhile.
	pub fn show_parsed_scan(&mut self, path: &Path, session: DocumentSession) -> bool {
		let Some(index) = self.find_tab_by_path(path) else { return false };
		let style = readability_style(&self.config.lock().unwrap());
		let tab = &mut self.tabs[index];
		let unchanged = session.handle().document().buffer.content == tab.session.handle().document().buffer.content;
		if !unchanged {
			install_session(tab, session, false, &style);
		}
		true
	}
}
