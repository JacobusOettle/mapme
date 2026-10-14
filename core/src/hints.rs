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
    classify_pois_reads, credit_text, has_angle_mark, imprint_text, is_measure, legend_members, lexicon_word,
    longest_common_run, median, mend_name, parse_coordinate, parse_height, parse_scale, reads_across,
    scale_bar_members, words_of, CoordValue, OcrObservation, PoiKind, Vocabulary, LEGEND_WORDS, META_WORDS, NOTE_WORDS,
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

/// Words of map furniture (series, publisher, sheet number) besides `META_WORDS`.
const FURNITURE_WORDS: &[&str] = &["mcsa", "map", "maps", "series", "no", "nr", "page", "of", "club", "mountain"];
/// Publishers' acronyms: dropped from any label.
const PUBLISHER_WORDS: &[&str] = &["mcsa", "usgs", "ngi", "ign", "os"];
/// Lexicon words that qualify a feature: with one, lexicon words are a place name as often as a
/// description ("LITTLE ROCK", "GREAT FALLS", "NEW FOREST", "GROOT RIVIER"; "LONG POOL").
const QUALIFIER_WORDS: &[&str] = &[
    "long", "short", "big", "little", "great", "middle", "high", "old", "new", "groot", "klein", "rooi", "wit",
    "swart", "blou", "hoog", "nuwe", "saint", "upper", "lower", "north", "south", "east", "west",
];

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
    // Leading / trailing connectives and articles go; "de" and "la" in front are the name's own
    // ("DE AAR", "DE HOOP", "LA PAZ").
    let leading = |w: &String| matches!(w.to_lowercase().as_str(), "of" | "the" | "and" | "at");
    let trailing = |w: &String| leading(w) || matches!(w.to_lowercase().as_str(), "de" | "la");
    while kept.first().is_some_and(leading) {
        kept.remove(0);
    }
    while kept.last().is_some_and(trailing) {
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

/// Does `key` (letters, upper case) occur in `name` starting inside one of its words?
fn inside_a_word(name: &str, key: &str) -> bool {
    let full = letters_key(name);
    let mut starts = vec![0usize];
    for w in name.split_whitespace() {
        let n = letters_key(w).len();
        starts.push(starts.last().copied().unwrap_or(0) + n);
    }
    full.match_indices(key).any(|(i, _)| !starts.contains(&i))
}

/// Lettering this many times the median font size, across the map, is a title.
const TITLE_FONT: f64 = 1.6;

/// Names on the map worth geocoding, best first (at most `MAX_HINT_QUERIES`):
///
/// 1. the title (large lettering across the map) and the series name from furniture lines
///    ("MCSA MAGALIESBERG SHEET 7 OF 10" → "MAGALIESBERG"): first those another read on the
///    sheet confirms (the title "CASTLE GORGE" and the "CASTLE GORGE WATERFALL" on the map),
///    then the others, less a misread of a confirmed one ("DOTKLOOF" beside "GROOTKLOOF");
/// 2. the names of `classify_pois` (place, peak and water names, cleaned of legend entries,
///    pieces and misreads), names of several words first, larger lettering first;
/// 3. names of lexicon words with a qualifier ("LITTLE ROCK", "GREAT FALLS", "LONG POOL"): place
///    names as often as descriptions.
///
/// Route notes, words that only say what a place is ("KRAAL", "TENTED CAMP"), credits,
/// coordinates and heights are left out; the same name read twice (or a partial read of it in
/// smaller or equal text) is kept once, preferring the reading with spaces.
pub fn hint_queries(obs: &[OcrObservation]) -> Vec<String> {
    let mut reads: Vec<OcrObservation> = Vec::new();
    for o in obs {
        let text = o.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() || !o.bbox.iter().all(|v| v.is_finite()) || o.bbox[2] <= 0.0 || o.bbox[3] <= 0.0 {
            continue;
        }
        reads.push(OcrObservation { text, bbox: o.bbox, confidence: o.confidence });
    }
    if reads.is_empty() {
        return Vec::new();
    }
    let th = median(reads.iter().map(|o| o.text_height()).collect()).unwrap_or(20.0).max(1.0);
    let font_med = median(reads.iter().map(font_px).collect()).unwrap_or(th).max(1.0);
    let scale_bar = scale_bar_members(&reads, th);
    let legend = legend_members(&reads, th);
    let vocab = Vocabulary::new(&reads);
    let keys: Vec<String> = reads.iter().map(|o| letters_key(&o.text)).collect();
    #[derive(Clone)]
    struct Query {
        name: String,
        key: String,
        /// 0: confirmed title, 1: other title, 2: name of several words, 3: one word, 4: lexicon
        /// words with a qualifier.
        group: u8,
        score: f64,
        font: f64,
    }
    let mut found: Vec<Query> = Vec::new();
    // Titles and series names.
    let mut titles: Vec<(Query, bool)> = Vec::new();
    for (i, o) in reads.iter().enumerate() {
        let text = o.text.as_str();
        if scale_bar[i]
            || legend[i]
            || !reads_across(o)
            || parse_coordinate(text).is_some()
            || parse_coordinate_pair(text).is_some()
            || parse_scale(text).is_some()
            || has_angle_mark(text)
            || parse_height(text).is_some()
            || text.contains('©')
            || text.to_lowercase().contains("(c)")
            || imprint_text(text)
            || credit_text(text)
        {
            continue;
        }
        let size = font_px(o) / font_med;
        let series = words_of(text).iter().any(|w| letter_runs(w).iter().any(|x| META_WORDS.contains(&x.as_str())));
        if size < TITLE_FONT && !series {
            continue;
        }
        for part in name_parts(text) {
            let Some(name) = clean_name(&part).map(|n| mend_name(&n, &vocab, &[])) else { continue };
            let key = letters_key(&name);
            let confirmed = keys.iter().enumerate().any(|(j, k)| j != i && k.contains(key.as_str()));
            let score = size.clamp(0.3, 4.0) + 0.3 * o.confidence as f64;
            titles
                .push((Query { name, key, group: if confirmed { 0 } else { 1 }, score, font: font_px(o) }, confirmed));
        }
    }
    for (t, confirmed) in &titles {
        // An unconfirmed title that shares most of its letters with a confirmed one is a misread of
        // it ("DOTKLOOF" of "GROOTKLOOF"), when what it has besides is no known word ("KLEIN
        // WINTERHOEK" beside "GROOT WINTERHOEK", "DRAKENSBERG SOUTH" beside "DRAKENSBERG NORTH" are
        // two places).
        let misread = !confirmed
            && titles.iter().any(|(c, ok)| {
                *ok && c.key != t.key && {
                    let (a, b): (Vec<char>, Vec<char>) = (t.key.chars().collect(), c.key.chars().collect());
                    let theirs = words_of(&c.name);
                    longest_common_run(&a, &b) as f64 >= 0.6 * a.len() as f64
                        && words_of(&t.name).iter().any(|w| !theirs.contains(w) && !vocab.known(w))
                }
            });
        if !misread {
            found.push(t.clone());
        }
    }
    // Names on the map.
    for (p, o) in classify_pois_reads(obs) {
        if p.kind == PoiKind::SpotHeight {
            continue;
        }
        let size = (font_px(&o) / font_med).clamp(0.3, 4.0);
        for part in name_parts(&p.name) {
            let Some(name) = clean_name(&part) else { continue };
            let n_words = name.split_whitespace().count();
            // Words that only say what a place is ("KRAAL", "FARM GATE") find no place; with a
            // qualifier they may ("LITTLE ROCK"), asked last.
            let words = words_of(&name);
            let mut group = if n_words >= 2 { 2 } else { 3 };
            if words.iter().all(|w| w.chars().count() <= 2 || lexicon_word(w)) {
                let qualified = words.iter().any(|w| QUALIFIER_WORDS.contains(&w.as_str()))
                    && words.iter().any(|w| w.chars().count() > 2 && !QUALIFIER_WORDS.contains(&w.as_str()));
                if !qualified {
                    continue;
                }
                group = 4;
            }
            let mut score = size + 0.3 * o.confidence as f64;
            if !reads_across(&o) {
                // Turned labels (ridges, kloofs, boundaries) read poorly.
                score *= 0.6;
            }
            let key = letters_key(&name);
            found.push(Query { name, key, group, score, font: font_px(&o) });
        }
    }
    // Best first; the same name read twice: keep the best, the spaced reading.
    found.sort_by(|a, b| a.group.cmp(&b.group).then(b.score.total_cmp(&a.score)));
    let mut out: Vec<Query> = Vec::new();
    for f in found {
        if let Some(o) = out.iter_mut().find(|o| o.key == f.key) {
            if f.name.split_whitespace().count() > o.name.split_whitespace().count() {
                o.name = f.name;
            }
            o.font = o.font.max(f.font);
            continue;
        }
        out.push(f);
    }
    // Glued words with their spacing from another read of the same letters ("CASTLEGORGE" and
    // "CASTLE GORGE").
    for q in out.iter_mut() {
        let n = q.name.split_whitespace().count();
        let spaced = reads.iter().find_map(|r| {
            // Words only: no number or symbol joins the name ("RAND WATER BOX 2").
            let toks: Vec<&str> = r
                .text
                .split_whitespace()
                .map(|t| t.trim_matches(|c: char| !c.is_alphabetic()))
                .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_alphabetic() || "'-".contains(c)))
                .collect();
            (n + 1..=toks.len().min(6))
                .find_map(|w| toks.windows(w).find(|win| letters_key(&win.join(" ")) == q.key).map(|win| win.join(" ")))
        });
        if let Some(t) = spaced {
            q.name = t;
        }
    }
    // Partial reads: a key inside a longer key read in text at least as
    // large, or inside a word of a name ranked as high ("ABATINI" of
    // "MHLABATINI"); and glued reads: another name with a short token (an
    // acronym, a symbol read as a letter) stuck to it ("MCSAMAGALIESBERG",
    // "PLONG POOL").
    let keep: Vec<bool> = out
        .iter()
        .map(|a| {
            let partial = out.iter().any(|b| {
                b.key.len() > a.key.len()
                    && b.key.contains(a.key.as_str())
                    && (b.font >= 0.9 * a.font || (b.group <= a.group && inside_a_word(&b.name, &a.key)))
            });
            let glued = out.iter().any(|b| {
                if b.key.len() < 5 || a.key.len() <= b.key.len() {
                    return false;
                }
                let extra = if a.key.starts_with(b.key.as_str()) {
                    &a.key[b.key.len()..]
                } else if a.key.ends_with(b.key.as_str()) {
                    &a.key[..a.key.len() - b.key.len()]
                } else {
                    return false;
                };
                let e = extra.to_lowercase();
                extra.chars().count() == 1 || FURNITURE_WORDS.contains(&e.as_str()) || META_WORDS.contains(&e.as_str())
            });
            !partial && !glued
        })
        .collect();
    out.into_iter().zip(keep).filter(|(_, k)| *k).map(|(o, _)| o.name).take(MAX_HINT_QUERIES).collect()
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
        // An article in front belongs to the name; connectives at the ends go.
        for n in ["DE AAR", "DE HOOP", "LA PAZ", "DE DOORNS"] {
            assert_eq!(clean_name(n).as_deref(), Some(n), "{n}");
        }
        assert_eq!(clean_name("OF THE GROOTKLOOF DE").as_deref(), Some("GROOTKLOOF"));
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
    fn titles_the_map_confirms_come_first() {
        // Grootkloof as Apple Vision reads it: the title twice (once misread), the series line,
        // a partial read of the declination note, names.
        let obs = vec![
            ob("DOTKLOOF & HAMERKOP..", 954.0, 4595.0, 884.0, 80.0),
            ob("GROOTKLOOF & HAME", 833.0, 4603.0, 765.0, 67.0),
            ob("MCSA MAGALIESBERG", 827.0, 4683.0, 767.0, 67.0),
            ob("OF 10", 1931.0, 4685.0, 193.0, 55.0),
            ob("GROOTKLOOF PEDESTRIAN GATE", 1858.0, 1562.0, 416.0, 30.0),
            ob("HAMERKOP KLOOF", 2858.0, 2968.0, 58.0, 249.0),
            ob("KWAMI LODGE", 2877.0, 4481.0, 175.0, 33.0),
            ob("KRAAL", 2133.0, 2340.0, 86.0, 26.0),
            ob("BARTLETT/FARM GATE", 1745.0, 1519.0, 281.0, 28.0),
            ob("ual Change: 4.8 per year West", 2877.0, 4735.0, 387.0, 26.0),
            ob("Annual Change: 4.8' per year West", 2829.0, 4733.0, 435.0, 28.0),
            ob("1523", 2430.0, 4428.0, 54.0, 20.0),
        ];
        let q = hint_queries(&obs);
        assert_eq!(&q[..3], &["HAMERKOP", "GROOTKLOOF", "MAGALIESBERG"], "{q:?}");
        for n in ["GROOTKLOOF PEDESTRIAN GATE", "HAMERKOP KLOOF", "KWAMI LODGE", "BARTLETT"] {
            assert!(q.contains(&n.to_string()), "{n}: {q:?}");
        }
        for bad in ["DOTKLOOF", "HAME", "KRAAL", "FARM GATE", "ual Change per year West", "Change", "1523"] {
            assert!(!q.contains(&bad.to_string()), "{bad}: {q:?}");
        }
        // A title read without its space takes it from another read of the same words; a title
        // read without its first letters is a piece of the one with them.
        let mut obs: Vec<OcrObservation> = ["1416", "1754", "1556", "1471", "1533", "1514", "1497", "1503"]
            .iter()
            .enumerate()
            .map(|(k, h)| ob(h, 400.0 + 300.0 * k as f64, 3400.0, 58.0, 22.0))
            .collect();
        obs.extend([
            ob("CASTLEGORGE", 836.0, 4604.0, 540.0, 58.0),
            ob("CASTLE GORGE", 421.0, 2100.0, 199.0, 32.0),
            ob("CASTLEGORGEWATERFALL(UPPER)", 423.0, 2104.0, 466.0, 27.0),
            ob("ABATINI & FERN KLOOF", 978.0, 4598.0, 834.0, 77.0),
            ob("MHLABATINI & FERN K", 836.0, 4610.0, 758.0, 49.0),
            ob("MHLABATINI PARKING", 1398.0, 1042.0, 277.0, 28.0),
            ob("FERN KLOOF", 387.0, 1176.0, 105.0, 194.0),
        ]);
        let q = hint_queries(&obs);
        assert_eq!(&q[..3], &["FERN KLOOF", "CASTLE GORGE", "MHLABATINI"], "{q:?}");
        assert!(!q.iter().any(|x| x == "ABATINI" || x == "FERN K"), "{q:?}");
        assert!(inside_a_word("MHLABATINI", "ABATINI") && !inside_a_word("CASTLE GORGE WATERFALL", "CASTLEGORGE"));
    }

    #[test]
    fn qualified_names_and_twin_titles_are_queries() {
        // A qualifier and a feature is a place name as often as a description ("LITTLE ROCK",
        // "GREAT FALLS", "NEW FOREST", "GROOT RIVIER"): asked after the other names, not left
        // out. What a place is ("KRAAL", "FARM GATE") and route notes ("KEEP RIGHT") stay out.
        let mut obs: Vec<OcrObservation> = ["1416", "1754", "1556", "1471", "1533"]
            .iter()
            .enumerate()
            .map(|(k, h)| ob(h, 3300.0, 300.0 + 300.0 * k as f64, 60.0, 24.0))
            .collect();
        let across = |t: &str, x: f64, y: f64| ob(t, x, y, 15.0 * t.chars().count() as f64, 24.0);
        obs.extend([
            across("LITTLE ROCK", 300.0, 300.0),
            across("GREAT FALLS", 300.0, 900.0),
            across("NEW FOREST", 300.0, 1500.0),
            across("GROOT RIVIER", 300.0, 2100.0),
            across("KRAAL", 1200.0, 300.0),
            across("FARM GATE", 1200.0, 900.0),
            across("KEEP RIGHT", 1200.0, 1500.0),
            across("HLOBANE", 1200.0, 2100.0),
        ]);
        let q = hint_queries(&obs);
        assert_eq!(q, vec!["HLOBANE", "LITTLE ROCK", "GREAT FALLS", "NEW FOREST", "GROOT RIVIER"]);
        // Two titles alike: "KLEIN WINTERHOEK" is no misread of "GROOT WINTERHOEK" (the one on the
        // map), "DRAKENSBERG SOUTH" none of "DRAKENSBERG NORTH" (but "DOTKLOOF" is one of
        // "GROOTKLOOF": `titles_the_map_confirms_come_first`).
        let mut obs: Vec<OcrObservation> = ["1416", "1754", "1556", "1471", "1533"]
            .iter()
            .enumerate()
            .map(|(k, h)| ob(h, 3300.0, 300.0 + 300.0 * k as f64, 60.0, 24.0))
            .collect();
        obs.extend([
            ob("GROOT WINTERHOEK & KLEIN WINTERHOEK", 800.0, 4600.0, 1600.0, 70.0),
            across("GROOT WINTERHOEK", 900.0, 900.0),
            ob("DRAKENSBERG NORTH & DRAKENSBERG SOUTH", 300.0, 4000.0, 1700.0, 70.0),
            across("DRAKENSBERG NORTH", 1500.0, 1500.0),
        ]);
        let q = hint_queries(&obs);
        assert_eq!(
            &q[..4],
            &["GROOT WINTERHOEK", "DRAKENSBERG NORTH", "KLEIN WINTERHOEK", "DRAKENSBERG SOUTH"],
            "{q:?}"
        );
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
