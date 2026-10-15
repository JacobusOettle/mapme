//! The wide search: the part of the search radius beyond the near disc,
//! chunk by chunk, nearest first (`wide::plan_wide`), on zoom-10 tiles up
//! to 150 km and zoom-9 tiles beyond (one zoom coarser per halving of
//! cos(latitude) past 60°; a map too small for them is searched to 150 km
//! or not at all: `wide::wide_reach`).
//!
//! Per chunk: the coarse search over the chunk's placements (a DEM mosaic of
//! at most 8 × 8 tiles; the chunk's tiles are released once no later chunk
//! needs them) keeps its best candidates by correlation; each is re-checked
//! with a zoom-11 search 4 km around it (scale ±10 %); a re-check that stands
//! out locally (peak ratio ≥ 2.2) is refined at zoom 13 and accepted when
//! confident (≥ `CONFIDENT`, not just `ACCEPT`) and its re-check correlation
//! beats every other re-checked candidate's by 10 %. The search stops at the
//! first accepted match. Every result is cached in the session, so the
//! app's need_dem loop continues where it left off; a chunk's coarse tiles,
//! its re-check tiles and a rejected candidate's zoom-13 tiles are released
//! once used.
//!
//! Measured (`tests/wide_measure.rs`, five cases: the Grootkloof scan, a
//! synthetic and the real phone photo of it, Castle Gorge, Mhlabatini): the
//! true location re-checks at a local peak ratio of 2.56–5.4, every false
//! candidate at ≤ 1.87; photo refinement fits a false candidate up to
//! 0.635, so a refinement alone (or its margin over junk candidates) must
//! never accept a wide match.

use super::*;
use crate::search::{distinct_m, map_half_extent, min_searchable_scale, Region};
use crate::wide::{merge_plans, plan_wide, recheck_zoom, wide_band_reaches, WidePlan};

/// Candidates a chunk's coarse search returns (by significance) and of
/// those kept (by correlation) for re-checking.
const CHUNK_TOP_K: usize = 6;
pub(crate) const CHUNK_CANDIDATES: usize = 3;
/// Zoom-11 re-check: radius around the candidate (ground metres) and scale
/// tolerance.
pub(crate) const RECHECK_RADIUS_M: f64 = 4_000.0;
const RECHECK_SCALE_TOL: f64 = 1.1;
/// A re-checked candidate is refined when its local peak ratio and
/// correlation reach these…
pub(crate) const WIDE_MIN_RATIO: f32 = 2.2;
pub(crate) const WIDE_MIN_NCC: f32 = 0.2;
/// …and accepted when its re-check correlation is this many times every
/// other re-checked candidate's (≥ 2 km away).
pub(crate) const WIDE_NCC_MARGIN: f32 = 1.1;

/// Identifies a wide search plan.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct WideKey {
    contour: ContourKey,
    origin: [i64; 2],
    radius_m: i64,
    near_m: i64,
    /// Per scale band: its wide zoom and scale range (bits).
    bands: Vec<(u8, [u64; 2])>,
    max_tiles: usize,
    /// The band from coordinate labels (Mercator box bits), if any.
    band: Option<[u64; 4]>,
}

/// The session's current wide search: its plan and searched chunks.
pub(crate) struct WideCache {
    key: WideKey,
    plan: Arc<WidePlan>,
    chunks: HashMap<usize, Arc<ChunkResult>>,
    /// Coarse tiles released after their last chunk was searched.
    released: usize,
}

/// A searched chunk: its best candidates and the coarse tiles it had.
pub(crate) struct ChunkResult {
    cands: Vec<Candidate>,
    present: Vec<TileId>,
    missing: Vec<TileId>,
}

/// A zoom-11 re-check of one coarse candidate.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct RecheckKey {
    contour: ContourKey,
    transform: [u64; 6],
}

/// Ground distance (metres) between two candidates' map centres (the
/// centre of `rect`, the map's rectangle of the raster).
fn cand_dist_m(a: &Candidate, b: &Candidate, rect: &MapRect) -> f64 {
    let (cx, cy) = ((rect.x0 + rect.x1) as f64 / 2.0, (rect.y0 + rect.y1) as f64 / 2.0);
    let ll = |c: &Candidate| {
        let (mx, my) = c.transform.apply(cx, cy);
        let (la, lo) = merc_to_latlon(mx, my);
        LatLon::new(la, lo)
    };
    ground_distance_m(ll(a), ll(b))
}

impl Run<'_> {
    /// Search the wide area (the disc of the search radius minus the near
    /// disc) around `gps` at the scale range `scale`; Some(located) at the
    /// first accepted match.
    pub(super) fn wide_search(
        &mut self,
        prod: &ContourProducts,
        gps: LatLon,
        bands: &[ScaleBand],
    ) -> Step<Option<Box<GeorefResponse>>> {
        let (w, h) = self.map_size();
        let key = self.contour_key();
        let near = NEAR_RADIUS_M.min(self.radius_m);
        let origin = latlon_to_merc(gps.lat, gps.lon);
        let Some((wkey, plan, bases)) = self.wide_setup(gps, bands) else { return Ok(None) };
        let (zoom, radius) = (plan.zoom, wkey.radius_m as f64);
        let n = plan.chunks.len();
        let min_scale = min_searchable_scale(w, h, crate::dem::resolution(zoom), gps.lat);
        let mut log: Vec<Value> = Vec::new();
        for ci in 0..n {
            self.check_cancel()?;
            let done = self.wide_chunk(&wkey, ci);
            self.wide_counts(&wkey, &plan);
            self.set_progress(SearchStage::Wide, zoom, plan.searched_radius_m(ci, near, radius));
            let res = match done {
                Some(r) => r,
                None => {
                    self.need(&plan.chunks[ci].tiles, "wide", key)?;
                    let base = &bases[plan.chunks[ci].scale_band.min(bases.len() - 1)];
                    self.search_chunk(prod, &wkey, &plan, ci, base, origin, near)?
                }
            };
            self.searched_tiles.extend(res.present.iter().copied());
            self.missing_tiles.extend(res.missing.iter().copied());
            self.wide_counts(&wkey, &plan);
            // The next chunk's tiles ride along with this one's checks; not
            // past the labels' band (whose chunks usually hold the map).
            let lookahead = match plan.chunks.get(ci + 1) {
                Some(next) if self.wide_chunk(&wkey, ci + 1).is_none() && ci + 1 != plan.band_chunks => {
                    next.tiles.clone()
                }
                _ => Vec::new(),
            };
            let found = self.verify_chunk(prod, &wkey, ci, &res, &lookahead, &mut log);
            let rechecked = self.wide_rechecked(&wkey, ci);
            self.diag.insert(
                "wide".into(),
                json!({
                    "zoom": zoom,
                    "zooms": wkey.bands.iter().map(|b| b.0).collect::<Vec<u8>>(),
                    "chunks": n,
                    "chunks_searched": ci + 1,
                    "band_chunks": plan.band_chunks,
                    "radius_m": radius,
                    "tiles": plan.unique_tiles,
                    "min_scale_m_per_px": round_dp(min_scale, 3),
                    "rechecked": rechecked,
                    "refined": log,
                }),
            );
            if let Some(r) = found? {
                return Ok(Some(r));
            }
            self.s.bump_progress(0.35 + 0.6 * (ci + 1) as f32 / n.max(1) as f32);
        }
        self.set_progress(SearchStage::Wide, zoom, radius);
        Ok(None)
    }

    /// The wide search's key, plan (cached in the session) and the coarse
    /// search parameters of each scale band's chunks; None when the map is
    /// too small to be searched beyond the near disc at every scale
    /// (`wide_reach`). Each band is searched out to the radius the map's size
    /// at its scales allows (a coarser band's maps cover more ground and
    /// reach farther); the plan covers the farthest of them (`wide_limit`).
    /// The bands' chunks are searched in one sweep, nearest first
    /// (`wide::merge_plans`).
    fn wide_setup(&mut self, gps: LatLon, bands: &[ScaleBand]) -> Option<(WideKey, Arc<WidePlan>, Vec<SearchParams>)> {
        let (w, h) = self.map_size();
        let near = NEAR_RADIUS_M.min(self.radius_m);
        // A band inside another one searched on the same zoom at least as far
        // (a scale bar's inside the default range's) adds nothing to a sweep
        // nearest first: the other finds what it would, at the same chunks.
        let reaches = wide_band_reaches(self.radius_m, bands, w, h, gps.lat);
        let farthest = reaches.iter().flatten().map(|r| r.1).reduce(f64::max);
        self.wide_limit = match farthest {
            Some(r) if r < self.radius_m => Some(r),
            Some(_) => None,
            None => Some(near),
        };
        let reach_m = farthest?;
        let origin = latlon_to_merc(gps.lat, gps.lon);
        let mut bases = Vec::new();
        let mut wide_bands = Vec::new();
        for (band, reach) in bands.iter().zip(&reaches) {
            let Some((zoom, r)) = *reach else { continue };
            bases.push(SearchParams {
                center_merc: origin,
                search_radius_m: r,
                scale_min: band.lo,
                scale_max: band.hi,
                // North up: a rotation sweep would cost 36 times as much.
                rotations_deg: vec![0.0],
                top_k: CHUNK_TOP_K,
                region: None,
            });
            wide_bands.push((zoom, [band.lo.to_bits(), band.hi.to_bits()]));
        }
        let wkey = WideKey {
            contour: self.contour_key(),
            origin: [origin.0.round() as i64, origin.1.round() as i64],
            radius_m: reach_m.round() as i64,
            near_m: near.round() as i64,
            bands: wide_bands.clone(),
            max_tiles: self.max_tiles,
            band: self.band_merc().map(|b| [b.min_x, b.min_y, b.max_x, b.max_y].map(f64::to_bits)),
        };
        let (max_tiles, band) = (self.max_tiles, self.band_merc());
        let plan = self.wide_plan(&wkey, || {
            let plans = bases
                .iter()
                .zip(&wide_bands)
                .map(|(base, &(zoom, _))| {
                    let half = map_half_extent(base, w, h);
                    plan_wide(origin, base.search_radius_m, near, zoom, half, max_tiles, band)
                })
                .collect();
            merge_plans(plans)
        });
        Some((wkey, plan, bases))
    }

    /// Plan the wide search before the hint and near areas run, so the
    /// progress counts its tiles from the first call.
    pub(super) fn plan_wide_progress(&mut self, gps: LatLon, bands: &[ScaleBand]) {
        if let Some((wkey, plan, _)) = self.wide_setup(gps, bands) {
            self.wide_counts(&wkey, &plan);
        }
    }

    /// The cached plan for `wkey`, or a new one (replacing any other wide search).
    fn wide_plan(&mut self, wkey: &WideKey, make: impl FnOnce() -> WidePlan) -> Arc<WidePlan> {
        let mut cache = lock(&self.s.wide);
        if let Some(c) = cache.as_ref().filter(|c| c.key == *wkey) {
            return c.plan.clone();
        }
        let t = Instant::now();
        let plan = Arc::new(make());
        *cache = Some(WideCache { key: wkey.clone(), plan: plan.clone(), chunks: HashMap::new(), released: 0 });
        drop(cache);
        self.time("wide_plan", t);
        plan
    }

    fn wide_chunk(&self, wkey: &WideKey, ci: usize) -> Option<Arc<ChunkResult>> {
        lock(&self.s.wide).as_ref().filter(|c| c.key == *wkey).and_then(|c| c.chunks.get(&ci).cloned())
    }

    /// Update the wide plan's tile counts: planned, and supplied so far
    /// (known now, or released after their chunks were searched).
    fn wide_counts(&mut self, wkey: &WideKey, plan: &WidePlan) {
        let released = lock(&self.s.wide).as_ref().filter(|c| c.key == *wkey).map_or(0, |c| c.released);
        let known = {
            let store = lock(&self.s.dem);
            plan.last_use.keys().filter(|t| store.known(t)).count()
        };
        self.wide_tiles = (plan.unique_tiles, known + released);
        self.update_fetched();
    }

    /// Coarse search of chunk `ci` (its tiles are all known): keep its best
    /// candidates by correlation, cache them and release the tiles no later
    /// chunk needs.
    #[allow(clippy::too_many_arguments)]
    fn search_chunk(
        &mut self,
        prod: &ContourProducts,
        wkey: &WideKey,
        plan: &WidePlan,
        ci: usize,
        base: &SearchParams,
        origin: (f64, f64),
        near: f64,
    ) -> Step<Arc<ChunkResult>> {
        let (w, h) = self.map_size();
        let chunk = &plan.chunks[ci];
        let (present, missing): (Vec<TileId>, Vec<TileId>) = {
            let store = lock(&self.s.dem);
            chunk.tiles.iter().partition(|t| store.present(t))
        };
        let mut cands = if present.is_empty() {
            Vec::new()
        } else {
            let t = Instant::now();
            let dem = lock(&self.s.dem).mosaic(chunk.zoom, &chunk.mosaic);
            self.time("wide_dem", t);
            let b = chunk.placement;
            let params = SearchParams {
                center_merc: ((b.min_x + b.max_x) / 2.0, (b.min_y + b.max_y) / 2.0),
                region: Some(Region {
                    bounds: b,
                    origin,
                    r_min_m: near,
                    // The chunk's band's own reach (`wide_setup`).
                    r_max_m: base.search_radius_m,
                    exclude: chunk.exclude,
                }),
                ..base.clone()
            };
            let n = plan.chunks.len().max(1) as f32;
            let (lo, hi) = (0.35 + 0.6 * ci as f32 / n, 0.35 + 0.6 * (ci + 1) as f32 / n);
            let s = self.s;
            let t = Instant::now();
            let c = coarse_search(&prod.field, &dem, w, h, &params, &|| s.cancelled(), &|f| {
                s.bump_progress(lo + (hi - lo) * f)
            });
            drop(dem);
            self.time("wide_search", t);
            c.into_iter().map(|c| self.cand_to_page(c)).collect::<Vec<_>>()
        };
        // A cancelled search returns partial results: never cache them.
        self.check_cancel()?;
        cands.sort_by(|a, b| b.score.total_cmp(&a.score));
        cands.truncate(CHUNK_CANDIDATES);
        let res = Arc::new(ChunkResult { cands, present, missing });
        let release: Vec<TileId> = chunk
            .tiles
            .iter()
            .filter(|t| plan.last_use.get(t) == Some(&ci) && !self.pinned.contains(t) && !self.kept.contains(t))
            .copied()
            .collect();
        {
            let mut store = lock(&self.s.dem);
            for t in &release {
                store.release(t);
            }
        }
        if let Some(c) = lock(&self.s.wide).as_mut().filter(|c| c.key == *wkey) {
            c.chunks.insert(ci, res.clone());
            c.released += release.len();
        }
        Ok(res)
    }

    /// Re-check parameters for a coarse candidate, with the DEM box and zoom
    /// of the re-check: zoom 11 for the near zoom's band, one zoom finer than
    /// its own band for a small-scale map (`wide::recheck_zoom`).
    fn recheck_params(&self, c: &Candidate) -> (SearchParams, MercBox, u8) {
        let (w, h) = self.map_size();
        let p = SearchParams {
            center_merc: self.map_centre(&c.transform),
            search_radius_m: RECHECK_RADIUS_M,
            scale_min: c.scale_m_per_px / RECHECK_SCALE_TOL,
            scale_max: c.scale_m_per_px * RECHECK_SCALE_TOL,
            rotations_deg: vec![c.rotation_deg],
            top_k: 1,
            region: None,
        };
        let lat = merc_to_latlon(p.center_merc.0, p.center_merc.1).0;
        let z = recheck_zoom(c.scale_m_per_px, w, h, lat);
        let band = ScaleBand { zoom: z, lo: p.scale_min, hi: p.scale_max };
        let b = required_dem_box_capped(&p, w, h, band.max_box_half_m(lat));
        (p, b, z)
    }

    /// The zoom-11 re-check of one candidate (cached): its best peak 4 km
    /// around at ±10 % scale, with the local peak ratio. A hint or near
    /// area's match that only the fit gap among its few refined candidates
    /// made confident must stand out locally like a wide match (`area_search`).
    pub(super) fn recheck_one(&mut self, prod: &ContourProducts, c: &Candidate) -> Step<Option<Candidate>> {
        let rk = self.recheck_key(c);
        if let Some(r) = lock(&self.s.rechecks).get(&rk).cloned() {
            return Ok(r);
        }
        let (w, h) = self.map_size();
        let (p, b, z) = self.recheck_params(c);
        self.need(&tiles_covering(z, &b), "recheck", self.contour_key())?;
        let t = Instant::now();
        let dem = lock(&self.s.dem).mosaic(z, &b);
        let s = self.s;
        let r = coarse_search(&prod.field, &dem, w, h, &p, &|| s.cancelled(), &|_| {})
            .into_iter()
            .next()
            .map(|c| self.cand_to_page(c));
        self.time("recheck", t);
        self.check_cancel()?;
        lock(&self.s.rechecks).insert(rk, r.clone());
        Ok(r)
    }

    fn recheck_key(&self, c: &Candidate) -> RecheckKey {
        let t = &c.transform;
        RecheckKey { contour: self.contour_key(), transform: [t.a, t.b, t.c, t.d, t.e, t.f].map(f64::to_bits) }
    }

    /// Candidates of chunks up to `ci` re-checked so far (over all calls).
    fn wide_rechecked(&self, wkey: &WideKey, ci: usize) -> usize {
        let chunks: Vec<Arc<ChunkResult>> = (0..=ci).filter_map(|i| self.wide_chunk(wkey, i)).collect();
        let rechecks = lock(&self.s.rechecks);
        chunks.iter().flat_map(|r| r.cands.iter()).filter(|c| rechecks.contains_key(&self.recheck_key(c))).count()
    }

    /// The re-checks of chunks up to `ci` (cached), each candidate's best
    /// zoom-11 peak.
    fn wide_pool(&self, wkey: &WideKey, ci: usize) -> Vec<Candidate> {
        let chunks: Vec<Arc<ChunkResult>> = (0..=ci).filter_map(|i| self.wide_chunk(wkey, i)).collect();
        let rechecks = lock(&self.s.rechecks);
        chunks
            .iter()
            .flat_map(|r| r.cands.iter())
            .filter_map(|c| rechecks.get(&self.recheck_key(c)).cloned().flatten())
            .collect()
    }

    /// Re-check a chunk's candidates at zoom 11 and refine those that stand
    /// out; Some(located) for an accepted match. `lookahead`: the next
    /// chunk's tiles, fetched in the same round.
    #[allow(clippy::too_many_arguments)]
    fn verify_chunk(
        &mut self,
        prod: &ContourProducts,
        wkey: &WideKey,
        ci: usize,
        res: &ChunkResult,
        lookahead: &[TileId],
        log: &mut Vec<Value>,
    ) -> Step<Option<Box<GeorefResponse>>> {
        let (w, h) = self.map_size();
        let rect = self.map_rect();
        let key = self.contour_key();
        let zoom = wkey.bands.first().map_or(crate::wide::NEAR_ZOOM, |b| b.0);
        // 1. Zoom-11 re-checks (coarser far north or south: `near_zoom`, and
        //    for small-scale maps: `wide::recheck_zoom`).
        let plans: Vec<(Candidate, SearchParams, MercBox, u8)> = res
            .cands
            .iter()
            .map(|c| {
                let (p, b, z) = self.recheck_params(c);
                (c.clone(), p, b, z)
            })
            .collect();
        let todo: Vec<TileId> = {
            let cache = lock(&self.s.rechecks);
            plans
                .iter()
                .filter(|(c, ..)| !cache.contains_key(&self.recheck_key(c)))
                .flat_map(|(_, _, b, z)| tiles_covering(*z, b))
                .collect()
        };
        if !todo.is_empty() {
            self.set_progress(SearchStage::Refine, zoom, 0.0);
            self.need_more(&todo, lookahead, "recheck", key)?;
        }
        let mut used: Vec<TileId> = Vec::new();
        let mut strong: Vec<Candidate> = Vec::new();
        for (c, p, b, z) in &plans {
            self.check_cancel()?;
            let rk = self.recheck_key(c);
            let cached = lock(&self.s.rechecks).get(&rk).cloned();
            let r = match cached {
                Some(r) => r,
                None => {
                    let t = Instant::now();
                    let dem = lock(&self.s.dem).mosaic(*z, b);
                    let s = self.s;
                    let r = coarse_search(&prod.field, &dem, w, h, p, &|| s.cancelled(), &|_| {})
                        .into_iter()
                        .next()
                        .map(|c| self.cand_to_page(c));
                    self.time("wide_recheck", t);
                    self.check_cancel()?;
                    used.extend(tiles_covering(*z, b));
                    lock(&self.s.rechecks).insert(rk, r.clone());
                    r
                }
            };
            if let Some(r) = r.filter(|r| r.peak_ratio >= WIDE_MIN_RATIO && r.score >= WIDE_MIN_NCC) {
                strong.push(r);
            }
        }
        // Re-check tiles go once used (the near and hint areas' stay).
        if !used.is_empty() {
            let mut store = lock(&self.s.dem);
            for t in used.iter().filter(|t| !self.pinned.contains(t) && !self.kept.contains(t)) {
                store.release(t);
            }
        }
        // 2. Refine those that stand out, best first.
        strong.sort_by(|a, b| b.score.total_cmp(&a.score));
        // Candidates apart by the distinct distance of the larger map extent.
        let apart = |a: &Candidate, b: &Candidate| {
            cand_dist_m(a, b, &rect) >= distinct_m(a.scale_m_per_px.max(b.scale_m_per_px), w, h)
        };
        let mut distinct: Vec<Candidate> = Vec::new();
        for c in strong {
            if distinct.iter().all(|d| apart(d, &c)) {
                distinct.push(c);
            }
        }
        let pool = self.wide_pool(wkey, ci);
        for g in distinct {
            self.check_cancel()?;
            let other = pool.iter().filter(|p| apart(p, &g)).map(|p| p.score).fold(0.0f32, f32::max);
            let margin = g.score / other.max(0.05);
            let Some(fp) = self.fine_plan(&g.transform) else { continue };
            let rk = self.recheck_key(&g);
            let cached = lock(&self.s.wide_refined).get(&rk).cloned();
            let refined = match cached {
                Some(r) => r,
                None => {
                    self.set_progress(SearchStage::Refine, zoom, 0.0);
                    self.need_more(&fp.tiles, lookahead, "refine", key)?;
                    let r = self.refine_one(prod, key, &fp, &g.transform, false);
                    self.check_cancel()?;
                    lock(&self.s.wide_refined).insert(rk, r.clone());
                    r
                }
            };
            let Some((r, fzoom)) = refined else { continue };
            let conf = confidence(&g, &r);
            let (mx, my) = self.map_centre(&g.transform);
            let (lat, lon) = merc_to_latlon(mx, my);
            let accepted = conf >= CONFIDENT && margin >= WIDE_NCC_MARGIN;
            if !accepted {
                // A rejected candidate's zoom-13 tiles go (its outcome is
                // cached); the accepted one keeps them for its answer.
                let mut store = lock(&self.s.dem);
                for t in fp.tiles.iter().filter(|t| !self.pinned.contains(t) && !self.kept.contains(t)) {
                    store.release(t);
                }
            }
            let entry = json!({
                "chunk": ci,
                "center": [round_dp(lat, 5), round_dp(lon, 5)],
                "recheck_ncc": round_dp(g.score as f64, 3),
                "recheck_ratio": round_dp(g.peak_ratio as f64, 2),
                "ncc_margin": round_dp(margin as f64, 2),
                "fit_confidence": round_dp(fit_confidence(&r), 3),
                "confidence": round_dp(conf, 3),
                "accepted": accepted,
            });
            log.push(entry.clone());
            if accepted {
                let resp = self.contour_ok(&r, conf, (fzoom, &fp.bbox), false, prod);
                let mut sel = entry;
                sel["source"] = json!("wide");
                return Ok(Some(self.own_diag(resp, sel)));
            }
        }
        Ok(None)
    }
}
