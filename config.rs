/*
 * Copyright (c) 2020, The Linux Foundation. All rights reserved.
 * Changes from Qualcomm Innovation Center are provided under the following license:
 * Copyright (c) 2023 Qualcomm Innovation Center, Inc. All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause-Clear
 */

//! Which thermal zones the HAL reports, and their throttling thresholds, per SoC. Only the sun
//! family is listed: no other SoC is built from this tree.

use crate::zones::SEVERITY_LEVELS;
use android_hardware_thermal::aidl::android::hardware::thermal::TemperatureType::TemperatureType;

const LIGHT: usize = 1;
const MODERATE: usize = 2;
const SEVERE: usize = 3;
const CRITICAL: usize = 4;
const EMERGENCY: usize = 5;
const SHUTDOWN: usize = 6;

#[derive(Clone, Debug)]
pub struct SensorConfig {
    pub kind: TemperatureType,
    /// Thermal zone types; CPU configs list one zone per CPU.
    pub zones: Vec<&'static str>,
    pub label: &'static str,
    /// Per severity, in the zone's unit; 0 means no threshold at that level.
    pub thresholds: [i32; SEVERITY_LEVELS],
    /// Throttling as the value rises (temperatures) rather than falls (battery voltage).
    pub positive: bool,
    /// The HAL polls these and does not program a userspace trip point.
    pub no_trip: bool,
}

fn sensor(
    kind: TemperatureType,
    zones: &[&'static str],
    label: &'static str,
    levels: &[(usize, i32)],
    positive: bool,
    no_trip: bool,
) -> SensorConfig {
    let mut thresholds = [0; SEVERITY_LEVELS];
    for &(level, value) in levels {
        thresholds[level] = value;
    }
    SensorConfig { kind, zones: zones.to_vec(), label, thresholds, positive, no_trip }
}

const SUN_SOC_IDS: [i32; 4] = [618, 639, 705, 706];

const SUN_CPU_ZONES: [&str; 8] = [
    "cpu-0-0-0", "cpu-0-1-0", "cpu-0-2-0", "cpu-0-3-0", "cpu-0-4-0", "cpu-0-5-0", "cpu-1-0-0",
    "cpu-1-1-0",
];

/// Thermal zone and trip of the limits profile: a 125 C shutdown trip means profile 1.
pub const LIMIT_PROFILE_ZONE: &str = "aoss-0";
pub const LIMIT_PROFILE_TRIP_TYPE: &str = "hot";
pub const LIMIT_PROFILE_1_SHUTDOWN: i32 = 125000;
pub const LIMIT_PROFILE_TRIP_MAX: u32 = 11;

/// CPUs, GPUs and NSPs, whose thresholds depend on the limits profile.
fn sun_profile(severe: i32, shutdown: i32) -> Vec<SensorConfig> {
    let levels = [(SEVERE, severe), (SHUTDOWN, shutdown)];
    let gpus = [
        ("gpuss-0", "GPU0"),
        ("gpuss-1", "GPU1"),
        ("gpuss-2", "GPU2"),
        ("gpuss-3", "GPU3"),
        ("gpuss-4", "GPU4"),
        ("gpuss-5", "GPU5"),
        ("gpuss-6", "GPU6"),
        ("gpuss-7", "GPU7"),
    ];
    let nsps = [
        ("nsphvx-0", "nsp0"),
        ("nsphvx-1", "nsp1"),
        ("nsphvx-2", "nsp2"),
        ("nsphmx-0", "nsp3"),
        ("nsphmx-1", "nsp4"),
        ("nsphmx-2", "nsp5"),
        ("nsphmx-3", "nsp6"),
    ];
    let mut config = vec![sensor(TemperatureType::CPU, &SUN_CPU_ZONES, "", &levels, true, true)];
    config.extend(gpus.iter().map(|&(zone, label)| {
        sensor(TemperatureType::GPU, &[zone], label, &levels, true, true)
    }));
    config.extend(nsps.iter().map(|&(zone, label)| {
        sensor(TemperatureType::NPU, &[zone], label, &levels, true, true)
    }));
    config
}

fn sun_specific() -> Vec<SensorConfig> {
    vec![
        sensor(
            TemperatureType::BCL_CURRENT,
            &["pmih010x-ibat-lvl0"],
            "ibat",
            &[(SEVERE, 13500), (SHUTDOWN, 15000)],
            true,
            true,
        ),
        sensor(
            TemperatureType::SKIN,
            &["sys-therm-2"],
            "skin",
            &[
                (LIGHT, 48000),
                (MODERATE, 49000),
                (SEVERE, 50000),
                (CRITICAL, 60000),
                (EMERGENCY, 61000),
                (SHUTDOWN, 90000),
            ],
            true,
            false,
        ),
    ]
}

/// Battery temperature and battery current limiting, on every SoC listed here.
fn battery() -> Vec<SensorConfig> {
    vec![
        sensor(
            TemperatureType::BATTERY,
            &["battery"],
            "battery",
            &[(SEVERE, 80000), (SHUTDOWN, 90000)],
            true,
            true,
        ),
        sensor(
            TemperatureType::BCL_VOLTAGE,
            &["vbat"],
            "vbat",
            &[(SEVERE, 3200), (SHUTDOWN, 3000)],
            false,
            true,
        ),
        sensor(
            TemperatureType::BCL_PERCENTAGE,
            &["socd"],
            "socd",
            &[(SEVERE, 90), (SHUTDOWN, 99)],
            true,
            false,
        ),
    ]
}

/// The sensors of `soc_id` under `limit_profile`, or none for an SoC this HAL does not know.
pub fn for_soc(soc_id: i32, limit_profile: i32) -> Vec<SensorConfig> {
    if !SUN_SOC_IDS.contains(&soc_id) {
        return vec![];
    }
    let mut config = sun_specific();
    config.extend(if limit_profile == 1 { sun_profile(105000, 125000) } else { sun_profile(95000, 115000) });
    config.extend(battery());
    config
}

/// Whether `soc_id` has a limits profile to look up.
pub fn has_limit_profiles(soc_id: i32) -> bool {
    SUN_SOC_IDS.contains(&soc_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sun_matches_the_qti_tables() {
        let config = for_soc(618, 0);
        // ibat, skin, CPU, 8 GPU, 7 NSP, battery, vbat, socd
        assert_eq!(config.len(), 2 + 1 + 8 + 7 + 3);
        let cpu = config.iter().find(|c| c.kind == TemperatureType::CPU).unwrap();
        assert_eq!(cpu.zones.len(), 8);
        assert_eq!((cpu.thresholds[SEVERE], cpu.thresholds[SHUTDOWN]), (95000, 115000));
        let skin = config.iter().find(|c| c.label == "skin").unwrap();
        assert!(!skin.no_trip);
        assert_eq!(skin.thresholds, [0, 48000, 49000, 50000, 60000, 61000, 90000]);
        let vbat = config.iter().find(|c| c.label == "vbat").unwrap();
        assert!(!vbat.positive);

        let cpu1 = for_soc(705, 1).into_iter().find(|c| c.kind == TemperatureType::CPU).unwrap();
        assert_eq!((cpu1.thresholds[SEVERE], cpu1.thresholds[SHUTDOWN]), (105000, 125000));
        assert!(for_soc(519, 0).is_empty());
    }
}
