// Copyright 2026 Chris Olson
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The statistics behind guard checks: Wilson score intervals, Newcombe's hybrid score
//! interval for a difference of proportions (method 10 of Newcombe 1998), Welch's interval for
//! a difference of means with a normal quantile, and the status rule on an interval of harm.
//!
//! An interval's z is `Z95` by default. A spec with `confidence` splits its error budget across
//! its interval checks (`bonferroni_z`, from the standard normal quantile `normal_quantile`);
//! with `sequential` each check uses the anytime z of Robbins' normal-mixture boundary
//! (`anytime_z`, its mixture parameter from `mixture_rho`), the asymptotic confidence sequence
//! of Waudby-Smith, Ramdas et al. (2024), *Time-uniform central limit theory and asymptotic
//! confidence sequences*.

use crate::guard_report::Status;

/// The one-sided 95% normal quantile: each bound of an interval built with it holds with 95%
/// confidence on its own.
pub(crate) const Z95: f64 = 1.6448536269514722;

/// The Wilson score interval of `successes` out of `n` (`n > 0`) at quantile `z`, clamped to
/// `[0, 1]`.
pub(crate) fn wilson(successes: u64, n: u64, z: f64) -> (f64, f64) {
    let n = n as f64;
    let p = successes as f64 / n;
    let z2 = z * z;
    let denominator = 1.0 + z2 / n;
    let center = (p + z2 / (2.0 * n)) / denominator;
    let half = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / denominator;
    ((center - half).max(0.0), (center + half).min(1.0))
}

/// Newcombe's hybrid score interval (method 10) of `p2 − p1`, where `p1 = s1 / n1` and
/// `p2 = s2 / n2` (`n1, n2 > 0`), built from the Wilson intervals `[l1, u1]` and `[l2, u2]`:
/// `L = d − √((p2 − l2)² + (u1 − p1)²)`, `U = d + √((u2 − p2)² + (p1 − l1)²)`.
pub(crate) fn newcombe_diff(s1: u64, n1: u64, s2: u64, n2: u64, z: f64) -> (f64, f64) {
    let p1 = s1 as f64 / n1 as f64;
    let p2 = s2 as f64 / n2 as f64;
    let (l1, u1) = wilson(s1, n1, z);
    let (l2, u2) = wilson(s2, n2, z);
    let d = p2 - p1;
    let lower = d - ((p2 - l2).powi(2) + (u1 - p1).powi(2)).sqrt();
    let upper = d + ((u2 - p2).powi(2) + (p1 - l1).powi(2)).sqrt();
    (lower, upper)
}

/// Welch's interval of `mean2 − mean1` with the normal quantile `z` (no t distribution), from
/// sample standard deviations and sizes (`n1, n2 > 0`). Zero standard deviations give
/// `L = U = mean2 − mean1`.
pub(crate) fn welch_diff(
    mean1: f64,
    sd1: f64,
    n1: u64,
    mean2: f64,
    sd2: f64,
    n2: u64,
    z: f64,
) -> (f64, f64) {
    let d = mean2 - mean1;
    let se = (sd1 * sd1 / n1 as f64 + sd2 * sd2 / n2 as f64).sqrt();
    (d - z * se, d + z * se)
}

/// The status of a check whose harm (positive = worse) lies in `[lower, upper]`: `Worse` when
/// `lower > tolerance`, `Ok` when `upper <= tolerance`, else `Unsure`.
pub(crate) fn classify(lower: f64, upper: f64, tolerance: f64) -> Status {
    if lower > tolerance {
        Status::Worse
    } else if upper <= tolerance {
        Status::Ok
    } else {
        Status::Unsure
    }
}

/// The standard normal quantile Φ⁻¹(`p`) for `p` in (0, 1): Wichura's algorithm AS 241
/// (PPND16), a rational approximation accurate to about 1e-16.
#[allow(clippy::excessive_precision)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn normal_quantile(p: f64) -> f64 {
    /// `c[0] + c[1]·x + … + c[7]·x⁷` by Horner's rule.
    fn poly(c: &[f64; 8], x: f64) -> f64 {
        c.iter().rev().fold(0.0, |acc, k| acc * x + k)
    }
    const A: [f64; 8] = [
        3.387132872796366608,
        1.3314166789178437745e2,
        1.9715909503065514427e3,
        1.3731693765509461125e4,
        4.5921953931549871457e4,
        6.7265770927008700853e4,
        3.3430575583588128105e4,
        2.5090809287301226727e3,
    ];
    const B: [f64; 8] = [
        1.0,
        4.2313330701600911252e1,
        6.8718700749205790830e2,
        5.3941960214247511077e3,
        2.1213794301586595867e4,
        3.9307895800092710610e4,
        2.8729085735721942674e4,
        5.2264952788528545610e3,
    ];
    const C: [f64; 8] = [
        1.42343711074968357734,
        4.63033784615654529590,
        5.76949722146069140550,
        3.64784832476320460504,
        1.27045825245236838258,
        2.41780725177450611770e-1,
        2.27238449892691845833e-2,
        7.74545014278341407640e-4,
    ];
    const D: [f64; 8] = [
        1.0,
        2.05319162663775882187,
        1.67638483018380384940,
        6.89767334985100004550e-1,
        1.48103976427480074590e-1,
        1.51986665636164571966e-2,
        5.47593808499534494600e-4,
        1.05075007164441684324e-9,
    ];
    const E: [f64; 8] = [
        6.65790464350110377720,
        5.46378491116411436990,
        1.78482653991729133580,
        2.96560571828504891230e-1,
        2.65321895265761230930e-2,
        1.24266094738807843860e-3,
        2.71155556874348757815e-5,
        2.01033439929228813265e-7,
    ];
    const F: [f64; 8] = [
        1.0,
        5.99832206555887937690e-1,
        1.36929880922735805310e-1,
        1.48753612908506148525e-2,
        7.86869131145613259100e-4,
        1.84631831751005468180e-5,
        1.42151175831644588870e-7,
        2.04426310338993978564e-15,
    ];
    let q = p - 0.5;
    if q.abs() <= 0.425 {
        let r = 0.180625 - q * q;
        return q * poly(&A, r) / poly(&B, r);
    }
    let r = if q < 0.0 { p } else { 1.0 - p };
    let r = (-r.ln()).sqrt();
    let x = if r <= 5.0 {
        let r = r - 1.6;
        poly(&C, r) / poly(&D, r)
    } else {
        let r = r - 5.0;
        poly(&E, r) / poly(&F, r)
    };
    if q < 0.0 { -x } else { x }
}

/// The one-sided normal quantile of each of `k` checks (`k ≥ 1`) sharing the error budget
/// `alpha` (Bonferroni): Φ⁻¹(1 − `alpha` ÷ `k`).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn bonferroni_z(alpha: f64, k: u32) -> f64 {
    normal_quantile(1.0 - alpha / f64::from(k))
}

/// The mixture parameter ρ > 0 that makes `anytime_z` smallest at the total sample `n_star`
/// for level `a`: a golden-section search over ln ρ in [ln(`n_star`·1e-4), ln(`n_star`·1e4)],
/// stopped once the bracket is narrower than 1e-9 (a relative tolerance of 1e-9 on ρ).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn mixture_rho(n_star: f64, a: f64) -> f64 {
    // z² as a function of ln ρ; it has one minimum.
    let z2 = |ln_rho: f64| {
        let rho = ln_rho.exp();
        (1.0 + rho / n_star) * ((n_star + rho) / (rho * a * a)).ln()
    };
    let shrink = (5.0_f64.sqrt() - 1.0) / 2.0;
    let (mut lo, mut hi) = ((n_star * 1e-4).ln(), (n_star * 1e4).ln());
    let mut c = hi - shrink * (hi - lo);
    let mut d = lo + shrink * (hi - lo);
    let (mut fc, mut fd) = (z2(c), z2(d));
    while hi - lo > 1e-9 {
        if fc < fd {
            hi = d;
            d = c;
            fd = fc;
            c = hi - shrink * (hi - lo);
            fc = z2(c);
        } else {
            lo = c;
            c = d;
            fc = fd;
            d = lo + shrink * (hi - lo);
            fd = z2(d);
        }
    }
    ((lo + hi) / 2.0).exp()
}

/// The anytime z at total sample `n` (`n ≥ 1`) with mixture parameter `rho` and level `a`:
/// √((1 + ρ/n) · ln((n + ρ) / (ρ · a²))). This is Robbins' two-sided normal-mixture boundary at
/// level `a`, used as a one-sided level-`a` bound on each side (conservative); it is above
/// Φ⁻¹(1 − `a`) for every `n`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn anytime_z(n: u64, rho: f64, a: f64) -> f64 {
    let n = n as f64;
    ((1.0 + rho / n) * ((n + rho) / (rho * a * a)).ln()).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `x` rounded to 4 decimals.
    fn r4(x: f64) -> f64 {
        (x * 1e4).round() / 1e4
    }

    fn r4_pair((a, b): (f64, f64)) -> (f64, f64) {
        (r4(a), r4(b))
    }

    #[test]
    fn wilson_known_values() {
        assert_eq!(r4_pair(wilson(81, 263, 1.96)), (0.2553, 0.3662));
        assert_eq!(r4_pair(wilson(15, 148, 1.96)), (0.0624, 0.1605));
        assert_eq!(r4_pair(wilson(1, 29, 1.96)), (0.0061, 0.1718));
    }

    #[test]
    fn wilson_edges_are_clamped() {
        let (l, u) = wilson(0, 20, 1.96);
        assert_eq!(l, 0.0);
        assert_eq!(r4(u), 0.1611);
        let (l, u) = wilson(29, 29, 1.96);
        assert_eq!(r4(l), 0.8830);
        assert_eq!(u, 1.0);
    }

    /// Newcombe (1998), Table II, method 10, at z = 1.96. The paper's difference is
    /// `a/m − b/n`; `newcombe_diff` takes the subtracted side first.
    #[test]
    fn newcombe_1998_table_ii() {
        let cases = [
            ((56, 70), (48, 80), (0.0524, 0.3339)),
            ((9, 10), (3, 10), (0.1705, 0.8090)),
            ((6, 7), (2, 7), (0.0582, 0.8063)),
            ((5, 56), (0, 29), (-0.0381, 0.1926)),
            ((0, 10), (0, 20), (-0.1611, 0.2775)),
            ((0, 10), (0, 10), (-0.2775, 0.2775)),
            ((10, 10), (0, 20), (0.6791, 1.0000)),
            ((10, 10), (0, 10), (0.6075, 1.0000)),
        ];
        for ((a, m), (b, n), expected) in cases {
            assert_eq!(
                r4_pair(newcombe_diff(b, n, a, m, 1.96)),
                expected,
                "{a}/{m} - {b}/{n}"
            );
        }
    }

    #[test]
    fn newcombe_is_antisymmetric() {
        let (l, u) = newcombe_diff(48, 80, 56, 70, 1.96);
        let (l2, u2) = newcombe_diff(56, 70, 48, 80, 1.96);
        assert!((l + u2).abs() < 1e-12 && (u + l2).abs() < 1e-12);
    }

    #[test]
    fn newcombe_no_errors_on_either_side_at_z95() {
        // 0/100 against 0/100: the README's example of how wide a small sample is.
        let (l, u) = newcombe_diff(0, 100, 0, 100, Z95);
        assert_eq!(r4(u), 0.0263);
        assert_eq!(r4(l), -0.0263);
        let (wl, wu) = wilson(0, 100, Z95);
        assert_eq!(wl, 0.0);
        assert!((wu - Z95 * Z95 / (100.0 + Z95 * Z95)).abs() < 1e-12);
    }

    #[test]
    fn welch_by_hand() {
        // d = 3.8 - 4.0 = -0.2; se = sqrt(1.0²/100 + 1.2²/50) = sqrt(0.0388) = 0.196977...
        let (l, u) = welch_diff(4.0, 1.0, 100, 3.8, 1.2, 50, 1.96);
        assert_eq!(r4(l), -0.5861);
        assert_eq!(r4(u), 0.1861);
        let se = 0.0388_f64.sqrt();
        assert!((l - (-0.2 - 1.96 * se)).abs() < 1e-12);
        assert!((u - (-0.2 + 1.96 * se)).abs() < 1e-12);
    }

    #[test]
    fn welch_zero_standard_deviations_give_the_difference() {
        let (l, u) = welch_diff(4.0, 0.0, 30, 3.5, 0.0, 40, Z95);
        assert_eq!((l, u), (-0.5, -0.5));
    }

    #[test]
    fn classify_each_outcome() {
        assert_eq!(classify(0.03, 0.08, 0.02), Status::Worse);
        assert_eq!(classify(-0.05, 0.02, 0.02), Status::Ok);
        assert_eq!(classify(-0.05, 0.01, 0.02), Status::Ok);
        assert_eq!(classify(0.02, 0.05, 0.02), Status::Unsure);
        assert_eq!(classify(-0.01, 0.03, 0.02), Status::Unsure);
        assert_eq!(classify(0.1, 0.1, 0.05), Status::Worse);
        assert_eq!(classify(0.05, 0.05, 0.05), Status::Ok);
    }

    #[test]
    fn normal_quantile_known_values() {
        assert!((normal_quantile(0.975) - 1.959963984540054).abs() < 1e-9);
        assert!((normal_quantile(0.95) - Z95).abs() < 1e-9);
        assert_eq!(normal_quantile(0.5), 0.0);
        for (p, want) in [
            (0.6, 0.2533471031357998),
            (0.9, 1.2815515655446006),
            (0.99, 2.3263478740408408),
            (0.999, 3.090232306167813),
            (1.0 - 1e-6, 4.753424308817086),
        ] {
            let got = normal_quantile(p);
            assert!((got - want).abs() < 1e-9, "{p}: {got}");
            // The lower tail mirrors the upper one.
            assert!((normal_quantile(1.0 - p) + got).abs() < 1e-9, "{p}");
        }
    }

    #[test]
    fn bonferroni_splits_alpha_across_the_checks() {
        assert!((bonferroni_z(0.05, 2) - normal_quantile(0.975)).abs() < 1e-12);
        assert!((bonferroni_z(0.05, 1) - Z95).abs() < 1e-9);
        assert!((bonferroni_z(0.05, 3) - 2.128045234184984).abs() < 1e-9);
    }

    #[test]
    fn anytime_z_by_hand() {
        // √((1 + 50/1000) · ln((1000 + 50) / (50 · 0.05²))) = √(1.05 · ln 8400)
        let z = anytime_z(1000, 50.0, 0.05);
        assert!((z - 3.0802250460109204).abs() < 1e-12, "{z}");
    }

    #[test]
    fn mixture_rho_minimises_z_at_n_star() {
        for n_star in [20_u64, 200, 2000] {
            let ns = n_star as f64;
            for a in [0.05, 0.01, 0.001] {
                let rho = mixture_rho(ns, a);
                // Well inside the searched bracket [n*·1e-4, n*·1e4].
                assert!(rho > ns * 1e-3 && rho < ns * 1e3, "{n_star} {a}: {rho}");
                // At the minimum, x = n*/ρ solves x = ln(1 + x) + ln(1/a²).
                let x = ns / rho;
                let residual = x - (1.0 + x).ln() - (1.0 / (a * a)).ln();
                assert!(residual.abs() < 1e-5, "{n_star} {a}: {residual}");
                let at = anytime_z(n_star, rho, a);
                assert!(at <= anytime_z(n_star / 2, rho, a), "{n_star} {a}");
                assert!(at <= anytime_z(n_star * 2, rho, a), "{n_star} {a}");
            }
        }
        let z = anytime_z(200, mixture_rho(200.0, 0.05), 0.05);
        assert!((z - 3.0351224130285512).abs() < 1e-9, "{z}");
    }

    #[test]
    fn the_anytime_z_is_wider_than_the_fixed_one_at_every_n() {
        for a in [0.05, 0.01, 0.001] {
            let fixed = normal_quantile(1.0 - a);
            for n_star in [20.0, 200.0] {
                let rho = mixture_rho(n_star, a);
                for n in [2, 10, 100, 10_000, 1_000_000] {
                    let z = anytime_z(n, rho, a);
                    assert!(z > fixed, "a {a}, n* {n_star}, n {n}: {z} <= {fixed}");
                }
            }
        }
    }

    /// SplitMix64: advances `state` and returns the next 64-bit output.
    fn splitmix64(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// The shares of 2,000 simulated rollouts that some look wrongly classifies `worse`, as
    /// `(sequential, fixed Z95)`. Both sides fail 5% of calls; each look adds 200 calls, each
    /// going to the rollout with probability `share`; 50 cumulative looks; a look counts once
    /// both sides have 100 calls (`min_calls`); k = 1, a = 0.05, tolerance 0, n* = 200. Each
    /// call draws one SplitMix64 output: its high 32 bits pick the side, its low 32 bits the
    /// error.
    fn false_aborts(share: f64) -> (f64, f64) {
        const ROLLOUTS: u32 = 2_000;
        let mut state: u64 = 20_261_126;
        let share_cut = (share * 4_294_967_296.0) as u64;
        let error_cut = (0.05 * 4_294_967_296.0) as u64;
        let a = 0.05;
        let rho = mixture_rho(200.0, a);
        let (mut sequential, mut fixed) = (0_u32, 0_u32);
        for _ in 0..ROLLOUTS {
            let (mut nb, mut eb, mut nr, mut er) = (0_u64, 0_u64, 0_u64, 0_u64);
            let (mut sequential_hit, mut fixed_hit) = (false, false);
            for _ in 0..50 {
                for _ in 0..200 {
                    let x = splitmix64(&mut state);
                    let error = u64::from((x & 0xFFFF_FFFF) < error_cut);
                    if (x >> 32) < share_cut {
                        nr += 1;
                        er += error;
                    } else {
                        nb += 1;
                        eb += error;
                    }
                }
                if nb < 100 || nr < 100 {
                    continue;
                }
                let worse = |z: f64| {
                    let (lower, upper) = newcombe_diff(eb, nb, er, nr, z);
                    classify(lower, upper, 0.0) == Status::Worse
                };
                sequential_hit = sequential_hit || worse(anytime_z(nb + nr, rho, a));
                fixed_hit = fixed_hit || worse(Z95);
            }
            sequential += u32::from(sequential_hit);
            fixed += u32::from(fixed_hit);
        }
        (
            f64::from(sequential) / f64::from(ROLLOUTS),
            f64::from(fixed) / f64::from(ROLLOUTS),
        )
    }

    /// R12. Measured while writing the plan: share 0.5 gives 0.30% sequential and 25.25%
    /// fixed; share 0.1 gives 1.15% and 24.65%.
    #[test]
    fn the_anytime_z_bounds_false_aborts_over_repeated_looks() {
        for share in [0.5, 0.1] {
            let (sequential, fixed) = false_aborts(share);
            assert!(sequential <= 0.02, "share {share}: sequential {sequential}");
            assert!(fixed > 0.15, "share {share}: fixed {fixed}");
        }
    }
}
