// SPDX-License-Identifier: MIT
//! Cost forecast and cap ladder. Data only (no UI): the engine stores both in `meta.json` after every turn, the panel reads them and raising
//! the limit is ONE step (`max_cost_usd` := `cost_cap_ladder.suggested`).
//!
//! * `cost_forecast`: what was spent, the average per turn, the turns still expected, the estimated cost left and whether the cap is enough.
//! * `cost_cap_ladder`: the current (saved) cap, the rungs above it and the `suggested` rung that covers the forecast.
//! * `turn_budget`: the per-turn budget of the final E2E phase, sized by the number of features still to check, so one serial turn is not
//!   cut mid-way.

use serde_json::{json, Value};

/// Multipliers of the current cap that make the rungs of the ladder (rounded up to a multiple of 5).
pub const LADDER_FACTORS: [f64; 4] = [1.25, 1.5, 2.0, 3.0];
/// The forecast is padded by this factor so the suggested cap is not the exact estimate.
pub const FORECAST_MARGIN: f64 = 1.2;
/// Largest multiple of the per-turn budget the E2E phase may ask for in one turn.
pub const MAX_E2E_BUDGET_FACTOR: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub struct Forecast {
    pub spent: f64,
    pub per_turn: f64,
    pub remaining_turns: u64,
    pub estimated_remaining: f64,
    pub estimated_total: f64,
    pub cap: f64,
    pub exceeds_cap: bool,
}

/// `turns` run so far at `spent` US$; `open_features` not yet closed (verified/blocked); `e2e_pending` verified features still to check
/// in the final phase. `closed_features` sets how many turns one feature took. `None` before the first turn (nothing to learn from).
pub fn forecast(spent: f64, turns: u64, closed_features: u64, open_features: u64, e2e_pending: u64, cap: f64) -> Option<Forecast> {
    if turns == 0 || spent <= 0.0 {
        return None;
    }
    let per_turn = spent / turns as f64;
    let turns_per_feature = (turns as f64 / closed_features.max(1) as f64).max(1.0);
    let remaining_turns = (open_features as f64 * turns_per_feature).ceil() as u64 + e2e_pending;
    let estimated_remaining = per_turn * remaining_turns as f64;
    let estimated_total = spent + estimated_remaining;
    Some(Forecast {
        spent: round2(spent),
        per_turn: round2(per_turn),
        remaining_turns,
        estimated_remaining: round2(estimated_remaining),
        estimated_total: round2(estimated_total),
        cap,
        exceeds_cap: cap > 0.0 && estimated_total > cap,
    })
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn up_to_five(value: f64) -> f64 {
    (value / 5.0).ceil() * 5.0
}

/// Rungs above `cap`, strictly increasing. Empty when there is no cap (0 = no limit).
pub fn cap_ladder(cap: f64) -> Vec<f64> {
    if cap <= 0.0 {
        return Vec::new();
    }
    let mut rungs: Vec<f64> = Vec::new();
    for factor in LADDER_FACTORS {
        let rung = up_to_five(cap * factor);
        if rung > cap && rungs.last().is_none_or(|last| rung > *last) {
            rungs.push(rung);
        }
    }
    rungs
}

/// The first rung that covers the estimated total (with margin); the last rung when none does; `None` when there is no cap.
pub fn suggested_cap(cap: f64, estimate: Option<&Forecast>) -> Option<f64> {
    let rungs = cap_ladder(cap);
    let need = estimate.map(|f| f.estimated_total * FORECAST_MARGIN).unwrap_or(0.0);
    rungs.iter().copied().find(|r| *r >= need).or_else(|| rungs.last().copied())
}

/// Writes `cost_forecast` and `cost_cap_ladder` into the run meta. Never touches `max_cost_usd` (owned by the app).
pub fn store(meta: &mut Value, estimate: Option<&Forecast>, cap: f64) {
    let Some(map) = meta.as_object_mut() else { return };
    match estimate {
        Some(f) => {
            map.insert(
                "cost_forecast".into(),
                json!({
                    "spent": f.spent, "per_turn": f.per_turn, "remaining_turns": f.remaining_turns,
                    "estimated_remaining": f.estimated_remaining, "estimated_total": f.estimated_total,
                    "cap": f.cap, "exceeds_cap": f.exceeds_cap
                }),
            );
        }
        None => {
            map.remove("cost_forecast");
        }
    }
    map.insert("cost_cap_ladder".into(), json!({"saved": cap, "rungs": cap_ladder(cap), "suggested": suggested_cap(cap, estimate)}));
}

/// Per-turn budget. Outside the E2E phase it is `base`. In the E2E phase (`e2e_pending` features to check, run serially in one turn) it is
/// `base` times the number of features, up to `MAX_E2E_BUDGET_FACTOR`, and never above what is left under the cap (when there is one,
/// but never below `base`).
pub fn turn_budget(base: f64, e2e_phase: bool, e2e_pending: usize, cost: f64, cap: f64) -> f64 {
    if !e2e_phase || e2e_pending <= 1 {
        return base;
    }
    let wanted = base * e2e_pending.min(MAX_E2E_BUDGET_FACTOR) as f64;
    if cap > 0.0 {
        wanted.min((cap - cost).max(base))
    } else {
        wanted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forecast_estimates_the_rest() {
        assert_eq!(forecast(0.0, 0, 0, 5, 0, 50.0), None);
        let f = forecast(40.0, 20, 4, 4, 8, 50.0).unwrap();
        assert_eq!((f.per_turn, f.remaining_turns), (2.0, 28)); // 5 turns per feature * 4 + 8 checks
        assert_eq!((f.estimated_remaining, f.estimated_total, f.exceeds_cap), (56.0, 96.0, true));
        assert!(!forecast(10.0, 5, 5, 0, 0, 50.0).unwrap().exceeds_cap);
        assert!(!forecast(10.0, 5, 5, 9, 0, 0.0).unwrap().exceeds_cap); // no cap, nothing to exceed
    }

    #[test]
    fn ladder_rises_and_suggests_one_step() {
        assert_eq!(cap_ladder(50.0), vec![65.0, 75.0, 100.0, 150.0]);
        assert_eq!(cap_ladder(0.0), Vec::<f64>::new());
        let f = forecast(40.0, 20, 4, 4, 8, 50.0).unwrap();
        assert_eq!(suggested_cap(50.0, Some(&f)), Some(150.0)); // 96 * 1.2 = 115.2 -> beyond the rungs up to 100 -> 150
        let small = forecast(40.0, 20, 4, 1, 0, 50.0).unwrap(); // total 50 * 1.2 = 60 -> 65
        assert_eq!(suggested_cap(50.0, Some(&small)), Some(65.0));
        assert_eq!(suggested_cap(0.0, None), None);
    }

    #[test]
    fn store_keeps_the_app_owned_key() {
        let mut meta = json!({"max_cost_usd": 65.0});
        let f = forecast(40.0, 20, 4, 1, 0, 65.0);
        store(&mut meta, f.as_ref(), 65.0);
        assert_eq!(meta["max_cost_usd"], 65.0);
        assert_eq!(meta["cost_cap_ladder"]["saved"], 65.0);
        assert_eq!(meta["cost_cap_ladder"]["rungs"][0], 85.0);
        assert_eq!(meta["cost_forecast"]["exceeds_cap"], false);
        store(&mut meta, None, 65.0);
        assert!(meta.get("cost_forecast").is_none());
    }

    #[test]
    fn e2e_turn_budget_scales_with_features() {
        assert_eq!(turn_budget(5.0, false, 8, 0.0, 0.0), 5.0);
        assert_eq!(turn_budget(5.0, true, 1, 0.0, 0.0), 5.0);
        assert_eq!(turn_budget(5.0, true, 3, 0.0, 0.0), 15.0);
        assert_eq!(turn_budget(5.0, true, 30, 0.0, 0.0), 40.0);
        assert_eq!(turn_budget(5.0, true, 8, 40.0, 50.0), 10.0); // only 10 left under the cap
        assert_eq!(turn_budget(5.0, true, 8, 49.0, 50.0), 5.0); // never below the base
    }
}
