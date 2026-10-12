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
        // OCR confusions for hemisphere letters ("25°52'00\"$", "27°23'00\"€").
        '$' => 'S',
        '€' => 'E',
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
            // "25 49 00 S": needs a hemisphere letter to be read as DMS. "25 4900 S" (minutes and
            // seconds run together, marks lost) is read the same way.
            let packed_minutes = comps.len() == 2 && comps[0].int.len() <= 3 && comps[1].int.len() == 4;
            if hemi.is_none() || comps.len() > 3 || (!packed_minutes && comps[1..].iter().any(|c| c.int.len() > 2)) {
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
    parse_strict(text).or_else(|| {
        // A garbled DMS label (`fuzzy_dms`) when every cheapest reading agrees.
        let r = fuzzy_dms(&normalize(text));
        let best = r.first()?;
        r.iter().take_while(|x| x.cost == best.cost).all(|x| same_value(x.value, best.value)).then_some(best.value)
    })
}

fn parse_strict(text: &str) -> Option<CoordValue> {
    let norm = normalize(text);
    if norm.iter().any(|&c| c == 'o' || c == 'O') {
        let zeroed = o_to_zero(&norm);
        return parse_tokens(&tokenize(&zeroed))
            .or_else(|| parse_tokens(&tokenize(&o_to_degree(&norm))))
            .or_else(|| parse_embedded_dms(&zeroed));
    }
    parse_tokens(&tokenize(&norm)).or_else(|| parse_embedded_dms(&norm))
}

/// One reading of a coordinate label: its value and how many repairs the
/// reading needed (0 for a label `parse_coordinate` reads as printed).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoordReading {
    pub value: CoordValue,
    pub cost: u8,
}

/// Every plausible reading of a coordinate label, cheapest first: the
/// label as printed (`parse_coordinate`'s strict reading, cost 0) and the
/// readings of a garbled DMS label (`fuzzy_dms`): marks read as other
/// symbols ("25°48/00\\S", "25°48'00~s", "27%34'00cE"), as digits or
/// letters ("2595000\"s", "2548100s", "25950l00s") or lost ("255000s",
/// "272400E", which reads as an easting too), zeros read as O or Q, a
/// lowercase hemisphere letter, other text run into it
/// ("27%3400cEV1Sept2017-ErasGous"). Several readings of one label are
/// alternatives (the grid fit keeps the one that agrees with the others:
/// "25-50'00\"S" is 50°00'S as printed, 25°50'00"S with a dash for the
/// degree mark); a text holding two different coordinates gives none.
pub fn coordinate_readings(text: &str) -> Vec<CoordReading> {
    let mut out: Vec<CoordReading> =
        parse_strict(text).map(|v| CoordReading { value: v, cost: 0 }).into_iter().collect();
    for r in fuzzy_dms(&normalize(text)) {
        if !out.iter().any(|o| same_value(o.value, r.value)) {
            out.push(r);
        }
    }
    out
}

fn same_value(a: CoordValue, b: CoordValue) -> bool {
    match (a, b) {
        (CoordValue::Lat(x), CoordValue::Lat(y))
        | (CoordValue::Lon(x), CoordValue::Lon(y))
        | (CoordValue::Degrees(x), CoordValue::Degrees(y))
        | (CoordValue::Grid(x), CoordValue::Grid(y)) => (x - y).abs() <= 1e-9 * x.abs().max(1.0),
        _ => false,
    }
}

/// Most repairs a garbled DMS reading may need (`fuzzy_dms`).
const FUZZY_MAX_COST: u8 = 5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkSlot {
    Deg,
    Min,
    Sec,
}

/// Cost of `ch` standing for the mark after the number in `slot` (None: it
/// cannot). The printed mark is free; any other symbol costs 1, and so do
/// letters OCR reads for marks (o for °, l/I for ', c for "); a digit read
/// for a mark (9 or 0 for °, 1 for ', 1 or 4 for ") costs 2.
fn mark_cost(ch: char, slot: MarkSlot) -> Option<u8> {
    let printed = match slot {
        MarkSlot::Deg => DEG,
        MarkSlot::Min => MIN,
        MarkSlot::Sec => SEC,
    };
    if ch == printed {
        return Some(0);
    }
    if !ch.is_alphanumeric() && !ch.is_whitespace() && ch != ':' {
        return Some(1);
    }
    match (slot, ch) {
        (MarkSlot::Deg, 'o' | 'O') => Some(1),
        (MarkSlot::Deg, '9' | '0') => Some(2),
        (MarkSlot::Min, 'l' | 'I' | 'i' | 'j' | 'J') => Some(1),
        (MarkSlot::Min, '1') => Some(2),
        (MarkSlot::Sec, 'c' | 'C' | 'u' | 'U' | 'l' | 'I' | 'i') => Some(1),
        (MarkSlot::Sec, '1' | '4') => Some(2),
        _ => None,
    }
}

/// A digit, or a letter OCR reads for one (O/o/Q/D for 0, l/I/| for 1, cost 1).
fn digit_like(ch: char) -> Option<(u32, u8)> {
    match ch {
        '0'..='9' => Some((ch as u32 - '0' as u32, 0)),
        'O' | 'o' | 'Q' | 'D' => Some((0, 1)),
        'l' | 'I' | '|' => Some((1, 1)),
        _ => None,
    }
}

/// `len` digit-like characters at `i`, at least one a real digit unless
/// `letters_ok`: (value, cost).
fn fuzzy_number(c: &[char], i: usize, len: usize, letters_ok: bool) -> Option<(f64, u8)> {
    if i + len > c.len() {
        return None;
    }
    let (mut v, mut cost, mut real) = (0u32, 0u8, false);
    for &ch in &c[i..i + len] {
        let (d, k) = digit_like(ch)?;
        v = v * 10 + d;
        cost += k;
        real |= k == 0;
    }
    (real || letters_ok).then_some((v as f64, cost))
}

/// One way the mark after a number was read (`fuzzy_marks`).
#[derive(Clone, Copy)]
struct MarkRead {
    /// Index after the mark (and the spaces around it).
    next: usize,
    cost: u8,
    /// Symbols read for it (not the letters or digits read for marks)…
    symbols: u8,
    /// …and whether anything was read for it at all.
    present: u8,
}

/// Ways the mark after a number may be read at `i`. Spaces around it are
/// free; the mark may be lost (cost 1) or read as one or two characters.
fn fuzzy_marks(c: &[char], i: usize, slot: MarkSlot) -> Vec<MarkRead> {
    let skip = |mut k: usize| {
        while k < c.len() && c[k] == ' ' {
            k += 1;
        }
        k
    };
    let k = skip(i);
    // Lost, or only a space left of it.
    let mut out = vec![MarkRead { next: k, cost: 1, symbols: 0, present: u8::from(k > i) }];
    let Some(first) = c.get(k).and_then(|&ch| mark_cost(ch, slot)) else { return out };
    let symbol = |ch: char| u8::from(!ch.is_alphanumeric());
    out.push(MarkRead { next: skip(k + 1), cost: first, symbols: symbol(c[k]), present: 1 });
    // Two characters for one mark: two symbols, or a double prime read as "11" or as a
    // symbol and a 1 ("25*51/00\"1s").
    let digit = |ch: char| ch.is_ascii_digit();
    if let Some(&second) = c.get(k + 1) {
        let both_symbols = !digit(c[k]) && !digit(second) && mark_cost(second, slot).is_some();
        let double_prime = slot == MarkSlot::Sec && (c[k] == '1' || symbol(c[k]) == 1) && second == '1';
        if both_symbols || double_prime {
            let cost = first.max(mark_cost(second, slot).unwrap_or(2));
            let symbols = symbol(c[k]).max(symbol(second));
            out.push(MarkRead { next: skip(k + 2), cost, symbols, present: 1 });
        }
    }
    out
}

/// Readings of garbled DMS labels (see `coordinate_readings`): degrees (1–3
/// digits), a mark, minutes (2 digits), a mark, optionally seconds (2
/// digits) and a mark, then a hemisphere letter, every mark possibly
/// misread or lost (`fuzzy_marks`). Without seconds both marks must be
/// symbols; without a hemisphere letter (a `Degrees` reading) the seconds,
/// two marks, one of them a symbol, must be there and nothing alphanumeric
/// after them; a hemisphere letter run into a word ("…cEV1Sept") needs the
/// seconds and a symbol for a mark. At most `FUZZY_MAX_COST` repairs.
fn fuzzy_dms(c: &[char]) -> Vec<CoordReading> {
    let n = c.len();
    // (start, end, reading) of every reading found.
    let mut found: Vec<(usize, usize, CoordReading)> = Vec::new();
    for start in 0..n {
        if !c[start].is_ascii_digit() || (start > 0 && digit_like(c[start - 1]).is_some()) {
            continue;
        }
        // Degrees never follow a number and a mark ("25°48'00\"S" holds no 48°) or a number and
        // a space or dash ("2017 5100S").
        let mut p = start;
        while p > 0 && matches!(c[p - 1], ' ' | '-') {
            p -= 1;
        }
        if p > 0 && c[p - 1].is_ascii_digit() {
            continue;
        }
        if start >= 2 && mark_cost(c[start - 1], MarkSlot::Deg).is_some() && c[start - 2].is_ascii_digit() {
            continue;
        }
        for dlen in 1..=3 {
            if start + dlen > n || !c[start..start + dlen].iter().all(char::is_ascii_digit) {
                break;
            }
            let Some((deg, c_deg)) = fuzzy_number(c, start, dlen, false) else { break };
            for m1 in fuzzy_marks(c, start + dlen, MarkSlot::Deg) {
                let Some((min, c_min)) = fuzzy_number(c, m1.next, 2, false).filter(|m| m.0 < 60.0) else { continue };
                for m2 in fuzzy_marks(c, m1.next + 2, MarkSlot::Min) {
                    let base = c_deg + m1.cost + c_min + m2.cost;
                    // With seconds.
                    // Zero seconds may be read as letters only ("25°53'QoS").
                    if let Some((sec, c_sec)) = fuzzy_number(c, m2.next, 2, true).filter(|s| s.0 < 60.0) {
                        let mut i5 = m2.next + 2;
                        let mut sec = sec;
                        if i5 + 1 < n && c[i5] == '.' && c[i5 + 1].is_ascii_digit() {
                            let mut f = String::from("0.");
                            i5 += 1;
                            while i5 < n && c[i5].is_ascii_digit() {
                                f.push(c[i5]);
                                i5 += 1;
                            }
                            sec += f.parse::<f64>().unwrap_or(0.0);
                        }
                        for m3 in fuzzy_marks(c, i5, MarkSlot::Sec) {
                            let marks = [m1, m2, m3];
                            let cost = base + c_sec + m3.cost;
                            let v = deg + min / 60.0 + sec / 3600.0;
                            if let Some((end, value, k)) = fuzzy_hemisphere(c, m3.next, v, &marks, true) {
                                if cost + k <= FUZZY_MAX_COST {
                                    found.push((start, end, CoordReading { value, cost: cost + k }));
                                }
                            }
                        }
                    }
                    // Without seconds: "25°49'S".
                    if m1.symbols > 0 && m2.symbols > 0 {
                        let v = deg + min / 60.0;
                        if let Some((end, value, k)) = fuzzy_hemisphere(c, m2.next, v, &[m1, m2], false) {
                            if base + k <= FUZZY_MAX_COST {
                                found.push((start, end, CoordReading { value, cost: base + k }));
                            }
                        }
                    }
                }
            }
        }
    }
    // Readings starting inside an earlier one are parts of it.
    found.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.cost.cmp(&b.2.cost)));
    let mut kept: Vec<(usize, usize, CoordReading)> = Vec::new();
    for f in found {
        if kept.iter().any(|k| k.0 != f.0 && f.0 < k.1) {
            continue;
        }
        kept.push(f);
    }
    // A reading without hemisphere is redundant next to one with it.
    let signed = |v: CoordValue| match v {
        CoordValue::Lat(x) | CoordValue::Lon(x) => Some(x.abs()),
        _ => None,
    };
    let magnitudes: Vec<f64> = kept.iter().filter_map(|k| signed(k.2.value)).collect();
    kept.retain(|k| match k.2.value {
        CoordValue::Degrees(v) => !magnitudes.iter().any(|m| (m - v).abs() <= 1e-9 * v.max(1.0)),
        _ => true,
    });
    let mut out: Vec<CoordReading> = Vec::new();
    for (s, _, r) in &kept {
        // Two labels in one text: neither position is known.
        if kept.iter().any(|(t, _, q)| t != s && !same_value(q.value, r.value)) {
            return Vec::new();
        }
        match out.iter_mut().find(|o| same_value(o.value, r.value)) {
            Some(o) => o.cost = o.cost.min(r.cost),
            None => out.push(*r),
        }
    }
    out.sort_by_key(|r| r.cost);
    out
}

/// The hemisphere after a garbled DMS label at `k` (`fuzzy_dms`): (end,
/// value, extra cost), or a `Degrees` value without one.
fn fuzzy_hemisphere(
    c: &[char],
    k: usize,
    v: f64,
    marks: &[MarkRead],
    seconds: bool,
) -> Option<(usize, CoordValue, u8)> {
    let n = c.len();
    let symbols: u8 = marks.iter().map(|m| m.symbols).sum();
    let present: u8 = marks.iter().map(|m| m.present).sum();
    let hemi = c.get(k).and_then(|&ch| match ch {
        'N' | 'n' => Some(('N', 0)),
        'S' | 's' => Some(('S', 0)),
        'E' | 'e' => Some(('E', 0)),
        'W' | 'w' => Some(('W', 0)),
        '5' if symbols > 0 => Some(('S', 1)),
        _ => None,
    });
    let Some((h, cost)) = hemi else {
        // No hemisphere letter (a `Degrees` reading): the seconds and a symbol for a mark, and
        // nothing after them ("273700\"", "25°51/00/"); other words after them only past a
        // space ("2734,00\" GIS V1…"), with two marks, or right after a degree sign
        // ("27°3400GIS V1 Sept 2017").
        let degree_sign = marks[0].cost == 0 && marks[0].present == 1;
        let end = c[k.min(n)..].iter().all(|ch| !ch.is_alphanumeric());
        let words_after = (k > 0 && c[k - 1] == ' ' && present >= 2)
            || (degree_sign && c.get(k).is_some_and(|ch| ch.is_alphabetic()));
        let ok = seconds && (symbols >= 1 || degree_sign) && (end || words_after) && v <= 180.0;
        return ok.then_some((k, CoordValue::Degrees(v), 1));
    };
    if c.get(k + 1).is_some_and(|ch| ch.is_alphanumeric() && !ch.is_ascii_digit()) && !(seconds && symbols > 0) {
        return None;
    }
    let value = match h {
        'N' | 'S' if v <= 90.0 => CoordValue::Lat(if h == 'S' { -v } else { v }),
        'E' | 'W' if v <= 180.0 => CoordValue::Lon(if h == 'W' { -v } else { v }),
        _ => return None,
    };
    Some((k + 1, value, cost))
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
        // Nor does a degrees-and-minutes run follow another number: in "25 5100S" (or "24-2400\"E")
        // the "5100" is minutes and seconds, not 51° 00'.
        let mut q = start;
        while q > 0 && matches!(c[q - 1], ' ' | '-') {
            q -= 1;
        }
        if j - start == 4 && q > 0 && c[q - 1].is_ascii_digit() {
            continue;
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

/// What `classify_pois` makes of one (merged) observation.
enum Verdict {
    Poi(PoiLabel),
    /// Map furniture: coordinates, scale, legend, title, imprint.
    Furniture,
    /// Unreadable (mostly symbols or digits that are no height).
    Junk,
}

/// Merge reads of one label from several OCR passes: identical text (case
/// and spacing aside) close together keeps the most confident read.
fn merge_identical(obs: &[OcrObservation]) -> Vec<OcrObservation> {
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
    merged
}

/// A word of legend text: one of `LEGEND_WORDS`, the start of one cut off by
/// the OCR ("Non-Peren", "Police Stati", "100m Inter"), several run together
/// ("PerennialRiver", "DryWaterCourse"), a number, a measure or a single letter.
fn generic_word(w: &str) -> bool {
    if w.chars().count() <= 1 || w.chars().all(|c| c.is_ascii_digit()) || is_measure(w) {
        return true;
    }
    let cut = |l: &&str| l.len() > w.len() && l.starts_with(w);
    if LEGEND_WORDS.contains(&w) || (w.len() >= 4 && LEGEND_WORDS.iter().chain(META_WORDS).any(cut)) {
        return true;
    }
    // Legend words run together: every split point reachable from the start.
    let n = w.len();
    let mut reach = vec![false; n + 1];
    reach[0] = true;
    for i in 0..n {
        if !reach[i] || !w.is_char_boundary(i) {
            continue;
        }
        for l in LEGEND_WORDS.iter().filter(|l| l.len() >= 3) {
            if w[i..].starts_with(l) {
                reach[i + l.len()] = true;
            }
        }
    }
    reach[n]
}

/// Height of the letters of a read: the short side of its box, or less for
/// a slanted line of text, whose box is about as tall as it is long (from
/// the box's diagonal and the number of characters, ~0.55 text heights each).
fn letter_height(o: &OcrObservation) -> f64 {
    let n = o.text.chars().filter(|c| !c.is_whitespace()).count().max(1) as f64;
    o.text_height().min(o.bbox[2].hypot(o.bbox[3]) / (0.55 * n))
}

fn verdict(o: &OcrObservation, furniture: bool, th: f64) -> Verdict {
    // Large lettering is the title.
    if furniture || letter_height(o) > TITLE_TEXT_HEIGHT * th {
        return Verdict::Furniture;
    }
    let text = o.text.as_str();
    if parse_coordinate(text).is_some() || parse_scale(text).is_some() || has_angle_mark(text) {
        return Verdict::Furniture;
    }
    let (x, y) = o.center();
    if let Some(h) = parse_height(text) {
        return Verdict::Poi(PoiLabel {
            name: text.to_string(),
            kind: PoiKind::SpotHeight,
            elevation_m: Some(h),
            x,
            y,
        });
    }
    if text.contains('©') || text.to_lowercase().contains("(c)") {
        return Verdict::Furniture;
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
    // Symbols the OCR read around a name ("AASVOELKRANS★", "/FARMGATE").
    let name = name.trim_matches(|c: char| !c.is_alphanumeric() && !"()'.".contains(c)).to_string();
    let letters = name.chars().filter(|c| c.is_alphabetic()).count();
    let others = name.chars().filter(|c| !c.is_whitespace() && !c.is_alphabetic()).count();
    if letters < 3 || (letters as f64) < 0.6 * (letters + others) as f64 {
        return Verdict::Junk;
    }
    if words.iter().any(|w| META_WORDS.contains(&w.as_str())) {
        return Verdict::Furniture;
    }
    if words.iter().all(|w| generic_word(w)) {
        return Verdict::Furniture;
    }
    // Letters and digits run together in most words (two or more): an imprint read badly
    // ("GISV1$eet20x"; "RANDWATERBOX2" is a name).
    let mixed = words
        .iter()
        .filter(|w| w.chars().any(|c| c.is_ascii_digit()) && w.chars().any(char::is_alphabetic) && !is_measure(w))
        .count();
    if mixed >= 2 && 2 * mixed > words.len() {
        return Verdict::Junk;
    }
    if letters <= 3 {
        // A short read with no vowel or no consonant is no word ("HHL", "Pws", "AAI"); three
        // letters read up or down the map are a piece of a longer label printed along a
        // stream or slope ("ERN", "ADE", "KLO").
        let vowel = |c: char| "aeiouyAEIOUY".contains(c);
        let alpha: Vec<char> = name.chars().filter(|c| c.is_alphabetic()).collect();
        let upright = o.bbox[3] >= 1.5 * o.bbox[2];
        if !alpha.iter().any(|&c| vowel(c)) || alpha.iter().all(|&c| vowel(c)) || upright {
            return Verdict::Junk;
        }
    }
    Verdict::Poi(PoiLabel { name, kind: poi_kind(&words), elevation_m: elevation, x, y })
}

/// Lettering this many times the median text height is the title.
const TITLE_TEXT_HEIGHT: f64 = 1.6;

/// Lowercase letters and digits of a text: what fragments are compared on.
fn fragment_key(text: &str) -> Vec<char> {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// `needle` occurs in `hay` with at most `k` edits (substitutions,
/// insertions, deletions; Sellers' algorithm).
fn approx_contains(hay: &[char], needle: &[char], k: usize) -> bool {
    let m = needle.len();
    let mut prev: Vec<usize> = (0..=m).collect();
    if prev[m] <= k {
        return true;
    }
    for &h in hay {
        let mut cur = vec![0usize; m + 1];
        for i in 1..=m {
            let sub = prev[i - 1] + usize::from(needle[i - 1] != h);
            cur[i] = sub.min(prev[i] + 1).min(cur[i - 1] + 1);
        }
        if cur[m] <= k {
            return true;
        }
        prev = cur;
    }
    false
}

/// Longest run of characters `a` and `b` share.
fn longest_common_run(a: &[char], b: &[char]) -> usize {
    let mut best = 0;
    let mut prev = vec![0usize; b.len() + 1];
    for &x in a {
        let mut cur = vec![0usize; b.len() + 1];
        for (j, &y) in b.iter().enumerate() {
            if x == y {
                cur[j + 1] = prev[j] + 1;
                best = best.max(cur[j + 1]);
            }
        }
        prev = cur;
    }
    best
}

/// Fraction of box `a`'s area inside box `b` (`[x, y, w, h]`).
fn covered_by(a: &[f64; 4], b: &[f64; 4]) -> f64 {
    let ix = (a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0]);
    let iy = (a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1]);
    let area = a[2] * a[3];
    if ix <= 0.0 || iy <= 0.0 || area <= 0.0 {
        0.0
    } else {
        ix * iy / area
    }
}

/// Several passes read one label in pieces ("KUDUKONA" / "UDU", "1283" /
/// "283", "MHLABATINI" / "BATI" / "ABATI", "Police Station" / "Stati") or
/// differently ("KRAAL" / "KRAAZ", "KWAMI LODGE" / "NAMI LODO", "1471" /
/// "1472"): a POI is a piece or another read of a read next to it when that
/// read holds its text (exactly, or within one edit from 5 characters), or
/// when their boxes all but coincide and they share most of their text. The
/// longer read wins, then the one in more words ("SPARKLING WATERS HOTEL"
/// over "SPARKLINGWATERSHOTEL"), then the more confident one; a piece of
/// legend text goes with it. Unreadable reads ("21728" around "1728") never
/// absorb a POI.
fn drop_fragments(merged: &[OcrObservation], verdicts: &mut [Verdict]) {
    let keys: Vec<Vec<char>> = merged.iter().map(|o| fragment_key(&o.text)).collect();
    let words: Vec<usize> = merged.iter().map(|o| o.text.split_whitespace().count()).collect();
    let mut drop = vec![false; merged.len()];
    for (a, oa) in merged.iter().enumerate() {
        if !matches!(verdicts[a], Verdict::Poi(_)) || keys[a].len() < 3 {
            continue;
        }
        let (ax, ay) = oa.center();
        drop[a] = merged.iter().enumerate().any(|(b, ob)| {
            // Unreadable reads and numbers that are no height ("21728") absorb nothing.
            let absorbs = match verdicts[b] {
                Verdict::Poi(_) => true,
                Verdict::Furniture => ob.text.chars().filter(|c| c.is_alphabetic()).count() >= 3,
                Verdict::Junk => false,
            };
            if b == a || !absorbs {
                return false;
            }
            let (ka, kb) = (&keys[a], &keys[b]);
            let rank = |k: usize, o: &OcrObservation| (keys[k].len(), words[k], o.confidence);
            let (ra, rb) = (rank(a, oa), rank(b, ob));
            // A misread of legend text ("TRIG BEAGON") is legend text.
            let better = (matches!(verdicts[b], Verdict::Furniture) && rb.0 >= ra.0)
                || rb.0 > ra.0
                || (rb.0 == ra.0 && (rb.1 > ra.1 || (rb.1 == ra.1 && (rb.2 > ra.2 || (rb.2 == ra.2 && b < a)))));
            if !better {
                return false;
            }
            let m = 1.5 * oa.text_height().max(ob.text_height());
            let [x, y, w, h] = ob.bbox;
            let near = ax >= x - m && ax <= x + w + m && ay >= y - m && ay <= y + h + m;
            if !near {
                return false;
            }
            // Two reads of one place: boxes that all but coincide, most of the text shared.
            let same_place = covered_by(&oa.bbox, &ob.bbox).max(covered_by(&ob.bbox, &oa.bbox)) >= 0.6;
            if same_place && longest_common_run(ka, kb) as f64 >= 0.6 * ka.len().min(kb.len()) as f64 {
                return true;
            }
            let edits = usize::from(ka.len() >= 5);
            if kb.len() == ka.len() {
                // Same length: the same read spaced differently, or one letter read differently.
                ka.iter().zip(kb).filter(|(p, q)| p != q).count() <= edits
            } else {
                approx_contains(kb, ka, edits)
            }
        });
    }
    for (v, d) in verdicts.iter_mut().zip(drop) {
        if d {
            *v = Verdict::Junk;
        }
    }
}

/// Turn OCR text into POI candidates (place names, spot heights), skipping
/// coordinate labels, scale text and legend/title boilerplate, and the
/// pieces and misreads several OCR passes make of one label.
pub fn classify_pois(obs: &[OcrObservation]) -> Vec<PoiLabel> {
    let merged = merge_identical(obs);
    let th = median(merged.iter().map(|o| o.text_height()).filter(|&h| h > 0.0).collect()).unwrap_or(20.0);
    let scale_bar = scale_bar_members(&merged, th);
    let legend = legend_members(&merged, th);
    let mut verdicts: Vec<Verdict> =
        merged.iter().enumerate().map(|(i, o)| verdict(o, scale_bar[i] || legend[i], th)).collect();
    drop_fragments(&merged, &mut verdicts);
    let mut out: Vec<PoiLabel> = verdicts
        .into_iter()
        .filter_map(|v| match v {
            Verdict::Poi(p) => Some(p),
            _ => None,
        })
        .collect();

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
/// grid line cuts through:
///
/// * on a sheet whose heights nearly all have the same number of digits (at least 4 heights,
///   80 % alike), a height with fewer digits and under half the median is not believable;
/// * on a sheet with at least `MIN_FOUR_DIGIT_HEIGHTS` heights of four digits, a three-digit
///   height `h` well below all of them (more than `TRUNCATED_HEIGHT_MARGIN_M` under the lowest)
///   is a misread when 1000 + h or 2000 + h falls among them (their range widened by that
///   margin): "478" for 1478 on a sheet of 1100–1800 m heights, however many such misreads
///   there are.
///
/// Sheets that span lowland and mountain keep their short heights.
fn drop_truncated_heights(pois: &mut Vec<PoiLabel>) {
    let digits = |h: f64| (h.abs().trunc() as u64).to_string().len();
    let heights = |pois: &[PoiLabel]| -> Vec<f64> {
        pois.iter().filter(|p| p.kind == PoiKind::SpotHeight).filter_map(|p| p.elevation_m).collect()
    };
    let four: Vec<f64> = heights(pois).into_iter().filter(|&h| h >= 1000.0).collect();
    if four.len() >= MIN_FOUR_DIGIT_HEIGHTS {
        let lo = four.iter().copied().fold(f64::INFINITY, f64::min) - TRUNCATED_HEIGHT_MARGIN_M;
        let hi = four.iter().copied().fold(f64::NEG_INFINITY, f64::max) + TRUNCATED_HEIGHT_MARGIN_M;
        let truncated = |h: f64| {
            (100.0..1000.0).contains(&h) && h < lo && [1000.0, 2000.0].iter().any(|k| (lo..=hi).contains(&(k + h)))
        };
        pois.retain(|p| p.kind != PoiKind::SpotHeight || !p.elevation_m.is_some_and(truncated));
    }
    let heights = heights(pois);
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

/// `drop_truncated_heights`: four-digit heights needed to judge three-digit ones…
const MIN_FOUR_DIGIT_HEIGHTS: usize = 3;
/// …and how far beyond their range (metres) a truncated height may point.
const TRUNCATED_HEIGHT_MARGIN_M: f64 = 250.0;

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
        // Reads of the real photo fixture by Apple Vision.
        assert!((lat("25 5100S") + 25.0 + 51.0 / 60.0).abs() < 1e-9);
        assert!((lat("25°52 00\"$") + 25.0 + 52.0 / 60.0).abs() < 1e-9);
        assert!((lon("27°24'00\"€") - (27.0 + 24.0 / 60.0)).abs() < 1e-9);
        assert!((lat("25°5000\"S") + 25.0 + 50.0 / 60.0).abs() < 1e-9);
        // "2400" after "24-" is minutes and seconds, not 24° 00' (a dash for the degree mark).
        assert!((lon("24-2400\"E") - (24.0 + 24.0 / 60.0)).abs() < 1e-9);
        for junk in [
            "GIS V1 Sept 2017- Eras Gous",
            "Magnetic Declination 18° West",
            "Sept 2017",
            "12 34 Street",
            "2017 5100S",
            "Price $25",
        ] {
            assert_eq!(parse_coordinate(junk), None, "{junk}");
        }
    }

    #[test]
    fn garbled_dms_labels() {
        let dm = |d: f64, m: f64| d + m / 60.0;
        // RapidOCR reads of the MCSA sheets' printed graticule labels (fixtures/castle-gorge,
        // fixtures/mhlabatini): marks read as other symbols, digits or letters, or lost.
        for (t, want) in [
            ("25°47/00\"S", CoordValue::Lat(-dm(25.0, 47.0))),
            ("25°48'00~s", CoordValue::Lat(-dm(25.0, 48.0))),
            ("25°48/00\\S", CoordValue::Lat(-dm(25.0, 48.0))),
            ("25°49/00*s", CoordValue::Lat(-dm(25.0, 49.0))),
            ("25-4900s", CoordValue::Lat(-dm(25.0, 49.0))),
            ("25-49004s", CoordValue::Lat(-dm(25.0, 49.0))),
            ("2595000\"s", CoordValue::Lat(-dm(25.0, 50.0))),
            ("255000s", CoordValue::Lat(-dm(25.0, 50.0))),
            ("25950l00s", CoordValue::Lat(-dm(25.0, 50.0))),
            ("2548100s", CoordValue::Lat(-dm(25.0, 48.0))),
            ("25/48100S", CoordValue::Lat(-dm(25.0, 48.0))),
            ("254810011s", CoordValue::Lat(-dm(25.0, 48.0))),
            ("25°4900*s", CoordValue::Lat(-dm(25.0, 49.0))),
            ("25°51'00/s", CoordValue::Lat(-dm(25.0, 51.0))),
            ("/2551/00/s", CoordValue::Lat(-dm(25.0, 51.0))),
            ("2552100/s", CoordValue::Lat(-dm(25.0, 52.0))),
            ("25°4900*", CoordValue::Degrees(dm(25.0, 49.0))),
            ("25°51/00/", CoordValue::Degrees(dm(25.0, 51.0))),
            ("2725l00*", CoordValue::Degrees(dm(27.0, 25.0))),
            ("2734,00\" GISV1Sept2017-ErasGous", CoordValue::Degrees(dm(27.0, 34.0))),
            ("27°3400GIS V1 Sept 2017- Eras Gous", CoordValue::Degrees(dm(27.0, 34.0))),
            ("2553 00/", CoordValue::Degrees(dm(25.0, 53.0))),
            ("273700\"", CoordValue::Degrees(dm(27.0, 37.0))),
            ("2791800*", CoordValue::Degrees(dm(27.0, 18.0))),
            ("27018’004 GISV1Sept2017-ErasGous", CoordValue::Degrees(dm(27.0, 18.0))),
            ("25048,00*S", CoordValue::Lat(-dm(25.0, 48.0))),
            ("25°53'QoS", CoordValue::Lat(-dm(25.0, 53.0))),
            ("27°37Q0\"E", CoordValue::Lon(dm(27.0, 37.0))),
            ("27°20'0Q\"E", CoordValue::Lon(dm(27.0, 20.0))),
            ("27936'00\"E", CoordValue::Lon(dm(27.0, 36.0))),
            ("27\"3500\"E", CoordValue::Lon(dm(27.0, 35.0))),
            ("27%3400cEV1Sept2017-ErasGous", CoordValue::Lon(dm(27.0, 34.0))),
            ("27%34/00cEs V1 sept 2017", CoordValue::Lon(dm(27.0, 34.0))),
            ("27%34'00cEV1 Sept 201", CoordValue::Lon(dm(27.0, 34.0))),
            ("27*34/00\"E GIS V1 Sept 2017- Eras Gous", CoordValue::Lon(dm(27.0, 34.0))),
            ("GIS V1 Sept 2017- Eras Gous 27*19/00\"E", CoordValue::Lon(dm(27.0, 19.0))),
            ("25*48/00~S", CoordValue::Lat(-dm(25.0, 48.0))),
            ("27°36'00~E", CoordValue::Lon(dm(27.0, 36.0))),
            // RapidOCR reads of fixtures/grootkloof.
            ("25*51/00\"1s", CoordValue::Lat(-dm(25.0, 51.0))),
            ("25°51/001S", CoordValue::Lat(-dm(25.0, 51.0))),
            ("2595300/S", CoordValue::Lat(-dm(25.0, 53.0))),
            ("2552/00*s", CoordValue::Lat(-dm(25.0, 52.0))),
            ("GISV Sept20t7-Eras Gous27°23'00\"E", CoordValue::Lon(dm(27.0, 23.0))),
        ] {
            let got = parse_coordinate(t);
            assert!(close(got, Some(want)), "{t:?} -> {got:?}, want {want:?}");
            assert_eq!(
                coordinate_readings(t).first().map(|r| r.value).filter(|&v| same_value(v, want)),
                Some(want),
                "{t}"
            );
        }
        // Read as printed, and with a mark misread: the grid fit picks the reading that agrees.
        for (t, printed, garbled) in [
            ("25-50'00\"S", CoordValue::Lat(-50.0), CoordValue::Lat(-dm(25.0, 50.0))),
            ("272400E", CoordValue::Grid(272_400.0), CoordValue::Lon(dm(27.0, 24.0))),
        ] {
            let r: Vec<CoordValue> = coordinate_readings(t).into_iter().map(|r| r.value).collect();
            assert!(r.len() == 2 && same_value(r[0], printed) && same_value(r[1], garbled), "{t:?} -> {r:?}");
        }
        // Fragments, other text and two labels in one read stay out.
        for t in [
            "25°49/003",
            "8'00",
            "200/",
            "\"E",
            "12 34 Street",
            "12 34 56 Street",
            "25 53 00",
            "Sept 2017-E",
            "1:25 000",
            "Annual Change: 4.8' per year West",
            "25°49/00\"S 27°23/00\"E",
            "MCSA MAGALIESBERG SHEET 3 OF 10",
            "11:30:00 S",
        ] {
            assert_eq!(parse_coordinate(t), None, "{t:?}");
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
    fn three_digit_misreads_of_mountain_heights_are_dropped() {
        let at = |hs: &[&str]| -> Vec<OcrObservation> {
            hs.iter()
                .enumerate()
                .map(|(k, t)| ob(t, 200.0 + 300.0 * k as f64, 400.0 + 97.0 * k as f64, 54.0, 20.0))
                .collect()
        };
        let names = |p: Vec<PoiLabel>| p.into_iter().map(|q| q.name).collect::<Vec<_>>();
        // Magaliesberg: 1100–1800 m, with as many misreads as heights read whole (the 80 % rule
        // gives up on such a sheet).
        let p = classify_pois(&at(&["1478", "514", "1283", "649", "1716", "341", "1554", "132", "478", "1141"]));
        assert_eq!(names(p), vec!["1478", "1283", "1716", "1554", "1141"]);
        // Lowland and mountain on one sheet: fewer than three four-digit heights.
        let p = classify_pois(&at(&["112", "348", "905", "1210", "1488", "760"]));
        assert_eq!(p.len(), 6);
        // Hills around 1000 m: three-digit heights close to the four-digit ones are real.
        let p = classify_pois(&at(&["1012", "1043", "1121", "987", "912", "844", "763"]));
        assert_eq!(p.len(), 7, "{:?}", names(p));
        // Low three-digit heights far below them that point nowhere into their range stay too.
        let p = classify_pois(&at(&["1012", "1043", "1121", "623"]));
        assert_eq!(p.len(), 4);
    }

    #[test]
    fn pieces_of_one_label_are_one_poi() {
        let obs = vec![
            ob("KUDUKONA", 397.0, 422.0, 145.0, 23.0),
            ob("UDU", 409.0, 427.0, 41.0, 19.0),
            ob("1283", 1033.0, 578.0, 53.0, 26.0),
            ob("283", 1037.0, 579.0, 47.0, 24.0),
            ob("KRAAL", 2143.0, 2340.0, 73.0, 24.0),
            ob("KRAAZ", 2135.0, 2338.0, 83.0, 27.0),
            ob("RANDWATERBOX2", 1553.0, 2259.0, 245.0, 23.0),
            ob("RAN", 1551.0, 2258.0, 48.0, 26.0),
            ob("UPPERCASTLEGUL", 380.0, 2539.0, 205.0, 205.0),
            ob("CASTLI", 443.0, 2593.0, 88.0, 88.0),
            ob("TRIGBEACON", 1000.0, 1000.0, 167.0, 23.0),
            ob("TRIG BEACON", 1002.0, 1000.0, 170.0, 31.0),
            // A read that is no label does not swallow the height inside it.
            ob("21728", 3300.0, 3800.0, 80.0, 41.0),
            ob("1728", 3323.0, 3807.0, 54.0, 28.0),
            // The same name elsewhere on the sheet is another POI.
            ob("KUDUKONA", 2400.0, 3000.0, 145.0, 23.0),
            ob("UDU", 3000.0, 4000.0, 41.0, 19.0),
        ];
        let mut obs = obs;
        obs[4].confidence = 0.95;
        let names: Vec<String> = classify_pois(&obs).into_iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec!["KUDUKONA", "1283", "KRAAL", "RANDWATERBOX2", "UPPERCASTLEGUL", "1728", "KUDUKONA", "UDU"]
        );
    }

    #[test]
    fn legend_pieces_and_unreadable_words_are_not_pois() {
        let obs = vec![
            ob("Non-Peren", 492.0, 4342.0, 136.0, 28.0),
            ob("Non-Perennial River", 492.0, 4344.0, 251.0, 24.0),
            ob("PerennialRiver", 493.0, 4380.0, 187.0, 23.0),
            ob("DryWaterCourse", 494.0, 4310.0, 222.0, 23.0),
            ob("Police Stati", 490.0, 4519.0, 140.0, 29.0),
            ob("100m Inter", 491.0, 4730.0, 137.0, 29.0),
            ob("Place of w", 491.0, 4625.0, 137.0, 29.0),
            ob("HHL", 177.0, 4091.0, 102.0, 103.0),
            ob("Pws", 170.0, 4608.0, 34.0, 105.0),
            ob("GUN", 520.0, 2538.0, 59.0, 26.0),
            ob("Hut", 2000.0, 2000.0, 50.0, 25.0),
            ob("Elm Hut", 2400.0, 2000.0, 100.0, 25.0),
            ob("AASVOELKRANS", 335.0, 2640.0, 205.0, 26.0),
        ];
        let names: Vec<String> = classify_pois(&obs).into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["GUN", "Elm Hut", "AASVOELKRANS"]);
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
