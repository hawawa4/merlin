//! Reading LV2 bundles: the Turtle files that describe a plugin (its ports, its UI, what it
//! needs from the host). No lilv is needed: the files are parsed here, so nothing beyond the
//! app has to be installed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use oxrdf::{NamedOrBlankNode, Term};
use oxttl::TurtleParser;

pub const LV2: &str = "http://lv2plug.in/ns/lv2core#";
pub const ATOM: &str = "http://lv2plug.in/ns/ext/atom#";
pub const MIDI: &str = "http://lv2plug.in/ns/ext/midi#";
pub const UI: &str = "http://lv2plug.in/ns/extensions/ui#";
const DOAP: &str = "http://usefulinc.com/ns/doap#";
const FOAF: &str = "http://xmlns.com/foaf/0.1/";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const SEE_ALSO: &str = "http://www.w3.org/2000/01/rdf-schema#seeAlso";
const PORT_PROPS: &str = "http://lv2plug.in/ns/ext/port-props#";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Node {
    Iri(String),
    Blank(String),
    Literal(String),
}

/// Every triple of the files read so far.
#[derive(Default)]
struct Graph {
    by_subject: HashMap<Node, Vec<(String, Node)>>,
}

impl Graph {
    fn add_file(&mut self, path: &Path) -> Result<()> {
        let text =
            std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        let base = file_url(path);
        let parser = TurtleParser::new()
            .with_base_iri(base)
            .map_err(|e| anyhow!("{e}"))?;
        for triple in parser.for_slice(&text) {
            // A broken statement must not hide the plugins that are described properly.
            let Ok(t) = triple else { continue };
            let subject = match t.subject {
                NamedOrBlankNode::NamedNode(n) => Node::Iri(n.into_string()),
                NamedOrBlankNode::BlankNode(b) => {
                    Node::Blank(format!("{}:{}", path.display(), b.as_str()))
                }
            };
            let object = match t.object {
                Term::NamedNode(n) => Node::Iri(n.into_string()),
                Term::BlankNode(b) => Node::Blank(format!("{}:{}", path.display(), b.as_str())),
                Term::Literal(l) => Node::Literal(l.value().to_owned()),
            };
            self.by_subject
                .entry(subject)
                .or_default()
                .push((t.predicate.into_string(), object));
        }
        Ok(())
    }

    fn objects<'a>(&'a self, subject: &Node, predicate: &'a str) -> impl Iterator<Item = &'a Node> {
        self.by_subject
            .get(subject)
            .into_iter()
            .flatten()
            .filter(move |(p, _)| p == predicate)
            .map(|(_, o)| o)
    }

    fn first<'a>(&'a self, subject: &Node, predicate: &'a str) -> Option<&'a Node> {
        self.objects(subject, predicate).next()
    }

    fn text(&self, subject: &Node, predicate: &str) -> Option<String> {
        match self.first(subject, predicate)? {
            Node::Literal(s) => Some(s.clone()),
            _ => None,
        }
    }

    fn number(&self, subject: &Node, predicate: &str) -> Option<f64> {
        self.text(subject, predicate)?.trim().parse().ok()
    }

    fn types(&self, subject: &Node) -> Vec<String> {
        self.objects(subject, RDF_TYPE)
            .filter_map(|n| match n {
                Node::Iri(i) => Some(i.clone()),
                _ => None,
            })
            .collect()
    }

    fn iris(&self, subject: &Node, predicate: &str) -> Vec<String> {
        self.objects(subject, predicate)
            .filter_map(|n| match n {
                Node::Iri(i) => Some(i.clone()),
                _ => None,
            })
            .collect()
    }
}

/// `file:///a/b%20c` for `/a/b c`.
fn file_url(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The path of a `file://` IRI.
fn path_of(iri: &str) -> Option<PathBuf> {
    let rest = iri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortKind {
    Audio,
    Control,
    Cv,
    Atom,
}

#[derive(Clone, Debug)]
pub struct PortDesc {
    pub index: usize,
    pub symbol: String,
    pub name: String,
    pub kind: PortKind,
    pub input: bool,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub toggled: bool,
    pub integer: bool,
    /// Not meant to be shown to the player (latency, enabled, ...).
    pub hidden: bool,
    /// An atom port that takes MIDI.
    pub midi: bool,
    /// An atom port the plugin sends patch/state messages through (kept connected).
    pub patch: bool,
}

#[derive(Clone, Debug)]
pub struct UiDesc {
    pub uri: String,
    pub binary: PathBuf,
    /// Features the UI cannot work without, by URI.
    pub required: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PluginDesc {
    pub uri: String,
    pub name: String,
    pub vendor: String,
    pub instrument: bool,
    pub bundle: PathBuf,
    pub binary: PathBuf,
    pub ports: Vec<PortDesc>,
    /// An X11 editor, if the plugin has one.
    pub ui: Option<UiDesc>,
    pub required: Vec<String>,
    pub has_state: bool,
    pub version: String,
}

/// Every plugin in a bundle (a `.lv2` folder).
pub fn read_bundle(bundle: &Path) -> Result<Vec<PluginDesc>> {
    let manifest = bundle.join("manifest.ttl");
    let mut graph = Graph::default();
    graph
        .add_file(&manifest)
        .with_context(|| format!("not an LV2 bundle: {}", bundle.display()))?;

    // The files each entry of the manifest points to.
    let subjects: Vec<Node> = graph.by_subject.keys().cloned().collect();
    let mut see_also = Vec::new();
    for s in &subjects {
        for iri in graph.iris(s, SEE_ALSO) {
            if let Some(p) = path_of(&iri) {
                see_also.push(p);
            }
        }
    }
    see_also.sort();
    see_also.dedup();
    for file in see_also {
        // A missing data file is that plugin's problem, not the bundle's.
        let _ = graph.add_file(&file);
    }

    let lv2 = |name: &str| format!("{LV2}{name}");
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for s in &subjects {
        let Node::Iri(uri) = s else { continue };
        if !graph.types(s).iter().any(|t| *t == lv2("Plugin")) {
            continue;
        }
        if !seen.insert(uri.clone()) {
            continue;
        }
        let Some(binary) = graph
            .iris(s, &lv2("binary"))
            .into_iter()
            .find_map(|b| path_of(&b))
        else {
            continue;
        };
        // The data may sit in a file the manifest pointed to: the full description is then on
        // the same subject.
        let types = graph.types(s);
        let name = graph
            .text(s, &format!("{DOAP}name"))
            .unwrap_or_else(|| uri.clone());
        let vendor = graph
            .first(s, &format!("{DOAP}maintainer"))
            .and_then(|m| graph.text(m, &format!("{FOAF}name")))
            .unwrap_or_default();
        let instrument = types.iter().any(|t| *t == lv2("InstrumentPlugin"));
        let required: Vec<String> = graph.iris(s, &lv2("requiredFeature"));
        let has_state = graph
            .iris(s, &lv2("extensionData"))
            .iter()
            .any(|e| e == "http://lv2plug.in/ns/ext/state#interface");

        let mut ports = Vec::new();
        for p in graph.objects(s, &lv2("port")) {
            let ptypes = graph.types(p);
            let has = |t: &str| ptypes.iter().any(|x| x == t);
            let kind = if has(&lv2("AudioPort")) {
                PortKind::Audio
            } else if has(&lv2("ControlPort")) {
                PortKind::Control
            } else if has(&lv2("CVPort")) {
                PortKind::Cv
            } else if has(&format!("{ATOM}AtomPort")) {
                PortKind::Atom
            } else {
                continue;
            };
            let Some(index) = graph.number(p, &lv2("index")) else {
                continue;
            };
            let props = graph.iris(p, &lv2("portProperty"));
            let prop = |name: &str| props.iter().any(|x| x == &lv2(name));
            let supports = graph.iris(p, &format!("{ATOM}supports"));
            let midi = supports.iter().any(|x| x == &format!("{MIDI}MidiEvent"));
            let patch = supports
                .iter()
                .any(|x| x == "http://lv2plug.in/ns/ext/patch#Message");
            let designation = graph.iris(p, &lv2("designation"));
            ports.push(PortDesc {
                index: index as usize,
                symbol: graph.text(p, &lv2("symbol")).unwrap_or_default(),
                name: graph.text(p, &lv2("name")).unwrap_or_default(),
                kind,
                input: has(&lv2("InputPort")),
                min: graph.number(p, &lv2("minimum")).unwrap_or(0.0),
                max: graph.number(p, &lv2("maximum")).unwrap_or(1.0),
                default: graph.number(p, &lv2("default")).unwrap_or(0.0),
                toggled: prop("toggled"),
                integer: prop("integer") || prop("enumeration"),
                hidden: props.iter().any(|x| x == &format!("{PORT_PROPS}notOnGUI"))
                    || designation
                        .iter()
                        .any(|d| d == &lv2("enabled") || d == &lv2("freeWheeling"))
                    || prop("reportsLatency"),
                midi,
                patch,
            });
        }
        ports.sort_by_key(|p| p.index);

        // The X11 editor: `ui:ui <uri>` on the plugin, with the UI's own subject in the manifest.
        let ui = graph
            .iris(s, &format!("{UI}ui"))
            .into_iter()
            .find_map(|ui_uri| {
                let node = Node::Iri(ui_uri.clone());
                if !graph
                    .types(&node)
                    .iter()
                    .any(|t| *t == format!("{UI}X11UI"))
                {
                    return None;
                }
                let binary = graph
                    .iris(&node, &lv2("binary"))
                    .into_iter()
                    .find_map(|b| path_of(&b))?;
                Some(UiDesc {
                    uri: ui_uri,
                    binary,
                    required: graph.iris(&node, &lv2("requiredFeature")),
                })
            });

        let version = format!(
            "{}.{}",
            graph.number(s, &lv2("minorVersion")).unwrap_or(0.0) as u32,
            graph.number(s, &lv2("microVersion")).unwrap_or(0.0) as u32
        );
        out.push(PluginDesc {
            uri: uri.clone(),
            name,
            vendor,
            instrument,
            bundle: bundle.to_owned(),
            binary,
            ports,
            ui,
            required,
            has_state,
            version,
        });
    }
    if out.is_empty() {
        return Err(anyhow!("no plugin in {}", bundle.display()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(name: &str, manifest: &str, data: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("merlin-lv2-{name}-{}.lv2", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.ttl"), manifest).unwrap();
        std::fs::write(dir.join("data.ttl"), data).unwrap();
        dir
    }

    #[test]
    fn urls_round_trip_through_paths_with_spaces() {
        let p = Path::new("/home/a b/c.lv2/lib x.so");
        assert_eq!(path_of(&file_url(p)).unwrap(), p);
    }

    #[test]
    fn a_synth_bundle_is_read_with_its_ports_ui_and_needs() {
        let dir = bundle(
            "synth",
            r#"@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix ui: <http://lv2plug.in/ns/extensions/ui#> .
<urn:test:synth> a lv2:Plugin ; lv2:binary <libsynth%20x.so> ; rdfs:seeAlso <data.ttl> .
<urn:test:synth:ui> a ui:X11UI ; lv2:binary <libui.so> ."#,
            r#"@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix atom: <http://lv2plug.in/ns/ext/atom#> .
@prefix midi: <http://lv2plug.in/ns/ext/midi#> .
@prefix urid: <http://lv2plug.in/ns/ext/urid#> .
@prefix ui: <http://lv2plug.in/ns/extensions/ui#> .
@prefix state: <http://lv2plug.in/ns/ext/state#> .
<urn:test:synth>
    a lv2:InstrumentPlugin ;
    doap:name "Test Synth" ;
    doap:maintainer [ foaf:name "Somebody" ] ;
    ui:ui <urn:test:synth:ui> ;
    lv2:requiredFeature urid:map ;
    lv2:extensionData state:interface ;
    lv2:port
      [ a lv2:InputPort , atom:AtomPort ; atom:bufferType atom:Sequence ; atom:supports midi:MidiEvent ;
        lv2:index 0 ; lv2:symbol "midi" ; lv2:name "MIDI" ] ,
      [ a lv2:OutputPort , lv2:AudioPort ; lv2:index 1 ; lv2:symbol "out_l" ; lv2:name "Out L" ] ,
      [ a lv2:OutputPort , lv2:AudioPort ; lv2:index 2 ; lv2:symbol "out_r" ; lv2:name "Out R" ] ,
      [ a lv2:InputPort , lv2:ControlPort ; lv2:index 3 ; lv2:symbol "gain" ; lv2:name "Gain" ;
        lv2:default 0.5 ; lv2:minimum 0.0 ; lv2:maximum 2.0 ] .
<urn:test:synth:ui> lv2:requiredFeature ui:parent , ui:idleInterface ."#,
        );
        let plugins = read_bundle(&dir).unwrap();
        assert_eq!(plugins.len(), 1);
        let p = &plugins[0];
        assert_eq!(p.uri, "urn:test:synth");
        assert_eq!(p.name, "Test Synth");
        assert_eq!(p.vendor, "Somebody");
        assert!(p.instrument && p.has_state);
        assert_eq!(p.binary, dir.join("libsynth x.so"));
        assert_eq!(p.ports.len(), 4);
        assert!(p.ports[0].midi && p.ports[0].kind == PortKind::Atom && p.ports[0].input);
        assert_eq!(p.ports[1].kind, PortKind::Audio);
        let gain = &p.ports[3];
        assert_eq!(
            (gain.kind, gain.input, gain.min, gain.max, gain.default),
            (PortKind::Control, true, 0.0, 2.0, 0.5)
        );
        let ui = p.ui.as_ref().unwrap();
        assert_eq!(ui.binary, dir.join("libui.so"));
        assert!(ui.required.iter().any(|r| r.ends_with("#parent")));
        assert!(p.required.iter().any(|r| r.ends_with("urid#map")));
    }

    #[test]
    fn a_bundle_without_a_manifest_or_plugins_is_an_error() {
        assert!(read_bundle(Path::new("/nonexistent/x.lv2")).is_err());
        let dir = bundle(
            "empty",
            "@prefix lv2: <http://lv2plug.in/ns/lv2core#> .",
            "",
        );
        assert!(read_bundle(&dir).is_err());
    }
}
