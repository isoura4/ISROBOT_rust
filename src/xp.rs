//! Formule XP ↔ niveau (identique à `utils/xp.py`).

pub const LEVEL_MULTIPLIER: f64 = 125.0;

pub fn level_from_xp(xp: f64) -> i64 {
    let xp = xp.max(0.0);
    (xp / LEVEL_MULTIPLIER).sqrt() as i64 + 1
}

pub fn xp_for_level(level: i64) -> f64 {
    ((level - 1) as f64).powi(2) * LEVEL_MULTIPLIER
}

/// Affiche un nombre d'XP sans « .0 » superflu.
pub fn fmt_xp(xp: f64) -> String {
    if xp.fract() == 0.0 {
        format!("{}", xp as i64)
    } else {
        format!("{xp:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_match_python() {
        assert_eq!(level_from_xp(0.0), 1);
        assert_eq!(level_from_xp(124.0), 1);
        assert_eq!(level_from_xp(125.0), 2);
        assert_eq!(level_from_xp(500.0), 3);
        assert_eq!(level_from_xp(-10.0), 1);
        assert_eq!(xp_for_level(1), 0.0);
        assert_eq!(xp_for_level(3), 500.0);
        for lvl in 1..50 {
            assert_eq!(level_from_xp(xp_for_level(lvl)), lvl);
        }
    }
}
