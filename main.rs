/*
 * Copyright (c) 2020, The Linux Foundation. All rights reserved.
 * Changes from Qualcomm Innovation Center are provided under the following license:
 * Copyright (c) 2023-2024 Qualcomm Innovation Center, Inc. All rights reserved.
 * SPDX-License-Identifier: BSD-3-Clause-Clear
 */

//! Thermal HAL for QTI targets: reports the configured thermal zones with their throttling
//! severity, and pushes severity and cooling device changes from the kernel thermal netlink.

// The test build has no caller for main() and the service type.
#![cfg_attr(test, allow(dead_code))]

mod config;
mod netlink;
mod nlmsg;
mod severity;
#[rustfmt::skip]
mod tables;
mod zones;

use android_hardware_thermal::aidl::android::hardware::thermal::{
    CoolingDevice::CoolingDevice,
    CoolingType::CoolingType,
    ICoolingDeviceChangedCallback::ICoolingDeviceChangedCallback,
    IThermal::{BnThermal, IThermal},
    IThermalChangedCallback::IThermalChangedCallback,
    Temperature::Temperature,
    TemperatureThreshold::TemperatureThreshold,
    TemperatureType::TemperatureType,
    ThrottlingSeverity::ThrottlingSeverity,
};
use binder::{BinderFeatures, ExceptionCode, Interface, Status, Strong};
use config::SensorConfig;
use log::{error, info, warn, LevelFilter};
use nlmsg::Event;
use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Mutex};
use zones::{Cdev, Sensor};

const LOG_TAG: &str = "thermal_hal";
const SOC_ID: &str = "/sys/devices/soc0/soc_id";
const HW_PLATFORM: &str = "/sys/devices/soc0/hw_platform";
/// The userspace trip point the HAL programs on each zone it watches.
const USER_TRIP: i32 = 1;

fn illegal_state(message: &str) -> Status {
    Status::new_exception_str(ExceptionCode::ILLEGAL_STATE, Some(message))
}

fn illegal_argument(message: &str) -> Status {
    Status::new_exception_str(ExceptionCode::ILLEGAL_ARGUMENT, Some(message))
}

fn soc_id() -> Option<i32> {
    (0..3).find_map(|_| fs::read_to_string(SOC_ID).ok()?.trim().parse().ok())
}

/// The QTI tables of this SoC, or none.
fn table_configs() -> Vec<SensorConfig> {
    let Some(soc) = soc_id() else {
        error!("soc ID fetch error");
        return vec![];
    };
    let hw_platform =
        fs::read_to_string(HW_PLATFORM).map(|s| s.trim().to_owned()).unwrap_or_default();
    let profile = if config::has_limit_profiles(soc) { zones::limit_profile() } else { 0 };
    let configs = config::for_soc(soc, &hw_platform, profile);
    if configs.is_empty() {
        error!("No config for soc ID: {soc}");
    }
    info!("soc {soc} ({hw_platform}), limits profile {profile}, {} sensor configs", configs.len());
    configs
}

/// See config.rs: the device config file, else the QTI tables, else the zones found in sysfs.
fn load_configs() -> Vec<SensorConfig> {
    let table = table_configs();
    for path in config::DEVICE_CONFIGS {
        let Ok(text) = fs::read_to_string(path) else { continue };
        match config::parse_device_config(&text) {
            Ok((inherit, sensors)) => {
                let mode = if inherit { "over the table" } else { "replacing the table" };
                info!("{path}: {} sensors, {mode}", sensors.len());
                return if inherit { config::merge(table, sensors) } else { sensors };
            }
            Err(e) => {
                error!("Ignoring {path}: {e}");
                break;
            }
        }
    }
    if !table.is_empty() {
        return table;
    }
    let found = config::discover(&zones::zone_types(), zones::critical_trip);
    warn!("No thermal config for this SoC, reporting {} zones found in sysfs", found.len());
    found
}

#[derive(Default)]
struct State {
    sensors: BTreeMap<i32, Sensor>,
    cdevs: BTreeMap<i32, Cdev>,
}

impl State {
    /// Reads `sensors` (all when `kind` is None); returns their temperatures and the ones whose
    /// severity changed. A zone that fails to read is skipped, not the whole list.
    fn read(&mut self, kind: Option<TemperatureType>) -> (Vec<Temperature>, Vec<Temperature>) {
        let mut temperatures = Vec::new();
        let mut changed = Vec::new();
        for sensor in self.sensors.values_mut() {
            if kind.is_some_and(|k| k != sensor.temperature.r#type) {
                continue;
            }
            if !sensor.read() {
                continue;
            }
            if sensor.update_severity() {
                changed.push(sensor.snapshot());
            }
            temperatures.push(sensor.snapshot());
        }
        (temperatures, changed)
    }

    fn has_sensor(&self, kind: TemperatureType) -> bool {
        self.sensors.values().any(|s| s.temperature.r#type == kind)
    }

    fn read_cdevs(&mut self, kind: Option<CoolingType>) -> Vec<CoolingDevice> {
        self.cdevs
            .values_mut()
            .filter(|c| kind.is_none_or(|k| k == c.device.r#type))
            .filter_map(|c| c.read().then(|| c.snapshot()))
            .collect()
    }
}

#[derive(Default)]
struct Callbacks {
    /// UNKNOWN receives every type.
    thermal: Vec<(Strong<dyn IThermalChangedCallback>, TemperatureType)>,
    cdev: Vec<(Strong<dyn ICoolingDeviceChangedCallback>, CoolingType)>,
}

struct Hal {
    configs: Vec<SensorConfig>,
    state: Mutex<State>,
    callbacks: Mutex<Callbacks>,
}

impl Hal {
    fn notify_throttling(&self, temperatures: &[Temperature]) {
        if temperatures.is_empty() {
            return;
        }
        let mut callbacks = self.callbacks.lock().unwrap();
        for t in temperatures {
            let severity = format!("{:?}", t.throttlingStatus);
            info!("sensor {} temperature {} severity {}", t.name, t.value, severity.trim_start_matches("r#"));
            callbacks.thermal.retain(|(callback, kind)| {
                if *kind != TemperatureType::UNKNOWN && *kind != t.r#type {
                    return true;
                }
                callback
                    .notifyThrottling(t)
                    .inspect_err(|e| error!("Throttling callback failed, removing it: {e}"))
                    .is_ok()
            });
        }
    }

    fn notify_cdev(&self, device: &CoolingDevice) {
        self.callbacks.lock().unwrap().cdev.retain(|(callback, kind)| {
            if *kind != device.r#type {
                return true;
            }
            callback
                .notifyCoolingDeviceChanged(device)
                .inspect_err(|e| error!("Cooling device callback failed, removing it: {e}"))
                .is_ok()
        });
    }

    fn handle(&self, event: Event) {
        let mut state = self.state.lock().unwrap();
        match event {
            // The trip event carries no reading; read the zone so the severity reflects it.
            Event::TripCrossed { zone, trip } if trip == USER_TRIP => {
                let Some(sensor) = state.sensors.get_mut(&zone) else { return };
                if sensor.read() && sensor.update_severity() {
                    let t = sensor.snapshot();
                    drop(state);
                    self.notify_throttling(&[t]);
                }
            }
            Event::TripCrossed { .. } => {}
            Event::Sample { zone, temperature } => {
                let Some(sensor) = state.sensors.get_mut(&zone) else { return };
                sensor.set_raw(temperature);
                if sensor.update_severity() {
                    let t = sensor.snapshot();
                    drop(state);
                    self.notify_throttling(&[t]);
                }
            }
            Event::ZoneCreated { zone, name } => {
                if state.sensors.contains_key(&zone) {
                    return;
                }
                let found = zones::discover(&self.configs, Some(&name));
                if !found.is_empty() {
                    info!("Monitoring new thermal zone {zone} ({name})");
                }
                state.sensors.extend(found);
            }
            Event::CdevAdded { cdev } => {
                // The modem cooling devices register ~20 s after boot, after the HAL started.
                if state.cdevs.contains_key(&cdev) {
                    return;
                }
                for (id, c) in zones::discover_cdevs() {
                    if let std::collections::btree_map::Entry::Vacant(e) = state.cdevs.entry(id) {
                        info!("New cooling device {id} ({})", c.device.name);
                        e.insert(c);
                    }
                }
            }
            Event::CdevState { cdev, state: value } => {
                let Some(c) = state.cdevs.get_mut(&cdev) else { return };
                if value < 0 {
                    return;
                }
                c.device.value = value;
                let device = c.snapshot();
                drop(state);
                self.notify_cdev(&device);
            }
        }
    }
}

struct Thermal(Arc<Hal>);

impl Interface for Thermal {}

#[allow(non_snake_case)]
impl IThermal for Thermal {
    fn getCoolingDevices(&self) -> binder::Result<Vec<CoolingDevice>> {
        let mut state = self.0.state.lock().unwrap();
        if state.cdevs.is_empty() {
            return Err(illegal_state("ThermalHAL given cdev not initialized."));
        }
        Ok(state.read_cdevs(None))
    }

    fn getCoolingDevicesWithType(&self, r#type: CoolingType) -> binder::Result<Vec<CoolingDevice>> {
        let mut state = self.0.state.lock().unwrap();
        if !state.cdevs.values().any(|c| c.device.r#type == r#type) {
            return Err(illegal_state("ThermalHAL given cdev type not initialized."));
        }
        Ok(state.read_cdevs(Some(r#type)))
    }

    fn getTemperatures(&self) -> binder::Result<Vec<Temperature>> {
        let mut state = self.0.state.lock().unwrap();
        if state.sensors.is_empty() {
            // As the QTI HAL: a placeholder reading where no zone is configured.
            return Ok(vec![Temperature {
                r#type: TemperatureType::SKIN,
                name: "test sensor".to_owned(),
                value: 30.0,
                throttlingStatus: ThrottlingSeverity::NONE,
            }]);
        }
        let (temperatures, changed) = state.read(None);
        drop(state);
        self.0.notify_throttling(&changed);
        Ok(temperatures)
    }

    fn getTemperaturesWithType(&self, r#type: TemperatureType) -> binder::Result<Vec<Temperature>> {
        let mut state = self.0.state.lock().unwrap();
        if !state.has_sensor(r#type) {
            return Err(illegal_state("ThermalHAL given sensor type Not initialized."));
        }
        let (temperatures, changed) = state.read(Some(r#type));
        drop(state);
        self.0.notify_throttling(&changed);
        Ok(temperatures)
    }

    fn getTemperatureThresholds(&self) -> binder::Result<Vec<TemperatureThreshold>> {
        let state = self.0.state.lock().unwrap();
        if state.sensors.is_empty() {
            return Err(illegal_state("ThermalHAL for sensor not initialized."));
        }
        Ok(state.sensors.values().map(Sensor::threshold).collect())
    }

    fn getTemperatureThresholdsWithType(
        &self,
        r#type: TemperatureType,
    ) -> binder::Result<Vec<TemperatureThreshold>> {
        let state = self.0.state.lock().unwrap();
        if !state.has_sensor(r#type) {
            return Err(illegal_state("ThermalHAL given sensor type not initialized."));
        }
        Ok(state
            .sensors
            .values()
            .filter(|s| s.temperature.r#type == r#type)
            .map(Sensor::threshold)
            .collect())
    }

    fn registerThermalChangedCallback(
        &self,
        callback: &Strong<dyn IThermalChangedCallback>,
    ) -> binder::Result<()> {
        self.registerThermalChangedCallbackWithType(callback, TemperatureType::UNKNOWN)
    }

    fn registerThermalChangedCallbackWithType(
        &self,
        callback: &Strong<dyn IThermalChangedCallback>,
        r#type: TemperatureType,
    ) -> binder::Result<()> {
        if r#type == TemperatureType::BCL_VOLTAGE || r#type == TemperatureType::BCL_CURRENT {
            return Err(illegal_argument("BCL current and voltage notification not supported"));
        }
        let mut callbacks = self.0.callbacks.lock().unwrap();
        if callbacks.thermal.iter().any(|(c, _)| c.as_binder() == callback.as_binder()) {
            return Err(illegal_argument("Callback already registered"));
        }
        callbacks.thermal.push((callback.clone(), r#type));
        Ok(())
    }

    fn unregisterThermalChangedCallback(
        &self,
        callback: &Strong<dyn IThermalChangedCallback>,
    ) -> binder::Result<()> {
        let mut callbacks = self.0.callbacks.lock().unwrap();
        let count = callbacks.thermal.len();
        callbacks.thermal.retain(|(c, _)| c.as_binder() != callback.as_binder());
        if callbacks.thermal.len() == count {
            return Err(illegal_argument("Callback wasn't registered"));
        }
        Ok(())
    }

    fn registerCoolingDeviceChangedCallbackWithType(
        &self,
        callback: &Strong<dyn ICoolingDeviceChangedCallback>,
        r#type: CoolingType,
    ) -> binder::Result<()> {
        let mut callbacks = self.0.callbacks.lock().unwrap();
        if callbacks.cdev.iter().any(|(c, _)| c.as_binder() == callback.as_binder()) {
            return Err(illegal_argument("Callback already registered"));
        }
        callbacks.cdev.push((callback.clone(), r#type));
        Ok(())
    }

    fn unregisterCoolingDeviceChangedCallback(
        &self,
        callback: &Strong<dyn ICoolingDeviceChangedCallback>,
    ) -> binder::Result<()> {
        let mut callbacks = self.0.callbacks.lock().unwrap();
        let count = callbacks.cdev.len();
        callbacks.cdev.retain(|(c, _)| c.as_binder() != callback.as_binder());
        if callbacks.cdev.len() == count {
            return Err(illegal_argument("CoolingDevice changed callback wasn't registered"));
        }
        Ok(())
    }
}

fn main() {
    logger::init(
        logger::Config::default().with_tag_on_device(LOG_TAG).with_max_level(LevelFilter::Info),
    );
    info!("Thermal HAL Service AIDL starting...");

    binder::ProcessState::set_thread_pool_max_thread_count(0);

    let configs = load_configs();
    // Everything is discovered before the listener starts, so no event sees a half built state.
    let state = State { sensors: zones::discover(&configs, None), cdevs: zones::discover_cdevs() };
    info!("Monitoring {} thermal zones", state.sensors.len());
    let hal = Arc::new(Hal { configs, state: Mutex::new(state), callbacks: Mutex::default() });

    let listener = hal.clone();
    netlink::start(move |event| listener.handle(event));

    let service = BnThermal::new_binder(Thermal(hal), BinderFeatures::default());
    let instance = format!("{}/default", <Thermal as IThermal>::get_descriptor());
    binder::add_service(&instance, service.as_binder()).expect("Failed to register service");
    info!("Thermal HAL Service AIDL started successfully.");

    binder::ProcessState::join_thread_pool();
}
