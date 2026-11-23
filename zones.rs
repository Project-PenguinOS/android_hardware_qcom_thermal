/*
 * Copyright (c) 2020, The Linux Foundation. All rights reserved.
 * Changes from Qualcomm Innovation Center are provided under the following license:
 * Copyright (c) 2023 Qualcomm Innovation Center, Inc. All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause-Clear
 */

//! Thermal zones and cooling devices in sysfs.

use crate::config::{self, SensorConfig};
use crate::severity::{Thresholds, LEVELS};
use android_hardware_thermal::aidl::android::hardware::thermal::{
    CoolingDevice::CoolingDevice, CoolingType::CoolingType, Temperature::Temperature,
    TemperatureThreshold::TemperatureThreshold, TemperatureType::TemperatureType,
    ThrottlingSeverity::ThrottlingSeverity,
};
use log::{error, info, warn};
use std::collections::BTreeMap;
use std::fs;

pub const SEVERITY_LEVELS: usize = LEVELS;
const THERMAL: &str = "/sys/class/thermal";
const READ_RETRIES: usize = 3;

const COOLING_DEVICES: &[(&str, CoolingType)] = &[
    ("thermal-cpufreq-0", CoolingType::CPU),
    ("thermal-cpufreq-1", CoolingType::CPU),
    ("thermal-cpufreq-2", CoolingType::CPU),
    ("thermal-cpufreq-3", CoolingType::CPU),
    ("thermal-cpufreq-4", CoolingType::CPU),
    ("thermal-cpufreq-5", CoolingType::CPU),
    ("thermal-cpufreq-6", CoolingType::CPU),
    ("thermal-cpufreq-7", CoolingType::CPU),
    ("cpufreq-cpu0", CoolingType::CPU),
    ("cpufreq-cpu1", CoolingType::CPU),
    ("cpufreq-cpu2", CoolingType::CPU),
    ("cpufreq-cpu3", CoolingType::CPU),
    ("cpufreq-cpu4", CoolingType::CPU),
    ("cpufreq-cpu5", CoolingType::CPU),
    ("cpufreq-cpu6", CoolingType::CPU),
    ("cpufreq-cpu7", CoolingType::CPU),
    ("thermal-cluster-4-7", CoolingType::CPU),
    ("thermal-cluster-3-7", CoolingType::CPU),
    ("cpu-isolate0", CoolingType::CPU),
    ("cpu-isolate1", CoolingType::CPU),
    ("cpu-isolate2", CoolingType::CPU),
    ("cpu-isolate3", CoolingType::CPU),
    ("cpu-isolate4", CoolingType::CPU),
    ("cpu-isolate5", CoolingType::CPU),
    ("cpu-isolate6", CoolingType::CPU),
    ("cpu-isolate7", CoolingType::CPU),
    ("thermal-pause-1", CoolingType::CPU),
    ("thermal-pause-2", CoolingType::CPU),
    ("thermal-pause-4", CoolingType::CPU),
    ("thermal-pause-8", CoolingType::CPU),
    ("thermal-pause-10", CoolingType::CPU),
    ("thermal-pause-20", CoolingType::CPU),
    ("thermal-pause-40", CoolingType::CPU),
    ("thermal-pause-80", CoolingType::CPU),
    ("thermal-devfreq-0", CoolingType::GPU),
    ("devfreq-3d00000.qcom,kgsl-3d0", CoolingType::GPU),
    ("gpu", CoolingType::GPU),
    ("modem_tj", CoolingType::MODEM),
    ("modem_lte_dsc", CoolingType::MODEM),
    ("modem_nr_dsc", CoolingType::MODEM),
    ("modem_nr_scg_dsc", CoolingType::MODEM),
    ("cdsp", CoolingType::NPU),
    ("cdsp_hw", CoolingType::NPU),
    ("battery", CoolingType::BATTERY),
    ("fan-max31760", CoolingType::FAN),
    ("display-fps", CoolingType::DISPLAY),
];

fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

/// Reads an integer node, retrying a garbled read like the QTI HAL did.
fn read_int(path: &str) -> Option<i64> {
    (0..READ_RETRIES).find_map(|_| read_trimmed(path)?.parse().ok())
}

fn write_int(path: &str, value: i32) {
    if let Err(e) = fs::write(path, value.to_string()) {
        error!("Cannot write {value} to {path}: {e}");
    }
}

/// `prefix` entries of /sys/class/thermal by number, with the contents of their type node.
fn entries(prefix: &str) -> Vec<(i32, String)> {
    let mut entries: Vec<(i32, String)> = fs::read_dir(THERMAL)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let id = name.strip_prefix(prefix)?.parse().ok()?;
            Some((id, read_trimmed(&format!("{THERMAL}/{name}/type"))?))
        })
        .collect();
    entries.sort();
    entries
}

/// The zone whose type is `name`; without one, the lowest numbered zone whose type starts with
/// it, which the QTI HAL accepted too.
fn match_zone(zones: &[(i32, String)], name: &str) -> Option<i32> {
    if let Some(&(id, _)) = zones.iter().find(|(_, kind)| kind == name) {
        return Some(id);
    }
    let &(id, ref kind) = zones.iter().find(|(_, kind)| kind.starts_with(name))?;
    warn!("No thermal zone named {name}, using {kind} (zone {id})");
    Some(id)
}

pub fn find_zone(name: &str) -> Option<i32> {
    match_zone(&entries("thermal_zone"), name)
}

/// The types of all thermal zones, in zone order.
pub fn zone_types() -> Vec<String> {
    entries("thermal_zone").into_iter().map(|(_, kind)| kind).collect()
}

/// The lowest critical trip of the zone of type `name`, in the zone's unit.
pub fn critical_trip(name: &str) -> Option<i32> {
    let tz = find_zone(name)?;
    let path = format!("{THERMAL}/thermal_zone{tz}");
    (0..=config::LIMIT_PROFILE_TRIP_MAX)
        .map_while(|trip| Some((trip, read_trimmed(&format!("{path}/trip_point_{trip}_type"))?)))
        .filter(|(_, kind)| kind == "critical")
        .filter_map(|(trip, _)| read_int(&format!("{path}/trip_point_{trip}_temp")))
        .filter_map(|temp| i32::try_from(temp).ok())
        .min()
}

/// Thermal limits profile 1 has a 125 C hot trip on the AOSS zone.
pub fn limit_profile() -> i32 {
    let Some(tz) = find_zone(config::LIMIT_PROFILE_ZONE) else {
        error!("No thermal zone {}", config::LIMIT_PROFILE_ZONE);
        return 0;
    };
    for trip in 1..=config::LIMIT_PROFILE_TRIP_MAX {
        let path = format!("{THERMAL}/thermal_zone{tz}");
        let Some(kind) = read_trimmed(&format!("{path}/trip_point_{trip}_type")) else { break };
        if kind.starts_with(config::LIMIT_PROFILE_TRIP_TYPE) {
            let temp = read_int(&format!("{path}/trip_point_{trip}_temp"));
            return i32::from(temp == Some(i64::from(config::LIMIT_PROFILE_1_SHUTDOWN)));
        }
    }
    0
}

pub struct Sensor {
    pub zone: i32,
    /// The thermal zone type in sysfs.
    pub zone_name: String,
    /// Zone units per HAL unit (millidegrees per degree, except BCL percentages).
    scale: i32,
    no_trip: bool,
    thresholds: Thresholds,
    pub temperature: Temperature,
    read_failed: bool,
}

impl Sensor {
    fn new(config: &SensorConfig, index: usize, zone: i32) -> Sensor {
        let scale = if config.kind == TemperatureType::BCL_PERCENTAGE { 1 } else { 1000 };
        let hysteresis = match config.kind {
            TemperatureType::SKIN => 2.0,
            // Volts and amps: the 5 units used for temperatures would never clear.
            TemperatureType::BCL_VOLTAGE => 0.1,
            TemperatureType::BCL_CURRENT => 0.5,
            _ => 5.0,
        };
        let mut hot = [f32::NAN; LEVELS];
        let mut cold = [f32::NAN; LEVELS];
        for (level, &value) in config.thresholds.iter().enumerate().filter(|(_, &v)| v != 0) {
            let value = value as f32 / scale as f32;
            if config.positive {
                hot[level] = value;
            } else {
                cold[level] = value;
            }
        }
        let name = if config.kind == TemperatureType::CPU {
            format!("CPU{index}")
        } else {
            config.label.to_owned()
        };
        Sensor {
            zone,
            zone_name: config.zones[index].to_owned(),
            scale,
            no_trip: config.no_trip,
            thresholds: Thresholds { hot, cold, positive: config.positive, hysteresis },
            temperature: Temperature {
                r#type: config.kind,
                name,
                value: f32::NAN,
                throttlingStatus: ThrottlingSeverity::NONE,
            },
            read_failed: false,
        }
    }

    /// A copy of the last reading; the AIDL parcelable is not Clone.
    pub fn snapshot(&self) -> Temperature {
        let t = &self.temperature;
        Temperature {
            r#type: t.r#type,
            name: t.name.clone(),
            value: t.value,
            throttlingStatus: t.throttlingStatus,
        }
    }

    pub fn threshold(&self) -> TemperatureThreshold {
        TemperatureThreshold {
            r#type: self.temperature.r#type,
            name: self.temperature.name.clone(),
            hotThrottlingThresholds: self.thresholds.hot.to_vec(),
            coldThrottlingThresholds: self.thresholds.cold.to_vec(),
        }
    }

    /// Reads the zone; logs only the first of consecutive failures.
    pub fn read(&mut self) -> bool {
        let raw = read_int(&format!("{THERMAL}/thermal_zone{}/temp", self.zone));
        if raw.is_none() && !self.read_failed {
            warn!("Cannot read thermal zone {} ({})", self.zone, self.zone_name);
        }
        self.read_failed = raw.is_none();
        raw.inspect(|&raw| self.set_raw(raw)).is_some()
    }

    pub fn set_raw(&mut self, raw: i64) {
        self.temperature.value = raw as f32 / self.scale as f32;
    }

    /// Updates the severity from the last reading. Returns true when it changed, after moving
    /// the userspace trip point to the next change.
    pub fn update_severity(&mut self) -> bool {
        let current = self.temperature.throttlingStatus.0 as usize;
        let severity = self.thresholds.severity(current, self.temperature.value);
        if severity == current {
            return false;
        }
        self.temperature.throttlingStatus = ThrottlingSeverity(severity as i32);
        if !self.no_trip {
            self.program_trip();
        }
        true
    }

    fn program_trip(&self) {
        let current = self.temperature.throttlingStatus.0 as usize;
        let Some((trip, hysteresis)) = self.thresholds.trip(current, self.scale) else { return };
        let path = format!("{THERMAL}/thermal_zone{}", self.zone);
        write_int(&format!("{path}/trip_point_1_temp"), trip);
        if let Some(hysteresis) = hysteresis {
            write_int(&format!("{path}/trip_point_1_hyst"), hysteresis);
        }
    }

    /// The first reading after discovery; programs the first trip point.
    fn start(&mut self) {
        self.read();
        let severity = self.thresholds.severity(0, self.temperature.value);
        self.temperature.throttlingStatus = ThrottlingSeverity(severity as i32);
        if !self.no_trip {
            self.program_trip();
        }
    }
}

/// The sensors of `configs` found in sysfs, by thermal zone. With `only`, just that zone type.
pub fn discover(configs: &[SensorConfig], only: Option<&str>) -> BTreeMap<i32, Sensor> {
    let zones = entries("thermal_zone");
    let mut sensors = BTreeMap::new();
    for config in configs {
        for (index, name) in config.zones.iter().enumerate() {
            if only.is_some_and(|only| only != *name) {
                continue;
            }
            let Some(zone) = match_zone(&zones, name) else {
                warn!("No thermal zone for sensor {name}");
                continue;
            };
            let mut sensor = Sensor::new(config, index, zone);
            sensor.start();
            sensors.insert(zone, sensor);
        }
    }
    sensors
}

pub struct Cdev {
    pub id: i32,
    pub device: CoolingDevice,
}

impl Cdev {
    /// A copy of the last state; the AIDL parcelable is not Clone.
    pub fn snapshot(&self) -> CoolingDevice {
        let d = &self.device;
        CoolingDevice {
            r#type: d.r#type,
            name: d.name.clone(),
            value: d.value,
            powerLimitMw: d.powerLimitMw,
            powerMw: d.powerMw,
            timeWindowMs: d.timeWindowMs,
        }
    }

    pub fn read(&mut self) -> bool {
        match read_int(&format!("{THERMAL}/cooling_device{}/cur_state", self.id)) {
            Some(state) => {
                self.device.value = state;
                true
            }
            None => false,
        }
    }
}

pub fn discover_cdevs() -> BTreeMap<i32, Cdev> {
    let mut cdevs = BTreeMap::new();
    for (id, kind) in entries("cooling_device") {
        let Some(&(name, r#type)) = COOLING_DEVICES.iter().find(|(n, _)| *n == kind) else {
            continue;
        };
        let mut cdev = Cdev {
            id,
            device: CoolingDevice { r#type, name: name.to_owned(), ..Default::default() },
        };
        cdev.read();
        cdevs.insert(id, cdev);
    }
    info!("{} cooling devices", cdevs.len());
    cdevs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_zone_name_wins_over_prefix() {
        let zones = vec![(3, "cpu-0-0-0-x".to_owned()), (7, "cpu-0-0-0".to_owned())];
        assert_eq!(match_zone(&zones, "cpu-0-0-0"), Some(7));
        assert_eq!(match_zone(&zones[..1], "cpu-0-0-0"), Some(3));
        assert_eq!(match_zone(&zones, "gpuss-0"), None);
    }
}
