//! The scanner used last and each scanner's own settings, so the second scan is one keypress: the list of scanners is only shown the first time, or when asked for, and a scanner's source, color and resolution come back as they were left.
//! Kept as one JSON string in the app settings, since scanner IDs hold backslashes and braces no settings key should.

use std::collections::BTreeMap;

use paperback_core::config::ConfigManager;
use serde_json::{Map, Value, json};

use crate::scanning::scanner::{ColorMode, RESOLUTIONS, ScanSettings, ScannerInfo, Source};

const KEY: &str = "scanners";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
	pub name: String,
	pub sources: Vec<Source>,
	pub settings: ScanSettings,
	/// A two-sided scan leaves out the sides with no text, rather than keeping every side.
	pub skip_blank_sides: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Remembered {
	pub last: Option<String>,
	pub scanners: BTreeMap<String, Entry>,
}

impl Remembered {
	#[must_use]
	pub fn load(config: &ConfigManager) -> Self {
		Self::from_json(&config.get_app_string(KEY, ""))
	}

	pub fn save(&self, config: &ConfigManager) {
		config.set_app_string(KEY, &self.to_json());
		config.flush();
	}

	/// The last scanner, as it was listed then, so it can be offered without asking the system again.
	#[must_use]
	pub fn last_scanner(&self) -> Option<ScannerInfo> {
		let id = self.last.as_ref()?;
		let entry = self.scanners.get(id)?;
		Some(ScannerInfo { id: id.clone(), name: entry.name.clone(), sources: entry.sources.clone(), problem: None })
	}

	/// Grayscale at 300 DPI from the flatbed, or the feeder on a scanner without one, until the reader changes them.
	#[must_use]
	pub fn settings_for(&self, scanner: &ScannerInfo) -> ScanSettings {
		let fallback = ScanSettings {
			source: scanner.sources.first().copied().unwrap_or(Source::Flatbed),
			..ScanSettings::default()
		};
		let Some(entry) = self.scanners.get(&scanner.id) else { return fallback };
		let mut settings = entry.settings;
		// The scanner may have lost its feeder since, or been listed differently.
		if !scanner.sources.is_empty() && !scanner.sources.contains(&settings.source) {
			settings.source = fallback.source;
		}
		settings
	}

	pub fn remember(&mut self, scanner: &ScannerInfo, settings: ScanSettings, skip_blank_sides: bool) {
		self.last = Some(scanner.id.clone());
		self.scanners.insert(
			scanner.id.clone(),
			Entry { name: scanner.name.clone(), sources: scanner.sources.clone(), settings, skip_blank_sides },
		);
	}

	#[must_use]
	pub fn skip_blank_sides(&self, scanner: &ScannerInfo) -> bool {
		self.scanners.get(&scanner.id).is_some_and(|entry| entry.skip_blank_sides)
	}

	/// Anything unreadable is dropped rather than failing: the worst case is being asked to pick a scanner again.
	fn from_json(text: &str) -> Self {
		let Ok(Value::Object(root)) = serde_json::from_str::<Value>(text) else { return Self::default() };
		let mut scanners = BTreeMap::new();
		if let Some(Value::Object(list)) = root.get("scanners") {
			for (id, entry) in list {
				if let Some(entry) = entry_from(entry) {
					scanners.insert(id.clone(), entry);
				}
			}
		}
		let last = root.get("last").and_then(Value::as_str).filter(|id| scanners.contains_key(*id)).map(str::to_string);
		Self { last, scanners }
	}

	fn to_json(&self) -> String {
		let mut list = Map::new();
		for (id, entry) in &self.scanners {
			let sources: Vec<&str> = entry.sources.iter().map(|source| source.key()).collect();
			list.insert(
				id.clone(),
				json!({
					"name": entry.name,
					"sources": sources,
					"source": entry.settings.source.key(),
					"dpi": entry.settings.dpi,
					"color": color_key(entry.settings.color),
					"skip_blank_sides": entry.skip_blank_sides,
				}),
			);
		}
		json!({ "last": self.last, "scanners": list }).to_string()
	}
}

fn entry_from(value: &Value) -> Option<Entry> {
	let name = value.get("name")?.as_str()?.to_string();
	let sources = value
		.get("sources")?
		.as_array()?
		.iter()
		.filter_map(|source| source.as_str().and_then(Source::from_key))
		.collect();
	let source = value.get("source").and_then(Value::as_str).and_then(Source::from_key).unwrap_or(Source::Flatbed);
	let dpi = value
		.get("dpi")
		.and_then(Value::as_u64)
		.and_then(|dpi| u32::try_from(dpi).ok())
		.filter(|dpi| RESOLUTIONS.contains(dpi))
		.unwrap_or_else(|| ScanSettings::default().dpi);
	let color =
		if value.get("color").and_then(Value::as_str) == Some("color") { ColorMode::Color } else { ColorMode::Gray };
	let skip_blank_sides = value.get("skip_blank_sides").and_then(Value::as_bool).unwrap_or(false);
	Some(Entry { name, sources, settings: ScanSettings { source, dpi, color }, skip_blank_sides })
}

const fn color_key(color: ColorMode) -> &'static str {
	match color {
		ColorMode::Gray => "gray",
		ColorMode::Color => "color",
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn scanner(id: &str, sources: Vec<Source>) -> ScannerInfo {
		ScannerInfo { id: id.into(), name: format!("Scanner {id}"), sources, problem: None }
	}

	#[test]
	fn nothing_is_remembered_at_first() {
		let remembered = Remembered::from_json("");
		assert_eq!(remembered.last_scanner(), None);
	}

	#[test]
	fn the_defaults_are_gray_at_300_dpi_from_the_flatbed() {
		let remembered = Remembered::default();
		let settings = remembered.settings_for(&scanner("a", vec![Source::Flatbed, Source::Feeder]));
		assert_eq!(settings, ScanSettings { source: Source::Flatbed, dpi: 300, color: ColorMode::Gray });
		let feeder_only = remembered.settings_for(&scanner("b", vec![Source::Feeder, Source::FeederDuplex]));
		assert_eq!(feeder_only.source, Source::Feeder, "a feeder defaults to one side; both sides is chosen");
	}

	#[test]
	fn the_last_scanner_and_each_scanner_s_settings_survive_a_round_trip() {
		let mut remembered = Remembered::default();
		let first = scanner(r"SWD\Escl\{3ebf}", vec![Source::Flatbed, Source::Feeder, Source::FeederDuplex]);
		let second = scanner("test:0", vec![Source::Flatbed]);
		remembered.remember(
			&first,
			ScanSettings { source: Source::FeederDuplex, dpi: 200, color: ColorMode::Color },
			true,
		);
		remembered.remember(&second, ScanSettings { source: Source::Flatbed, dpi: 600, color: ColorMode::Gray }, false);
		let back = Remembered::from_json(&remembered.to_json());
		assert_eq!(back, remembered);
		assert_eq!(back.last_scanner().unwrap().id, "test:0");
		let settings = back.settings_for(&first);
		assert_eq!((settings.source, settings.dpi, settings.color), (Source::FeederDuplex, 200, ColorMode::Color));
		assert!(back.skip_blank_sides(&first));
		assert!(!back.skip_blank_sides(&second));
		assert!(!Remembered::default().skip_blank_sides(&first), "skipping blank sides is opt-in");
	}

	#[test]
	fn a_remembered_source_the_scanner_no_longer_has_falls_back() {
		let mut remembered = Remembered::default();
		remembered.remember(
			&scanner("a", vec![Source::Feeder]),
			ScanSettings { source: Source::Feeder, ..ScanSettings::default() },
			false,
		);
		assert_eq!(remembered.settings_for(&scanner("a", vec![Source::Flatbed])).source, Source::Flatbed);
	}

	#[test]
	fn damaged_settings_are_ignored_rather_than_trusted() {
		assert_eq!(Remembered::from_json("{not json"), Remembered::default());
		let odd = r#"{"last":"gone","scanners":{"a":{"name":"A","sources":["flatbed","film"],"source":"x","dpi":7,"color":"sepia"}}}"#;
		let remembered = Remembered::from_json(odd);
		assert_eq!(remembered.last, None);
		let entry = &remembered.scanners["a"];
		assert_eq!(entry.sources, vec![Source::Flatbed]);
		assert_eq!(entry.settings, ScanSettings::default());
	}
}
