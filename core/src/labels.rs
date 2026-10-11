//! OCR text parsing: coordinate labels, map scale, POI names.
//!
//! OWNER: grid agent. Contract (may be extended, not broken):

use serde::{Deserialize, Serialize};

/// One text observation from the platform OCR (Apple Vision / ML Kit).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrObservation {
    pub text: String,
    /// [x, y, w, h] in base-image pixels, top-left origin.
    pub bbox: [f64; 4],
    #[serde(default = "default_conf")]
    pub confidence: f32,
}

fn default_conf() -> f32 {
    0.5
}

impl OcrObservation {
    pub fn center(&self) -> (f64, f64) {
        (self.bbox[0] + self.bbox[2] / 2.0, self.bbox[1] + self.bbox[3] / 2.0)
    }

    /// Text height in pixels: the short side of the box (vertical text has w < h).
    pub fn text_height(&self) -> f64 {
        self.bbox[2].min(self.bbox[3]).max(0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CoordValue {
    /// Latitude in signed degrees (south negative).
    Lat(f64),
    /// Longitude in signed degrees (west negative).
    Lon(f64),
    /// Ambiguous degrees with no hemisphere letter.
    Degrees(f64),
    /// Projected grid value in metres (easting or northing, unknown which).
    Grid(f64),
}

const DEG: char = '°';
const MIN: char = '\'';
const SEC: char = '"';

/// Map the many glyphs OCR produces for degree / minute / second marks, dashes
/// and spaces to one canonical character each.
fn canon_char(c: char) -> char {
    match c {
        '°' | 'º' | '˚' | '*' | '∘' | 'ᵒ' | '◦' => DEG,
        '\'' | '′' | '’' | '‘' | '`' | '´' | 'ʹ' | 'ʼ' | '‛' => MIN,
        '"' | '″' | '”' | '“' | '＂' | 'ʺ' | '˝' | '〃' | '„' => SEC,
        '−' | '–' | '—' | '‐' | '‑' | '‒' => '-',
        '∶' => ':',
        '⁰' => '0',
        '¹' => '1',
        '²' => '2',
        '³' => '3',
        '⁴' => '4',
        '⁵' => '5',
        '⁶' => '6',
        '⁷' => '7',
        '⁸' => '8',
        '⁹' => '9',
        c if c.is_whitespace() => ' ',
        c => c,
    }
}

fn normalize(text: &str) -> Vec<char> {
    let mut out: Vec<char> = Vec::with_capacity(text.len());
    for c in text.chars().map(canon_char) {
        if c == MIN && out.last() == Some(&MIN) {
            // Two primes are a double prime.
            out.pop();
            out.push(SEC);
        } else {
            out.push(c);
        }
    }
    out
}

/// OCR reads 0 as O/o inside numbers. Replace O/o next to a digit (cascading
/// through runs like "0OO").
fn o_to_zero(chars: &[char]) -> Vec<char> {
    let mut out = chars.to_vec();
    loop {
        let mut changed = false;
        for i in 0..out.len() {
            if matches!(out[i], 'O' | 'o') {
                let prev = i > 0 && out[i - 1].is_ascii_digit();
                let next = i + 1 < out.len() && out[i + 1].is_ascii_digit();
                if prev || next {
                    out[i] = '0';
                    changed = true;
                }
            }
        }
        if !changed {
            return out;
        }
    }
}

/// Alternative reading: a superscript-like "o" after a number is a degree sign.
fn o_to_degree(chars: &[char]) -> Vec<char> {
    let mut out = chars.to_vec();
    for i in 1..out.len() {
        if matches!(out[i], 'O' | 'o') && out[i - 1].is_ascii_digit() {
            out[i] = DEG;
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num { int: String, frac: Option<String> },
    Deg,
    Min,
    Sec,
    Word(String),
    Minus,
    Plus,
    Colon,
    Punct,
}

fn tokenize(chars: &[char]) -> Vec<Tok> {
    let n = chars.len();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c.is_ascii_digit() {
            let mut int = String::new();
            while i < n && chars[i].is_ascii_digit() {
                int.push(chars[i]);
                i += 1;
            }
            let mut frac = None;
            loop {
                if i + 1 < n && (chars[i] == '.' || chars[i] == ',') && chars[i + 1].is_ascii_digit() {
                    let mut j = i + 1;
                    while j < n && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                    let digits: String = chars[i + 1..j].iter().collect();
                    if chars[i] == ',' && digits.len() == 3 && frac.is_none() {
                        // Thousands separator.
                        int.push_str(&digits);
                        i = j;
                        continue;
                    }
                    if frac.is_none() {
                        frac = Some(digits);
                        i = j;
                    }
                }
                break;
            }
            toks.push(Tok::Num { int, frac });
        } else if c.is_alphabetic() {
            let mut w = String::new();
            while i < n && chars[i].is_alphabetic() {
                w.push(chars[i]);
                i += 1;
            }
            toks.push(Tok::Word(w));
        } else {
            match c {
                DEG => toks.push(Tok::Deg),
                MIN => toks.push(Tok::Min),
                SEC => toks.push(Tok::Sec),
                '-' => toks.push(Tok::Minus),
                '+' => toks.push(Tok::Plus),
                ':' => toks.push(Tok::Colon),
                ' ' => {}
                _ => toks.push(Tok::Punct),
            }
            i += 1;
        }
    }
    toks
}

fn hemisphere(w: &str) -> Option<char> {
    match w {
        "N" | "n" => Some('N'),
        "S" | "s" => Some('S'),
        "E" | "e" => Some('E'),
        "W" | "w" => Some('W'),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct Comp {
    int: String,
    frac: Option<String>,
    /// 0 = degrees, 1 = minutes, 2 = seconds (from an explicit symbol).
    sym: Option<u8>,
}

impl Comp {
    fn value(&self) -> f64 {
        let mut s = self.int.clone();
        if let Some(f) = &self.frac {
            s.push('.');
            s.push_str(f);
        }
        s.parse().unwrap_or(f64::NAN)
    }

    fn int_value(&self) -> u64 {
        self.int.parse().unwrap_or(u64::MAX)
    }
}

/// Degrees + minutes + seconds from components with (some) explicit symbols.
fn dms_value(comps: &[Comp]) -> Option<f64> {
    let mut parts: [Option<f64>; 3] = [None; 3];
    let mut digits: [usize; 3] = [0; 3];
    let mut prev: i32 = -1;
    let last = comps.len() - 1;
    for (k, c) in comps.iter().enumerate() {
        if c.frac.is_some() && k != last {
            return None;
        }
        let role = c.sym.map_or(prev + 1, |r| r as i32);
        let frac = c.frac.as_ref().map_or(0.0, |f| format!("0.{f}").parse::<f64>().unwrap_or(0.0));
        // Lost degree sign: "2549'" → 25° 49'.
        if k == 0 && role == 1 && (3..=5).contains(&c.int.len()) {
            let n = c.int_value();
            parts[0] = Some((n / 100) as f64);
            parts[1] = Some((n % 100) as f64 + frac);
            digits[0] = c.int.len() - 2;
            digits[1] = 2;
            prev = 1;
            continue;
        }
        // Lost minute sign: "25°4900\"" → 25° 49' 00".
        if prev == 0 && c.int.len() == 4 && (role == 2 || (c.sym.is_none() && k == last)) {
            let n = c.int_value();
            parts[1] = Some((n / 100) as f64);
            parts[2] = Some((n % 100) as f64 + frac);
            digits[1] = 2;
            digits[2] = 2;
            prev = 2;
            continue;
        }
        if role != prev + 1 || role > 2 {
            return None;
        }
        parts[role as usize] = Some(c.value());
        digits[role as usize] = c.int.len();
        prev = role;
    }
    let d = parts[0]?;
    if digits[0] > 3 || digits[1] > 2 || digits[2] > 2 {
        return None;
    }
    let m = parts[1].unwrap_or(0.0);
    let s = parts[2].unwrap_or(0.0);
    if !(m < 60.0 && s < 60.0 && d.is_finite() && m.is_finite() && s.is_finite()) {
        return None;
    }
    Some(d + m / 60.0 + s / 3600.0)
}

fn parse_tokens(toks: &[Tok]) -> Option<CoordValue> {
    if toks.contains(&Tok::Colon) {
        return None;
    }
    let mut i = 0;
    while matches!(toks.get(i), Some(Tok::Punct)) {
        i += 1;
    }
    let mut hemi: Option<char> = None;
    let mut prefix_hemi = false;
    if let Some(Tok::Word(w)) = toks.get(i) {
        hemi = Some(hemisphere(w)?);
        prefix_hemi = true;
        i += 1;
        while matches!(toks.get(i), Some(Tok::Punct)) {
            i += 1;
        }
    }
    let mut neg = false;
    match toks.get(i) {
        Some(Tok::Minus) => {
            neg = true;
            i += 1;
        }
        Some(Tok::Plus) => i += 1,
        _ => {}
    }
    let mut comps: Vec<Comp> = Vec::new();
    while let Some(t) = toks.get(i) {
        match t {
            Tok::Num { int, frac } => comps.push(Comp { int: int.clone(), frac: frac.clone(), sym: None }),
            Tok::Deg | Tok::Min | Tok::Sec => {
                let r = match t {
                    Tok::Deg => 0,
                    Tok::Min => 1,
                    _ => 2,
                };
                match comps.last_mut() {
                    Some(c) if c.sym.is_none() => c.sym = Some(r),
                    _ => break,
                }
            }
            _ => break,
        }
        i += 1;
    }
    if comps.is_empty() {
        return None;
    }
    // Suffix: hemisphere letter and/or a metres marker ("mE", "mN", "m").
    let mut metres = false;
    while let Some(Tok::Word(w)) = toks.get(i) {
        let h = match w.as_str() {
            "m" | "M" => {
                metres = true;
                None
            }
            "mE" | "ME" | "mN" | "MN" => {
                metres = true;
                hemisphere(&w[1..])
            }
            _ => match hemisphere(w) {
                Some(h) => Some(h),
                None => break,
            },
        };
        if let Some(h) = h {
            if hemi.is_some() && hemi != Some(h) {
                return None;
            }
            hemi = Some(h);
        }
        i += 1;
    }
    // Trailing junk: punctuation or a stray letter is fine, more numbers or words are not.
    for t in &toks[i..] {
        match t {
            Tok::Num { .. } | Tok::Minus | Tok::Plus => return None,
            Tok::Word(w) if w.chars().count() >= 2 || hemisphere(w).is_some() => return None,
            _ => {}
        }
    }

    let has_sym = comps.iter().any(|c| c.sym.is_some());
    let mut grid: Option<f64> = None;
    let value: f64;
    if has_sym {
        if metres {
            return None;
        }
        value = dms_value(&comps)?;
    } else if comps.len() >= 2 {
        let thousands = comps.iter().all(|c| c.frac.is_none())
            && (1..=3).contains(&comps[0].int.len())
            && comps[1..].iter().all(|c| c.int.len() == 3);
        if thousands {
            let s: String = comps.iter().map(|c| c.int.as_str()).collect();
            grid = Some(s.parse().ok()?);
            value = 0.0;
        } else {
            // "25 49 00 S": needs a hemisphere letter to be read as DMS.
            if hemi.is_none() || comps.len() > 3 || comps[1..].iter().any(|c| c.int.len() > 2) {
                return None;
            }
            value = dms_value(&comps)?;
        }
    } else {
        let c = &comps[0];
        if c.frac.is_some() {
            if metres || (hemi.is_none() && c.frac.as_ref().map_or(0, |f| f.len()) < 2) {
                return None;
            }
            value = c.value();
        } else if metres || c.int.len() >= 5 {
            if c.int.len() < 5 {
                return None;
            }
            grid = Some(c.int_value() as f64);
            value = 0.0;
        } else if hemi.is_some() && !prefix_hemi {
            let n = c.int_value();
            value = if c.int.len() >= 4 {
                // DDMM / DDDMM with the symbols lost.
                if n % 100 >= 60 {
                    return None;
                }
                (n / 100) as f64 + (n % 100) as f64 / 60.0
            } else {
                n as f64
            };
        } else {
            return None;
        }
    }

    if let Some(g) = grid {
        if neg || matches!(hemi, Some('S') | Some('W')) || !(10_000.0..=20_000_000.0).contains(&g) {
            return None;
        }
        return Some(CoordValue::Grid(g));
    }
    if !value.is_finite() {
        return None;
    }
    match hemi {
        Some('N') if value <= 90.0 => Some(CoordValue::Lat(value)),
        Some('S') if value <= 90.0 => Some(CoordValue::Lat(-value)),
        Some('E') if value <= 180.0 => Some(CoordValue::Lon(value)),
        Some('W') if value <= 180.0 => Some(CoordValue::Lon(-value)),
        None if value <= 180.0 => Some(CoordValue::Degrees(if neg { -value } else { value })),
        _ => None,
    }
}

/// Parse a coordinate label: DMS (25°49'00"S, 25 49 00 S, 25°49′S, OCR
/// variants with *, º, ’, ”), decimal degrees (-25.8167, 25.8167°S) and
/// projected grid values (540000, 540 000mE). Plain 2–4 digit numbers (spot
/// heights, abbreviated grid labels) are not coordinates here; `grid.rs`
/// resolves abbreviated grid labels against detected lines.
pub fn parse_coordinate(text: &str) -> Option<CoordValue> {
    let norm = normalize(text);
    if norm.iter().any(|&c| c == 'o' || c == 'O') {
        let zeroed = o_to_zero(&norm);
        return parse_tokens(&tokenize(&zeroed))
            .or_else(|| parse_tokens(&tokenize(&o_to_degree(&norm))))
            .or_else(|| parse_embedded_dms(&zeroed));
    }
    parse_tokens(&tokenize(&norm)).or_else(|| parse_embedded_dms(&norm))
}

/// Fallback for OCR lines where a DMS label is merged with other text
/// ("Gous 27°23'00\"E", "I 25°50'00\"S") or its marks are confused
/// ("25'53'00\"S", "25°53°00\"S"). Requires a hemisphere letter so stray
/// numbers never qualify.
fn parse_embedded_dms(c: &[char]) -> Option<CoordValue> {
    let n = c.len();
    let is_mark = |ch: char| ch == DEG || ch == MIN || ch == SEC;
    // Skip spaces and at most `marks` mark characters.
    let skip = |mut k: usize, marks: usize| -> usize {
        let mut seen = 0;
        while k < n && (c[k] == ' ' || (is_mark(c[k]) && seen < marks)) {
            if c[k] != ' ' {
                seen += 1;
            }
            k += 1;
        }
        k
    };
    let two_digits = |k: usize| -> Option<f64> {
        if k + 2 <= n
            && c[k].is_ascii_digit()
            && c[k + 1].is_ascii_digit()
            && (k + 2 == n || !c[k + 2].is_ascii_digit())
        {
            Some(((c[k] as u8 - b'0') * 10 + (c[k + 1] as u8 - b'0')) as f64)
        } else {
            None
        }
    };
    let mut found: Option<CoordValue> = None;
    for start in 0..n {
        if !c[start].is_ascii_digit() || (start > 0 && c[start - 1].is_ascii_digit()) {
            continue;
        }
        // Degrees never follow a mark: "25°75'00\"S" must not read as 75°00'S.
        let mut p = start;
        while p > 0 && c[p - 1] == ' ' {
            p -= 1;
        }
        if p > 0 && (is_mark(c[p - 1]) || c[p - 1] == '.') {
            continue;
        }
        let mut j = start;
        while j < n && c[j].is_ascii_digit() {
            j += 1;
        }
        let run: String = c[start..j].iter().collect();
        let (deg, minutes, mut k) = match run.len() {
            // Degrees and minutes run together: "2553'00\"S".
            4 => (run[..2].parse::<f64>().ok()?, run[2..].parse::<f64>().ok()?, j),
            1..=3 => {
                let d: f64 = run.parse().ok()?;
                let k = skip(j, 1);
                match two_digits(k) {
                    Some(m) => (d, m, k + 2),
                    None => continue,
                }
            }
            _ => continue,
        };
        let mut seconds = 0.0;
        let after_min = skip(k, 1);
        if let Some(sec) = two_digits(after_min) {
            seconds = sec;
            k = after_min + 2;
            if k + 1 < n && c[k] == '.' && c[k + 1].is_ascii_digit() {
                k += 1;
                let mut frac = String::from("0.");
                while k < n && c[k].is_ascii_digit() {
                    frac.push(c[k]);
                    k += 1;
                }
                seconds += frac.parse::<f64>().unwrap_or(0.0);
            }
        }
        let before_hemi = k;
        k = skip(k, 2);
        if k >= n || (k + 1 < n && c[k + 1].is_ascii_alphabetic()) {
            continue;
        }
        let marked = (before_hemi..k).any(|i| is_mark(c[i]));
        let hemi = match c[k] {
            '5' if marked => 'S',
            h @ ('N' | 'S' | 'E' | 'W') => h,
            _ => continue,
        };
        if minutes >= 60.0 || seconds >= 60.0 {
            continue;
        }
        let v = deg + minutes / 60.0 + seconds / 3600.0;
        let value = match hemi {
            'N' | 'S' if v <= 90.0 => CoordValue::Lat(if hemi == 'S' { -v } else { v }),
            'E' | 'W' if v <= 180.0 => CoordValue::Lon(if hemi == 'W' { -v } else { v }),
            _ => continue,
        };
        if found.is_some() {
            // Two coordinates in one observation: ambiguous which one sits where.
            return None;
        }
        found = Some(value);
    }
    found
}

/// Parse a representative fraction like "1:25 000" / "1 : 50,000" → 25000.
pub fn parse_scale(text: &str) -> Option<f64> {
    let c: Vec<char> = text.chars().map(canon_char).collect();
    let n = c.len();
    for i in 0..n {
        if c[i] != '1' || (i > 0 && (c[i - 1].is_ascii_digit() || c[i - 1] == '.' || c[i - 1] == ',')) {
            continue;
        }
        let mut j = i + 1;
        while j < n && c[j] == ' ' {
            j += 1;
        }
        if j >= n || !(c[j] == ':' || c[j] == ';') {
            continue;
        }
        j += 1;
        while j < n && c[j] == ' ' {
            j += 1;
        }
        let mut digits = String::new();
        while j < n && c[j].is_ascii_digit() {
            digits.push(c[j]);
            j += 1;
        }
        if digits.is_empty() {
            continue;
        }
        // Thousands groups: separator + exactly three digits.
        loop {
            if j < n && matches!(c[j], ' ' | ',' | '.' | MIN) {
                let k = j + 1;
                if k + 3 <= n
                    && c[k..k + 3].iter().all(|d| d.is_ascii_digit())
                    && (k + 3 == n || !c[k + 3].is_ascii_digit())
                {
                    digits.extend(c[k..k + 3].iter());
                    j = k + 3;
                    continue;
                }
            }
            break;
        }
        if let Ok(d) = digits.parse::<f64>() {
            if (500.0..=10_000_000.0).contains(&d) {
                return Some(d);
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoiKind {
    Place,
    Peak,
    SpotHeight,
    Water,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoiLabel {
    pub name: String,
    pub kind: PoiKind,
    pub elevation_m: Option<f64>,
    /// Base-image pixel position.
    pub x: f64,
    pub y: f64,
}

/// Words that mark map furniture (legend, imprint, projection notes): a label
/// containing any of them is never a POI.
const META_WORDS: &[&str] = &[
    "legend",
    "scale",
    "declination",
    "magnetic",
    "annual",
    "sheet",
    "copyright",
    "gis",
    "datum",
    "projection",
    "spheroid",
    "ellipsoid",
    "wgs",
    "hartebeesthoek",
    "utm",
    "edition",
    "printed",
    "published",
    "compiled",
    "reproduced",
    "version",
    "contour",
    "contours",
    "interval",
    "intervals",
    "metres",
    "meters",
    "kilometres",
    "kilometers",
    "grid",
    "yards",
    "feet",
    "miles",
    "equidistance",
    "isohypse",
];

/// Generic feature words used in legends. A label made only of these (and
/// connectives) is legend text, e.g. "Trig Beacon", "Non-Perennial River".
const LEGEND_WORDS: &[&str] = &[
    "trig",
    "beacon",
    "parking",
    "park",
    "gate",
    "poi",
    "resort",
    "access",
    "camp",
    "camping",
    "campsite",
    "cave",
    "hill",
    "pool",
    "waterfall",
    "spring",
    "river",
    "perennial",
    "non",
    "dry",
    "water",
    "course",
    "watercourse",
    "building",
    "buildings",
    "ruin",
    "ruins",
    "clinic",
    "hospital",
    "police",
    "station",
    "grave",
    "graves",
    "cemetery",
    "monument",
    "place",
    "worship",
    "church",
    "vegetation",
    "road",
    "roads",
    "track",
    "tracks",
    "path",
    "paths",
    "footpath",
    "trail",
    "route",
    "fence",
    "boundary",
    "power",
    "line",
    "lines",
    "main",
    "secondary",
    "minor",
    "major",
    "tarred",
    "gravel",
    "dirt",
    "national",
    "hut",
    "shelter",
    "viewpoint",
    "view",
    "point",
    "picnic",
    "site",
    "toilet",
    "toilets",
    "information",
    "office",
    "school",
    "mine",
    "quarry",
    "dam",
    "lake",
    "marsh",
    "swamp",
    "forest",
    "bridge",
    "railway",
    "airfield",
    "peak",
    "summit",
    "spot",
    "height",
    "heights",
    "elevation",
    "of",
    "the",
    "and",
    "or",
    "with",
    "m",
    "km",
];

const PEAK_WORDS: &[&str] = &["peak", "kop", "berg", "hill", "mount", "mt", "piek", "spitz", "kopje", "summit"];
const WATER_WORDS: &[&str] =
    &["falls", "waterfall", "spring", "pool", "dam", "lake", "river", "rivier", "fontein", "creek", "stream", "spruit"];
/// Suffixes of compound (Afrikaans/Dutch/German) names, e.g. "Spitzkop", "Sterkfontein".
const PEAK_SUFFIXES: &[&str] = &["kop", "kopje", "berg", "piek", "spitz"];
const WATER_SUFFIXES: &[&str] = &["fontein", "rivier", "spruit", "waterval"];

fn words_of(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.to_lowercase()).collect()
}

/// "1428", "1428m", "1428.5 m" → elevation.
fn parse_height(text: &str) -> Option<f64> {
    let t = text.trim();
    let t = t.strip_suffix('m').or_else(|| t.strip_suffix('M')).unwrap_or(t).trim_end();
    let (int, frac) = match t.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (t, None),
    };
    if !(3..=4).contains(&int.len()) || !int.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if let Some(f) = frac {
        if f.is_empty() || f.len() > 1 || !f.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
    }
    let v: f64 = t.parse().ok()?;
    let iv = int.parse::<i64>().ok()?;
    if !(-400.0..=8850.0).contains(&v) || iv % 50 == 0 {
        return None;
    }
    Some(v)
}

const SCALE_UNITS: &[&str] =
    &["kilometres", "kilometers", "metres", "meters", "miles", "yards", "feet", "yds", "km", "mi", "ft", "yd", "m"];

/// Plain number with an optional distance unit ("250", "1250 m", "2 km").
fn scale_bar_number(text: &str) -> Option<f64> {
    let t = text.trim().to_lowercase();
    let t = SCALE_UNITS.iter().find_map(|u| t.strip_suffix(u)).unwrap_or(t.as_str()).trim().to_string();
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ',') {
        return None;
    }
    t.replace(',', ".").parse().ok()
}

/// Labels forming a scale bar: three or more numbers on one baseline in
/// arithmetic progression ("0 250 500 750 1000 1250 m").
fn scale_bar_members(obs: &[OcrObservation], th: f64) -> Vec<bool> {
    let mut flag = vec![false; obs.len()];
    let nums: Vec<(usize, f64)> =
        obs.iter().enumerate().filter_map(|(i, o)| Some((i, scale_bar_number(&o.text)?))).collect();
    for &(i, _) in &nums {
        let (_, cy) = obs[i].center();
        let mut row: Vec<(f64, f64, usize)> = nums
            .iter()
            .filter(|&&(j, _)| (obs[j].center().1 - cy).abs() < 0.6 * th)
            .map(|&(j, v)| (obs[j].center().0, v, j))
            .collect();
        if row.len() < 3 {
            continue;
        }
        row.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Longest chain of neighbours with a constant value step.
        for s in 0..row.len().saturating_sub(2) {
            let step = row[s + 1].1 - row[s].1;
            if step <= 0.0 || row[s + 1].0 - row[s].0 > 12.0 * th {
                continue;
            }
            let mut e = s + 1;
            while e + 1 < row.len()
                && ((row[e + 1].1 - row[e].1) - step).abs() <= 1e-6 * step.abs().max(1.0)
                && row[e + 1].0 - row[e].0 <= 12.0 * th
            {
                e += 1;
            }
            if e - s >= 2 {
                for r in &row[s..=e] {
                    flag[r.2] = true;
                }
            }
        }
    }
    flag
}

/// Legend columns: three or more short labels sharing a left edge, stacked at
/// line spacing.
fn legend_members(obs: &[OcrObservation], th: f64) -> Vec<bool> {
    let mut flag = vec![false; obs.len()];
    let short: Vec<usize> = (0..obs.len())
        .filter(|&i| {
            let o = &obs[i];
            o.bbox[2] >= o.bbox[3] && words_of(&o.text).len() <= 4
        })
        .collect();
    let tol = (0.4 * th).max(4.0);
    for &i in &short {
        let x0 = obs[i].bbox[0];
        let mut col: Vec<usize> = short.iter().copied().filter(|&j| (obs[j].bbox[0] - x0).abs() <= tol).collect();
        if col.len() < 3 {
            continue;
        }
        col.sort_by(|&a, &b| obs[a].bbox[1].total_cmp(&obs[b].bbox[1]));
        let mut chain = vec![col[0]];
        let mut flush = |chain: &mut Vec<usize>| {
            if chain.len() >= 3 {
                for &k in chain.iter() {
                    flag[k] = true;
                }
            }
            chain.clear();
        };
        for w in col.windows(2) {
            let (a, b) = (&obs[w[0]], &obs[w[1]]);
            let h = a.text_height().max(b.text_height()).max(1.0);
            let gap = b.bbox[1] - a.bbox[1];
            if gap >= 0.9 * h && gap <= 3.0 * h {
                chain.push(w[1]);
            } else {
                flush(&mut chain);
                chain.push(w[1]);
            }
        }
        flush(&mut chain);
    }
    flag
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

/// Does the text look like part of a coordinate ("4.8'", "18°")?
fn has_angle_mark(text: &str) -> bool {
    let c: Vec<char> = text.chars().map(canon_char).collect();
    c.windows(2).any(|w| w[0].is_ascii_digit() && matches!(w[1], DEG | MIN | SEC))
}

fn poi_kind(words: &[String]) -> PoiKind {
    let first = words.first().map(String::as_str).unwrap_or("");
    let last = words.last().map(String::as_str).unwrap_or("");
    if matches!(first, "mount" | "mt" | "piek") || PEAK_WORDS.contains(&last) {
        return PoiKind::Peak;
    }
    if matches!(first, "lake" | "lac" | "lago") || WATER_WORDS.contains(&last) {
        return PoiKind::Water;
    }
    if last.len() > 5 {
        if PEAK_SUFFIXES.iter().any(|s| last.ends_with(s)) {
            return PoiKind::Peak;
        }
        if WATER_SUFFIXES.iter().any(|s| last.ends_with(s)) {
            return PoiKind::Water;
        }
    }
    PoiKind::Place
}

/// Turn OCR text into POI candidates (place names, spot heights), skipping
/// coordinate labels, scale text and legend/title boilerplate.
pub fn classify_pois(obs: &[OcrObservation]) -> Vec<PoiLabel> {
    // Normalise whitespace and merge duplicate observations (multi-pass OCR).
    let mut merged: Vec<OcrObservation> = Vec::new();
    for o in obs {
        let text = o.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() || !o.bbox.iter().all(|v| v.is_finite()) {
            continue;
        }
        let key = text.to_lowercase();
        let (cx, cy) = o.center();
        let dup = merged.iter_mut().find(|m| {
            let (mx, my) = m.center();
            let r = 2.0 * m.text_height().max(o.text_height()).max(4.0);
            m.text.to_lowercase() == key && (mx - cx).abs() <= r.max(0.5 * m.bbox[2]) && (my - cy).abs() <= r
        });
        match dup {
            Some(m) => {
                if o.confidence > m.confidence {
                    *m = OcrObservation { text, bbox: o.bbox, confidence: o.confidence };
                }
            }
            None => merged.push(OcrObservation { text, bbox: o.bbox, confidence: o.confidence }),
        }
    }
    let th = median(merged.iter().map(|o| o.text_height()).filter(|&h| h > 0.0).collect()).unwrap_or(20.0);
    let scale_bar = scale_bar_members(&merged, th);
    let legend = legend_members(&merged, th);

    let mut out: Vec<PoiLabel> = Vec::new();
    for (i, o) in merged.iter().enumerate() {
        if scale_bar[i] || legend[i] || o.text_height() > 1.8 * th {
            continue;
        }
        let text = o.text.as_str();
        if parse_coordinate(text).is_some() || parse_scale(text).is_some() || has_angle_mark(text) {
            continue;
        }
        let (x, y) = o.center();
        if let Some(h) = parse_height(text) {
            out.push(PoiLabel { name: text.to_string(), kind: PoiKind::SpotHeight, elevation_m: Some(h), x, y });
            continue;
        }
        if text.contains('©') || text.to_lowercase().contains("(c)") {
            continue;
        }
        let mut words = words_of(text);
        // A trailing height belongs to the name ("Sugarbush Hill 1523m").
        let mut elevation = None;
        let mut name = text.to_string();
        if words.len() >= 2 {
            let raw: Vec<&str> = text.split_whitespace().collect();
            let tail = if raw.len() >= 2 && raw[raw.len() - 1].eq_ignore_ascii_case("m") {
                raw[raw.len() - 2..].join("")
            } else {
                raw[raw.len() - 1].to_string()
            };
            if let Some(h) = parse_height(&tail).or_else(|| {
                let t = tail.trim_end_matches(['m', 'M']);
                t.parse::<f64>().ok().filter(|v| t.len() >= 3 && (-400.0..=8850.0).contains(v))
            }) {
                elevation = Some(h);
                let keep = if tail.len() > raw[raw.len() - 1].len() { raw.len() - 2 } else { raw.len() - 1 };
                name = raw[..keep].join(" ");
                words = words_of(&name);
            }
        }
        let letters = name.chars().filter(|c| c.is_alphabetic()).count();
        let others = name.chars().filter(|c| !c.is_whitespace() && !c.is_alphabetic()).count();
        if letters < 3 || (letters as f64) < 0.6 * (letters + others) as f64 {
            continue;
        }
        if words.iter().any(|w| META_WORDS.contains(&w.as_str())) {
            continue;
        }
        let generic =
            |w: &String| LEGEND_WORDS.contains(&w.as_str()) || w.chars().all(|c| c.is_ascii_digit()) || is_measure(w);
        if words.iter().all(generic) {
            continue;
        }
        out.push(PoiLabel { name, kind: poi_kind(&words), elevation_m: elevation, x, y });
    }

    drop_truncated_heights(&mut out);

    // Peaks without a height take the nearest spot height next to them.
    let spots: Vec<(f64, f64, f64)> =
        out.iter().filter(|p| p.kind == PoiKind::SpotHeight).filter_map(|p| Some((p.x, p.y, p.elevation_m?))).collect();
    for p in out.iter_mut().filter(|p| p.kind == PoiKind::Peak && p.elevation_m.is_none()) {
        let near = spots
            .iter()
            .map(|s| ((s.0 - p.x).hypot(s.1 - p.y), s.2))
            .filter(|(d, _)| *d <= 6.0 * th)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, h)) = near {
            p.elevation_m = Some(h);
        }
    }
    out
}

/// Drop spot heights that are misreads with digits missing, such as "112" for a "1412" that a
/// grid line cuts through: on a sheet whose heights nearly all have the same number of digits
/// (at least 4 heights, 80 % alike), a height with fewer digits and under half the median is
/// not believable. Sheets that span lowland and mountain keep their short heights.
fn drop_truncated_heights(pois: &mut Vec<PoiLabel>) {
    let digits = |h: f64| (h.abs().trunc() as u64).to_string().len();
    let heights: Vec<f64> =
        pois.iter().filter(|p| p.kind == PoiKind::SpotHeight).filter_map(|p| p.elevation_m).collect();
    if heights.len() < 4 {
        return;
    }
    let Some(med) = median(heights.clone()) else { return };
    let typical = digits(med);
    let alike = heights.iter().filter(|&&h| digits(h) == typical).count();
    if (alike as f64) < 0.8 * heights.len() as f64 {
        return;
    }
    pois.retain(|p| {
        p.kind != PoiKind::SpotHeight || p.elevation_m.is_none_or(|h| digits(h) >= typical || h >= 0.5 * med)
    });
}

/// "5m", "100m", "2km" style measurement tokens.
fn is_measure(w: &str) -> bool {
    let t = w.trim_end_matches(|c: char| c.is_alphabetic());
    !t.is_empty() && t.len() < w.len() && t.chars().all(|c| c.is_ascii_digit()) && w.len() - t.len() <= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_and_confused_dms_labels() {
        let lat = |t: &str| match parse_coordinate(t) {
            Some(CoordValue::Lat(v)) => v,
            other => panic!("{t:?} -> {other:?}"),
        };
        let lon = |t: &str| match parse_coordinate(t) {
            Some(CoordValue::Lon(v)) => v,
            other => panic!("{t:?} -> {other:?}"),
        };
        assert!((lon("GIS V1 Sept 2017- Eras Gous 27°23'00\"E") - (27.0 + 23.0 / 60.0)).abs() < 1e-9);
        assert!((lat("I 25°50'00\"S") + 25.0 + 50.0 / 60.0).abs() < 1e-9);
        assert!((lat("25°50'00\"S 1") + 25.0 + 50.0 / 60.0).abs() < 1e-9);
        assert!((lat("25'53'00\"S") + 25.0 + 53.0 / 60.0).abs() < 1e-9);
        assert!((lat("25°53°00\"S") + 25.0 + 53.0 / 60.0).abs() < 1e-9);
        assert!((lat("25°50'00\"5") + 25.0 + 50.0 / 60.0).abs() < 1e-9);
        for junk in ["GIS V1 Sept 2017- Eras Gous", "Magnetic Declination 18° West", "Sept 2017", "12 34 Street"] {
            assert_eq!(parse_coordinate(junk), None, "{junk}");
        }
    }

    fn lat(v: f64) -> Option<CoordValue> {
        Some(CoordValue::Lat(v))
    }

    fn close(a: Option<CoordValue>, b: Option<CoordValue>) -> bool {
        match (a, b) {
            (Some(CoordValue::Lat(x)), Some(CoordValue::Lat(y)))
            | (Some(CoordValue::Lon(x)), Some(CoordValue::Lon(y)))
            | (Some(CoordValue::Degrees(x)), Some(CoordValue::Degrees(y)))
            | (Some(CoordValue::Grid(x)), Some(CoordValue::Grid(y))) => (x - y).abs() < 1e-6,
            (None, None) => true,
            _ => false,
        }
    }

    #[test]
    fn dms_variants() {
        let v = -(25.0 + 49.0 / 60.0);
        for t in [
            "25°49'00\"S",
            "25°49′00″S",
            "25°49’00”S",
            "25*49'00\"S",
            "25º49'00\"S",
            "25°4900\"S",
            "25 49 00 S",
            "S 25°49'",
            "25°49'S",
            "25°49'0O\"S",
            "25o49'00\"S",
            "25°49'00\"S.",
            "25°49'00\"S |",
            " 25° 49' 00\" S ",
            "2549'00\"S",
            "25°49'00\"S l",
        ] {
            let p = parse_coordinate(t);
            assert!(close(p, lat(v)), "{t:?} -> {p:?}");
        }
        assert!(close(parse_coordinate("25°49.5'S"), lat(-(25.0 + 49.5 / 60.0))));
        assert!(close(parse_coordinate("25°49'30\"N"), lat(25.0 + 49.5 / 60.0)));
        assert!(close(parse_coordinate("25.8167°S"), lat(-25.8167)));
    }

    #[test]
    fn lon_and_degrees() {
        let v = 27.0 + 23.0 / 60.0;
        for t in ["27°23'00\"E", "E027°23'", "27° 23' E", "E 27°23'00\"", "27°23’00”E"] {
            assert!(close(parse_coordinate(t), Some(CoordValue::Lon(v))), "{t}");
        }
        assert!(close(parse_coordinate("122°30'W"), Some(CoordValue::Lon(-122.5))));
        assert!(close(parse_coordinate("-25.8167"), Some(CoordValue::Degrees(-25.8167))));
        assert!(close(parse_coordinate("−25.8167"), Some(CoordValue::Degrees(-25.8167))));
        assert!(close(parse_coordinate("27.40"), Some(CoordValue::Degrees(27.4))));
        assert!(close(parse_coordinate("25°49'00\""), Some(CoordValue::Degrees(25.0 + 49.0 / 60.0))));
        assert!(close(parse_coordinate("2723E"), Some(CoordValue::Lon(v))));
    }

    #[test]
    fn grid_values() {
        for (t, v) in [
            ("540000", 540000.0),
            ("540 000", 540000.0),
            ("540000mE", 540000.0),
            ("540 000 mE", 540000.0),
            ("540,000", 540000.0),
            ("7140000mN", 7140000.0),
            ("7 140 000", 7140000.0),
            ("7140000 m N", 7140000.0),
        ] {
            assert!(close(parse_coordinate(t), Some(CoordValue::Grid(v))), "{t} -> {:?}", parse_coordinate(t));
        }
    }

    #[test]
    fn rejects_non_coordinates() {
        for t in [
            "1428",
            "1250 m",
            "1:25 000",
            "250",
            "41",
            "0",
            "4.8",
            "1428m",
            "N4",
            "R24",
            "KWAMI LODGE",
            "Magnetic Declination 18° West",
            "Annual Change: 4.8' per year West",
            "GIS V1 Sept 2017- Eras Gous",
            "MCSA MAGALIESBERG SHEET 3 OF 10",
            "25°75'00\"S",
            "25°49'75\"S",
            "95°00'S",
            "190°00'E",
            "25°49'00\"S 27°23'00\"E",
            "",
            "°'\"",
        ] {
            assert_eq!(parse_coordinate(t), None, "{t:?}");
        }
    }

    #[test]
    fn scales() {
        for (t, d) in [
            ("1:25 000", 25000.0),
            ("1 : 50,000", 50000.0),
            ("1:25000", 25000.0),
            ("Scale 1:50 000", 50000.0),
            ("1:25.000", 25000.0),
            ("1:1 000 000", 1_000_000.0),
            ("SCALE 1:250 000 (approx)", 250000.0),
        ] {
            assert_eq!(parse_scale(t), Some(d), "{t}");
        }
        for t in ["1:10", "1:20 000 000", "11:30", "25 000", "1250 m", "1:"] {
            assert_eq!(parse_scale(t), None, "{t}");
        }
    }

    fn ob(text: &str, x: f64, y: f64, w: f64, h: f64) -> OcrObservation {
        OcrObservation { text: text.into(), bbox: [x, y, w, h], confidence: 0.9 }
    }

    #[test]
    fn poi_kinds_and_heights() {
        let obs = vec![
            ob("Sugarbush Hill", 100.0, 100.0, 180.0, 22.0),
            ob("1523", 120.0, 130.0, 54.0, 20.0),
            ob("Sterkfontein", 600.0, 600.0, 160.0, 22.0),
            ob("Mount Frere 1720m", 900.0, 900.0, 240.0, 22.0),
            ob("Hennops River", 1200.0, 300.0, 180.0, 22.0),
            ob("1500", 300.0, 300.0, 52.0, 20.0),
            ob("Cave", 1500.0, 1500.0, 60.0, 22.0),
            ob("KWAMI LODGE", 1800.0, 1800.0, 178.0, 22.0),
            ob("KWAMI LODGE", 1801.0, 1802.0, 176.0, 22.0),
        ];
        let p = classify_pois(&obs);
        let find = |n: &str| p.iter().find(|q| q.name == n).cloned();
        let hill = find("Sugarbush Hill").unwrap();
        assert_eq!(hill.kind, PoiKind::Peak);
        assert_eq!(hill.elevation_m, Some(1523.0));
        assert_eq!(find("Sterkfontein").unwrap().kind, PoiKind::Water);
        let mt = find("Mount Frere").unwrap();
        assert_eq!((mt.kind, mt.elevation_m), (PoiKind::Peak, Some(1720.0)));
        assert_eq!(find("Hennops River").unwrap().kind, PoiKind::Water);
        assert_eq!(find("1523").unwrap().kind, PoiKind::SpotHeight);
        assert!(find("1500").is_none(), "contour label");
        assert!(find("Cave").is_none(), "generic legend word");
        assert_eq!(p.iter().filter(|q| q.name == "KWAMI LODGE").count(), 1);
    }

    #[test]
    fn truncated_spot_heights_are_dropped() {
        let heights = ["1488", "1518", "1412", "1526", "1604", "112"];
        let obs: Vec<_> =
            heights.iter().enumerate().map(|(k, t)| ob(t, 200.0 + 300.0 * k as f64, 400.0, 54.0, 20.0)).collect();
        let p = classify_pois(&obs);
        let names: Vec<&str> = p.iter().map(|q| q.name.as_str()).collect();
        assert_eq!(names, vec!["1488", "1518", "1412", "1526", "1604"], "{p:?}");

        // Lowland and mountain on one sheet: short heights are not the exception.
        let heights = ["112", "348", "905", "1210", "1488", "760"];
        let obs: Vec<_> =
            heights.iter().enumerate().map(|(k, t)| ob(t, 200.0 + 300.0 * k as f64, 400.0, 54.0, 20.0)).collect();
        assert_eq!(classify_pois(&obs).len(), heights.len());
    }

    #[test]
    fn scale_bar_and_legend_column() {
        let mut obs = vec![
            ob("0", 100.0, 500.0, 18.0, 24.0),
            ob("220", 200.0, 500.0, 50.0, 24.0),
            ob("440", 300.0, 500.0, 50.0, 24.0),
            ob("660", 400.0, 500.0, 50.0, 24.0),
            ob("880 yards", 500.0, 500.0, 140.0, 24.0),
        ];
        for (k, t) in ["Hut", "Shelter Rock", "Viewing Deck", "Braai Area"].iter().enumerate() {
            obs.push(ob(t, 50.0, 800.0 + 35.0 * k as f64, 120.0, 22.0));
        }
        obs.push(ob("Rhino Pools", 900.0, 900.0, 150.0, 22.0));
        let p = classify_pois(&obs);
        let names: Vec<&str> = p.iter().map(|q| q.name.as_str()).collect();
        assert_eq!(names, vec!["Rhino Pools"], "{p:?}");
    }
}
