/*
 * Copyright (c) 2020, The Linux Foundation. All rights reserved.
 * Changes from Qualcomm Innovation Center are provided under the following license:
 * Copyright (c) 2023 Qualcomm Innovation Center, Inc. All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause-Clear
 */

//! Throttling severity from a reading, and the userspace trip point that reports the next
//! change. No side effects here.

/// ThrottlingSeverity NONE..=SHUTDOWN.
pub const LEVELS: usize = 7;

pub struct Thresholds {
    /// Per level, in the HAL unit (C, mV, %...); NaN where the level has no threshold.
    pub hot: [f32; LEVELS],
    pub cold: [f32; LEVELS],
    pub positive: bool,
    /// The margin a reading must clear before the severity drops again.
    pub hysteresis: f32,
}

impl Thresholds {
    fn reached(&self, level: usize, value: f32, margin: f32) -> bool {
        if self.positive {
            !self.hot[level].is_nan() && value >= self.hot[level] - margin
        } else {
            !self.cold[level].is_nan() && value <= self.cold[level] + margin
        }
    }

    /// The severity of `value` when the current one is `current`. A level already reached
    /// holds until the reading clears it by the hysteresis.
    pub fn severity(&self, current: usize, value: f32) -> usize {
        (0..LEVELS)
            .rev()
            .find(|&level| {
                let margin = if level <= current { self.hysteresis } else { 0.0 };
                self.reached(level, value, margin)
            })
            .unwrap_or(0)
    }

    /// The trip point (and its hysteresis) that fires on the next severity change from
    /// `current`, in the zone's unit (`scale` per HAL unit). The trip sits at the next level up;
    /// its hysteresis reaches down to the current level minus the hysteresis. At the top level
    /// the trip stays at the current threshold so dropping below it still reports.
    pub fn trip(&self, current: usize, scale: i32) -> Option<(i32, Option<i32>)> {
        if !self.positive {
            return None;
        }
        let to_zone = |value: f32| (value * scale as f32) as i32;
        let next = (current + 1..LEVELS).find(|&l| !self.hot[l].is_nan()).map(|l| to_zone(self.hot[l]));
        let hysteresis = to_zone(self.hysteresis);
        if current == 0 || self.hot[current].is_nan() {
            return next.map(|trip| (trip, None));
        }
        let at = to_zone(self.hot[current]);
        Some(match next {
            Some(trip) => (trip, Some(trip - at + hysteresis)),
            None => (at, Some(hysteresis)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAN: f32 = f32::NAN;

    fn skin() -> Thresholds {
        Thresholds {
            hot: [NAN, 48.0, 49.0, 50.0, 60.0, 61.0, 90.0],
            cold: [NAN; LEVELS],
            positive: true,
            hysteresis: 2.0,
        }
    }

    #[test]
    fn rises_through_levels() {
        let t = skin();
        assert_eq!(t.severity(0, 47.9), 0);
        assert_eq!(t.severity(0, 48.0), 1);
        assert_eq!(t.severity(0, 55.0), 3);
        assert_eq!(t.severity(0, 95.0), 6);
    }

    #[test]
    fn falls_only_past_the_hysteresis() {
        let t = skin();
        assert_eq!(t.severity(3, 48.5), 3);
        assert_eq!(t.severity(3, 47.9), 2);
        assert_eq!(t.severity(1, 46.0), 1);
        assert_eq!(t.severity(1, 45.9), 0);
    }

    #[test]
    fn falling_values_for_battery_voltage() {
        let mut cold = [NAN; LEVELS];
        cold[3] = 3.2;
        cold[6] = 3.0;
        let t = Thresholds { hot: [NAN; LEVELS], cold, positive: false, hysteresis: 0.1 };
        assert_eq!(t.severity(0, 3.3), 0);
        assert_eq!(t.severity(0, 3.2), 3);
        assert_eq!(t.severity(0, 2.99), 6);
        assert_eq!(t.severity(3, 3.25), 3);
        // Recovers once the voltage is back above the threshold plus the hysteresis.
        assert_eq!(t.severity(3, 3.31), 0);
        assert!(t.trip(0, 1000).is_none());
    }

    #[test]
    fn trip_plans() {
        let t = skin();
        assert_eq!(t.trip(0, 1000), Some((48000, None)));
        // Next level 49 C, back down below 48 - 2 C.
        assert_eq!(t.trip(1, 1000), Some((49000, Some(3000))));
        // Top level: the trip stays at 90 C and fires again below 88 C.
        assert_eq!(t.trip(6, 1000), Some((90000, Some(2000))));
    }

    #[test]
    fn no_thresholds_no_trip() {
        let t = Thresholds { hot: [NAN; LEVELS], cold: [NAN; LEVELS], positive: true, hysteresis: 5.0 };
        assert_eq!(t.severity(0, 200.0), 0);
        assert!(t.trip(0, 1000).is_none());
    }
}
