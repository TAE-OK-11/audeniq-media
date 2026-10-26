//! The parts of an ADM document (ITU-R BS.2076) a delivery check needs,
//! collected in one pass over the XML: programmes and their loudness,
//! contents, objects, pack formats (bed vs objects), channel formats with
//! their speaker labels, and track UIDs. Block formats are only counted.
use crate::Error;
use crate::xml::{Event, Parser};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Loudness {
    pub integrated: Option<f64>,
    pub true_peak: Option<f64>,
    pub dialogue: Option<f64>,
}

#[derive(Debug, Default, Clone)]
pub struct Programme {
    pub name: String,
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub contents: Vec<String>,
    pub loudness: Option<Loudness>,
}

#[derive(Debug, Default, Clone)]
pub struct Object {
    pub id: String,
    pub packs: Vec<String>,
    pub tracks: Vec<String>,
    pub objects: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct Pack {
    pub name: String,
    pub channels: Vec<String>,
    pub packs: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct Channel {
    pub name: String,
    pub speaker: Option<String>,
    pub blocks: u64,
}

#[derive(Debug, Default)]
pub struct Document {
    pub programmes: Vec<Programme>,
    pub contents: BTreeMap<String, Vec<String>>,
    pub objects: BTreeMap<String, Object>,
    pub packs: BTreeMap<String, Pack>,
    pub channels: BTreeMap<String, Channel>,
    pub track_uids: BTreeSet<String>,
    pub elements: u64,
}

/// `hh:mm:ss.fffff` (or `…S48000` sample form) → seconds.
pub fn time(s: &str) -> Option<f64> {
    let s = s.trim();
    let (hms, frac) = match s.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let mut parts = hms.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next()?.parse().ok()?;
    let mut t = h * 3600.0 + m * 60.0 + sec;
    if let Some(f) = frac {
        if let Some((num, den)) = f.split_once('S') {
            let (n, d): (f64, f64) = (num.parse().ok()?, den.parse().ok()?);
            if d > 0.0 {
                t += n / d;
            }
        } else {
            t += format!("0.{f}").parse::<f64>().ok()?;
        }
    }
    Some(t)
}

/// The type label inside an ADM id (`AP_yyyyxxxx`, `AC_yyyyxxxx`): `0001`
/// DirectSpeakers, `0002` Matrix, `0003` Objects, `0004` HOA, `0005` Binaural.
pub fn id_type(id: &str) -> Option<&str> {
    id.get(3..7)
}

/// Elements past this many are a hostile document, not a master.
const MAX_ELEMENTS: u64 = 20_000_000;

pub fn parse(xml: &str) -> Result<Document, Error> {
    let mut d = Document::default();
    let mut p = Parser::new(xml);
    let mut path: Vec<&str> = Vec::new();
    let mut prog: Option<Programme> = None;
    let mut obj: Option<Object> = None;
    let mut pack: Option<(String, Pack)> = None;
    let mut chan: Option<(String, Channel)> = None;
    let mut content: Option<(String, Vec<String>)> = None;
    let mut loud: Option<Loudness> = None;
    let mut text = String::new();
    while let Some(ev) = p.next()? {
        match ev {
            Event::Start(name, attrs) => {
                d.elements += 1;
                if d.elements > MAX_ELEMENTS {
                    return Err(Error::Limit("ADM elements".into()));
                }
                text.clear();
                let attr = |k: &str| attrs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
                match name {
                    "audioProgramme" => {
                        prog = Some(Programme {
                            name: attr("audioProgrammeName").unwrap_or_default(),
                            start: attr("start").as_deref().and_then(time),
                            end: attr("end").as_deref().and_then(time),
                            ..Programme::default()
                        })
                    }
                    "audioContent" => {
                        content = Some((attr("audioContentID").unwrap_or_default(), Vec::new()))
                    }
                    "audioObject" => {
                        obj = Some(Object {
                            id: attr("audioObjectID").unwrap_or_default(),
                            ..Object::default()
                        })
                    }
                    "audioPackFormat" => {
                        let id = attr("audioPackFormatID").unwrap_or_default();
                        pack = Some((
                            id,
                            Pack {
                                name: attr("audioPackFormatName").unwrap_or_default(),
                                ..Pack::default()
                            },
                        ));
                    }
                    "audioChannelFormat" => {
                        let id = attr("audioChannelFormatID").unwrap_or_default();
                        chan = Some((
                            id,
                            Channel {
                                name: attr("audioChannelFormatName").unwrap_or_default(),
                                ..Channel::default()
                            },
                        ));
                    }
                    "audioBlockFormat" => {
                        if let Some((_, c)) = &mut chan {
                            c.blocks += 1;
                        }
                    }
                    "audioTrackUID" => {
                        if let Some(uid) = attr("UID") {
                            d.track_uids.insert(uid);
                        }
                    }
                    "loudnessMetadata" => loud = Some(Loudness::default()),
                    _ => {}
                }
                path.push(name);
            }
            Event::Text(t) => text.push_str(&t),
            Event::End(name) => {
                path.pop();
                let t = text.trim().to_owned();
                text.clear();
                let num = || t.parse::<f64>().ok();
                match name {
                    "audioContentIDRef" => {
                        if let Some(p) = &mut prog {
                            p.contents.push(t);
                        }
                    }
                    "audioObjectIDRef" => {
                        if let Some(o) = &mut obj {
                            o.objects.push(t);
                        } else if let Some((_, c)) = &mut content {
                            c.push(t);
                        }
                    }
                    "audioPackFormatIDRef" => {
                        if let Some(o) = &mut obj {
                            o.packs.push(t);
                        } else if let Some((_, p)) = &mut pack {
                            p.packs.push(t);
                        }
                    }
                    "audioTrackUIDRef" => {
                        if let Some(o) = &mut obj {
                            o.tracks.push(t);
                        }
                    }
                    "audioChannelFormatIDRef" => {
                        if let Some((_, p)) = &mut pack {
                            p.channels.push(t);
                        }
                    }
                    "speakerLabel" => {
                        if let Some((_, c)) = &mut chan
                            && c.speaker.is_none()
                        {
                            c.speaker = Some(t);
                        }
                    }
                    "integratedLoudness" => {
                        if let Some(l) = &mut loud {
                            l.integrated = num();
                        }
                    }
                    "maxTruePeak" => {
                        if let Some(l) = &mut loud {
                            l.true_peak = num();
                        }
                    }
                    "dialogueLoudness" => {
                        if let Some(l) = &mut loud {
                            l.dialogue = num();
                        }
                    }
                    "loudnessMetadata" => {
                        if let (Some(l), Some(p)) = (loud.take(), &mut prog)
                            && p.loudness.is_none()
                        {
                            p.loudness = Some(l);
                        }
                    }
                    "audioProgramme" => {
                        if let Some(p) = prog.take() {
                            d.programmes.push(p);
                        }
                    }
                    "audioContent" => {
                        if let Some((id, objs)) = content.take() {
                            d.contents.insert(id, objs);
                        }
                    }
                    "audioObject" => {
                        if let Some(o) = obj.take() {
                            d.objects.insert(o.id.clone(), o);
                        }
                    }
                    "audioPackFormat" => {
                        if let Some((id, p)) = pack.take() {
                            d.packs.insert(id, p);
                        }
                    }
                    "audioChannelFormat" => {
                        if let Some((id, c)) = chan.take() {
                            d.channels.insert(id, c);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(time("00:01:02.50000"), Some(62.5));
        assert_eq!(time("00:00:01.24000S48000"), Some(1.5));
        assert_eq!(time("1:00:00"), Some(3600.0));
        assert_eq!(time("bad"), None);
    }
}
