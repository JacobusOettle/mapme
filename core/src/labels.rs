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
/// projected grid values (540000, 540 000mE); failing those, a garbled DMS
/// label whose cheapest readings agree (`coordinate_readings`). Plain 2–4
/// digit numbers (spot heights, abbreviated grid labels) are not
/// coordinates here; `grid.rs` resolves abbreviated grid labels against
/// detected lines.
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
        // Nor a degree, minute or second sign: "°53'00\"S" is the end of a label cut off.
        if start >= 1 && matches!(c[start - 1], DEG | MIN | SEC) {
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
pub(crate) const META_WORDS: &[&str] = &[
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
pub(crate) const LEGEND_WORDS: &[&str] = &[
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

/// Words of signage and route notes ("NO ENTRY SIGN", "KEEP LEFT", "TURN RIGHT AT SIGN") and of
/// what a name says a place is ("TENTED CAMP", "STAFF HOUSES"): never a place to geocode
/// (`hints::hint_queries`), but real words (`Vocabulary::known`).
pub(crate) const NOTE_WORDS: &[&str] = &[
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

/// Common words of place names besides `LEGEND_WORDS` and `NOTE_WORDS`: features and their
/// qualifiers, in English and Afrikaans. Only used to tell a well-read name from a misread one
/// (`Vocabulary::known`) and to complete a word cut short; never to call a label legend text.
pub(crate) const PLACE_WORDS: &[&str] = &[
    "kloof",
    "kloofs",
    "gully",
    "gullies",
    "ridge",
    "krans",
    "kranse",
    "kranz",
    "krantz",
    "kop",
    "kopje",
    "koppie",
    "berg",
    "gorge",
    "ravine",
    "valley",
    "vallei",
    "vlakte",
    "vlei",
    "poort",
    "nek",
    "neck",
    "pass",
    "spruit",
    "rivier",
    "fontein",
    "waterval",
    "falls",
    "stream",
    "creek",
    "pan",
    "plateau",
    "cliff",
    "cliffs",
    "rock",
    "rocks",
    "klip",
    "bos",
    "wood",
    "woods",
    "farm",
    "plaas",
    "kraal",
    "hotel",
    "house",
    "centre",
    "center",
    "estate",
    "reserve",
    "mount",
    "mountain",
    "mountains",
    "long",
    "short",
    "big",
    "little",
    "great",
    "middle",
    "high",
    "old",
    "new",
    "groot",
    "klein",
    "rooi",
    "wit",
    "swart",
    "blou",
    "hoog",
    "nuwe",
    "saint",
];

/// A word (lowercase) of `LEGEND_WORDS`, `NOTE_WORDS` or `PLACE_WORDS`.
pub(crate) fn lexicon_word(w: &str) -> bool {
    LEGEND_WORDS.contains(&w) || NOTE_WORDS.contains(&w) || PLACE_WORDS.contains(&w)
}

/// The words of a sheet's OCR, with the number of reads each one is in: a word read in two
/// places (or twice) is no misread of the moment.
pub(crate) struct Vocabulary {
    counts: std::collections::HashMap<String, usize>,
}

impl Vocabulary {
    pub(crate) fn new(obs: &[OcrObservation]) -> Vocabulary {
        let mut counts = std::collections::HashMap::new();
        for o in obs {
            let mut words: Vec<String> = words_of(&o.text).iter().map(|w| fold_accents(w)).collect();
            words.sort();
            words.dedup();
            for w in words {
                *counts.entry(w).or_insert(0) += 1;
            }
        }
        Vocabulary { counts }
    }

    /// Reads holding the (lowercase) word, accents aside.
    fn count(&self, w: &str) -> usize {
        self.counts.get(&fold_accents(w)).copied().unwrap_or(0)
    }

    /// A real word (lowercase): one of the lexicon, or one of four letters or more read in two
    /// reads or more (accents aside: "AASVOËLKRANS" and "AASVOELKRANS RESORT"). Shorter pieces
    /// repeat too easily ("GUL" beside "EASY GUL").
    pub(crate) fn known(&self, w: &str) -> bool {
        w.chars().all(char::is_alphabetic) && (lexicon_word(w) || (w.chars().count() >= 4 && self.count(w) >= 2))
    }

    /// Letters of a text in known words of two letters or more: how dictionary-like a read is.
    pub(crate) fn known_letters(&self, text: &str) -> usize {
        words_of(text).iter().filter(|w| w.chars().count() >= 2 && self.known(w)).map(|w| w.chars().count()).sum()
    }

    /// The one lexicon word on the sheet that `stem` (lowercase, cut short by the OCR) is the
    /// start of, one to three letters short: "gul" → "gully" when "WOODY GULLY" is on the sheet.
    /// None when no such word or several are. A name read twice completes nothing: "RAND" (of
    /// "RAND WATER BOX 1" and "2") is no end of "LIGHTNING RA…" ("LIGHTNING RAVINE").
    fn completion(&self, stem: &str) -> Option<&str> {
        let stem = fold_accents(stem);
        let n = stem.chars().count();
        let mut found = self.counts.keys().filter(|w| {
            let m = w.chars().count();
            m > n && m <= n + 3 && w.starts_with(&stem) && lexicon_word(w)
        });
        let first = found.next()?;
        found.next().is_none().then_some(first.as_str())
    }
}

/// A lowercase word without its accents ("aasvoël" → "aasvoel").
fn fold_accents(w: &str) -> String {
    w.chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ç' | 'č' => 'c',
            'ñ' => 'n',
            'š' => 's',
            'ž' => 'z',
            c => c,
        })
        .collect()
}

/// Letters that start words (lowercase): any two with a vowel, or two consonants of this list
/// (English, Afrikaans, Nguni, Sotho-Tswana, Welsh, Norse, Slavic and other European spellings:
/// "FFOREST FAWR", "MTHATHA", "KGALAGADI", "KJERAG", "ZLÍN"). A word starting otherwise ("WC",
/// "KD") has a stray glyph in front.
fn plausible_onset(a: char, b: char) -> bool {
    const ONSETS: &[&str] = &[
        "bh", "bj", "bl", "br", "ch", "cl", "cr", "cz", "dh", "dj", "dl", "dn", "dr", "dv", "dw", "dz", "ff", "fj",
        "fl", "fr", "gb", "gc", "gd", "gh", "gj", "gl", "gn", "gq", "gr", "gs", "gw", "gx", "hj", "hl", "hr", "hv",
        "hw", "kg", "kh", "kj", "kl", "kn", "kp", "kr", "ks", "kv", "kw", "lh", "lj", "ll", "mb", "mc", "mf", "mg",
        "mh", "mk", "ml", "mm", "mn", "mp", "mq", "ms", "mt", "mv", "mz", "nc", "nd", "ng", "nh", "nj", "nk", "nq",
        "ns", "nt", "nx", "ny", "nz", "pf", "ph", "pl", "pn", "pr", "ps", "pt", "qh", "rh", "sc", "sh", "sj", "sk",
        "sl", "sm", "sn", "sp", "sq", "st", "sv", "sw", "sz", "tb", "th", "tj", "tl", "tr", "ts", "tv", "tw", "tx",
        "tz", "vl", "vr", "vs", "wh", "wr", "xh", "zb", "zd", "zh", "zl", "zn", "zv", "zw",
    ];
    let vowel = |c: char| "aeiouyàáâäèéêëìíîïòóôöùúûü".contains(c);
    vowel(a) || vowel(b) || !a.is_alphabetic() || !b.is_alphabetic() || ONSETS.iter().any(|o| *o == format!("{a}{b}"))
}

/// Edit distance (substitutions, insertions, deletions) between two texts.
fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, &x) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, &y) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(x != y)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

const MONTHS: &[&str] = &[
    "jan",
    "january",
    "feb",
    "february",
    "mar",
    "march",
    "apr",
    "april",
    "may",
    "jun",
    "june",
    "jul",
    "july",
    "aug",
    "august",
    "sep",
    "sept",
    "september",
    "oct",
    "october",
    "nov",
    "november",
    "dec",
    "december",
];

/// Text of an imprint or a credit: a month next to a year ("Sept 2017", "Sept2017") or a
/// version tag ("V1", "GISV1") with a month or a year in the text ("ISV1 Sept 2017").
pub(crate) fn imprint_text(text: &str) -> bool {
    let toks: Vec<String> = text
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    let year =
        |t: &str| t.len() == 4 && t.chars().all(|c| c.is_ascii_digit()) && (t.starts_with("19") || t.starts_with("20"));
    let month = |t: &str| MONTHS.contains(&t);
    let glued = |t: &str| {
        let lead = t.trim_end_matches(|c: char| c.is_ascii_digit());
        month(lead) && year(&t[lead.len()..])
    };
    let version = |t: &str| {
        let lead = t.trim_end_matches(|c: char| c.is_ascii_digit());
        lead.len() < t.len() && t.len() - lead.len() <= 2 && lead.ends_with('v') && lead.len() <= 4
    };
    let dated = toks.iter().any(|t| month(t) || year(t) || glued(t));
    toks.windows(2).any(|w| (month(&w[0]) && year(&w[1])) || (year(&w[0]) && month(&w[1])))
        || toks.iter().any(|t| glued(t))
        || (dated && toks.iter().any(|t| version(t)))
}

/// A distance at the end of a name ("1.7Km", "0.6km)", "2,3 km"): a route note beside it.
fn distance_token(t: &str) -> bool {
    let t = t.trim_matches(|c: char| "()[],;".contains(c)).to_lowercase();
    let num = t.trim_end_matches(|c: char| c.is_alphabetic());
    let unit = &t[num.len()..];
    let number = !num.is_empty()
        && num.chars().any(|c| c.is_ascii_digit())
        && num.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ',');
    let decimal = num.contains(['.', ',']);
    number && (matches!(unit, "km" | "mi" | "nm") || (decimal && matches!(unit, "m" | "ft")))
}

const PEAK_WORDS: &[&str] = &["peak", "kop", "berg", "hill", "mount", "mt", "piek", "spitz", "kopje", "summit"];
const WATER_WORDS: &[&str] =
    &["falls", "waterfall", "spring", "pool", "dam", "lake", "river", "rivier", "fontein", "creek", "stream", "spruit"];
/// Suffixes of compound (Afrikaans/Dutch/German) names, e.g. "Spitzkop", "Sterkfontein".
const PEAK_SUFFIXES: &[&str] = &["kop", "kopje", "berg", "piek", "spitz"];
const WATER_SUFFIXES: &[&str] = &["fontein", "rivier", "spruit", "waterval"];

pub(crate) fn words_of(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.to_lowercase()).collect()
}

/// "1428", "1428m", "1428.5 m" → elevation.
pub(crate) fn parse_height(text: &str) -> Option<f64> {
    parse_height_in(text, HeightUnit::Metres)
}

/// Unit of the heights printed on a map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeightUnit {
    Metres,
    Feet,
}

/// Metres per international foot.
pub const FOOT_M: f64 = 0.3048;

impl HeightUnit {
    /// Metres per printed unit.
    pub fn metres(self) -> f64 {
        match self {
            HeightUnit::Metres => 1.0,
            HeightUnit::Feet => FOOT_M,
        }
    }
}

/// A printed height in `unit` (the value as printed, not converted): three
/// or four digits in metres ("1428", "1428m", "1428.5 m"), three to five in
/// feet ("10876", "6316 ft"), never a multiple of 50 (contour labels), at
/// most Everest's height.
pub(crate) fn parse_height_in(text: &str, unit: HeightUnit) -> Option<f64> {
    let t = text.trim();
    let t = match unit {
        HeightUnit::Metres => t.strip_suffix('m').or_else(|| t.strip_suffix('M')).unwrap_or(t),
        HeightUnit::Feet => t.strip_suffix("ft").or_else(|| t.strip_suffix("FT")).unwrap_or(t),
    }
    .trim_end();
    let (int, frac) = match t.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (t, None),
    };
    let digits = match unit {
        HeightUnit::Metres => 3..=4,
        HeightUnit::Feet => 3..=5,
    };
    // No height is printed with a leading zero ("0348" is "10348" cut short).
    if !digits.contains(&int.len()) || !int.chars().all(|c| c.is_ascii_digit()) || int.starts_with('0') {
        return None;
    }
    if let Some(f) = frac {
        if f.is_empty() || f.len() > 1 || !f.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
    }
    let v: f64 = t.parse().ok()?;
    let iv = int.parse::<i64>().ok()?;
    if !(-400.0..=8850.0).contains(&(v * unit.metres())) || iv % 50 == 0 {
        return None;
    }
    Some(v)
}

/// The unit of the heights printed on a map, from its text: a note about
/// heights or contours with its unit ("Gaia Topo (feet)", "CONTOUR INTERVAL
/// 40 FEET", "Elevations in metres"), or five-digit heights, which no metre
/// height reaches (two or more "10876"s). None when nothing says: the
/// terrain may tell (`georef`, spot heights against the DEM). A scale bar's
/// "500 ft" or "1 km" says nothing about heights.
pub fn height_unit(obs: &[OcrObservation]) -> Option<HeightUnit> {
    let (mut feet, mut metres) = (0usize, 0usize);
    for o in obs {
        let lower = o.text.to_lowercase();
        let words = words_of(&o.text);
        let has = |set: &[&str]| words.iter().any(|w| set.contains(&w.as_str()));
        let about_heights = has(&[
            "contour",
            "contours",
            "interval",
            "elevation",
            "elevations",
            "height",
            "heights",
            "altitude",
            "topo",
        ]) || lower.contains("(feet)")
            || lower.contains("(ft)")
            || lower.contains("(metres)")
            || lower.contains("(meters)");
        if about_heights {
            if has(&["feet", "foot", "ft"]) {
                feet += 1;
            }
            if has(&["metres", "meters", "metre", "meter", "m"]) {
                metres += 1;
            }
        }
    }
    // Most of the heights, and two at least: a garbled read ("11283" for
    // 1283) is no unit.
    let five_digit = obs
        .iter()
        .filter(|o| {
            let t = o.text.trim();
            t.len() == 5
                && parse_height_in(t, HeightUnit::Feet).is_some()
                && parse_height_in(t, HeightUnit::Metres).is_none()
        })
        .count();
    let heights = obs.iter().filter(|o| parse_height_in(o.text.trim(), HeightUnit::Feet).is_some()).count();
    if five_digit >= 2 && 2 * five_digit > heights {
        feet += 1;
    }
    match feet.cmp(&metres) {
        std::cmp::Ordering::Greater => Some(HeightUnit::Feet),
        std::cmp::Ordering::Less => Some(HeightUnit::Metres),
        std::cmp::Ordering::Equal => None,
    }
}

/// Credits, attribution and projection notes of map apps and tile services
/// ("Gaia Topo (feet): © Gaia GPS, OpenStreetMap", the "GAIAGPS" logo, "Web
/// Mercator | EPSG: 3857", "© Mapbox © OpenStreetMap contributors"): map
/// furniture, never a place name.
pub(crate) fn credit_text(text: &str) -> bool {
    /// Letter runs (in the text's letters, lower case, spaces and symbols
    /// dropped) of brands and attribution: cut short by the OCR too
    /// ("OpenStreetN").
    const RUNS: &[&str] = &[
        "openstreet",
        "gaiagps",
        "gaiatopo",
        "caltopo",
        "alltrails",
        "mapbox",
        "maptiler",
        "thunderforest",
        "opentopomap",
        "outdooractive",
        "webmercator",
        "contributors",
        "naturalearth",
        "swisstopo",
        "ordnancesurvey",
    ];
    /// Whole words.
    const WORDS: &[&str] = &["epsg", "osm", "esri", "mercator", "wgs84", "nad83", "nad27", "attribution", "komoot"];
    let letters: String = text.chars().filter(|c| c.is_alphabetic()).flat_map(char::to_lowercase).collect();
    RUNS.iter().any(|r| letters.contains(r)) || words_of(text).iter().any(|w| WORDS.contains(&w.as_str()))
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
pub(crate) fn scale_bar_members(obs: &[OcrObservation], th: f64) -> Vec<bool> {
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
pub(crate) fn legend_members(obs: &[OcrObservation], th: f64) -> Vec<bool> {
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

pub(crate) fn median(mut v: Vec<f64>) -> Option<f64> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

/// Does the text look like part of a coordinate ("4.8'", "18°")?
pub(crate) fn has_angle_mark(text: &str) -> bool {
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

/// A word of legend text (as printed, any case): one of `LEGEND_WORDS`, the
/// start of one cut off by the OCR ("Non-Peren", "Police Stati", "100m
/// Inter"), several run together with their capitals ("PerennialRiver",
/// "DryWaterCourse"; a place name such as "Lakeview" or "PARKVIEW" is one
/// word), a number, a measure or a single letter.
fn generic_word(word: &str) -> bool {
    let lower = word.to_lowercase();
    let w = lower.as_str();
    if w.chars().count() <= 1 || w.chars().all(|c| c.is_ascii_digit()) || is_measure(w) {
        return true;
    }
    let cut = |l: &&str| l.len() > w.len() && l.starts_with(w);
    if LEGEND_WORDS.contains(&w) || (w.len() >= 4 && LEGEND_WORDS.iter().chain(META_WORDS).any(cut)) {
        return true;
    }
    // Legend words run together: every split point reachable from the start, each one a
    // capital after a lowercase letter.
    let orig: Vec<char> = word.chars().collect();
    let low: Vec<char> = orig.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let n = low.len();
    let mut reach = vec![false; n + 1];
    reach[0] = true;
    for i in 0..n {
        if !reach[i] || (i > 0 && !(orig[i].is_uppercase() && orig[i - 1].is_lowercase())) {
            continue;
        }
        for l in LEGEND_WORDS.iter().filter(|l| l.len() >= 3) {
            let l: Vec<char> = l.chars().collect();
            if low[i..].starts_with(&l) {
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

/// What one read is. `unit`: the map's height unit (heights stay in it here,
/// as printed; `classify_pois_reads` converts them to metres at the end).
fn verdict(o: &OcrObservation, furniture: bool, th: f64, unit: HeightUnit) -> Verdict {
    // Large lettering is the title.
    if furniture || letter_height(o) > TITLE_TEXT_HEIGHT * th {
        return Verdict::Furniture;
    }
    let text = o.text.as_str();
    let parse_height = |t: &str| parse_height_in(t, unit);
    // A grid value is round: a five-digit height in feet ("10305") is no easting.
    let coordinate = match parse_coordinate(text) {
        Some(CoordValue::Grid(_)) => parse_height(text).is_none(),
        c => c.is_some(),
    };
    if coordinate || parse_scale(text).is_some() || has_angle_mark(text) || credit_text(text) {
        return Verdict::Furniture;
    }
    let (x, y) = o.center();
    // A height, or one with its triangle read beside it ("1501 A", "▲ 1720"); an "A" in front is
    // a road's ("A 470").
    let height = parse_height(text).map(|h| (h, text.to_string())).or_else(|| {
        let t: Vec<&str> = text.split_whitespace().collect();
        let symbol = |s: &str| matches!(s, "^" | "Λ" | "Δ" | "△" | "▲");
        match t.as_slice() {
            [a, b] if symbol(a) => parse_height(b).map(|h| (h, b.to_string())),
            [a, b] if symbol(b) || *b == "A" => parse_height(a).map(|h| (h, a.to_string())),
            _ => None,
        }
    });
    if let Some((h, name)) = height {
        return Verdict::Poi(PoiLabel { name, kind: PoiKind::SpotHeight, elevation_m: Some(h), x, y });
    }
    if text.contains('©') || text.to_lowercase().contains("(c)") || imprint_text(text) {
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
            t.parse::<f64>().ok().filter(|v| t.len() >= 3 && (-400.0..=8850.0).contains(&(v * unit.metres())))
        }) {
            elevation = Some(h);
            let keep = if tail.len() > raw[raw.len() - 1].len() { raw.len() - 2 } else { raw.len() - 1 };
            name = raw[..keep].join(" ");
            words = words_of(&name);
        }
    }
    // A route distance printed beside the name ("STILE 1.7Km", "2,3 km").
    let mut raw: Vec<String> = name.split_whitespace().map(str::to_string).collect();
    loop {
        let n = raw.len();
        if n >= 2 && distance_token(&raw[n - 1]) {
            raw.pop();
        } else if n >= 3
            && matches!(raw[n - 1].to_lowercase().as_str(), "km" | "mi" | "nm")
            && distance_token(&format!("{}km", raw[n - 2]))
        {
            raw.truncate(n - 2);
        } else {
            break;
        }
    }
    if raw.len() < name.split_whitespace().count() {
        name = raw.join(" ");
        words = words_of(&name);
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
    // A feature with its number is a place ("CAMP 2", "HUT 4"), not legend text ("Camp", "2 km").
    let numbered =
        name.split_whitespace().last().is_some_and(|t| t.len() <= 2 && t.chars().all(|c| c.is_ascii_digit()))
            && words.iter().any(|w| w.chars().count() >= 3 && w.chars().all(char::is_alphabetic) && !is_measure(w));
    if !numbered && name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).all(generic_word) {
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
    let upright = o.bbox[3] >= 1.5 * o.bbox[2];
    if letters <= 3 {
        // A short read with no vowel or no consonant is no word ("HHL", "Pws", "AAI"; "EYE" is,
        // its Y a consonant); three letters read up or down the map are a piece of a longer label
        // printed along a stream or slope ("ERN", "ADE", "KLO"). Four are a name as often ("ISAR",
        // "TEES" along their rivers): `drop_garbled_turned` tells pieces ("ANTZ") by their neighbours.
        let vowel = |c: char| "aeiouyAEIOUY".contains(c);
        let alpha: Vec<char> = name.chars().filter(|c| c.is_alphabetic()).collect();
        if !alpha.iter().any(|&c| vowel(c)) || alpha.iter().all(|&c| "aeiouAEIOU".contains(c)) || upright {
            return Verdict::Junk;
        }
    }
    // A lone abbreviation is no name ("KUMB." of a name the frame cuts).
    if words.len() == 1 && letters <= 4 && name.ends_with('.') && !lexicon_word(&words[0]) {
        return Verdict::Junk;
    }
    let name = split_glued(&name);
    let words = words_of(&name);
    Verdict::Poi(PoiLabel { name, kind: poi_kind(&words), elevation_m: elevation, x, y })
}

/// Two words the OCR ran together, the second's capital showing where
/// ("MountDurnford" → "Mount Durnford"), when one of them is a feature or
/// lexicon word of three letters or more; a name's own capitals stay
/// ("KaMasihlenga", "uKhahlamba", "McGregor", "DeHoop").
fn split_glued(name: &str) -> String {
    let feature = |w: &str| {
        let l = w.to_lowercase();
        l.chars().count() >= 3
            && (lexicon_word(&l) || PEAK_WORDS.contains(&l.as_str()) || WATER_WORDS.contains(&l.as_str()))
    };
    let mut out: Vec<String> = Vec::new();
    for tok in name.split_whitespace() {
        let c: Vec<char> = tok.chars().collect();
        let cut = (1..c.len()).find(|&k| {
            c[k].is_uppercase() && c[k - 1].is_lowercase() && k >= 3 && c.len() - k >= 3 && {
                let (a, b): (String, String) = (c[..k].iter().collect(), c[k..].iter().collect());
                a.chars().all(char::is_alphabetic) && b.chars().all(char::is_alphabetic) && (feature(&a) || feature(&b))
            }
        });
        match cut {
            Some(k) => {
                out.push(c[..k].iter().collect());
                out.push(c[k..].iter().collect());
            }
            None => out.push(tok.to_string()),
        }
    }
    out.join(" ")
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
pub(crate) fn longest_common_run(a: &[char], b: &[char]) -> usize {
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

/// Does the box read as a line of text across the map (much wider than tall), not up, down or
/// slanting along a stream or a slope?
pub(crate) fn reads_across(o: &OcrObservation) -> bool {
    text_reads_across(o, &o.text)
}

/// `reads_across` for `text`, the part of the read that matters ("STILE" of "STILE 1.7Km").
fn text_reads_across(o: &OcrObservation, text: &str) -> bool {
    let n = text.chars().filter(|c| !c.is_whitespace()).count().max(1) as f64;
    o.bbox[2] >= o.bbox[3] * (0.3 * n).clamp(1.5, 4.0)
}

/// Words that take the name after them ("RIVER CAM", "RÍO PAS", "LOCH EIL", "ST LO"): a short
/// word behind one is the name, not a word cut short.
const NAME_AFTER: &[&str] = &[
    "river", "rio", "lake", "loch", "lough", "llyn", "mount", "mt", "ben", "glen", "cape", "port", "fort", "isle",
    "st", "saint", "san", "santa", "lac", "lago", "monte", "mont", "pic", "pico", "piz", "col", "punta", "cerro",
    "sierra", "baie", "ile", "laguna", "riviere", "rivier", "kaap",
];

/// Lexicon words whose accented spelling is the right one, in French: kept as read.
const ACCENTED_SPELLINGS: &[&str] = &["hôtel", "hôtels", "réserve", "réserves"];

/// Mend a name Apple Vision read with a symbol or cut short, using the sheet's words
/// (`Vocabulary`) as the dictionary:
///
/// * a lexicon word read with an accent loses it ("GÁTE", "AČCOMMODATION"; not "HÔTEL",
///   `ACCENTED_SPELLINGS`);
/// * a stray glyph glued in front goes: a digit before a word ("1KWAMI LODGE"), a lone consonant
///   before the words ("D LONG POOL": the pool's symbol; a vowel or a Y before a known word too,
///   "O LONG POOL", else it is an article: "A CORUÑA", "Y GARN"), or a letter in front of a known
///   word that cannot start a word ("WCASTLE GORGE WATERFALL": the waterfall's W;
///   `plausible_onset`) or that another read of the label at the same place (`echoes`, their
///   `fragment_key`s) is without ("PLONG POOL" over "LONG POOL"); a stroke glued behind a known
///   word likewise ("FERN KLOOFI"; another letter makes another word: "RIVIERA", "MAINE",
///   "HOOGE");
/// * a last word cut short is completed when exactly one lexicon word on the sheet starts with
///   it, one to three letters longer ("EASY GUL" → "EASY GULLY" beside "WOODY GULLY", "CASCADE
///   KL" → "CASCADE KLOOF"), unless the word before it takes a name (`NAME_AFTER`: "RIVER CAM").
pub(crate) fn mend_name(name: &str, vocab: &Vocabulary, echoes: &[Vec<char>]) -> String {
    // A lexicon word never has accents: one read with an accent is a speck on the paper
    // ("GÁTE", "AČCOMMODATION").
    let mut toks: Vec<String> = name
        .split_whitespace()
        .map(|t| {
            let lower = t.to_lowercase();
            let plain = fold_accents(&lower);
            if plain != lower
                && lexicon_word(&plain)
                && !ACCENTED_SPELLINGS.contains(&lower.as_str())
                && t.chars().all(char::is_alphabetic)
            {
                if t.chars().all(|c| !c.is_lowercase()) {
                    plain.to_uppercase()
                } else {
                    plain
                }
            } else {
                t.to_string()
            }
        })
        .collect();
    let alpha = |t: &str| !t.is_empty() && t.chars().all(char::is_alphabetic);
    let letters = |t: &str| t.chars().filter(|c| c.is_alphabetic()).count();
    if toks.len() >= 2
        && toks[0].chars().count() == 1
        && alpha(&toks[0])
        && (!"aeiouy".contains(fold_accents(&toks[0].to_lowercase()).as_str()) || vocab.known(&toks[1].to_lowercase()))
        && toks[1].chars().count() >= 2
        && toks[1..].iter().map(|t| letters(t)).sum::<usize>() >= 4
    {
        toks.remove(0);
    }
    if let Some(first) = toks.first().cloned() {
        let c: Vec<char> = first.chars().collect();
        let rest: String = c.iter().skip(1).collect();
        let lower: Vec<char> = first.to_lowercase().chars().collect();
        // A digit glued to a word ("1KWAMI").
        let digit = c.len() >= 4 && c[0].is_ascii_digit() && alpha(&rest);
        // A letter glued to a known word that cannot start one, or that another read lacks.
        let letter = c.len() >= 5
            && alpha(&first)
            && lower.len() == c.len()
            && !vocab.known(&first.to_lowercase())
            && vocab.known(&rest.to_lowercase())
            && (!plausible_onset(lower[0], lower[1]) || {
                let key = fragment_key(&toks.join(" "));
                echoes.iter().any(|e| e.as_slice() == &key[1..])
            });
        if digit || letter {
            toks[0] = rest;
        }
    }
    if let Some(last) = toks.last().cloned() {
        let c: Vec<char> = last.chars().collect();
        let stem: String = c.iter().take(c.len().saturating_sub(1)).collect();
        if c.len() >= 5
            && alpha(&last)
            && matches!(c[c.len() - 1], 'I' | 'l' | 'ı')
            && !vocab.known(&last.to_lowercase())
            && vocab.known(&stem.to_lowercase())
        {
            *toks.last_mut().unwrap() = stem;
        }
    }
    if toks.len() >= 2 {
        let last = toks[toks.len() - 1].clone();
        let lower = last.to_lowercase();
        let named = NAME_AFTER.contains(&fold_accents(&toks[toks.len() - 2].to_lowercase()).trim_end_matches('.'));
        if last.chars().count() >= 2 && alpha(&last) && !vocab.known(&lower) && !named {
            if let Some(w) = vocab.completion(&lower) {
                let full = if last.chars().all(|c| !c.is_lowercase()) {
                    w.to_uppercase()
                } else if last.chars().next().is_some_and(char::is_uppercase) {
                    let mut c = w.chars();
                    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
                } else {
                    w.to_string()
                };
                *toks.last_mut().unwrap() = full;
            }
        }
    }
    toks.join(" ")
}

/// Mend the names (`mend_name`), and drop a name the OCR was unsure of (confidence under 0.5:
/// Apple Vision gives 0.3 to its guesses) unless every word of it is a known word
/// ("PHANL GULLI" is not).
fn mend_names(merged: &[OcrObservation], verdicts: &mut [Verdict], vocab: &Vocabulary) {
    for (i, (o, v)) in merged.iter().zip(verdicts.iter_mut()).enumerate() {
        let Verdict::Poi(p) = v else { continue };
        if p.kind == PoiKind::SpotHeight {
            continue;
        }
        if o.confidence < 0.5 && words_of(&p.name).iter().any(|w| w.chars().count() >= 2 && !vocab.known(w)) {
            *v = Verdict::Junk;
            continue;
        }
        // Other reads at the same place.
        let echoes: Vec<Vec<char>> = merged
            .iter()
            .enumerate()
            .filter(|&(j, b)| j != i && covered_by(&o.bbox, &b.bbox).max(covered_by(&b.bbox, &o.bbox)) >= 0.6)
            .map(|(_, b)| fragment_key(&b.text))
            .collect();
        let name = mend_name(&p.name, vocab, &echoes);
        if name != p.name {
            p.kind = poi_kind(&words_of(&name));
            p.name = name;
        }
    }
}

/// Several passes read one label in pieces ("KUDUKONA" / "UDU", "1283" /
/// "283", "MHLABATINI" / "BATI" / "ABATI", "Police Station" / "Stati") or
/// differently ("KRAAL" / "KRAAZ", "KWAMI LODGE" / "NAMI LODO", "1471" /
/// "1472", "MIDDLE RIDGE KLOOF" / "MIODLE RIDGEKIOOF"): a POI is a piece or
/// another read of a read next to it when that read holds its text (exactly,
/// or within one edit from 5 characters), or when their boxes all but
/// coincide and they share most of their text or are a few edits apart (a
/// quarter of the letters). A piece goes with the longer read, then the one
/// in more words ("SPARKLING WATERS HOTEL" over "SPARKLINGWATERSHOTEL"), then
/// the more confident one; of two reads of one label the better read stays
/// (`better_read`); a piece of legend text goes with it. Unreadable reads
/// ("21728" around "1728") never absorb a POI. POIs are compared by their
/// mended names.
fn drop_fragments(merged: &[OcrObservation], verdicts: &mut [Verdict], vocab: &Vocabulary) {
    let texts: Vec<String> = merged
        .iter()
        .zip(verdicts.iter())
        .map(|(o, v)| match v {
            Verdict::Poi(p) => p.name.clone(),
            _ => o.text.clone(),
        })
        .collect();
    let keys: Vec<Vec<char>> = texts.iter().map(|t| fragment_key(t)).collect();
    let words: Vec<usize> = texts.iter().map(|t| t.split_whitespace().count()).collect();
    let known: Vec<usize> = texts.iter().map(|t| vocab.known_letters(t)).collect();
    // Of two reads of one label, is `b` the better one? A clearly more confident read (Apple
    // Vision: 1.0 over 0.5 over 0.3), then the one in more known words ("KEEP RIGHT" over
    // "KEED RIGHT", "LONG POOL" over "KONG POOL"), more words, more confident, longer.
    let better_read = |b: usize, a: usize| {
        let (cb, ca) = (merged[b].confidence, merged[a].confidence);
        if (cb - ca).abs() >= 0.25 {
            return cb > ca;
        }
        (known[b], words[b], cb, keys[b].len(), a) > (known[a], words[a], ca, keys[a].len(), b)
    };
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
            let m = 1.5 * oa.text_height().max(ob.text_height());
            let [x, y, w, h] = ob.bbox;
            let near = ax >= x - m && ax <= x + w + m && ay >= y - m && ay <= y + h + m;
            if !near {
                return false;
            }
            let (ka, kb) = (&keys[a], &keys[b]);
            // Two reads of one place: boxes that all but coincide.
            let same_place = covered_by(&oa.bbox, &ob.bbox).max(covered_by(&ob.bbox, &oa.bbox)) >= 0.6;
            let poi_b = matches!(verdicts[b], Verdict::Poi(_));
            let tolerance = ((0.25 * ka.len().max(kb.len()) as f64).round() as usize).max(1);
            // A read cut short or with a piece missing is a piece, not a misread.
            let piece = |x: &[char], y: &[char]| y.windows(x.len().max(1)).any(|w| w == x);
            let misread = poi_b
                && same_place
                && ka.len().abs_diff(kb.len()) <= 1
                && !piece(ka, kb)
                && !piece(kb, ka)
                && levenshtein(ka, kb) <= tolerance;
            let better = if poi_b && (misread || ka.len() == kb.len()) {
                better_read(b, a)
            } else {
                let rank = |k: usize, o: &OcrObservation| (keys[k].len(), words[k], o.confidence);
                let (ra, rb) = (rank(a, oa), rank(b, ob));
                // A misread of legend text ("TRIG BEAGON") is legend text.
                (matches!(verdicts[b], Verdict::Furniture) && rb.0 >= ra.0)
                    || rb.0 > ra.0
                    || (rb.0 == ra.0 && (rb.1 > ra.1 || (rb.1 == ra.1 && (rb.2 > ra.2 || (rb.2 == ra.2 && b < a)))))
            };
            if !better {
                return false;
            }
            if misread || (same_place && longest_common_run(ka, kb) as f64 >= 0.6 * ka.len().min(kb.len()) as f64) {
                return true;
            }
            let edits = usize::from(ka.len() >= 5);
            if kb.len() == ka.len() {
                // Same length: the same read spaced differently, or one letter read differently;
                // names numbered apart in two places are two places ("RAND WATER BOX 1" over "2").
                let diff: Vec<(&char, &char)> = ka.iter().zip(kb).filter(|(p, q)| p != q).collect();
                let numbered = ka.iter().any(|c| c.is_alphabetic())
                    && !diff.is_empty()
                    && diff.iter().all(|(p, q)| p.is_ascii_digit() && q.is_ascii_digit());
                diff.len() <= edits && !numbered
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

/// A label printed up, down or slanting along a stream or a slope that Apple Vision reads in
/// pieces that disagree ("ROOIKRI", "ANTZ" and "PHANL GULLI" for "ROOIKRANTZ GULLY"): a name read
/// that way with no known word, whose box (widened by a quarter of the text height) meets another
/// such read with no known word either, that it shares no text with (no run of three characters)
/// and that runs on from it (the two boxes together about as long, corner to corner, as both;
/// two names printed side by side along a border overlap instead: "KWAZULU-NATAL", "LESOTHO"), is
/// a garbled piece; so is one such read of a single word that only says what the feature is
/// ("GULLY" of "UPPER CASTLE GULLY", the "KLOOF" of "HLOBANE" "KLOOF"). Reads across the map are
/// left alone: Vision reads those well.
fn drop_garbled_turned(merged: &[OcrObservation], verdicts: &mut [Verdict], vocab: &Vocabulary, th: f64) {
    let keys: Vec<Vec<char>> = merged.iter().map(|o| fragment_key(&o.text)).collect();
    let turned: Vec<bool> = merged.iter().map(|o| !reads_across(o)).collect();
    let pad = 0.25 * th;
    let garbled: Vec<bool> = merged
        .iter()
        .enumerate()
        .map(|(a, oa)| {
            let Verdict::Poi(p) = &verdicts[a] else { return false };
            if p.kind == PoiKind::SpotHeight || text_reads_across(oa, &p.name) {
                return false;
            }
            // One word that only says what the feature is: the end of a longer label.
            let words = words_of(&p.name);
            if words.len() == 1 && lexicon_word(&words[0]) {
                return true;
            }
            if vocab.known_letters(&p.name) > 0 {
                return false;
            }
            let [x, y, w, h] = oa.bbox;
            let wide = [x - pad, y - pad, w + 2.0 * pad, h + 2.0 * pad];
            let diagonal = |b: &[f64; 4]| b[2].hypot(b[3]);
            merged.iter().enumerate().any(|(b, ob)| {
                let [bx, by, bw, bh] = ob.bbox;
                let union = [x.min(bx), y.min(by), (x + w).max(bx + bw) - x.min(bx), (y + h).max(by + bh) - y.min(by)];
                b != a
                    && turned[b]
                    && !matches!(verdicts[b], Verdict::Furniture)
                    && ob.text.chars().filter(|c| c.is_alphabetic()).count() >= 3
                    && vocab.known_letters(&ob.text) == 0
                    && covered_by(&ob.bbox, &wide) > 0.0
                    && longest_common_run(&keys[a], &keys[b]) < 3
                    && diagonal(&union) >= 0.8 * (diagonal(&oa.bbox) + diagonal(&ob.bbox))
            })
        })
        .collect();
    for (v, g) in verdicts.iter_mut().zip(garbled) {
        if g {
            *v = Verdict::Junk;
        }
    }
}

/// Turn OCR text into POI candidates (place names, spot heights), skipping
/// coordinate labels, scale text and legend/title boilerplate, imprints and
/// route distances, the pieces and misreads several OCR passes make of one
/// label, and mending names a symbol glued onto or the OCR cut short.
pub fn classify_pois(obs: &[OcrObservation]) -> Vec<PoiLabel> {
    classify_pois_reads(obs).into_iter().map(|(p, _)| p).collect()
}

/// `classify_pois` with the read each POI came from (its box and confidence).
pub(crate) fn classify_pois_reads(obs: &[OcrObservation]) -> Vec<(PoiLabel, OcrObservation)> {
    let mut merged = merge_identical(obs);
    let th = median(merged.iter().map(|o| o.text_height()).filter(|&h| h > 0.0).collect()).unwrap_or(20.0);
    let scale_bar = scale_bar_members(&merged, th);
    let legend = legend_members(&merged, th);
    let vocab = Vocabulary::new(&merged);
    // Heights stay as printed until the end (the truncated-height rules
    // count digits); unknown is read as metres.
    let unit = height_unit(obs);
    let read_unit = unit.unwrap_or(HeightUnit::Metres);
    let mut verdicts: Vec<Verdict> =
        merged.iter().enumerate().map(|(i, o)| verdict(o, scale_bar[i] || legend[i], th, read_unit)).collect();
    mend_names(&merged, &mut verdicts, &vocab);
    drop_fragments(&merged, &mut verdicts, &vocab);
    drop_garbled_turned(&merged, &mut verdicts, &vocab, th);
    join_stacked(&mut merged, &mut verdicts);
    let mut out: Vec<(PoiLabel, OcrObservation)> = verdicts
        .into_iter()
        .zip(merged)
        .filter_map(|(v, o)| match v {
            Verdict::Poi(p) => Some((p, o)),
            _ => None,
        })
        .collect();

    drop_truncated_heights(&mut out, |x| &x.0);

    // Peaks without a height take the nearest spot height next to them.
    let spots: Vec<(f64, f64, f64)> = out
        .iter()
        .map(|x| &x.0)
        .filter(|p| p.kind == PoiKind::SpotHeight)
        .filter_map(|p| Some((p.x, p.y, p.elevation_m?)))
        .collect();
    for (p, _) in out.iter_mut().filter(|x| x.0.kind == PoiKind::Peak && x.0.elevation_m.is_none()) {
        let near = spots
            .iter()
            .map(|s| ((s.0 - p.x).hypot(s.1 - p.y), s.2))
            .filter(|(d, _)| *d <= 6.0 * th)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, h)) = near {
            p.elevation_m = Some(h);
        }
    }
    // Printed heights to metres (the name keeps the printed text: "10876").
    let k = read_unit.metres();
    if k != 1.0 {
        for (p, _) in out.iter_mut() {
            if let Some(e) = p.elevation_m.as_mut() {
                *e = (*e * k * 10.0).round() / 10.0;
            }
        }
    }
    out
}

/// Names printed on two or more lines, one under the other and centred
/// ("Giant's" / "Castle", "Thaba-" / "Putsoa", "Bannerman" / "Hut": Gaia GPS
/// and other screen maps wrap their labels), are one name: lines across the
/// map of about the same letter height, centred on each other within 0.6
/// letter heights, the lower one starting at most 0.6 letter heights under
/// the upper one (and overlapping it by at most 0.35). A line ending in a
/// hyphen runs on without a space. A height centred under the name (Gaia's
/// summits: "Giant's" / "Castle" / "10876") gives a name without one its
/// elevation and makes it a peak; the height stays a spot height of its own.
/// Separate labels of a sheet are rarely centred on each other that close.
fn join_stacked(merged: &mut [OcrObservation], verdicts: &mut [Verdict]) {
    let line = |o: &OcrObservation| o.bbox[2] >= o.bbox[3] && o.bbox[2] > 0.0 && o.bbox[3] > 0.0;
    let is_name = |v: &Verdict| matches!(v, Verdict::Poi(p) if p.kind != PoiKind::SpotHeight);
    // A line of one or two feature words ("Hut", "Cave", "Camp"): legend
    // text on its own, the end of the name above it here.
    let feature_line = |o: &OcrObservation, v: &Verdict| {
        let words = words_of(&o.text);
        matches!(v, Verdict::Furniture)
            && (1..=2).contains(&words.len())
            && words.iter().all(|w| w.chars().count() >= 3 && lexicon_word(w))
            && o.text.chars().all(|c| c.is_alphabetic() || c.is_whitespace() || "'-.".contains(c))
    };
    let usable = |o: &OcrObservation, v: &Verdict| matches!(v, Verdict::Poi(_)) || feature_line(o, v);
    let mut order: Vec<usize> =
        (0..merged.len()).filter(|&i| line(&merged[i]) && usable(&merged[i], &verdicts[i])).collect();
    order.sort_by(|&a, &b| merged[a].bbox[1].total_cmp(&merged[b].bbox[1]));
    // The next line under a line centred at `cx`, ending at `bottom`, of letter height `lh`.
    let below = |merged: &[OcrObservation], verdicts: &[Verdict], cx: f64, bottom: f64, lh: f64| -> Option<usize> {
        order
            .iter()
            .copied()
            .filter(|&j| usable(&merged[j], &verdicts[j]))
            .filter(|&j| {
                let o = &merged[j];
                let h = o.bbox[3];
                let th = lh.max(h);
                let gap = o.bbox[1] - bottom;
                let ocx = o.bbox[0] + o.bbox[2] / 2.0;
                h >= 0.7 * lh && h <= lh / 0.7 && (ocx - cx).abs() <= 0.6 * th && gap >= -0.35 * th && gap <= 0.6 * th
            })
            .min_by(|&a, &b| merged[a].bbox[1].total_cmp(&merged[b].bbox[1]))
    };
    for &i in &order {
        if !is_name(&verdicts[i]) {
            continue;
        }
        // The read keeps its first line's height (its letter size); its box
        // widens to the lines', its position is their middle.
        let first = merged[i].bbox;
        let (cx, mut bottom, mut lh) = (first[0] + first[2] / 2.0, first[1] + first[3], first[3]);
        let (mut x0, mut x1) = (first[0], first[0] + first[2]);
        // Absorb the lines under it, then a height under them.
        while let Some(j) = below(merged, verdicts, cx, bottom, lh).filter(|&j| j != i) {
            let Verdict::Poi(upper) = &verdicts[i] else { break };
            let upper = upper.clone();
            let lower = match &verdicts[j] {
                Verdict::Poi(p) => p.clone(),
                _ => {
                    let (x, y) = merged[j].center();
                    PoiLabel { name: merged[j].text.clone(), kind: PoiKind::Place, elevation_m: None, x, y }
                }
            };
            if lower.kind == PoiKind::SpotHeight {
                if upper.elevation_m.is_none() {
                    if let Verdict::Poi(p) = &mut verdicts[i] {
                        p.elevation_m = lower.elevation_m;
                        p.kind = PoiKind::Peak;
                    }
                }
                break;
            }
            if upper.elevation_m.is_some() {
                break;
            }
            // Two names of several words each, or sharing a word, are two
            // places one under the other ("NORTH PEAK" / "SOUTH PEAK", "RAND
            // WATER BOX 1" / "… 2"); a wrapped label breaks off one word.
            // A line ending in a hyphen (as read: the name lost it to the symbol trim).
            let hyphen = merged[i].text.trim_end().ends_with('-');
            let (wu, wl) = (words_of(&upper.name), words_of(&lower.name));
            let shared = wu.iter().any(|w| w.chars().count() >= 3 && wl.contains(w));
            if shared || !(wu.len() == 1 || wl.len() == 1 || hyphen) {
                break;
            }
            let join = |a: &str, b: &str| {
                if hyphen {
                    format!("{}-{b}", a.trim_end().trim_end_matches('-'))
                } else {
                    format!("{a} {b}")
                }
            };
            let name = join(&upper.name, &lower.name);
            let text = join(&merged[i].text, &merged[j].text);
            let b = merged[j].bbox;
            x0 = x0.min(b[0]);
            x1 = x1.max(b[0] + b[2]);
            bottom = b[1] + b[3];
            lh = b[3];
            let confidence = merged[i].confidence.min(merged[j].confidence);
            let (x, y) = ((x0 + x1) / 2.0, (first[1] + bottom) / 2.0);
            let kind = match lower.kind {
                PoiKind::Peak | PoiKind::Water => lower.kind,
                _ => poi_kind(&words_of(&name)),
            };
            let kind = if upper.kind == PoiKind::Peak { PoiKind::Peak } else { kind };
            verdicts[i] =
                Verdict::Poi(PoiLabel { name, kind, elevation_m: lower.elevation_m.or(upper.elevation_m), x, y });
            verdicts[j] = Verdict::Junk;
            merged[i] = OcrObservation { text, bbox: [x0, first[1], x1 - x0, first[3]], confidence };
        }
    }
}

/// Drop spot heights that are misreads with digits missing, such as "112" for a "1412" that a
/// grid line cuts through:
///
/// * on a sheet whose heights nearly all have the same number of digits (at least 4 heights,
///   80 % alike), a height with fewer digits and under half the median is not believable;
/// * on a sheet with at least `MIN_FOUR_DIGIT_HEIGHTS` heights of four digits, nearly all in
///   one thousand (`k`000–`k`999 m, 90 %), a three-digit height `h` well below them (more than
///   `TRUNCATED_HEIGHT_MARGIN_M` under the lowest) is a misread when `k`000 + h falls among
///   them (their range widened by that margin): "478" for 1478 on a sheet of 1100–1800 m
///   heights, however many such misreads there are. Not when other three-digit heights lie
///   within that margin under the lowest: the terrain runs on down (a fjord, an escarpment),
///   nor on a sheet whose heights span thousands (an alpine valley under 3000 m peaks).
///
/// Sheets that span lowland and mountain keep their short heights.
fn drop_truncated_heights<T>(pois: &mut Vec<T>, label: impl Fn(&T) -> &PoiLabel) {
    let digits = |h: f64| (h.abs().trunc() as u64).to_string().len();
    let heights = |pois: &[T]| -> Vec<f64> {
        pois.iter().map(&label).filter(|p| p.kind == PoiKind::SpotHeight).filter_map(|p| p.elevation_m).collect()
    };
    let all = heights(pois);
    let four: Vec<f64> = all.iter().copied().filter(|&h| (1000.0..10_000.0).contains(&h)).collect();
    // The thousand most four-digit heights are in.
    let thousand = (1..=9)
        .map(|k| (k as f64 * 1000.0, four.iter().filter(|&&h| (h / 1000.0).trunc() as i32 == k).count()))
        .max_by_key(|x| x.1);
    if let Some((k, _)) = thousand.filter(|&(_, n)| n >= MIN_FOUR_DIGIT_HEIGHTS && n as f64 >= 0.9 * four.len() as f64)
    {
        let ks: Vec<f64> = four.iter().copied().filter(|&h| (k..k + 1000.0).contains(&h)).collect();
        let lowest = ks.iter().copied().fold(f64::INFINITY, f64::min);
        let lo = lowest - TRUNCATED_HEIGHT_MARGIN_M;
        let hi = ks.iter().copied().fold(f64::NEG_INFINITY, f64::max) + TRUNCATED_HEIGHT_MARGIN_M;
        let short = |h: f64| (100.0..1000.0).contains(&h);
        let continues = all.iter().any(|&h| short(h) && (lo..lowest).contains(&h));
        let truncated = |h: f64| short(h) && h < lo && (lo..=hi).contains(&(k + h));
        if !continues {
            pois.retain(|x| {
                let p = label(x);
                p.kind != PoiKind::SpotHeight || !p.elevation_m.is_some_and(truncated)
            });
        }
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
    pois.retain(|x| {
        let p = label(x);
        p.kind != PoiKind::SpotHeight || p.elevation_m.is_none_or(|h| digits(h) >= typical || h >= 0.5 * med)
    });
}

/// `drop_truncated_heights`: four-digit heights needed to judge three-digit ones…
const MIN_FOUR_DIGIT_HEIGHTS: usize = 3;
/// …and how far beyond their range (metres) a truncated height may point.
const TRUNCATED_HEIGHT_MARGIN_M: f64 = 250.0;

/// "5m", "100m", "2km" style measurement tokens.
pub(crate) fn is_measure(w: &str) -> bool {
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
            "°53'00\"S",
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
        // An alpine valley under peaks of two and three thousand metres: the four-digit heights
        // span thousands, so a three-digit one is no thousand short of them.
        let alps = ["796", "802", "2970", "1462", "3454", "2108", "1823", "2663"];
        assert_eq!(classify_pois(&at(&alps)).len(), alps.len());
        // A fjord: heights from the shore to the plateau, some just under 1000 m.
        let fjord = ["312", "455", "640", "870", "960", "1012", "1240", "1488", "1530"];
        assert_eq!(classify_pois(&at(&fjord)).len(), fjord.len());
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
    fn vision_misreads_are_mended_or_dropped() {
        // Apple Vision's reads on the Castle Gorge and Grootkloof sheets (iOS 26.2 simulator).
        let mut obs = vec![
            // Two reads of one turned label: the one in known words stays.
            ob("MIDDLE RIDGE KLOOF", 3088.0, 1244.0, 90.0, 280.0),
            ob("MIODLE RIDGEKIOOF", 3090.0, 1249.0, 68.0, 270.0),
            ob("KEED RIGHT", 1568.0, 2291.0, 158.0, 53.0),
            ob("KEEP RIGHT", 1577.0, 2307.0, 147.0, 30.0),
            ob("B KONG POOL", 730.0, 2331.0, 194.0, 48.0),
            ob("D LONG POOL", 733.0, 2333.0, 191.0, 42.0),
            ob("GRASSY POOL", 711.0, 2194.0, 190.0, 41.0),
            // A symbol glued in front, a route distance behind, a word cut short.
            ob("WCASTLE GORGE WATERFALL (LOWER)", 865.0, 2088.0, 486.0, 38.0),
            ob("CASTLE GORGE WATERFALL (UPPER)", 423.0, 2098.0, 451.0, 35.0),
            ob("1KWAMI LODGE*", 2829.0, 4484.0, 214.0, 28.0),
            ob("STILE 1.7Km", 2205.0, 2216.0, 164.0, 75.0),
            ob("PIHRANA POO", 860.0, 2249.0, 188.0, 28.0),
            ob("CASCADE KL", 836.0, 2860.0, 62.0, 159.0),
            ob("BUSSHBREAK AČCOMMODATION", 198.0, 279.0, 409.0, 33.0),
            // An imprint, a lone abbreviation, a height with its triangle read as a letter.
            ob("ISV1 Sept 2017", 452.0, 4807.0, 177.0, 28.0),
            ob("KUMB.", 3306.0, 916.0, 86.0, 23.0),
            ob("1501 A", 428.0, 1755.0, 86.0, 24.0),
            // Pieces of "ROOIKRANTZ GULLY" read every which way, one Vision was unsure of.
            ob("ROOIKRI", 2822.0, 1286.0, 80.0, 116.0),
            ob("ANTZ", 2877.0, 1230.0, 38.0, 74.0),
            ob("PHANL GULLI", 2877.0, 1146.0, 70.0, 198.0),
            // Turned names on their own stay.
            ob("GAUTENG", 1087.0, 3389.0, 123.0, 105.0),
            ob("UPPER CASTLE GULLY", 376.0, 2509.0, 237.0, 242.0),
        ];
        obs[19].confidence = 0.3;
        let names: Vec<String> = classify_pois(&obs).into_iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec![
                "MIDDLE RIDGE KLOOF",
                "KEEP RIGHT",
                "LONG POOL",
                "GRASSY POOL",
                "CASTLE GORGE WATERFALL (LOWER)",
                "CASTLE GORGE WATERFALL (UPPER)",
                "KWAMI LODGE",
                "STILE",
                "PIHRANA POOL",
                "CASCADE KLOOF",
                "BUSSHBREAK ACCOMMODATION",
                "1501",
                "GAUTENG",
                "UPPER CASTLE GULLY",
            ]
        );
        // A name made of real words is no misread, however unsure the OCR: "LONG POOL" at 0.3.
        let mut sure = obs.clone();
        sure[5].confidence = 0.3;
        sure[4].confidence = 0.3;
        assert!(classify_pois(&sure).iter().any(|p| p.name == "LONG POOL"));
    }

    #[test]
    fn mending_rules() {
        let vocab = Vocabulary::new(&[
            ob("FERN KLOOF", 0.0, 0.0, 100.0, 20.0),
            ob("RAND WATER BOX 1", 0.0, 100.0, 100.0, 20.0),
            ob("RAND WATER BOX 2", 0.0, 200.0, 100.0, 20.0),
            ob("WOODY GULLY", 0.0, 300.0, 100.0, 20.0),
            ob("KLOOFI", 0.0, 400.0, 100.0, 20.0),
        ]);
        let mend = |n: &str| mend_name(n, &vocab, &[]);
        assert_eq!(mend("MHLABATINI & FERN KLOOFI"), "MHLABATINI & FERN KLOOF");
        assert_eq!(mend("PEDESTRIAN GÁTE"), "PEDESTRIAN GATE");
        assert_eq!(mend("AASVOËL GULLY"), "AASVOËL GULLY");
        assert_eq!(mend("EASY GUL"), "EASY GULLY");
        assert_eq!(mend("Easy Gul"), "Easy Gully");
        // A letter that may start a word stays ("BRAND" is no B glued to "RAND"), unless another
        // read of the label at the same place is without it.
        assert_eq!(mend("BRAND KOP"), "BRAND KOP");
        assert_eq!(mend_name("BRAND KOP", &vocab, &["randkop".chars().collect()]), "RAND KOP");
        assert_eq!(mend("WRAND KOP"), "WRAND KOP");
        assert_eq!(mend("XRAND KOP"), "RAND KOP");
        // A vowel alone in front is an article, unless a known word follows (a pool's ring).
        assert_eq!(mend("O RAND KOP"), "RAND KOP");
        assert_eq!(mend("A CORUÑA"), "A CORUÑA");
        // Initials are no stray letters; nor is a word that is known itself.
        assert_eq!(mend("J G STRIJDOM DAM"), "J G STRIJDOM DAM");
        assert_eq!(mend("1ST RAND"), "1ST RAND");
        assert_eq!(mend("KUDU KONA"), "KUDU KONA");
        assert!(plausible_onset('m', 'h') && plausible_onset('t', 's') && !plausible_onset('w', 'c'));
        for t in ["ISV1 Sept 2017", "GIS V1 Sept 2017- Eras Gous", "Sept2017", "printed March 1998"] {
            assert!(imprint_text(t), "{t}");
        }
        for t in ["MAY KLOOF", "2017", "MARCH HARE HUT", "V1 BRIDGE"] {
            assert!(!imprint_text(t), "{t}");
        }
        for t in ["1.7Km", "0.6km)", "13km", "2,5m", "4mi"] {
            assert!(distance_token(t), "{t}");
        }
        assert!(!distance_token("300m") && !distance_token("1523m") && !distance_token("km") && !distance_token("3"));
        let p = classify_pois(&[ob("WATERVAL HUT 2,3 km", 0.0, 0.0, 260.0, 24.0)]);
        assert_eq!(p[0].name, "WATERVAL HUT");
    }

    /// A label read across the map at (x, y): 24 px letters, 15 px a character.
    fn across(text: &str, x: f64, y: f64) -> OcrObservation {
        ob(text, x, y, 15.0 * text.chars().count() as f64, 24.0)
    }

    /// The names `classify_pois` finds among `labels` on a sheet of mountain spot heights (left
    /// out of the answer).
    fn sheet_names(labels: &[OcrObservation]) -> Vec<String> {
        let mut obs = labels.to_vec();
        for (k, h) in ["1416", "1754", "1556", "1471", "1533"].iter().enumerate() {
            obs.push(ob(h, 3300.0, 300.0 + 300.0 * k as f64, 60.0, 24.0));
        }
        classify_pois(&obs).into_iter().filter(|p| p.x < 3200.0).map(|p| p.name).collect()
    }

    #[test]
    fn names_of_other_regions_are_not_mended_into_lexicon_words() {
        // Each label alone on its sheet: read well, nothing to mend.
        for name in [
            // Articles of one letter: Galician, Spanish, Welsh.
            "A CORUÑA",
            "O CEBREIRO",
            "Y GARN",
            "Y LLIWEDD",
            // A lexicon word and a letter: real words, not a glyph glued behind.
            "RIVIERA",
            "MAINE",
            "HOOGE",
            "DE GROOTE",
            "GROSSE POINTE",
            "BIG ROCKY",
            // Onsets of Welsh, Nguni, Sotho-Tswana and Norse names.
            "FFOREST FAWR",
            // Accents that are the spelling.
            "HÔTEL DE VILLE",
            "RÉSERVE NATURELLE",
            // Names of other regions and languages.
            "HLOBANE",
            "TSITSIKAMMA",
            "NGWENYA",
            "UITKYK",
            "YSTERVARK",
            "ZUGSPITZE",
            "KRKONOŠE",
            "LION'S HEAD",
            "MONT-AUX-SOURCES",
            "AYR",
            "ELY",
            "UIG",
            "HOY",
            "ECK",
            "EYE",
        ] {
            assert_eq!(sheet_names(&[across(name, 300.0, 300.0)]), vec![name], "{name}");
        }
        for (a, b) in [('f', 'f'), ('m', 't'), ('m', 'k'), ('m', 'g'), ('m', 'q'), ('k', 'g'), ('k', 'j'), ('g', 'j')] {
            assert!(plausible_onset(a, b), "{a}{b}");
        }
        // A name after a word that introduces it is not cut short: no "RIVER CAMP" because a camp
        // is on the sheet, no "RÍO PASS" (the Río Pas) beside "SANI PASS"; "SANI PAS" is.
        let names = sheet_names(&[
            across("RIVER CAM", 300.0, 300.0),
            across("BARAFU CAMP", 300.0, 900.0),
            across("RÍO PAS", 300.0, 1500.0),
            across("SANI PASS", 1200.0, 300.0),
            across("SANI PAS", 1200.0, 900.0),
        ]);
        assert_eq!(names, vec!["RIVER CAM", "BARAFU CAMP", "RÍO PAS", "SANI PASS", "SANI PASS"]);
        // Only a lexicon word completes a word: "RAND" (of "RAND WATER BOX 1" and "2") is no end of
        // "LIGHTNING RA…" (the sheet's "LIGHTNING RAVINE").
        let vocab = Vocabulary::new(&[across("RAND WATER BOX 1", 0.0, 0.0), across("RAND WATER BOX 2", 0.0, 100.0)]);
        assert_eq!(mend_name("LGHTNING RA", &vocab, &[]), "LGHTNING RA");
        // A road number is no spot height with its triangle: "A 470" on a lowland sheet ("1501 A"
        // and "▲ 1720" are).
        let mut obs: Vec<OcrObservation> = ["245", "312", "188", "521", "377"]
            .iter()
            .enumerate()
            .map(|(k, h)| ob(h, 3300.0, 300.0 + 300.0 * k as f64, 45.0, 24.0))
            .collect();
        obs.extend([across("A 470", 300.0, 300.0), across("A 3059", 300.0, 900.0), across("1501 A", 1200.0, 300.0)]);
        obs.push(across("▲ 1720", 1200.0, 900.0));
        let heights: Vec<String> = classify_pois(&obs).into_iter().filter(|p| p.x < 3200.0).map(|p| p.name).collect();
        assert_eq!(heights, vec!["1501", "1720"]);
    }

    #[test]
    fn turned_names_of_one_word_are_kept() {
        // Rivers named along their course, up the map: four letters that are a name.
        for name in ["ISAR", "LECH", "Elbe", "TEES"] {
            assert_eq!(sheet_names(&[ob(name, 300.0, 300.0, 26.0, 70.0)]), vec![name], "{name}");
        }
        // A turned label Apple Vision reads as two words, the name and what the place is: the name
        // stays (the feature word alone goes).
        let names = sheet_names(&[ob("HLOBANE", 1000.0, 1000.0, 30.0, 110.0), ob("KLOOF", 1000.0, 1115.0, 30.0, 80.0)]);
        assert_eq!(names, vec!["HLOBANE"]);
        // Two names printed along a border, slanting side by side: their boxes overlap, they are
        // not pieces of one label.
        let names = sheet_names(&[
            ob("KWAZULU-NATAL", 1000.0, 1000.0, 150.0, 150.0),
            ob("LESOTHO", 1040.0, 1040.0, 88.0, 88.0),
        ]);
        assert_eq!(names, vec!["KWAZULU-NATAL", "LESOTHO"]);
    }

    #[test]
    fn numbered_places_and_twin_names_are_each_a_poi() {
        // Numbered camps and huts are places ("CAMP 2"); legend text is not ("Camp", "2 km").
        let names = sheet_names(&[
            across("CAMP 2", 300.0, 300.0),
            across("HUT 4", 300.0, 900.0),
            across("Camp", 1200.0, 300.0),
            across("2 km", 1200.0, 900.0),
            across("ROUTE 66", 1200.0, 1500.0),
        ]);
        assert_eq!(names, vec!["CAMP 2", "HUT 4", "ROUTE 66"]);
        // Two places with names alike, one under the other: both stay.
        let names = sheet_names(&[
            across("NORTH PEAK", 300.0, 300.0),
            across("SOUTH PEAK", 300.0, 335.0),
            across("UPPER FALLS", 900.0, 300.0),
            across("LOWER FALLS", 900.0, 335.0),
            across("RAND WATER BOX 1", 300.0, 900.0),
            across("RAND WATER BOX 2", 300.0, 935.0),
            across("CASTLE GORGE WATERFALL (UPPER)", 900.0, 900.0),
            across("CASTLE GORGE WATERFALL (LOWER)", 900.0, 935.0),
        ]);
        assert_eq!(
            names,
            vec![
                "NORTH PEAK",
                "SOUTH PEAK",
                "UPPER FALLS",
                "LOWER FALLS",
                "RAND WATER BOX 1",
                "RAND WATER BOX 2",
                "CASTLE GORGE WATERFALL (UPPER)",
                "CASTLE GORGE WATERFALL (LOWER)",
            ]
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
        // Place names that happen to join legend words are names: legend text run together
        // keeps its capitals ("PerennialRiver"), a name is one word ("Lakeview", "PARKVIEW").
        let names = ["Lakeview", "RIVERVIEW", "Bridgewater", "PARKVIEW", "Hillview"];
        let obs: Vec<OcrObservation> =
            names.iter().enumerate().map(|(k, t)| ob(t, 600.0 * k as f64, 900.0, 160.0, 25.0)).collect();
        let got: Vec<String> = classify_pois(&obs).into_iter().map(|p| p.name).collect();
        assert_eq!(got, names);
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

    #[test]
    fn heights_in_feet() {
        // As printed: five digits in feet, never with a leading zero or round.
        assert_eq!(parse_height_in("10876", HeightUnit::Feet), Some(10876.0));
        assert_eq!(parse_height_in("6316 ft", HeightUnit::Feet), Some(6316.0));
        assert_eq!(parse_height_in("10876", HeightUnit::Metres), None);
        assert_eq!(parse_height_in("0348", HeightUnit::Feet), None);
        assert_eq!(parse_height_in("10800", HeightUnit::Feet), None);
        assert_eq!(parse_height_in("29100", HeightUnit::Feet), None);
        // The unit: said on the map, or most heights of five digits.
        let o = |t: &str| ob(t, 0.0, 0.0, 100.0, 20.0);
        assert_eq!(height_unit(&[o("Gaia Topo (feet): © Gaia GPS")]), Some(HeightUnit::Feet));
        assert_eq!(height_unit(&[o("CONTOUR INTERVAL 40 FEET")]), Some(HeightUnit::Feet));
        assert_eq!(height_unit(&[o("Contour interval 20 m")]), Some(HeightUnit::Metres));
        assert_eq!(height_unit(&[o("10807"), o("10328"), o("6316")]), Some(HeightUnit::Feet));
        // A scale bar's unit says nothing; garbled reads are no unit.
        assert_eq!(height_unit(&[o("500 ft"), o("1 mi")]), None);
        assert_eq!(height_unit(&[o("11283"), o("17384"), o("1246"), o("1331"), o("1492")]), None);
        // POIs: names keep the printed text, elevations are metres; a five-digit
        // height in feet is no UTM easting.
        let obs = vec![
            ob("Gaia Topo (feet): © Gaia GPS, OpenStreetMap", 100.0, 2300.0, 560.0, 28.0),
            ob("Web Mercator | EPSG: 3857", 2960.0, 2364.0, 265.0, 26.0),
            ob("Mount", 1909.0, 896.0, 94.0, 35.0),
            ob("Durnford", 1894.0, 930.0, 119.0, 34.0),
            ob("10807", 1913.0, 963.0, 84.0, 33.0),
            ob("10305", 1063.0, 1040.0, 85.0, 27.0),
        ];
        let p = classify_pois(&obs);
        let got: Vec<(&str, PoiKind, Option<f64>)> =
            p.iter().map(|q| (q.name.as_str(), q.kind, q.elevation_m)).collect();
        assert_eq!(
            got,
            vec![
                ("Mount Durnford", PoiKind::Peak, Some(3294.0)),
                ("10807", PoiKind::SpotHeight, Some(3294.0)),
                ("10305", PoiKind::SpotHeight, Some(3141.0)),
            ],
            "{p:?}"
        );
    }

    #[test]
    fn wrapped_labels_and_credits() {
        let names =
            |obs: &[OcrObservation]| -> Vec<String> { classify_pois(obs).into_iter().map(|p| p.name).collect() };
        // Centred lines, one word each (or a hyphen): one name; legend words may end it.
        let obs = vec![
            ob("Giant's", 2472.0, 1466.0, 99.0, 34.0),
            ob("Castle", 2479.0, 1504.0, 74.0, 29.0),
            ob("Thaba-", 1227.0, 919.0, 93.0, 35.0),
            ob("Putsoa", 1226.0, 952.0, 96.0, 35.0),
            ob("Bannerman", 1909.0, 138.0, 182.0, 30.0),
            ob("Hut", 1968.0, 167.0, 64.0, 32.0),
        ];
        assert_eq!(names(&obs), vec!["Giant's Castle", "Thaba-Putsoa", "Bannerman Hut"]);
        // Off centre, or too far under, or two names of several words: apart.
        let obs = vec![
            ob("Grysbok", 2594.0, 415.0, 153.0, 40.0),
            ob("Bush", 2722.0, 458.0, 97.0, 37.0),
            ob("Erskine", 931.0, 575.0, 105.0, 34.0),
            ob("Kambule", 941.0, 680.0, 128.0, 31.0),
            ob("NORTH PEAK", 300.0, 300.0, 200.0, 30.0),
            ob("SOUTH PEAK", 300.0, 335.0, 200.0, 30.0),
        ];
        assert_eq!(names(&obs), vec!["Grysbok", "Bush", "Erskine", "Kambule", "NORTH PEAK", "SOUTH PEAK"]);
        // Glued words split at a capital only beside a feature word.
        assert_eq!(split_glued("MountDurnford"), "Mount Durnford");
        for n in ["KaMasihlenga", "uKhahlamba", "McGregor", "DeHoop"] {
            assert_eq!(split_glued(n), n);
        }
        for c in [
            "Gaia Topo (feet): @ Gaia GPS, OpenStreetN",
            "GAIAGPS",
            "Web Mercator|EPSG:3857",
            "© Mapbox © OpenStreetMap contributors",
        ] {
            assert!(credit_text(c), "{c}");
        }
        for n in ["Giant's Castle", "Esperance", "Mercury Bay", "Gaiaville"] {
            assert!(!credit_text(n), "{n}");
        }
    }
}
