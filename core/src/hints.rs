//! Where else to search: search hints and the names worth geocoding.
//!
//! A *hint* is a candidate area for the map (a point and a radius), searched
//! before the area around the GPS fix at the normal resolution (zoom-11
//! coarse search, zoom-13 refinement). Hints come from the request (places
//! the app geocoded from `hint_queries`, or an area the user picked) and
//! from the map itself: coordinate labels the grid fit could not use (one
//! latitude and one longitude label, or a corner label holding both) become
//! `label` hints (`label_hints`).
//!
//! `hint_queries` picks the names on the map worth geocoding: the title and
//! the place, peak and water names, best first, without legend entries, map
//! furniture (sheet numbers, credits, scale, declination) or route notes.

use crate::geo::{destination, latlon_to_merc, merc_to_latlon, LatLon};
use crate::labels::{
    has_angle_mark, is_measure, legend_members, median, parse_coordinate, parse_height, parse_scale, scale_bar_members,
    words_of, CoordValue, OcrObservation, LEGEND_WORDS, META_WORDS,
};
use serde::{Deserialize, Serialize};

/// Where a hint came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HintSource {
    /// A place named on the map, geocoded by the app.
    Place,
    /// Coordinates printed on the map.
    Label,
    /// An area the user picked.
    User,
}

impl HintSource {
    /// Parse a wire value; anything unknown is a place.
    pub fn parse(s: &str) -> HintSource {
        match s.trim().to_ascii_lowercase().as_str() {
            "label" => HintSource::Label,
            "user" => HintSource::User,
            _ => HintSource::Place,
        }
    }

    /// Search order: the user's own pick, then the map's coordinates, then
    /// geocoded names.
    pub fn rank(self) -> u8 {
        match self {
            HintSource::User => 0,
            HintSource::Label => 1,
            HintSource::Place => 2,
        }
    }
}

/// A candidate area for the map: its centre lies within `radius_m` (ground
/// metres) of (`lat`, `lon`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHint {
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
    pub source: HintSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Search radius of a request hint without one (ground metres).
pub const DEFAULT_HINT_RADIUS_M: f64 = 15_000.0;
/// Radius range of a hint search (ground metres): a hint is searched like the
/// near area, so at most the near radius.
pub const HINT_RADIUS_RANGE_M: (f64, f64) = (1_000.0, 50_000.0);
/// Most `hint_queries` returned.
pub const MAX_HINT_QUERIES: usize = 12;

impl SearchHint {
    pub fn latlon(&self) -> LatLon {
        LatLon::new(self.lat, self.lon)
    }

    /// Plausible coordinates (Web Mercator stops at ±85°).
    pub fn is_valid(&self) -> bool {
        self.lat.is_finite() && self.lon.is_finite() && self.lat.abs() <= 85.0 && self.lon.abs() <= 180.0
    }
}

// ---------------------------------------------------------------------------
// Label hints
// ---------------------------------------------------------------------------

/// A latitude and a longitude read in one observation ("25°49'S 27°23'E",
/// a corner label): split at every space and keep the split where one side
/// reads as a latitude and the other as a longitude.
pub fn parse_coordinate_pair(text: &str) -> Option<(f64, f64)> {
    let t: Vec<&str> = text.split_whitespace().collect();
    for k in 1..t.len() {
        let (a, b) = (t[..k].join(" "), t[k..].join(" "));
        match (parse_coordinate(&a), parse_coordinate(&b)) {
            (Some(CoordValue::Lat(la)), Some(CoordValue::Lon(lo)))
            | (Some(CoordValue::Lon(lo)), Some(CoordValue::Lat(la))) => return Some((la, lo)),
            _ => {}
        }
    }
    None
}

/// "25°49'S 27°23'E" style text for a hint name.
fn dm_text(lat: f64, lon: f64) -> String {
    let f = |v: f64, pos: char, neg: char| {
        let a = v.abs();
        let mut d = a.trunc();
        let mut m = ((a - d) * 60.0).round();
        if m >= 60.0 {
            d += 1.0;
            m -= 60.0;
        }
        format!("{d:.0}°{m:02.0}'{}", if v < 0.0 { neg } else { pos })
    };
    format!("{} {}", f(lat, 'N', 'S'), f(lon, 'E', 'W'))
}

/// Hints from coordinate labels on the map (one latitude and one longitude
/// label are enough, or one label holding both): the map centre lies along
/// the image-centre offset from the labelled point, at the unknown scale in
/// `scale` (ground m/px, min..max), so the hint is the disc around that
/// segment, assuming a north-up map, plus 15 % of the offset for a turned
/// one and 2 km. `img_w`/`img_h`: the raster size.
pub fn label_hints(obs: &[OcrObservation], img_w: usize, img_h: usize, scale: (f64, f64)) -> Vec<SearchHint> {
    let mut lats: Vec<(f64, f64)> = Vec::new(); // (value, pixel y)
    let mut lons: Vec<(f64, f64)> = Vec::new(); // (value, pixel x)
    let mut pairs: Vec<(f64, f64, (f64, f64))> = Vec::new();
    for o in obs {
        if !o.bbox.iter().all(|v| v.is_finite()) {
            continue;
        }
        let c = o.center();
        match parse_coordinate(&o.text) {
            Some(CoordValue::Lat(v)) if v.abs() <= 85.0 => lats.push((v, c.1)),
            Some(CoordValue::Lon(v)) if v.abs() <= 180.0 => lons.push((v, c.0)),
            Some(_) => {}
            None => {
                if let Some((la, lo)) =
                    parse_coordinate_pair(&o.text).filter(|p| p.0.abs() <= 85.0 && p.1.abs() <= 180.0)
                {
                    pairs.push((la, lo, c));
                }
            }
        }
    }
    let mut points: Vec<(f64, f64, (f64, f64))> = Vec::new();
    if !lats.is_empty() && !lons.is_empty() {
        let la = median(lats.iter().map(|v| v.0).collect()).unwrap_or(0.0);
        let lo = median(lons.iter().map(|v| v.0).collect()).unwrap_or(0.0);
        let py = median(lats.iter().map(|v| v.1).collect()).unwrap_or(img_h as f64 / 2.0);
        let px = median(lons.iter().map(|v| v.1).collect()).unwrap_or(img_w as f64 / 2.0);
        points.push((la, lo, (px, py)));
    }
    points.extend(pairs);
    let (lo_s, hi_s) = (scale.0.max(0.0), scale.1.max(scale.0).max(0.0));
    let mut out: Vec<SearchHint> = Vec::new();
    for (la, lo, (px, py)) in points {
        // Offset of the image centre from the labelled pixel (east, south).
        let (dx, dy) = (img_w as f64 / 2.0 - px, img_h as f64 / 2.0 - py);
        let off = dx.hypot(dy);
        let mid = 0.5 * (lo_s + hi_s) * off;
        let centre = if off > 0.0 {
            // Bearing of the image centre from the point: +x east, +y south.
            let bearing = dx.atan2(-dy).to_degrees().rem_euclid(360.0);
            destination(LatLon::new(la, lo), bearing, mid)
        } else {
            LatLon::new(la, lo)
        };
        let radius = (0.5 * (hi_s - lo_s) * off + 0.15 * hi_s * off + 2000.0)
            .clamp(HINT_RADIUS_RANGE_M.0.max(3000.0), HINT_RADIUS_RANGE_M.1);
        let h = SearchHint {
            lat: centre.lat,
            lon: centre.lon,
            radius_m: radius,
            source: HintSource::Label,
            name: Some(dm_text(la, lo)),
        };
        if h.is_valid() && !out.iter().any(|o| o.name == h.name) {
            out.push(h);
        }
    }
    out
}

/// Where coordinate labels of one axis alone put the map centre: a band of
/// latitudes or of longitudes (degrees), for a search radius to be cut down to.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LabelBand {
    /// Latitude range (south, north) of the map centre, or None.
    pub lat: Option<(f64, f64)>,
    /// Longitude range (west, east) of the map centre, or None.
    pub lon: Option<(f64, f64)>,
    pub name: String,
}

/// A band from coordinate labels of one axis only (latitude labels and no
/// longitude label, or the reverse): the label nearest the image centre
/// gives the line's value and pixel row (column); the map centre is the
/// image-centre offset from it at the unknown scale in `scale` (ground m/px),
/// north up, plus 15 % of the offset and 2 km. `lat_hint`: a latitude for
/// the length of a degree of longitude (the GPS fix). None with labels of
/// both axes (a point: `label_hints`) or none.
pub fn label_band(
    obs: &[OcrObservation],
    img_w: usize,
    img_h: usize,
    scale: (f64, f64),
    lat_hint: f64,
) -> Option<LabelBand> {
    let mut lats: Vec<(f64, f64)> = Vec::new();
    let mut lons: Vec<(f64, f64)> = Vec::new();
    for o in obs.iter().filter(|o| o.bbox.iter().all(|v| v.is_finite())) {
        let c = o.center();
        match parse_coordinate(&o.text) {
            Some(CoordValue::Lat(v)) if v.abs() <= 85.0 => lats.push((v, c.1)),
            Some(CoordValue::Lon(v)) if v.abs() <= 180.0 => lons.push((v, c.0)),
            _ => {}
        }
    }
    let (lo_s, hi_s) = (scale.0.max(0.0), scale.1.max(scale.0).max(0.0));
    // Degrees of the map-centre offset range for a label at pixel `p` of an
    // axis of `n` pixels, `m_per_deg` metres a degree; `sign` turns pixels
    // (down, right) into degrees (north, east).
    let range = |p: f64, n: usize, value: f64, m_per_deg: f64, sign: f64| {
        let off = n as f64 / 2.0 - p;
        let (a, b) = (value + sign * off * lo_s / m_per_deg, value + sign * off * hi_s / m_per_deg);
        let slack = (0.15 * off.abs() * hi_s + 2000.0) / m_per_deg;
        (a.min(b) - slack, a.max(b) + slack)
    };
    const M_PER_DEG: f64 = 111_320.0;
    let nearest =
        |v: &[(f64, f64)], mid: f64| v.iter().copied().min_by(|a, b| (a.1 - mid).abs().total_cmp(&(b.1 - mid).abs()));
    match (lats.is_empty(), lons.is_empty()) {
        (false, true) => {
            let (v, py) = nearest(&lats, img_h as f64 / 2.0)?;
            let (a, b) = range(py, img_h, v, M_PER_DEG, -1.0);
            Some(LabelBand { lat: Some((a.max(-85.0), b.min(85.0))), lon: None, name: dm_lat(v) })
        }
        (true, false) => {
            let (v, px) = nearest(&lons, img_w as f64 / 2.0)?;
            let m = M_PER_DEG * lat_hint.to_radians().cos().max(0.05);
            let (a, b) = range(px, img_w, v, m, 1.0);
            Some(LabelBand { lat: None, lon: Some((a.max(-180.0), b.min(180.0))), name: dm_lon(v) })
        }
        _ => None,
    }
}

fn dm_lat(v: f64) -> String {
    dm_text(v, 0.0).split(' ').next().unwrap_or_default().to_string()
}

fn dm_lon(v: f64) -> String {
    dm_text(0.0, v).split(' ').nth(1).unwrap_or_default().to_string()
}

/// Order hints for searching (user, then label, then place hints, each in
/// the order given) and drop those an earlier hint already covers (its disc
/// holds theirs, give or take 25 %).
pub fn order_hints(mut hints: Vec<SearchHint>) -> Vec<SearchHint> {
    hints.sort_by_key(|h| h.source.rank());
    let mut out: Vec<SearchHint> = Vec::new();
    for h in hints {
        let covered = out.iter().any(|o| {
            let d = ground_distance_m(o.latlon(), h.latlon());
            d + h.radius_m <= 1.25 * o.radius_m
        });
        if !covered {
            out.push(h);
        }
    }
    out
}

/// Great-circle distance (metres).
pub fn ground_distance_m(a: LatLon, b: LatLon) -> f64 {
    crate::geo::haversine_m(a, b)
}

/// Mercator position of a hint.
pub fn hint_merc(h: &SearchHint) -> (f64, f64) {
    latlon_to_merc(h.lat, h.lon)
}

/// Lat/lon of a Mercator point.
pub fn merc_latlon(m: (f64, f64)) -> LatLon {
    let (la, lo) = merc_to_latlon(m.0, m.1);
    LatLon::new(la, lo)
}

// ---------------------------------------------------------------------------
// Names worth geocoding
// ---------------------------------------------------------------------------

/// Words of signage and route notes ("NO ENTRY SIGN", "KEEP LEFT", "TURN
/// RIGHT AT SIGN"): never a place to geocode.
const NOTE_WORDS: &[&str] = &[
    "no",
    "entry",
    "sign",
    "signs",
    "signpost",
    "post",
    "keep",
    "left",
    "right",
    "turn",
    "at",
    "private",
    "property",
    "vehicle",
    "joins",
    "toilet",
    "stile",
    "crossing",
    "box",
    "staff",
    "houses",
    "accommodation",
    "tented",
    "lodge",
    "cottage",
    "country",
    "school",
    "bush",
    "lightning",
    "upper",
    "lower",
    "north",
    "south",
    "east",
    "west",
];

/// Words of map furniture (series, publisher, sheet number) besides `META_WORDS`.
const FURNITURE_WORDS: &[&str] = &["mcsa", "map", "maps", "series", "no", "nr", "page", "of", "club", "mountain"];
/// Publishers' acronyms: dropped from any label.
const PUBLISHER_WORDS: &[&str] = &["mcsa", "usgs", "ngi", "ign", "os"];

/// Runs of letters in a word ("sheet2" → ["sheet"]), lowercase.
fn letter_runs(w: &str) -> Vec<String> {
    w.split(|c: char| !c.is_alphabetic()).filter(|r| !r.is_empty()).map(|r| r.to_lowercase()).collect()
}

/// Can `word` (lowercase letters) be split into words of `dict`?
fn segments_into(word: &str, dict: &[&[&str]]) -> bool {
    let w = word.as_bytes();
    let n = w.len();
    if n == 0 {
        return true;
    }
    let mut ok = vec![false; n + 1];
    ok[0] = true;
    for i in 0..n {
        if !ok[i] {
            continue;
        }
        for d in dict.iter().flat_map(|d| d.iter()) {
            let l = d.len();
            if l > 0 && i + l <= n && &w[i..i + l] == d.as_bytes() {
                ok[i + l] = true;
            }
        }
    }
    ok[n]
}

/// Is a (lowercase) word generic: a legend, furniture or note word, a
/// measure ("5m"), a number, or a run of such words OCR merged
/// ("drywatercourse", "100mintervals", "noentrysign")?
fn generic_word(w: &str) -> bool {
    if w.chars().all(|c| c.is_ascii_digit()) || is_measure(w) {
        return true;
    }
    // A leading measure merged into the word: "5mintervals".
    let lead = w.trim_start_matches(|c: char| c.is_ascii_digit());
    let lead = if lead.len() < w.len() { lead.strip_prefix('m').unwrap_or(lead) } else { lead };
    let dict: [&[&str]; 4] = [LEGEND_WORDS, META_WORDS, NOTE_WORDS, FURNITURE_WORDS];
    // A legend word cut short by the OCR ("interva", "monumen").
    let truncated =
        lead.len() >= 5 && dict.iter().flat_map(|d| d.iter()).any(|d| d.len() > lead.len() && d.starts_with(lead));
    dict.iter().any(|d| d.contains(&lead)) || truncated || (lead.len() >= 6 && segments_into(lead, &dict))
}

/// Font size estimate (px) of an observation: the box's short side, or for
/// a turned label (the box is far squarer than `n` characters of text) its
/// diagonal over the characters (~0.6 em each).
fn font_px(o: &OcrObservation) -> f64 {
    let n = o.text.chars().filter(|c| !c.is_whitespace()).count().max(1) as f64;
    let (w, h) = (o.bbox[2].abs(), o.bbox[3].abs());
    w.min(h).min(w.hypot(h) / (0.6 * n))
}

/// Does the box read as horizontal text (much wider than tall)?
fn horizontal(o: &OcrObservation) -> bool {
    let n = o.text.chars().filter(|c| !c.is_whitespace()).count().max(1) as f64;
    o.bbox[2] >= o.bbox[3] * (0.3 * n).clamp(1.5, 4.0)
}

/// Upper-case letters of a name, the key for spotting the same name read twice.
fn letters_key(s: &str) -> String {
    s.chars().filter(|c| c.is_alphabetic()).flat_map(|c| c.to_uppercase()).collect()
}

/// A word of credits or editions ("2017", "V1"): the label is not a name.
fn credit_token(w: &str) -> bool {
    let year = w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()) && (w.starts_with("19") || w.starts_with("20"));
    let version = w.len() >= 2 && w.starts_with('v') && w[1..].chars().all(|c| c.is_ascii_digit());
    year || version
}

/// The parts of a label that may be names: split at "&", "+", ",", ";", "/",
/// "|", " and " and " - ", parenthesised words dropped.
fn name_parts(text: &str) -> Vec<String> {
    let mut no_paren = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => no_paren.push(c),
            _ => {}
        }
    }
    let spaced = no_paren.replace(" - ", "&").replace(" and ", "&").replace(" AND ", "&").replace(" And ", "&");
    spaced
        .split(['&', '+', ',', ';', '/', '|'])
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|p| !p.is_empty())
        .collect()
}

/// Clean one part into a name to geocode, or None. Furniture lines
/// ("MCSA MAGALIESBERG SHEET 3 OF 10") keep the words that are not
/// furniture, numbers or short acronyms.
fn clean_name(part: &str) -> Option<String> {
    let raw: Vec<&str> = part.split_whitespace().collect();
    let lower: Vec<String> = raw.iter().map(|w| w.to_lowercase()).collect();
    if lower.iter().any(|w| credit_token(w.trim_matches(|c: char| !c.is_alphanumeric()))) {
        return None;
    }
    let furniture = lower.iter().any(|w| letter_runs(w).iter().any(|x| META_WORDS.contains(&x.as_str())));
    let mut kept: Vec<String> = Vec::new();
    for (w, wl) in raw.iter().zip(&lower) {
        // Strip symbols at the ends ("★ZIMBABWE", "AASVOELKRANS★").
        let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'');
        let wl = wl.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'');
        if w.is_empty() {
            continue;
        }
        let letters = w.chars().filter(|c| c.is_alphabetic()).count();
        if letters == 0 || PUBLISHER_WORDS.contains(&wl) {
            continue;
        }
        if furniture {
            let short_caps = letters <= 4 && w.chars().all(|c| !c.is_lowercase());
            let ws = letter_runs(wl);
            if short_caps
                || w.chars().any(|c| c.is_ascii_digit())
                || ws.iter().any(|x| META_WORDS.contains(&x.as_str()) || FURNITURE_WORDS.contains(&x.as_str()))
            {
                continue;
            }
        }
        kept.push(w.to_string());
    }
    // Leading / trailing connectives and articles go.
    let connective = |w: &String| matches!(w.to_lowercase().as_str(), "of" | "the" | "and" | "at" | "de" | "la");
    while kept.first().is_some_and(connective) {
        kept.remove(0);
    }
    while kept.last().is_some_and(connective) {
        kept.pop();
    }
    if kept.is_empty() {
        return None;
    }
    let name = kept.join(" ");
    let words: Vec<String> = kept.iter().flat_map(|w| words_of(w)).collect();
    if words.iter().all(|w| generic_word(w) || w.len() <= 2) {
        return None;
    }
    let letters = name.chars().filter(|c| c.is_alphabetic()).count();
    let others = name.chars().filter(|c| !c.is_whitespace() && !c.is_alphabetic() && *c != '\'').count();
    if letters < 5 || (letters as f64) < 0.85 * (letters + others) as f64 {
        return None;
    }
    // At least one real word (three letters or more).
    if !kept.iter().any(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 3) {
        return None;
    }
    Some(name)
}

/// Names on the map worth geocoding, best first (at most
/// `MAX_HINT_QUERIES`): the title (large horizontal text) and the series
/// name from furniture lines, then place, peak and water names by text size.
/// Legend entries (columns of short labels, or legend and furniture words
/// only, merged or not), scale bars, coordinates, spot heights, credits and
/// route notes are left out; the same name read twice (or a partial read of
/// it in smaller or equal text) is kept once, preferring the reading with spaces.
pub fn hint_queries(obs: &[OcrObservation]) -> Vec<String> {
    let mut merged: Vec<OcrObservation> = Vec::new();
    for o in obs {
        let text = o.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() || !o.bbox.iter().all(|v| v.is_finite()) || o.bbox[2] <= 0.0 || o.bbox[3] <= 0.0 {
            continue;
        }
        merged.push(OcrObservation { text, bbox: o.bbox, confidence: o.confidence });
    }
    if merged.is_empty() {
        return Vec::new();
    }
    let th = median(merged.iter().map(|o| o.text_height()).collect()).unwrap_or(20.0).max(1.0);
    let fonts: Vec<f64> = merged.iter().map(font_px).collect();
    let font_med = median(fonts.clone()).unwrap_or(th).max(1.0);
    let scale_bar = scale_bar_members(&merged, th);
    let legend = legend_members(&merged, th);
    // (name, key, score, font)
    let mut found: Vec<(String, String, f64, f64)> = Vec::new();
    for (i, o) in merged.iter().enumerate() {
        if scale_bar[i] || legend[i] {
            continue;
        }
        let text = o.text.as_str();
        if parse_coordinate(text).is_some()
            || parse_coordinate_pair(text).is_some()
            || parse_scale(text).is_some()
            || has_angle_mark(text)
            || parse_height(text).is_some()
            || text.contains('©')
            || text.to_lowercase().contains("(c)")
        {
            continue;
        }
        let size = (fonts[i] / font_med).clamp(0.3, 4.0);
        let horiz = horizontal(o);
        for part in name_parts(text) {
            let Some(name) = clean_name(&part) else { continue };
            let n_words = name.split_whitespace().count() as f64;
            let title = horiz && size >= 1.6;
            let mut score = size + if title { 1.5 } else { 0.0 } + 0.3 * o.confidence as f64 + 0.15 * n_words.min(3.0);
            if !horiz {
                // Turned labels (ridges, kloofs, boundaries) read poorly.
                score *= 0.6;
            }
            found.push((name.clone(), letters_key(&name), score, fonts[i]));
        }
    }
    // Same name read twice: keep the best score, the spaced reading.
    found.sort_by(|a, b| b.2.total_cmp(&a.2));
    let mut out: Vec<(String, String, f64, f64)> = Vec::new();
    for f in found {
        if let Some(o) = out.iter_mut().find(|o| o.1 == f.1) {
            if f.0.split_whitespace().count() > o.0.split_whitespace().count() {
                o.0 = f.0;
            }
            continue;
        }
        out.push(f);
    }
    // Partial reads: a key inside a longer key read in text at least as
    // large; and glued reads: another name with a short token (an acronym,
    // a symbol read as a letter) stuck to it ("MCSAMAGALIESBERG", "PLONG POOL").
    let keep: Vec<bool> = out
        .iter()
        .map(|a| {
            let partial = out.iter().any(|b| b.1.len() > a.1.len() && b.1.contains(a.1.as_str()) && b.3 >= 0.9 * a.3);
            let glued = out.iter().any(|b| {
                if b.1.len() < 5 || a.1.len() <= b.1.len() {
                    return false;
                }
                let extra = if a.1.starts_with(b.1.as_str()) {
                    &a.1[b.1.len()..]
                } else if a.1.ends_with(b.1.as_str()) {
                    &a.1[..a.1.len() - b.1.len()]
                } else {
                    return false;
                };
                let e = extra.to_lowercase();
                extra.chars().count() == 1 || FURNITURE_WORDS.contains(&e.as_str()) || META_WORDS.contains(&e.as_str())
            });
            !partial && !glued
        })
        .collect();
    out.into_iter().zip(keep).filter(|(_, k)| *k).map(|(o, _)| o.0).take(MAX_HINT_QUERIES).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ob(text: &str, x: f64, y: f64, w: f64, h: f64) -> OcrObservation {
        OcrObservation { text: text.into(), bbox: [x, y, w, h], confidence: 0.9 }
    }

    #[test]
    fn coordinate_pairs_and_names() {
        let (la, lo) = parse_coordinate_pair("25°49'00\"S 27°23'00\"E").unwrap();
        assert!((la - -(25.0 + 49.0 / 60.0)).abs() < 1e-9 && (lo - (27.0 + 23.0 / 60.0)).abs() < 1e-9);
        assert_eq!(parse_coordinate_pair("27°23'00\"E 25°49'00\"S").map(|p| p.0 < 0.0), Some(true));
        assert_eq!(parse_coordinate_pair("25°49'00\"S"), None);
        assert_eq!(dm_text(-25.8167, 27.3833), "25°49'S 27°23'E");
    }

    #[test]
    fn generic_words_and_names() {
        for g in ["drywatercourse", "noentrysign", "trigbeacon", "100mintervals", "policestation", "5m", "parking"] {
            assert!(generic_word(g), "{g}");
        }
        for n in ["grootkloof", "magaliesberg", "hamerkop", "kwami"] {
            assert!(!generic_word(n), "{n}");
        }
        assert_eq!(clean_name("MCSA MAGALIESBERG SHEET 3 OF 10").as_deref(), Some("MAGALIESBERG"));
        assert_eq!(clean_name("SA MAGALIESBERG SHEET Z OF 1O").as_deref(), Some("MAGALIESBERG"));
        assert_eq!(clean_name("GIS V1 Sept 2017- Eras Gous"), None);
        assert_eq!(clean_name("Trig Beacon"), None);
        assert_eq!(clean_name("Non-Perennial River"), None);
        assert_eq!(clean_name("★ZIMBABWE VLAKTE").as_deref(), Some("ZIMBABWE VLAKTE"));
        assert_eq!(name_parts("GROOTKLOOF & HAMERKOP"), vec!["GROOTKLOOF", "HAMERKOP"]);
        assert_eq!(name_parts("CASTLE GORGE WATERFALL (UPPER)"), vec!["CASTLE GORGE WATERFALL"]);
    }

    #[test]
    fn label_hint_from_one_latitude_and_one_longitude() {
        // Grootkloof: 25°49'S labels the line at y = 541, 27°23'E the line
        // at x = 878; at 1.906 m/px the map centre is ~4.2 km SE of there.
        let obs = vec![ob("25°49'00\"S", 116.0, 449.0, 36.0, 185.0), ob("27°23'00\"E", 797.0, 4802.0, 163.0, 31.0)];
        let h = label_hints(&obs, 3507, 4960, (1.43, 2.65));
        assert_eq!(h.len(), 1, "{h:?}");
        let h = &h[0];
        assert_eq!(h.source, HintSource::Label);
        let truth = LatLon::new(-25.8499, 27.4000);
        let d = ground_distance_m(h.latlon(), truth);
        assert!(d < h.radius_m, "{d} m from the centre, radius {}", h.radius_m);
        assert!(h.radius_m < 6000.0, "{}", h.radius_m);
        // Without a scale prior the disc is wider but still holds the centre.
        let h = &label_hints(&obs, 3507, 4960, (0.4, 12.0))[0];
        assert!(ground_distance_m(h.latlon(), truth) < h.radius_m && h.radius_m <= 50_000.0);
        // One axis alone is no position.
        assert!(label_hints(&obs[..1], 3507, 4960, (0.4, 12.0)).is_empty());
    }

    #[test]
    fn one_axis_of_labels_gives_a_band() {
        // Grootkloof: 25°49'S labels the line at y = 541; the centre row is
        // 2480, 1939 px (3.7 km at 1.906 m/px) south of it, at -25.8499.
        let lat = ob("25°49'00\"S", 116.0, 449.0, 36.0, 185.0);
        let lon = ob("27°23'00\"E", 797.0, 4802.0, 163.0, 31.0);
        let b = label_band(std::slice::from_ref(&lat), 3507, 4960, (1.43, 2.65), -25.8).unwrap();
        let (s, n) = b.lat.unwrap();
        assert!(b.lon.is_none() && s < -25.8499 && n > -25.8499, "{b:?}");
        assert!((n - s) * 111.32 < 8.0, "{} km wide", (n - s) * 111.32);
        assert_eq!(b.name, "25°49'S");
        let b = label_band(std::slice::from_ref(&lon), 3507, 4960, (1.43, 2.65), -25.8).unwrap();
        let (wl, el) = b.lon.unwrap();
        assert!(b.lat.is_none() && wl < 27.4 && el > 27.4, "{b:?}");
        assert_eq!(b.name, "27°23'E");
        // Both axes: a point (label_hints), no band; nothing: nothing.
        assert!(label_band(&[lat, lon], 3507, 4960, (1.43, 2.65), -25.8).is_none());
        assert!(label_band(&[], 3507, 4960, (1.43, 2.65), -25.8).is_none());
    }

    #[test]
    fn hints_are_ordered_and_deduplicated() {
        let h = |lat: f64, r: f64, s: HintSource| SearchHint { lat, lon: 27.4, radius_m: r, source: s, name: None };
        let o = order_hints(vec![
            h(-25.0, 15_000.0, HintSource::Place),
            h(-26.0, 15_000.0, HintSource::Label),
            h(-25.01, 5_000.0, HintSource::Place),
            h(-24.0, 15_000.0, HintSource::User),
        ]);
        assert_eq!(
            o.iter().map(|x| x.source).collect::<Vec<_>>(),
            vec![HintSource::User, HintSource::Label, HintSource::Place]
        );
        assert_eq!(o[2].lat, -25.0);
        assert_eq!(HintSource::parse("USER"), HintSource::User);
        assert_eq!(HintSource::parse("geocoder"), HintSource::Place);
    }

    #[test]
    fn queries_skip_legend_and_furniture() {
        let obs = vec![
            ob("GROOTKLOOF & HAMERKOP", 836.0, 4604.0, 974.0, 56.0),
            ob("MCSA MAGALIESBERG SHEET 3 OF 10", 836.0, 4684.0, 1290.0, 56.0),
            ob("KWAMI LODGE", 2855.0, 4488.0, 178.0, 22.0),
            ob("SUGARBUSH HILL COUNTRY COTTAGE", 2640.0, 450.0, 470.0, 26.0),
            ob("Trig Beacon", 251.0, 4310.0, 148.0, 22.0),
            ob("Parking", 251.0, 4345.0, 94.0, 26.0),
            ob("Gate", 251.0, 4381.0, 58.0, 22.0),
            ob("1:25 000", 3132.0, 4665.0, 130.0, 25.0),
            ob("Magnetic Declination 18° West", 2883.0, 4703.0, 380.0, 22.0),
            ob("GIS V1 Sept 2017- Eras Gous", 436.0, 4808.0, 337.0, 26.0),
            ob("1523", 2430.0, 4428.0, 54.0, 20.0),
            ob("25°49'00\"S", 116.0, 449.0, 36.0, 36.0),
        ];
        let q = hint_queries(&obs);
        assert_eq!(&q[..3], &["GROOTKLOOF", "HAMERKOP", "MAGALIESBERG"], "{q:?}");
        assert!(q.contains(&"KWAMI LODGE".to_string()), "{q:?}");
        for bad in ["Trig", "Parking", "Gate", "Declination", "Gous", "1:25", "1523", "25°"] {
            assert!(!q.iter().any(|x| x.contains(bad)), "{bad} in {q:?}");
        }
        assert!(hint_queries(&[]).is_empty());
    }
}
