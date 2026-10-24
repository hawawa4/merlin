//! What kind of sound a plugin makes, as the user tags it: a family (Keys) and, within it, an
//! instrument (Piano). Tags are stored by name, so the lists below can grow or be reordered
//! without touching anyone's saved settings, and a plugin can carry several (a workstation is
//! Keys > Piano and Keys > Organ).

use serde::{Deserialize, Serialize};

/// The families and their instruments, in the order menus show them.
pub const TAXONOMY: &[(&str, &[&str])] = &[
    (
        "Keys",
        &[
            "Piano",
            "Electric Piano",
            "Organ",
            "Harpsichord",
            "Clavinet",
            "Accordion",
        ],
    ),
    (
        "Strings",
        &[
            "Violin",
            "Viola",
            "Cello",
            "Double Bass",
            "String Ensemble",
            "Harp",
        ],
    ),
    (
        "Guitars & Plucked",
        &[
            "Acoustic Guitar",
            "Electric Guitar",
            "Bass Guitar",
            "Banjo",
            "Mandolin",
            "Ukulele",
        ],
    ),
    (
        "Woodwinds",
        &[
            "Flute",
            "Piccolo",
            "Oboe",
            "Clarinet",
            "Bassoon",
            "Saxophone",
            "Recorder",
        ],
    ),
    (
        "Brass",
        &[
            "Trumpet",
            "Trombone",
            "French Horn",
            "Tuba",
            "Brass Section",
        ],
    ),
    ("Voices", &["Choir", "Solo Voice"]),
    (
        "Percussion",
        &[
            "Drum Kit",
            "Tuned Percussion",
            "Timpani",
            "Orchestral Percussion",
            "World Percussion",
        ],
    ),
    (
        "Synth",
        &["Lead", "Pad", "Bass", "Pluck", "Arpeggio", "Poly"],
    ),
    (
        "World",
        &["Sitar", "Oud", "Koto", "Kalimba", "Bagpipes", "Hang Drum"],
    ),
    (
        "Multi-instrument",
        &["General MIDI", "Orchestral Library", "Workstation"],
    ),
    ("Other", &["Texture", "Sound Effects"]),
];

/// One tag. `instrument` empty: the plugin is in the family without being a specific
/// instrument of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub family: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<String>,
}

impl Tag {
    pub fn new(family: &str, instrument: Option<&str>) -> Self {
        Self {
            family: family.to_owned(),
            instrument: instrument.map(str::to_owned),
        }
    }

    /// `Keys › Piano`, or just `Keys`.
    pub fn label(&self) -> String {
        match &self.instrument {
            Some(i) => format!("{} › {i}", self.family),
            None => self.family.clone(),
        }
    }
}

/// Adds the tag if the plugin does not have it, removes it if it does.
pub fn toggle(tags: &mut Vec<Tag>, tag: Tag) {
    match tags.iter().position(|t| *t == tag) {
        Some(p) => {
            tags.remove(p);
        }
        None => tags.push(tag),
    }
}

/// The tags a plugin shows and is filtered by.
#[derive(Clone, Copy, Debug)]
pub struct Tags<'a> {
    pub list: &'a [Tag],
    /// Worked out by the scanner (the user has not tagged this plugin).
    pub automatic: bool,
    /// The user has tagged this plugin, even if with no tags, so the scanner keeps out of it.
    pub manual: bool,
}

/// The user's tags if they ever set any for the plugin (an empty list is a choice too), else the
/// scanner's.
pub fn effective<'a>(manual: Option<&'a Vec<Tag>>, auto: Option<&'a Vec<Tag>>) -> Tags<'a> {
    match (manual, auto) {
        (Some(m), _) => Tags {
            list: m,
            automatic: false,
            manual: true,
        },
        (None, Some(a)) => Tags {
            list: a,
            automatic: true,
            manual: false,
        },
        (None, None) => Tags {
            list: &[],
            automatic: false,
            manual: false,
        },
    }
}

/// What the browser narrows to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    Any,
    /// Plugins with no tag yet, to work through them.
    Untagged,
    Family(&'static str),
    Instrument(&'static str, &'static str),
}

impl Filter {
    pub fn matches(self, tags: &[Tag]) -> bool {
        match self {
            Self::Any => true,
            Self::Untagged => tags.is_empty(),
            Self::Family(f) => tags.iter().any(|t| t.family == f),
            Self::Instrument(f, i) => tags
                .iter()
                .any(|t| t.family == f && t.instrument.as_deref() == Some(i)),
        }
    }

    /// The family being filtered to, if any (what the instrument list under it is for).
    pub fn family(self) -> Option<&'static str> {
        match self {
            Self::Family(f) | Self::Instrument(f, _) => Some(f),
            _ => None,
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Any => "Any".into(),
            Self::Untagged => "Untagged".into(),
            Self::Family(f) => f.into(),
            Self::Instrument(_, i) => i.into(),
        }
    }
}

/// The instruments of a family (none for a name that is not in the list).
pub fn instruments(family: &str) -> &'static [&'static str] {
    TAXONOMY
        .iter()
        .find(|(f, _)| *f == family)
        .map_or(&[], |(_, i)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_so_a_tag_means_one_thing() {
        let mut families: Vec<_> = TAXONOMY.iter().map(|(f, _)| *f).collect();
        families.sort();
        families.dedup();
        assert_eq!(families.len(), TAXONOMY.len());
        for (f, list) in TAXONOMY {
            let mut v = list.to_vec();
            v.sort();
            v.dedup();
            assert_eq!(v.len(), list.len(), "duplicate instrument in {f}");
        }
    }

    #[test]
    fn toggling_adds_then_removes_and_keeps_other_tags() {
        let mut tags = vec![Tag::new("Keys", Some("Organ"))];
        toggle(&mut tags, Tag::new("Keys", Some("Piano")));
        assert_eq!(tags.len(), 2);
        toggle(&mut tags, Tag::new("Keys", Some("Organ")));
        assert_eq!(tags, vec![Tag::new("Keys", Some("Piano"))]);
    }

    #[test]
    fn filters_narrow_by_family_then_instrument() {
        let piano = [Tag::new("Keys", Some("Piano"))];
        let family_only = [Tag::new("Keys", None)];
        assert!(Filter::Any.matches(&[]));
        assert!(Filter::Untagged.matches(&[]));
        assert!(!Filter::Untagged.matches(&piano));
        assert!(Filter::Family("Keys").matches(&piano));
        assert!(Filter::Family("Keys").matches(&family_only));
        assert!(!Filter::Family("Brass").matches(&piano));
        assert!(Filter::Instrument("Keys", "Piano").matches(&piano));
        assert!(
            !Filter::Instrument("Keys", "Piano").matches(&family_only),
            "a plugin tagged only with the family is not claimed by an instrument"
        );
        // A plugin with two tags is found under both.
        let both = [
            Tag::new("Keys", Some("Piano")),
            Tag::new("Synth", Some("Pad")),
        ];
        assert!(Filter::Family("Synth").matches(&both));
        // "Piano" under Keys must not match a Piano tag filed under another family.
        assert!(!Filter::Instrument("Strings", "Piano").matches(&piano));
    }

    #[test]
    fn the_users_choice_beats_the_scanner_even_when_it_is_nothing() {
        let auto = vec![Tag::new("Keys", Some("Piano"))];
        let mine = vec![Tag::new("Keys", Some("Organ"))];
        let none = Vec::new();
        let e = effective(None, Some(&auto));
        assert!(e.automatic && e.list == auto);
        let e = effective(Some(&mine), Some(&auto));
        assert!(!e.automatic && e.list == mine);
        let e = effective(Some(&none), Some(&auto));
        assert!(
            !e.automatic && e.list.is_empty(),
            "cleared on purpose stays cleared"
        );
        assert!(effective(None, None).list.is_empty());
    }

    #[test]
    fn instruments_of_a_family() {
        assert!(instruments("Brass").contains(&"Trumpet"));
        assert!(instruments("Nope").is_empty());
    }
}
