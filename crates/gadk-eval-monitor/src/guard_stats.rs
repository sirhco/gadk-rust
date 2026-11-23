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
}
