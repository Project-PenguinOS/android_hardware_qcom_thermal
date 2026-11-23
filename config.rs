/*
 * Copyright (c) 2020, The Linux Foundation. All rights reserved.
 * Changes from Qualcomm Innovation Center are provided under the following license:
 * Copyright (c) 2023 Qualcomm Innovation Center, Inc. All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause-Clear
 */

//! Which thermal zones the HAL reports, and their throttling thresholds. In order:
//!
//! 1. A device config file (`/odm/etc/thermal-hal.json` or `/vendor/etc/thermal-hal.json`),
//!    for boards whose zones differ from the QTI reference designs.
//! 2. The QTI per-SoC tables (tables.rs, generated from the C++ HAL), looked up like the C++
//!    HAL did.
//! 3. For an SoC in neither, zones found by name in sysfs with the thresholds every QTI table
//!    shares, so the device still reports temperatures and throttles.
//!
//! # Device config file
//!
//! ```json
//! {
//!   "inherit": true,
//!   "sensors": [
//!     { "type": "SKIN", "zones": ["sys-therm-2"], "label": "skin",
//!       "thresholds": { "LIGHT": 48000, "SEVERE": 50000, "SHUTDOWN": 90000 } }
//!   ]
//! }
//! ```
//!
//! Without `inherit` (the default) the file lists every sensor. With it, the file's sensors are
//! added to the SoC's table and replace the table entries with the same type and label.
//! Thresholds are in the zone's unit (millidegrees, millivolts, milliamps or percent); `positive`
//! (default true) is false for values that throttle as they fall, and `no_trip` (default false)
//! makes the HAL poll the zone instead of programming its trip point 1.

use crate::tables;
use crate::zones::SEVERITY_LEVELS;
use android_hardware_thermal::aidl::android::hardware::thermal::TemperatureType::TemperatureType;
use serde_json::Value;

#[cfg(test)]
const LIGHT: usize = 1;
const SEVERE: usize = 3;
const SHUTDOWN: usize = 6;

const SEVERITY_NAMES: [&str; SEVERITY_LEVELS] =
    ["NONE", "LIGHT", "MODERATE", "SEVERE", "CRITICAL", "EMERGENCY", "SHUTDOWN"];

/// Device config files, the first one present wins.
pub const DEVICE_CONFIGS: [&str; 2] = ["/odm/etc/thermal-hal.json", "/vendor/etc/thermal-hal.json"];

/// Thermal zone and trip of the limits profile: a 125 C shutdown trip means profile 1.
pub const LIMIT_PROFILE_ZONE: &str = "aoss-0";
pub const LIMIT_PROFILE_TRIP_TYPE: &str = "hot";
pub const LIMIT_PROFILE_1_SHUTDOWN: i32 = 125000;
pub const LIMIT_PROFILE_TRIP_MAX: u32 = 11;

/// A sensor of the generated tables.
pub struct StaticSensor {
    pub kind: TemperatureType,
    pub zones: &'static [&'static str],
    pub label: &'static str,
    pub thresholds: [i32; SEVERITY_LEVELS],
    pub positive: bool,
    pub no_trip: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SensorConfig {
    pub kind: TemperatureType,
    /// Thermal zone types; CPU configs list one zone per CPU.
    pub zones: Vec<String>,
    pub label: String,
    /// Per severity, in the zone's unit; 0 means no threshold at that level.
    pub thresholds: [i32; SEVERITY_LEVELS],
    /// Throttling as the value rises (temperatures) rather than falls (battery voltage).
    pub positive: bool,
    /// The HAL polls these and does not program a userspace trip point.
    pub no_trip: bool,
}

impl From<&StaticSensor> for SensorConfig {
    fn from(s: &StaticSensor) -> SensorConfig {
        SensorConfig {
            kind: s.kind,
            zones: s.zones.iter().map(|z| z.to_string()).collect(),
            label: s.label.to_owned(),
            thresholds: s.thresholds,
            positive: s.positive,
            no_trip: s.no_trip,
        }
    }
}

fn extend(config: &mut Vec<SensorConfig>, table: &[StaticSensor]) {
    config.extend(table.iter().map(SensorConfig::from));
}

// Layer 2: tables.

/// Whether `soc_id` has a limits profile to look up.
pub fn has_limit_profiles(soc_id: i32) -> bool {
    tables::LIMIT_PROFILE_SPECIFIC.iter().any(|&(soc, _, _)| soc == soc_id)
}

/// The QTI tables of `soc_id`, as the C++ HAL assembled them, or none for an SoC they do not
/// list. `hw_platform` is /sys/devices/soc0/hw_platform.
pub fn for_soc(soc_id: i32, hw_platform: &str, limit_profile: i32) -> Vec<SensorConfig> {
    let Some(&(_, base)) = tables::SOC_MAP.iter().find(|&&(soc, _)| soc == soc_id) else {
        return vec![];
    };
    let mut config = Vec::new();
    extend(&mut config, base);
    if let Some(&(_, specific)) = tables::SOC_SPECIFIC.iter().find(|&&(soc, _)| soc == soc_id) {
        extend(&mut config, specific);
    }
    if let Some(&(_, _, profile)) = tables::LIMIT_PROFILE_SPECIFIC
        .iter()
        .find(|&&(soc, lp, _)| soc == soc_id && lp == limit_profile)
    {
        extend(&mut config, profile);
    }
    if let Some(&(_, _, platform)) = tables::PLATFORM_SPECIFIC
        .iter()
        .find(|&&(soc, hw, _)| soc == soc_id && hw == hw_platform)
    {
        extend(&mut config, platform);
    }
    if !tables::BATT_BCL_NOT_SUPPORTED.contains(&soc_id) {
        let bcl_defined = config.iter().any(|c| c.kind == TemperatureType::BCL_PERCENTAGE);
        extend(&mut config, tables::BAT_CONF);
        if !bcl_defined {
            extend(&mut config, tables::BCL_CONF);
        }
    }
    config
}

// Layer 1: device config file.

fn temperature_type(name: &str) -> Option<TemperatureType> {
    Some(match name {
        "CPU" => TemperatureType::CPU,
        "GPU" => TemperatureType::GPU,
        "BATTERY" => TemperatureType::BATTERY,
        "SKIN" => TemperatureType::SKIN,
        "USB_PORT" => TemperatureType::USB_PORT,
        "POWER_AMPLIFIER" => TemperatureType::POWER_AMPLIFIER,
        "BCL_VOLTAGE" => TemperatureType::BCL_VOLTAGE,
        "BCL_CURRENT" => TemperatureType::BCL_CURRENT,
        "BCL_PERCENTAGE" => TemperatureType::BCL_PERCENTAGE,
        "NPU" => TemperatureType::NPU,
        "TPU" => TemperatureType::TPU,
        "DISPLAY" => TemperatureType::DISPLAY,
        "MODEM" => TemperatureType::MODEM,
        "SOC" => TemperatureType::SOC,
        _ => return None,
    })
}

fn parse_sensor(value: &Value) -> Result<SensorConfig, String> {
    let kind_name = value["type"].as_str().ok_or("sensor without a type")?;
    let kind = temperature_type(kind_name).ok_or_else(|| format!("unknown type {kind_name}"))?;
    let zones: Vec<String> = value["zones"]
        .as_array()
        .ok_or_else(|| format!("{kind_name} sensor without zones"))?
        .iter()
        .map(|z| z.as_str().map(str::to_owned).ok_or("zone names are strings"))
        .collect::<Result<_, _>>()?;
    if zones.is_empty() {
        return Err(format!("{kind_name} sensor without zones"));
    }
    let mut thresholds = [0; SEVERITY_LEVELS];
    let levels = value["thresholds"]
        .as_object()
        .ok_or_else(|| format!("{kind_name} sensor without thresholds"))?;
    for (level, v) in levels {
        let index = SEVERITY_NAMES[1..]
            .iter()
            .position(|n| n == level)
            .ok_or_else(|| format!("unknown severity {level}"))?
            + 1;
        thresholds[index] = v
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| format!("threshold {level} is not an integer"))?;
    }
    let flag = |key: &str, default: bool| match &value[key] {
        Value::Null => Ok(default),
        v => v.as_bool().ok_or(format!("{key} is not a boolean")),
    };
    Ok(SensorConfig {
        kind,
        zones,
        label: value["label"].as_str().unwrap_or_default().to_owned(),
        thresholds,
        positive: flag("positive", true)?,
        no_trip: flag("no_trip", false)?,
    })
}

/// A device config file: (inherit, sensors).
pub fn parse_device_config(text: &str) -> Result<(bool, Vec<SensorConfig>), String> {
    let root: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let inherit = match &root["inherit"] {
        Value::Null => false,
        v => v.as_bool().ok_or("inherit is not a boolean")?,
    };
    let sensors = root["sensors"]
        .as_array()
        .ok_or("no sensors list")?
        .iter()
        .map(parse_sensor)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((inherit, sensors))
}

/// The table entries `device` sensors replace (same type and label) dropped, then `device`.
pub fn merge(mut table: Vec<SensorConfig>, device: Vec<SensorConfig>) -> Vec<SensorConfig> {
    table.retain(|t| !device.iter().any(|d| d.kind == t.kind && d.label == t.label));
    table.extend(device);
    table
}

// Layer 3: discovery.

/// Thresholds every QTI table uses for these types; skin limits are board specific, so a
/// discovered skin zone only gets its kernel critical trip as the shutdown level.
fn default_thresholds(kind: TemperatureType) -> [i32; SEVERITY_LEVELS] {
    let mut t = [0; SEVERITY_LEVELS];
    let (severe, shutdown) = match kind {
        TemperatureType::CPU | TemperatureType::GPU | TemperatureType::NPU => (95000, 115000),
        TemperatureType::BATTERY => (80000, 90000),
        TemperatureType::BCL_VOLTAGE => (3200, 3000),
        TemperatureType::BCL_PERCENTAGE => (90, 99),
        _ => (0, 0),
    };
    t[SEVERE] = severe;
    t[SHUTDOWN] = shutdown;
    t
}

fn is_index(s: &str) -> bool {
    !s.is_empty() && s.split('-').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The type a thermal zone name stands for on QTI kernels, if it is one the HAL reports.
fn classify(zone: &str) -> Option<TemperatureType> {
    if zone.strip_prefix("cpu-").is_some_and(is_index) {
        return Some(TemperatureType::CPU);
    }
    if zone.strip_prefix("gpuss-").or(zone.strip_prefix("gpu-")).is_some_and(is_index) {
        return Some(TemperatureType::GPU);
    }
    if let Some(rest) = zone.strip_prefix("nsp") {
        let index = rest.trim_start_matches(|c: char| c.is_ascii_alphabetic());
        if index.strip_prefix('-').is_some_and(is_index) {
            return Some(TemperatureType::NPU);
        }
    }
    match zone {
        "battery" => Some(TemperatureType::BATTERY),
        "vbat" => Some(TemperatureType::BCL_VOLTAGE),
        "socd" => Some(TemperatureType::BCL_PERCENTAGE),
        _ if zone.ends_with("-ibat-lvl0") => Some(TemperatureType::BCL_CURRENT),
        _ if zone.contains("skin") => Some(TemperatureType::SKIN),
        _ => None,
    }
}

/// Configs for the zones of an unknown SoC: `zones` are the thermal zone types in zone order,
/// `critical` the critical trip of a zone in its unit, if it has one. All of them are polled:
/// trip point 1 of an unknown kernel may be one of its own trips.
pub fn discover(zones: &[String], critical: impl Fn(&str) -> Option<i32>) -> Vec<SensorConfig> {
    let mut config: Vec<SensorConfig> = Vec::new();
    let mut cpus = Vec::new();
    for zone in zones {
        let Some(kind) = classify(zone) else { continue };
        if kind == TemperatureType::CPU {
            cpus.push(zone.clone());
            continue;
        }
        if config.iter().any(|c| c.zones[0] == *zone) {
            continue;
        }
        let count = config.iter().filter(|c| c.kind == kind).count();
        let label = match kind {
            TemperatureType::GPU => format!("GPU{count}"),
            TemperatureType::NPU => format!("nsp{count}"),
            TemperatureType::BCL_CURRENT => "ibat".to_owned(),
            _ => zone.clone(),
        };
        let positive = kind != TemperatureType::BCL_VOLTAGE;
        let mut thresholds = default_thresholds(kind);
        if let Some(trip) = critical(zone).filter(|&t| positive && t > 0) {
            // The kernel acts there anyway: report it as SHUTDOWN, never later than the tables.
            if thresholds[SHUTDOWN] == 0 || trip < thresholds[SHUTDOWN] {
                thresholds[SHUTDOWN] = trip;
            }
        }
        // A skin zone without limits still reports its temperature; anything else needs one.
        if kind != TemperatureType::SKIN && thresholds.iter().all(|&t| t == 0) {
            continue;
        }
        config.push(SensorConfig {
            kind,
            zones: vec![zone.clone()],
            label,
            thresholds,
            positive,
            no_trip: true,
        });
    }
    if !cpus.is_empty() {
        cpus.dedup();
        config.insert(
            0,
            SensorConfig {
                kind: TemperatureType::CPU,
                zones: cpus,
                label: String::new(),
                thresholds: default_thresholds(TemperatureType::CPU),
                positive: true,
                no_trip: true,
            },
        );
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(config: &'a [SensorConfig], label: &str) -> &'a SensorConfig {
        config.iter().find(|c| c.label == label).unwrap()
    }

    #[test]
    fn sun_matches_the_qti_tables() {
        let config = for_soc(618, "MTP", 0);
        // ibat, skin, CPU, 8 GPU, 7 NSP, battery, vbat, socd
        assert_eq!(config.len(), 2 + 1 + 8 + 7 + 3);
        let cpu = config.iter().find(|c| c.kind == TemperatureType::CPU).unwrap();
        assert_eq!(cpu.zones.len(), 8);
        assert_eq!((cpu.thresholds[SEVERE], cpu.thresholds[SHUTDOWN]), (95000, 115000));
        let skin = find(&config, "skin");
        assert!(!skin.no_trip);
        assert_eq!(skin.thresholds, [0, 48000, 49000, 50000, 60000, 61000, 90000]);
        assert!(!find(&config, "vbat").positive);

        assert!(has_limit_profiles(705));
        let cpu1 = for_soc(705, "", 1).into_iter().find(|c| c.kind == TemperatureType::CPU);
        let cpu1 = cpu1.unwrap();
        assert_eq!((cpu1.thresholds[SEVERE], cpu1.thresholds[SHUTDOWN]), (105000, 125000));
    }

    #[test]
    fn pineapple_matches_the_qti_tables() {
        assert!(!has_limit_profiles(557));
        let config = for_soc(557, "MTP", 0);
        // CPU, 8 GPU, 6 NSP, ibat, skin, battery, vbat, socd
        assert_eq!(config.len(), 1 + 8 + 6 + 2 + 3);
        let cpu = config.iter().find(|c| c.kind == TemperatureType::CPU).unwrap();
        assert_eq!(cpu.zones.len(), 8);
        assert_eq!(cpu.zones[2], "cpu-2-0-0");
        assert!(!cpu.no_trip);
        let nsp = find(&config, "nsp5");
        assert_eq!(nsp.zones, ["nsphmx-3"]);
        assert_eq!((nsp.thresholds[SEVERE], nsp.thresholds[SHUTDOWN]), (100000, 115000));
        assert!(find(&config, "ibat").no_trip);
        let skin = find(&config, "skin");
        assert_eq!(skin.zones, ["skin-msm-therm"]);
        assert_eq!(skin.thresholds, [0, 36500, 40000, 46500, 50000, 55000, 95000]);
        assert_eq!(for_soc(702, "", 1).len(), config.len());
    }

    #[test]
    fn older_socs_come_back() {
        // kalama (8 Gen 2) and kona (865) had no config in the first Rust port.
        assert!(!for_soc(519, "MTP", 0).is_empty());
        assert!(!for_soc(356, "MTP", 0).is_empty());
        assert!(for_soc(1, "MTP", 0).is_empty());
    }

    #[test]
    fn platform_and_bcl_rules() {
        // ravelin: the hw_platform picks the skin table.
        let qrd = for_soc(568, "QRD", 0);
        let idp = for_soc(568, "IDP", 0);
        assert_ne!(qrd, idp);
        // Automotive SoCs have neither battery nor BCL configs.
        let talos_au = for_soc(384, "", 0);
        assert!(!talos_au.is_empty());
        assert!(!talos_au.iter().any(|c| c.kind == TemperatureType::BATTERY));
        // A table that defines its own socd does not get a second one.
        for (soc, _) in tables::SOC_MAP {
            let config = for_soc(*soc, "", 0);
            let socd = config.iter().filter(|c| c.kind == TemperatureType::BCL_PERCENTAGE);
            assert!(socd.count() <= 1, "soc {soc}");
        }
    }

    #[test]
    fn every_table_entry_is_sane() {
        for (soc, _) in tables::SOC_MAP {
            for c in for_soc(*soc, "", 0) {
                assert!(!c.zones.is_empty() && c.zones.iter().all(|z| !z.is_empty()), "soc {soc}");
                assert!(c.thresholds.iter().any(|&t| t != 0), "soc {soc} {}", c.label);
            }
        }
    }

    #[test]
    fn parses_a_device_config() {
        let text = r#"{
            "inherit": true,
            "sensors": [
                { "type": "SKIN", "zones": ["sys-therm-2"], "label": "skin",
                  "thresholds": { "LIGHT": 41000, "SEVERE": 45000, "SHUTDOWN": 90000 } },
                { "type": "BCL_VOLTAGE", "zones": ["vbat"], "label": "vbat", "positive": false,
                  "no_trip": true, "thresholds": { "SEVERE": 3300 } }
            ]
        }"#;
        let (inherit, sensors) = parse_device_config(text).unwrap();
        assert!(inherit);
        assert_eq!(sensors.len(), 2);
        assert_eq!(sensors[0].thresholds, [0, 41000, 0, 45000, 0, 0, 90000]);
        assert!(sensors[0].positive && !sensors[0].no_trip);
        assert!(!sensors[1].positive && sensors[1].no_trip);

        let merged = merge(for_soc(618, "", 0), sensors);
        assert_eq!(merged.len(), 2 + 1 + 8 + 7 + 3);
        assert_eq!(find(&merged, "skin").thresholds[LIGHT], 41000);
        assert_eq!(find(&merged, "vbat").thresholds[SEVERE], 3300);
    }

    #[test]
    fn rejects_bad_device_configs() {
        assert!(parse_device_config("{").is_err());
        assert!(parse_device_config(r#"{"sensors": [{"type": "FOO", "zones": ["a"],
            "thresholds": {}}]}"#)
        .is_err());
        assert!(parse_device_config(r#"{"sensors": [{"type": "SKIN", "zones": [],
            "thresholds": {}}]}"#)
        .is_err());
        assert!(parse_device_config(r#"{"sensors": [{"type": "SKIN", "zones": ["a"],
            "thresholds": {"HOT": 1}}]}"#)
        .is_err());
        assert!(parse_device_config(r#"{"sensors": [{"type": "SKIN", "zones": ["a"],
            "thresholds": {"NONE": 1}}]}"#)
        .is_err());
        assert!(parse_device_config(r#"{"inherit": 1, "sensors": []}"#).is_err());
    }

    #[test]
    fn classifies_qti_zone_names() {
        for (zone, kind) in [
            ("cpu-0-0-0", Some(TemperatureType::CPU)),
            ("cpu-1-3", Some(TemperatureType::CPU)),
            ("cpu-0-0-usr", None),
            ("cpuss-0", None),
            ("gpuss-3", Some(TemperatureType::GPU)),
            ("gpuss-max", None),
            ("nsphvx-1", Some(TemperatureType::NPU)),
            ("nsp-0-0-0", Some(TemperatureType::NPU)),
            ("nspss-2", Some(TemperatureType::NPU)),
            ("nsp", None),
            ("pm8550b-ibat-lvl0", Some(TemperatureType::BCL_CURRENT)),
            ("skin-msm-therm", Some(TemperatureType::SKIN)),
            ("battery", Some(TemperatureType::BATTERY)),
            ("aoss-0", None),
        ] {
            assert_eq!(classify(zone), kind, "{zone}");
        }
    }

    #[test]
    fn discovers_an_unknown_soc() {
        let zones: Vec<String> = [
            "aoss-0", "cpu-0-0-0", "cpu-0-1-0", "gpuss-0", "gpuss-1", "nsphvx-0", "skin-msm-therm",
            "quiet-therm", "battery", "vbat", "socd", "pm8550b-ibat-lvl0", "cpu-1-0-0",
        ]
        .iter()
        .map(|z| z.to_string())
        .collect();
        let critical = |zone: &str| match zone {
            "skin-msm-therm" => Some(70000),
            "gpuss-0" => Some(125000),
            "pm8550b-ibat-lvl0" => Some(10000),
            _ => None,
        };
        let config = discover(&zones, critical);
        let labels: Vec<&str> = config.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            ["", "GPU0", "GPU1", "nsp0", "skin-msm-therm", "battery", "vbat", "socd", "ibat"]
        );
        assert_eq!(config[0].zones, ["cpu-0-0-0", "cpu-0-1-0", "cpu-1-0-0"]);
        assert!(config.iter().all(|c| c.no_trip));
        // A later kernel trip does not delay the table shutdown level.
        assert_eq!(find(&config, "GPU0").thresholds[SHUTDOWN], 115000);
        assert_eq!(find(&config, "skin-msm-therm").thresholds, [0, 0, 0, 0, 0, 0, 70000]);
        assert_eq!(find(&config, "ibat").thresholds[SHUTDOWN], 10000);
        assert!(!find(&config, "vbat").positive);
        assert!(discover(&[], |_| None).is_empty());
    }
}
