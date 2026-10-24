//! Tags the scanner gives plugins by itself: from a built-in database of well-known instruments
//! and words (`data/instruments.txt`), and failing that from the category the plugin declares.
//! Nothing here is saved: the tags are worked out again from the plugin and this database, so a
//! better database improves every plugin that nobody tagged by hand.

use std::sync::OnceLock;

use merlin_hosting::PluginInfo;
use merlin_model::Format;

use crate::tags::{self, Tag};

const DATABASE: &str = include_str!("../data/instruments.txt");

/// One piece of text to find in a name, from the start of a word.
#[derive(Debug)]
struct Pattern {
    /// Normalised, with the space that starts the word.
    needle: String,
}

impl Pattern {
    /// `piano` matches from the start of any word; `organ$` also needs the word to end there.
    fn new(text: &str) -> Self {
        let (text, whole) = match text.strip_suffix('$') {
            Some(t) => (t, true),
            None => (text, false),
        };
        let mut needle = format!(" {}", normalise(text));
        if whole {
            needle.push(' ');
        }
        Self { needle }
    }

    /// `haystack` is [`padded`].
    fn found_in(&self, haystack: &str) -> bool {
        haystack.contains(&self.needle)
    }
}

/// Lowercase words separated by single spaces: `"B-3 V (Mk2)"` becomes `"b 3 v mk2"`.
fn normalise(s: &str) -> String {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn padded(s: &str) -> String {
    format!(" {} ", normalise(s))
}

#[derive(Debug)]
struct Rule {
    tags: Vec<Tag>,
    names: Vec<Pattern>,
    vendors: Vec<Pattern>,
}

impl Rule {
    fn matches(&self, name: &str, vendor: &str) -> bool {
        self.names.iter().any(|p| p.found_in(name))
            || self.vendors.iter().any(|p| p.found_in(vendor))
    }
}

/// The rules, in the order they are tried.
#[derive(Debug)]
pub struct Database(Vec<Rule>);

impl Database {
    /// Reads the rule lines; the error names the line that is wrong.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut rules = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let at = |msg: &str| format!("line {}: {msg}", n + 1);
            let (tag_part, pattern_part) = line
                .split_once(':')
                .ok_or_else(|| at("expected `tags : patterns`"))?;
            let mut tags_of_rule = Vec::new();
            for t in tag_part.split(';') {
                let (family, instrument) = match t.split_once('>') {
                    Some((f, i)) => (f.trim(), Some(i.trim())),
                    None => (t.trim(), None),
                };
                let known = tags::TAXONOMY.iter().find(|(f, _)| *f == family);
                match (known, instrument) {
                    (None, _) => return Err(at(&format!("no family `{family}`"))),
                    (Some((_, list)), Some(i)) if !list.contains(&i) => {
                        return Err(at(&format!("no instrument `{i}` in {family}")));
                    }
                    _ => tags_of_rule.push(Tag::new(family, instrument)),
                }
            }
            let (mut names, mut vendors) = (Vec::new(), Vec::new());
            for p in pattern_part
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
            {
                match p.strip_prefix("vendor:") {
                    Some(v) => vendors.push(Pattern::new(v.trim())),
                    None => names.push(Pattern::new(p)),
                }
            }
            if names.is_empty() && vendors.is_empty() {
                return Err(at("no patterns"));
            }
            rules.push(Rule {
                tags: tags_of_rule,
                names,
                vendors,
            });
        }
        Ok(Self(rules))
    }

    /// The built-in database.
    pub fn builtin() -> &'static Self {
        static DB: OnceLock<Database> = OnceLock::new();
        DB.get_or_init(|| Self::parse(DATABASE).expect("data/instruments.txt is checked by a test"))
    }

    /// What the plugin is, as far as the database and its declared category tell; nothing when
    /// neither does.
    pub fn tags_for(&self, info: &PluginInfo) -> Vec<Tag> {
        // Some formats leave the vendor empty and put the maker among the categories instead
        // (u-he's VST3s are `Instrument|u-he`), so `vendor:` patterns look at both.
        let maker = format!("{} {}", info.vendor, info.features.join(" "));
        let (name, maker) = (padded(&info.name), padded(&maker));
        match self.0.iter().find(|r| r.matches(&name, &maker)) {
            Some(rule) => rule.tags.clone(),
            None => from_category(info),
        }
    }
}

/// Words plugins use in their category to say how their sound is made, which only synths do.
const SYNTH_WORDS: &[&str] = &[
    "analog",
    "analogue",
    "virtual analog",
    "wavetable",
    "fm",
    "subtractive",
    "additive",
    "modular",
];

/// What a plugin's declared category says, which is little: CLAP names `synthesizer`, `sampler`,
/// `drum` and `drum-machine` for instruments, and VST3 sub-categories add `piano`. A sampler
/// can be anything, so it says nothing. VST2 and LV2 call every instrument a synthesizer, so
/// for them that word says nothing either.
fn from_category(info: &PluginInfo) -> Vec<Tag> {
    let has = |f: &str| info.features.iter().any(|x| x == f);
    let says_synth = (matches!(info.format, Format::Clap | Format::Vst3)
        && (has("synthesizer") || has("synth")))
        || SYNTH_WORDS.iter().any(|w| has(w));
    if has("piano") {
        vec![Tag::new("Keys", Some("Piano"))]
    } else if has("drum") || has("drum-machine") {
        vec![Tag::new("Percussion", Some("Drum Kit"))]
    } else if says_synth {
        vec![Tag::new("Synth", None)]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(format: Format, name: &str, vendor: &str, features: &[&str]) -> PluginInfo {
        PluginInfo {
            format,
            id: name.into(),
            name: name.into(),
            vendor: vendor.into(),
            version: String::new(),
            features: features.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn tagged(name: &str) -> Vec<String> {
        let info = plugin(Format::Clap, name, "", &["instrument"]);
        Database::builtin()
            .tags_for(&info)
            .iter()
            .map(Tag::label)
            .collect()
    }

    #[test]
    fn the_database_parses_and_every_tag_in_it_exists() {
        // `builtin` would panic otherwise, with the line to fix.
        Database::parse(DATABASE).unwrap();
        assert!(Database::builtin().0.len() > 40);
    }

    #[test]
    fn bad_lines_are_named() {
        let e = Database::parse("# ok\nKeys > Piano : piano\nKeys > Kazoo : kazoo").unwrap_err();
        assert!(e.starts_with("line 3") && e.contains("Kazoo"), "{e}");
        assert!(
            Database::parse("Nowhere : x")
                .unwrap_err()
                .contains("no family")
        );
        assert!(
            Database::parse("Keys > Piano")
                .unwrap_err()
                .contains("tags : patterns")
        );
        assert!(
            Database::parse("Keys > Piano : ,")
                .unwrap_err()
                .contains("no patterns")
        );
    }

    #[test]
    fn well_known_names_are_recognised() {
        assert_eq!(tagged("Pianoteq 8 STAGE"), ["Keys › Piano"]);
        assert_eq!(tagged("Grand Piano"), ["Keys › Piano"]);
        assert_eq!(tagged("B-3 V"), ["Keys › Organ"]);
        assert_eq!(tagged("Lounge Lizard EP-4"), ["Keys › Electric Piano"]);
        assert_eq!(tagged("Surge XT"), ["Synth"]);
        assert_eq!(
            tagged("MuseScore General"),
            ["Multi-instrument › General MIDI"]
        );
        assert_eq!(tagged("Solo Trumpet"), ["Brass › Trumpet"]);
        assert_eq!(tagged("Alto Sax"), ["Woodwinds › Saxophone"]);
        assert_eq!(tagged("Concert Harp"), ["Strings › Harp"]);
        assert_eq!(tagged("Drumlabooh"), ["Percussion › Drum Kit"]);
    }

    #[test]
    fn the_first_rule_wins_so_specific_beats_general() {
        // "piano" is also in the name, but the electric piano rule is above it.
        assert_eq!(tagged("Electric Piano 2"), ["Keys › Electric Piano"]);
        // "steel string" is a guitar, not a string ensemble.
        assert_eq!(
            tagged("Steel String Guitar"),
            ["Guitars & Plucked › Acoustic Guitar"]
        );
        // Harpsichord is not a harp.
        assert_eq!(tagged("Harpsichord"), ["Keys › Harpsichord"]);
        assert_eq!(tagged("Horn Section"), ["Brass › Brass Section"]);
        assert_eq!(tagged("French Horn"), ["Brass › French Horn"]);
        assert_eq!(tagged("Choir Pad"), ["Voices › Choir"]);
    }

    #[test]
    fn patterns_match_from_the_start_of_a_word_not_inside_one() {
        // `organ$` is a whole word: Organic is not an organ.
        assert_eq!(tagged("Organic Pad"), ["Synth › Pad"]);
        assert_eq!(tagged("Organ"), ["Keys › Organ"]);
        // "ivory" does not match inside another word.
        assert!(tagged("Quivory").is_empty());
        // Punctuation and case do not matter.
        assert_eq!(tagged("b3 ORGAN!"), ["Keys › Organ"]);
        // Plugins nobody knows stay untagged.
        assert!(tagged("Zorblax 3000").is_empty());
    }

    #[test]
    fn a_vendor_can_be_enough() {
        let db = Database::builtin();
        let podolski = plugin(Format::Vst3, "Podolski", "u-he", &["instrument"]);
        assert_eq!(db.tags_for(&podolski), [Tag::new("Synth", None)]);
    }

    #[test]
    fn the_maker_is_found_among_the_categories_when_the_vendor_is_empty() {
        // How u-he's VST3s read: no vendor, the maker as a category.
        let db = Database::builtin();
        let p = plugin(Format::Vst3, "SomeNewSynth", "", &["instrument", "u-he"]);
        assert_eq!(db.tags_for(&p), [Tag::new("Synth", None)]);
        assert_eq!(tagged("TimGM6mb"), ["Multi-instrument › General MIDI"]);
    }

    #[test]
    fn plugins_that_say_analog_or_wavetable_are_synths_in_any_format() {
        let db = Database::builtin();
        let lv2 = plugin(
            Format::Lv2,
            "Zorblax",
            "Acme",
            &["instrument", "synthesizer", "analogue"],
        );
        assert_eq!(db.tags_for(&lv2), [Tag::new("Synth", None)]);
    }

    #[test]
    fn the_declared_category_is_the_fallback_for_unknown_names() {
        let db = Database::builtin();
        let tags = |p: PluginInfo| db.tags_for(&p);
        let clap_synth = plugin(
            Format::Clap,
            "Zorblax",
            "Acme",
            &["instrument", "synthesizer"],
        );
        assert_eq!(tags(clap_synth), [Tag::new("Synth", None)]);
        let vst3_piano = plugin(Format::Vst3, "Zorblax", "Acme", &["instrument", "piano"]);
        assert_eq!(tags(vst3_piano), [Tag::new("Keys", Some("Piano"))]);
        let drum_machine = plugin(
            Format::Clap,
            "Zorblax",
            "Acme",
            &["instrument", "drum-machine", "synthesizer"],
        );
        assert_eq!(
            tags(drum_machine),
            [Tag::new("Percussion", Some("Drum Kit"))]
        );
        // A sampler could be anything.
        assert!(
            tags(plugin(
                Format::Clap,
                "Zorblax",
                "Acme",
                &["instrument", "sampler"]
            ))
            .is_empty()
        );
        // LV2 and VST2 say "synthesizer" about every instrument: not evidence.
        assert!(
            tags(plugin(
                Format::Lv2,
                "Zorblax",
                "Acme",
                &["instrument", "synthesizer"]
            ))
            .is_empty()
        );
        assert!(
            tags(plugin(
                Format::Vst2,
                "Zorblax",
                "Acme",
                &["instrument", "synthesizer"]
            ))
            .is_empty()
        );
        // The database beats the category.
        let known = plugin(
            Format::Clap,
            "Pianoteq",
            "Modartt",
            &["instrument", "synthesizer"],
        );
        assert_eq!(tags(known), [Tag::new("Keys", Some("Piano"))]);
    }
}
