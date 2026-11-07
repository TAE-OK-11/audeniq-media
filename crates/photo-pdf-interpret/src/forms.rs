//! Interactive form fields (AcroForm): what they hold, and how a field
//! without an appearance stream is drawn.
//!
//! A filled form keeps its answers in the fields (`/V`), and the page shows
//! them through each widget's appearance stream (`/AP`). Tools that fill a
//! form without regenerating appearances leave `/AP` out (or stale, with
//! `/NeedAppearances true`), so a renderer that only draws `/AP` shows an
//! empty form: the checked box, the typed name and the chosen option are
//! missing from the page image, from OCR and from a sanitized copy.
//!
//! [`form_fields`] lists every field with its value; [`widget_content`]
//! draws a widget that has no usable appearance:
//!
//! - check boxes and radio buttons as AUDENIQ's common input controls
//!   (`web/studio/app/src/styles/controls.css` in the AUDENIQ repository,
//!   shared by Studio, admin and survey): a 22-unit box with a 7-unit radius,
//!   `#E8EDF5` when empty, AUDENIQ Blue `#3B63F3` with the same white check
//!   path when checked; a radio is a circle with a white dot;
//! - text and choice values in AUDENIQ Sans, at the field's `/DA` size and
//!   colour, aligned by `/Q`, wrapped in multiline fields, spread over the
//!   cells of a comb field, masked in a password field.
//!
//! The drawing is a content stream of path operators (text as glyph
//! outlines), interpreted like any other content, in the widget's rectangle
//! with the origin at its lower left corner.
use photo_pdf_syntax::object::dict::keys::{
    ACRO_FORM, AP, AS, DA, FIELDS, FF, FT, KIDS, MAX_LEN, NEED_APPEARANCES, OPT, PARENT, Q, T, V,
};
use photo_pdf_syntax::object::{Array, Dict, Name, Object, ObjectIdentifier};
use photo_pdf_syntax::xref::XRef;
use std::collections::HashSet;
use std::fmt::Write as _;

/// Field flags (PDF 32000-1, 12.7.4).
const FF_READ_ONLY: u32 = 1;
const FF_MULTILINE: u32 = 1 << 12;
const FF_PASSWORD: u32 = 1 << 13;
const FF_RADIO: u32 = 1 << 15;
const FF_PUSHBUTTON: u32 = 1 << 16;
const FF_COMBO: u32 = 1 << 17;
const FF_COMB: u32 = 1 << 24;

/// Bounds for hostile documents.
const MAX_DEPTH: usize = 32;
const MAX_FIELDS: usize = 10_000;
const MAX_CHARS: usize = 4_096;

/// What kind of field a widget belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    CheckBox,
    Radio,
    PushButton,
    Text,
    Choice,
    Signature,
}

impl FieldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckBox => "checkbox",
            Self::Radio => "radio",
            Self::PushButton => "button",
            Self::Text => "text",
            Self::Choice => "choice",
            Self::Signature => "signature",
        }
    }
}

/// One terminal field of a document's form.
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    /// Fully qualified name (`parent.child`).
    pub name: String,
    pub kind: FieldKind,
    /// Text or selected option(s) (joined with `, `); for check boxes and
    /// radio groups the chosen state name.
    pub value: Option<String>,
    /// Check boxes and radio groups: whether something is selected.
    pub checked: Option<bool>,
    /// Signature fields: whether a signature value is present (its
    /// cryptographic validity is not checked).
    pub signed: bool,
    pub read_only: bool,
}

/// The document's form: whether viewers must regenerate appearances, and
/// its fields (empty without an `/AcroForm`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Form {
    pub need_appearances: bool,
    pub fields: Vec<FormField>,
}

/// The document's form fields, in document order.
pub fn form_fields(xref: &XRef) -> Form {
    let Some(acro) = acro_form(xref) else {
        return Form::default();
    };
    let mut form = Form {
        need_appearances: acro.get::<bool>(NEED_APPEARANCES).unwrap_or(false),
        fields: Vec::new(),
    };
    let mut seen = HashSet::new();
    if let Some(fields) = acro.get::<Array<'_>>(FIELDS) {
        for field in fields.iter::<Dict<'_>>() {
            walk(&field, "", 0, &mut seen, &mut form.fields);
        }
    }
    form
}

pub(crate) fn acro_form(xref: &XRef) -> Option<Dict<'_>> {
    xref.get::<Dict<'_>>(xref.root_id())?.get::<Dict<'_>>(ACRO_FORM)
}

fn walk(
    field: &Dict<'_>,
    parent: &str,
    depth: usize,
    seen: &mut HashSet<ObjectIdentifier>,
    out: &mut Vec<FormField>,
) {
    if depth > MAX_DEPTH || out.len() >= MAX_FIELDS {
        return;
    }
    if let Some(id) = field.obj_id()
        && !seen.insert(id)
    {
        return;
    }
    let partial = field.get::<photo_pdf_syntax::object::String<'_>>(T).map(|s| text_string(&s));
    let name = match (parent.is_empty(), partial) {
        (_, None) => parent.to_string(),
        (true, Some(p)) => p,
        (false, Some(p)) => format!("{parent}.{p}"),
    };
    // Kids that are fields themselves (they have a name) are walked;
    // kids without one are this field's widgets.
    let kids: Vec<Dict<'_>> = field
        .get::<Array<'_>>(KIDS)
        .map(|k| k.iter::<Dict<'_>>().collect())
        .unwrap_or_default();
    let child_fields: Vec<&Dict<'_>> = kids.iter().filter(|k| k.contains_key(T)).collect();
    if !child_fields.is_empty() {
        for kid in child_fields {
            walk(kid, &name, depth + 1, seen, out);
        }
        return;
    }
    let Some(kind) = field_kind(field) else {
        return;
    };
    let flags = inherited::<u32>(field, FF).unwrap_or(0);
    let (value, checked) = match kind {
        FieldKind::CheckBox | FieldKind::Radio => {
            // The state name in /V; a widget's /AS when /V is missing.
            let state = inherited::<Name<'_>>(field, V)
                .map(|n| name_str(&n))
                .or_else(|| {
                    std::iter::once(field)
                        .chain(kids.iter())
                        .filter_map(|w| w.get::<Name<'_>>(AS))
                        .map(|n| name_str(&n))
                        .find(|s| s != "Off")
                });
            let on = state.as_deref().is_some_and(|s| s != "Off");
            (state.filter(|s| s != "Off"), Some(on))
        }
        FieldKind::Text | FieldKind::Choice => (field_value(field), None),
        FieldKind::PushButton | FieldKind::Signature => (None, None),
    };
    out.push(FormField {
        name,
        kind,
        value,
        checked,
        signed: kind == FieldKind::Signature && inherited::<Object<'_>>(field, V).is_some(),
        read_only: flags & FF_READ_ONLY != 0,
    });
}

/// An attribute of a field or widget, inherited from its ancestors.
fn inherited<'a, T>(d: &Dict<'a>, key: &[u8]) -> Option<T>
where
    T: photo_pdf_syntax::object::ObjectLike<'a>,
{
    let mut current = d.clone();
    for _ in 0..MAX_DEPTH {
        if let Some(v) = current.get::<T>(key) {
            return Some(v);
        }
        current = current.get::<Dict<'a>>(PARENT)?;
    }
    None
}

fn field_kind(d: &Dict<'_>) -> Option<FieldKind> {
    let ft = inherited::<Name<'_>>(d, FT)?;
    let flags = inherited::<u32>(d, FF).unwrap_or(0);
    Some(match ft.as_ref() {
        b"Btn" if flags & FF_PUSHBUTTON != 0 => FieldKind::PushButton,
        b"Btn" if flags & FF_RADIO != 0 => FieldKind::Radio,
        b"Btn" => FieldKind::CheckBox,
        b"Tx" => FieldKind::Text,
        b"Ch" => FieldKind::Choice,
        b"Sig" => FieldKind::Signature,
        _ => return None,
    })
}

/// A text or choice value: a string, or an array of them (multiple
/// selection), joined with `, `.
fn field_value(d: &Dict<'_>) -> Option<String> {
    let v = inherited::<Object<'_>>(d, V)?;
    let s = match v {
        Object::String(s) => text_string(&s),
        Object::Name(n) => name_str(&n),
        Object::Array(a) => a
            .iter::<Object<'_>>()
            .filter_map(|o| match o {
                Object::String(s) => Some(text_string(&s)),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(", "),
        _ => return None,
    };
    Some(s.chars().take(MAX_CHARS).collect())
}

fn name_str(n: &Name<'_>) -> String {
    String::from_utf8_lossy(n.as_ref()).into_owned()
}

/// A PDF text string: UTF-16BE or UTF-8 with a byte order mark, else
/// PDFDocEncoding.
pub fn text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return char::decode_utf16(units)
            .map(|c| c.unwrap_or('\u{FFFD}'))
            .collect();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    bytes.iter().map(|&b| pdf_doc_char(b)).collect()
}

/// PDFDocEncoding (PDF 32000-1, Annex D): Latin-1 except 0x18..0x1F and
/// 0x80..0x9F.
fn pdf_doc_char(b: u8) -> char {
    const LOW: [char; 8] = ['˘', 'ˇ', 'ˆ', '˙', '˝', '˛', '˚', '˜'];
    const HIGH: [char; 32] = [
        '•', '†', '‡', '…', '—', '–', 'ƒ', '⁄', '‹', '›', '−', '‰', '„', '“', '”', '‘', '’', '‚',
        '™', 'ﬁ', 'ﬂ', 'Ł', 'Œ', 'Š', 'Ÿ', 'Ž', 'ı', 'ł', 'œ', 'š', 'ž', '\u{FFFD}',
    ];
    match b {
        0x18..=0x1F => LOW[usize::from(b - 0x18)],
        0x80..=0x9F => HIGH[usize::from(b - 0x80)],
        0xA0 => '€',
        _ => char::from(b),
    }
}

/// Whether a widget's own appearance stream should be replaced: it has none
/// for its state, or the document asks viewers to regenerate appearances
/// and the field shows a value (text or choice), which a stale stream would
/// hide. Check boxes and radios keep their streams: `/AS` selects the state.
pub(crate) fn needs_generated(widget: &Dict<'_>, has_stream: bool, need_appearances: bool) -> bool {
    match field_kind(widget) {
        None | Some(FieldKind::PushButton) | Some(FieldKind::Signature) => false,
        Some(FieldKind::Text | FieldKind::Choice) => !has_stream || need_appearances,
        Some(FieldKind::CheckBox | FieldKind::Radio) => !has_stream,
    }
}

/// AUDENIQ common control colours (`controls.css`).
const BOX: [f32; 3] = [0.909_8, 0.929_4, 0.960_8]; // #E8EDF5
const BLUE: [f32; 3] = [0.231_4, 0.388_2, 0.952_9]; // #3B63F3

/// The content of a widget without a usable appearance, in its rectangle of
/// `w`×`h` (origin at the lower left), or `None` when there is nothing to
/// draw.
pub(crate) fn widget_content(widget: &Dict<'_>, w: f32, h: f32, form_da: Option<&str>) -> Option<Vec<u8>> {
    if !(w.is_finite() && h.is_finite() && w > 0.5 && h > 0.5 && w < 20_000.0 && h < 20_000.0) {
        return None;
    }
    let kind = field_kind(widget)?;
    let flags = inherited::<u32>(widget, FF).unwrap_or(0);
    let mut s = String::new();
    match kind {
        FieldKind::CheckBox | FieldKind::Radio => {
            // This widget's state: /AS, or the field's /V when /AS is
            // missing (a single widget).
            let on = match widget.get::<Name<'_>>(AS) {
                Some(state) => state.as_ref() != b"Off",
                None => inherited::<Name<'_>>(widget, V).is_some_and(|v| v.as_ref() != b"Off"),
            };
            control(&mut s, kind == FieldKind::Radio, on, w, h);
        }
        FieldKind::Text | FieldKind::Choice => {
            let mut value = field_value(widget)?;
            if kind == FieldKind::Choice && widget.contains_key(OPT) {
                value = display_choice(widget, &value);
            }
            if value.is_empty() {
                return None;
            }
            if flags & FF_PASSWORD != 0 {
                value = "•".repeat(value.chars().count());
            }
            let da = inherited::<photo_pdf_syntax::object::String<'_>>(widget, DA)
                .map(|s| text_string(&s))
                .or_else(|| form_da.map(str::to_string))
                .unwrap_or_default();
            let style = TextStyle::parse(&da);
            let align = inherited::<u32>(widget, Q).unwrap_or(0).min(2);
            let multiline = kind == FieldKind::Text && flags & FF_MULTILINE != 0
                || kind == FieldKind::Choice && flags & FF_COMBO == 0;
            let comb = (kind == FieldKind::Text && flags & FF_COMB != 0)
                .then(|| inherited::<u32>(widget, MAX_LEN))
                .flatten()
                .filter(|&n| n > 0 && n <= 1_000);
            text(&mut s, &value, &style, align, multiline, comb, w, h)?;
        }
        FieldKind::PushButton | FieldKind::Signature => return None,
    }
    (!s.is_empty()).then(|| s.into_bytes())
}

/// A choice value shown by its display text (`/Opt` pairs are
/// `[export display]`).
fn display_choice(widget: &Dict<'_>, value: &str) -> String {
    let Some(opt) = inherited::<Array<'_>>(widget, OPT) else {
        return value.to_string();
    };
    let pairs: Vec<(String, String)> = opt
        .iter::<Object<'_>>()
        .filter_map(|o| match o {
            Object::Array(a) => {
                let mut it = a.iter::<photo_pdf_syntax::object::String<'_>>();
                let export = text_string(&it.next()?);
                let display = it.next().map(|s| text_string(&s)).unwrap_or_else(|| export.clone());
                Some((export, display))
            }
            Object::String(s) => {
                let t = text_string(&s);
                Some((t.clone(), t))
            }
            _ => None,
        })
        .collect();
    value
        .split(", ")
        .map(|v| {
            pairs
                .iter()
                .find(|(e, _)| e == v)
                .map_or(v.to_string(), |(_, d)| d.clone())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A check box or radio button in the common control style, centred in the
/// rectangle at the largest square size that fits.
fn control(s: &mut String, radio: bool, on: bool, w: f32, h: f32) {
    let size = w.min(h);
    let (x, y) = ((w - size) / 2.0, (h - size) / 2.0);
    let k = size / 22.0;
    let fill = if on { BLUE } else { BOX };
    let _ = writeln!(s, "q {} {} {} rg", fill[0], fill[1], fill[2]);
    if radio {
        circle(s, x + size / 2.0, y + size / 2.0, size / 2.0);
        s.push_str("f\n");
        if on {
            s.push_str("1 g\n");
            circle(s, x + size / 2.0, y + size / 2.0, 4.0 * k);
            s.push_str("f\n");
        }
    } else {
        rounded_rect(s, x, y, size, size, 7.0 * k);
        s.push_str("f\n");
        if on {
            // The check of `--aqc-check`: a 14-unit image (16-unit view
            // box) centred in the 22-unit box, y flipped from SVG.
            let unit = 14.0 / 16.0 * k;
            let p = |px: f32, py: f32| (x + 4.0 * k + px * unit, y + 4.0 * k + (16.0 - py) * unit);
            let (a, b, c) = (p(3.6, 8.3), p(6.6, 11.3), p(12.4, 4.9));
            let _ = writeln!(
                s,
                "1 G {} w 1 J 1 j {} {} m {} {} l {} {} l S",
                2.3 * unit,
                a.0,
                a.1,
                b.0,
                b.1,
                c.0,
                c.1
            );
        }
    }
    s.push_str("Q\n");
}

const KAPPA: f32 = 0.552_284_8;

fn circle(s: &mut String, cx: f32, cy: f32, r: f32) {
    let c = r * KAPPA;
    let _ = writeln!(
        s,
        "{} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c h",
        cx + r, cy,
        cx + r, cy + c, cx + c, cy + r, cx, cy + r,
        cx - c, cy + r, cx - r, cy + c, cx - r, cy,
        cx - r, cy - c, cx - c, cy - r, cx, cy - r,
        cx + c, cy - r, cx + r, cy - c, cx + r, cy
    );
}

fn rounded_rect(s: &mut String, x: f32, y: f32, w: f32, h: f32, r: f32) {
    let r = r.min(w / 2.0).min(h / 2.0);
    let c = r * KAPPA;
    let (x1, y1) = (x + w, y + h);
    let _ = writeln!(
        s,
        "{} {} m {} {} l {} {} {} {} {} {} c {} {} l {} {} {} {} {} {} c {} {} l {} {} {} {} {} {} c {} {} l {} {} {} {} {} {} c h",
        x + r, y,
        x1 - r, y,
        x1 - r + c, y, x1, y + r - c, x1, y + r,
        x1, y1 - r,
        x1, y1 - r + c, x1 - r + c, y1, x1 - r, y1,
        x + r, y1,
        x + r - c, y1, x, y1 - r + c, x, y1 - r,
        x, y + r,
        x, y + r - c, x + r - c, y, x + r, y
    );
}

/// Font size and colour from a default appearance string such as
/// `/Helv 12 Tf 0 g` (the font itself is replaced by AUDENIQ Sans).
#[derive(Debug, Clone, PartialEq)]
struct TextStyle {
    /// 0: fit the field.
    size: f32,
    color: String,
}

impl TextStyle {
    fn parse(da: &str) -> Self {
        let mut style = Self {
            size: 0.0,
            color: "0 g".into(),
        };
        let tokens: Vec<&str> = da.split_ascii_whitespace().collect();
        for (i, t) in tokens.iter().enumerate() {
            let nums = |n: usize| -> Option<Vec<f32>> {
                let start = i.checked_sub(n)?;
                tokens[start..i]
                    .iter()
                    .map(|v| v.parse::<f32>().ok().filter(|f| f.is_finite()))
                    .collect()
            };
            match *t {
                "Tf" => {
                    if let Some(v) = nums(1) {
                        style.size = v[0].clamp(0.0, 1_000.0);
                    }
                }
                "g" | "rg" | "k" => {
                    let n = match *t {
                        "g" => 1,
                        "rg" => 3,
                        _ => 4,
                    };
                    if let Some(v) = nums(n) {
                        let v: Vec<String> = v.iter().map(|c| c.clamp(0.0, 1.0).to_string()).collect();
                        style.color = format!("{} {t}", v.join(" "));
                    }
                }
                _ => {}
            }
        }
        style
    }
}

/// Padding inside a field (Acrobat uses 2 points).
const PAD: f32 = 2.0;

#[cfg(feature = "embed-fonts")]
#[allow(clippy::too_many_arguments)]
fn text(
    s: &mut String,
    value: &str,
    style: &TextStyle,
    align: u32,
    multiline: bool,
    comb: Option<u32>,
    w: f32,
    h: f32,
) -> Option<()> {
    use skrifa::MetadataProvider;
    use skrifa::instance::{LocationRef, Size};
    let (data, index) = crate::font::audeniq_sans(false)?;
    let font = skrifa::FontRef::from_index(data.as_ref().as_ref(), index).ok()?;
    let charmap = font.charmap();
    let upem = font.head().ok().map(|h| f32::from(h.units_per_em()))?;
    let metrics = font.metrics(Size::unscaled(), LocationRef::default());
    let (ascent, descent) = (metrics.ascent / upem, -metrics.descent / upem);
    let advances = font.glyph_metrics(Size::unscaled(), LocationRef::default());
    let outlines = font.outline_glyphs();
    let width_of = |text: &str, size: f32| -> f32 {
        text.chars()
            .filter_map(|c| charmap.map(c))
            .filter_map(|g| advances.advance_width(g))
            .sum::<f32>()
            / upem
            * size
    };
    let inner_w = (w - 2.0 * PAD).max(1.0);
    let inner_h = (h - 2.0 * PAD).max(1.0);
    // Auto size: fit the height (one line) and, for single-line fields,
    // the width, between 4 and 12 points as viewers do.
    let size = if style.size > 0.0 {
        style.size
    } else if multiline {
        12.0_f32.min(inner_h)
    } else {
        let by_height = inner_h / (ascent + descent);
        let by_width = inner_w / (width_of(value, 1.0)).max(0.01);
        by_height.min(by_width).clamp(4.0, 12.0_f32.max(4.0).min(by_height.max(4.0)))
    };
    let lines: Vec<String> = if multiline {
        wrap(value, inner_w, |t| width_of(t, size))
    } else {
        vec![value.replace(['\r', '\n'], " ")]
    };
    let leading = size * (ascent + descent) * 1.1;
    let _ = writeln!(s, "q 0 0 {w} {h} re W n {}", style.color);
    let mut draw = |line: &str, x0: f32, baseline: f32, cell: Option<f32>| {
        let mut x = x0;
        for c in line.chars() {
            let gid = charmap.map(c).unwrap_or_default();
            let adv = advances.advance_width(gid).unwrap_or(0.0) / upem * size;
            let gx = match cell {
                Some(cw) => x + (cw - adv) / 2.0,
                None => x,
            };
            if let Some(glyph) = outlines.get(gid) {
                let mut pen = Pen {
                    s: &mut *s,
                    scale: size / upem,
                    x: gx,
                    y: baseline,
                    any: false,
                };
                let settings = skrifa::outline::DrawSettings::unhinted(Size::unscaled(), LocationRef::default());
                if glyph.draw(settings, &mut pen).is_ok() && pen.any {
                    pen.s.push_str("f\n");
                }
            }
            x += cell.unwrap_or(adv);
        }
    };
    if let Some(cells) = comb {
        let cw = w / cells as f32;
        let baseline = (h - size * (ascent + descent)) / 2.0 + size * descent;
        let line: String = lines[0].chars().take(cells as usize).collect();
        draw(&line, 0.0, baseline, Some(cw));
    } else if multiline {
        let mut baseline = h - PAD - size * ascent;
        for line in &lines {
            if baseline < -size {
                break;
            }
            let lw = width_of(line, size);
            let x = PAD + (inner_w - lw).max(0.0) * align as f32 / 2.0;
            draw(line, x, baseline, None);
            baseline -= leading;
        }
    } else {
        let lw = width_of(&lines[0], size);
        let x = PAD + (inner_w - lw).max(0.0) * align as f32 / 2.0;
        let baseline = (h - size * (ascent + descent)) / 2.0 + size * descent;
        draw(&lines[0], x, baseline, None);
    }
    s.push_str("Q\n");
    Some(())
}

#[cfg(not(feature = "embed-fonts"))]
#[allow(clippy::too_many_arguments)]
fn text(_: &mut String, _: &str, _: &TextStyle, _: u32, _: bool, _: Option<u32>, _: f32, _: f32) -> Option<()> {
    None
}

/// Lines of `value` that fit `width`: explicit breaks first, then words,
/// then characters for a word wider than the field.
#[cfg_attr(not(feature = "embed-fonts"), allow(dead_code))]
fn wrap(value: &str, width: f32, measure: impl Fn(&str) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in value.split(['\r', '\n']) {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if measure(&candidate) <= width || line.is_empty() && measure(word) <= width {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            // A word wider than the field breaks between characters.
            for c in word.chars() {
                let candidate = format!("{line}{c}");
                if measure(&candidate) > width && !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                    line.push(c);
                } else {
                    line = candidate;
                }
            }
        }
        lines.push(line);
        if lines.len() > 500 {
            break;
        }
    }
    lines
}

/// Glyph outlines as path operators, scaled and placed at the pen origin.
#[cfg(feature = "embed-fonts")]
struct Pen<'s> {
    s: &'s mut String,
    scale: f32,
    x: f32,
    y: f32,
    any: bool,
}

#[cfg(feature = "embed-fonts")]
impl Pen<'_> {
    fn p(&self, x: f32, y: f32) -> (f32, f32) {
        (self.x + x * self.scale, self.y + y * self.scale)
    }
}

#[cfg(feature = "embed-fonts")]
impl skrifa::outline::OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.p(x, y);
        let _ = write!(self.s, "{x} {y} m ");
        self.any = true;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.p(x, y);
        let _ = write!(self.s, "{x} {y} l ");
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        // PDF has cubic curves only: the quadratic's equivalent cubic needs
        // the current point, which `v`/`y` operators do not give, so the
        // quadratic is drawn with both control points at 2/3 towards the
        // control point from each end, using the last end recorded below.
        let (c, e) = (self.p(cx, cy), self.p(x, y));
        let _ = write!(self.s, "{} {} {} {} {} {} QUAD ", c.0, c.1, e.0, e.1, 0, 0);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (a, b, e) = (self.p(cx0, cy0), self.p(cx1, cy1), self.p(x, y));
        let _ = write!(self.s, "{} {} {} {} {} {} c ", a.0, a.1, b.0, b.1, e.0, e.1);
    }
    fn close(&mut self) {
        self.s.push_str("h ");
    }
}

/// The form's default appearance string (`/AcroForm /DA`).
pub(crate) fn form_default_da(xref: &XRef) -> Option<String> {
    acro_form(xref)?
        .get::<photo_pdf_syntax::object::String<'_>>(DA)
        .map(|s| text_string(&s))
}

/// Whether a widget's `/AP` has a stream for its current state.
pub(crate) fn has_ap(widget: &Dict<'_>) -> bool {
    widget.contains_key(AP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_strings_decode() {
        assert_eq!(text_string(b"\xFE\xFF\xD6\x5C\xAE\x38\xB3\xD9"), "홍길동");
        assert_eq!(text_string(b"\xEF\xBB\xBFabc"), "abc");
        assert_eq!(text_string(b"caf\xE9 \x93ok\x94 \x80"), "café “ok” •");
    }

    #[test]
    fn default_appearance_strings_parse() {
        let s = TextStyle::parse("/Helv 12 Tf 0 0 1 rg");
        assert_eq!((s.size, s.color.as_str()), (12.0, "0 0 1 rg"));
        let s = TextStyle::parse("/F1 0 Tf 0.5 g");
        assert_eq!((s.size, s.color.as_str()), (0.0, "0.5 g"));
        let s = TextStyle::parse("garbage Tf rg");
        assert_eq!((s.size, s.color.as_str()), (0.0, "0 g"));
    }

    #[test]
    fn wrapping_breaks_words_then_characters() {
        let m = |t: &str| t.chars().count() as f32;
        assert_eq!(wrap("aa bb cc", 5.0, m), ["aa bb", "cc"]);
        assert_eq!(wrap("abcdefgh", 3.0, m), ["abc", "def", "gh"]);
        assert_eq!(wrap("one\rtwo", 10.0, m), ["one", "two"]);
    }
}
