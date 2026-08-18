//! Power state and battery health.
//!
//! Source of truth: `GetSystemPowerStatus` for live status; `Win32_Battery`
//! (WMI) for design/full-charge capacities and health.

use crate::errors::WinkitError;
use crate::models::{
    BatteryHealth, BatteryStatus, PowerStateInfo, PowerStatus, SensorAvailability,
    UnavailableReading,
};
use crate::platform::windows::wmi::{WmiObject, WmiSession, WmiValue};

const UNKNOWN_FLAG: u8 = 255;

/// Raw `GetSystemPowerStatus` projection.
struct RawPowerStatus {
    ac_online: Option<bool>,
    battery_percent: Option<u8>,
    charging: bool,
    remaining_seconds: Option<u64>,
}

fn system_power_status() -> Option<RawPowerStatus> {
    unsafe {
        let mut raw: windows_sys::Win32::System::Power::SYSTEM_POWER_STATUS = std::mem::zeroed();
        if windows_sys::Win32::System::Power::GetSystemPowerStatus(&mut raw) == 0 {
            return None;
        }
        Some(RawPowerStatus {
            ac_online: match raw.ACLineStatus {
                0 => Some(false),
                1 => Some(true),
                _ => None,
            },
            battery_percent: if raw.BatteryLifePercent == UNKNOWN_FLAG {
                None
            } else {
                Some(raw.BatteryLifePercent)
            },
            charging: (raw.BatteryFlag & 8) != 0,
            remaining_seconds: if raw.BatteryLifeTime == u32::MAX {
                None
            } else {
                Some(raw.BatteryLifeTime as u64)
            },
        })
    }
}

fn battery_state_label(
    present: bool,
    percent: Option<u8>,
    charging: bool,
    ac_online: Option<bool>,
) -> Option<String> {
    if !present {
        return None;
    }
    match percent {
        Some(p) if p <= 10 => return Some("critical".into()),
        Some(p) if p <= 25 => return Some("low".into()),
        _ => {}
    }
    if charging {
        return Some("charging".into());
    }
    // Plugged in but not charging: full charge or paused charge. Only call a
    // plugged-in battery "full" when it is at 99%+; otherwise it is
    // "not_charging" (never "discharging" while on AC).
    if ac_online == Some(true) {
        return match percent {
            Some(p) if p >= 99 => Some("full".into()),
            _ => Some("not_charging".into()),
        };
    }
    Some("discharging".into())
}

fn query_batteries() -> Result<Vec<WmiObject>, WinkitError> {
    let session = WmiSession::connect("root\\cimv2")?;
    session.query(
        "SELECT DesignCapacity, FullChargeCapacity, EstimatedChargeRemaining, Status \
         FROM Win32_Battery",
    )
}

/// Battery health from design vs full-charge capacity (both in mWh).
fn battery_health(design: Option<f64>, full: Option<f64>) -> Option<BatteryHealth> {
    let (d, f) = (design?, full?);
    if d <= 0.0 {
        return None;
    }
    let pct = (f / d * 100.0).clamp(0.0, 100.0);
    Some(BatteryHealth {
        designed_capacity_mwh: Some(d as u64),
        full_charge_capacity_mwh: Some(f as u64),
        current_charge_mwh: None,
        cycle_count: None,
        health_percent: Some(pct),
        temperature_c: None,
        availability: SensorAvailability::Available,
        reason: None,
    })
}

/// Read a single unsigned property from a `root\WMI` battery class.
///
/// These classes (unlike `Win32_Battery`) expose the design/full charge
/// capacity and cycle count without elevation. Returns `None` when the
/// property is missing or the query fails (e.g. no battery driver support).
fn query_root_wmi_battery(class: &str, property: &str) -> Option<u32> {
    let session = WmiSession::connect("root\\WMI").ok()?;
    let wql = format!("SELECT {property} FROM {class}");
    let objs = session.query(&wql).ok()?;
    objs.first().and_then(|o| o.get_u32(property))
}

/// Battery health from the `root\WMI` battery classes, which report design
/// and full-charge capacity without elevation where the ACPI battery driver
/// is present.
fn battery_health_root_wmi() -> (Option<BatteryHealth>, Vec<UnavailableReading>) {
    let mut unavailable = Vec::new();
    let full = query_root_wmi_battery("BatteryFullChargedCapacity", "FullChargedCapacity");
    let design = query_root_wmi_battery("BatteryStaticData", "DesignedCapacity");
    let cycle = query_root_wmi_battery("BatteryCycleCount", "CycleCount");
    let remaining = query_root_wmi_battery("BatteryStatus", "RemainingCapacity");

    let full = match full {
        Some(v) if v > 0 => v as u64,
        _ => {
            unavailable.push(UnavailableReading::new(
                "battery",
                "health",
                SensorAvailability::Unavailable,
                "root\\WMI BatteryFullChargedCapacity reported no usable capacity",
            ));
            return (None, unavailable);
        }
    };
    let design = design.filter(|&v| v > 0).map(|v| v as u64);
    let health_percent = design.map(|d| (full as f64 / d as f64 * 100.0).clamp(0.0, 100.0));
    let health = BatteryHealth {
        designed_capacity_mwh: design,
        full_charge_capacity_mwh: Some(full),
        current_charge_mwh: remaining.filter(|&v| v > 0).map(|v| v as u64),
        cycle_count: cycle.filter(|&v| v > 0),
        health_percent,
        temperature_c: None,
        availability: SensorAvailability::Available,
        reason: health_percent
            .is_none()
            .then(|| "full-charge capacity reported, but design capacity is not readable without elevation".to_string()),
    };
    (Some(health), unavailable)
}

/// Compact power picture used by `hardware_snapshot`.
pub fn power_state() -> (PowerStateInfo, Vec<UnavailableReading>) {
    let mut unavailable = Vec::new();
    let raw = system_power_status();
    let mut info = PowerStateInfo {
        power_source: "unknown".into(),
        ac_online: raw.as_ref().and_then(|r| r.ac_online),
        battery_present: false,
        battery_percent: raw.as_ref().and_then(|r| r.battery_percent),
        battery_state: None,
        charging: raw.as_ref().map(|r| r.charging),
        estimated_time_remaining_seconds: raw.as_ref().and_then(|r| r.remaining_seconds),
    };
    if raw.is_none() {
        unavailable.push(UnavailableReading::new(
            "system_power_status",
            "ac_line_battery",
            SensorAvailability::Unavailable,
            "GetSystemPowerStatus failed",
        ));
    }

    match query_batteries() {
        Ok(batteries) => {
            if let Some(b) = batteries.first() {
                info.battery_present = true;
                let remaining = b
                    .get("EstimatedChargeRemaining")
                    .and_then(WmiValue::as_u16)
                    .filter(|&v| v <= 100)
                    .map(|v| v as u8);
                if remaining.is_some() {
                    info.battery_percent = remaining;
                }
                let charging = b
                    .get_string("Status")
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case("charging"));
                if charging {
                    info.charging = Some(true);
                }
                info.battery_state = battery_state_label(
                    true,
                    info.battery_percent,
                    info.charging.unwrap_or(false),
                    info.ac_online,
                );
            } else {
                info.battery_present = false;
                info.battery_state = None;
            }
        }
        Err(e) => {
            unavailable.push(UnavailableReading::new(
                "battery",
                "health",
                SensorAvailability::Unavailable,
                format!("Win32_Battery query failed: {}", e.message),
            ));
        }
    }
    if info.battery_present {
        info.power_source = match info.ac_online {
            Some(true) => "ac".into(),
            Some(false) => "battery".into(),
            None => "unknown".into(),
        };
    } else {
        info.power_source = "ac".into();
    }
    (info, unavailable)
}

fn read_battery_health() -> (Option<BatteryHealth>, Vec<UnavailableReading>) {
    let mut unavailable = Vec::new();
    match query_batteries() {
        Ok(batteries) => {
            if let Some(b) = batteries.first() {
                let design = b.get("DesignCapacity").and_then(WmiValue::as_f64);
                let full = b.get("FullChargeCapacity").and_then(WmiValue::as_f64);
                if let Some(health) = battery_health(design, full) {
                    let remaining = b
                        .get("EstimatedChargeRemaining")
                        .and_then(WmiValue::as_u16)
                        .filter(|&v| v <= 100)
                        .map(|v| v as u64);
                    let design_mwh = design.map(|d| d as u64);
                    let full_mwh = full.map(|f| f as u64);
                    let mut health = health;
                    health.designed_capacity_mwh = design_mwh;
                    health.full_charge_capacity_mwh = full_mwh;
                    health.current_charge_mwh = remaining
                        .zip(full_mwh)
                        .map(|(r, f)| (r as f64 / 100.0 * f as f64) as u64);
                    return (Some(health), unavailable);
                }
                // Win32_Battery capacity fields are empty on some hosts; the
                // root\WMI classes report them without elevation.
                let (health, mut wmi_unavailable) = battery_health_root_wmi();
                unavailable.append(&mut wmi_unavailable);
                return (health, unavailable);
            } else {
                unavailable.push(UnavailableReading::new(
                    "battery",
                    "health",
                    SensorAvailability::NotPresent,
                    "no Win32_Battery instance; this machine has no battery",
                ));
            }
        }
        Err(e) => {
            unavailable.push(UnavailableReading::new(
                "battery",
                "health",
                SensorAvailability::Unavailable,
                format!("Win32_Battery query failed: {}", e.message),
            ));
            let (health, mut wmi_unavailable) = battery_health_root_wmi();
            unavailable.append(&mut wmi_unavailable);
            return (health, unavailable);
        }
    }
    (None, unavailable)
}

/// Detailed battery report (tool `battery_status`).
pub fn battery_status() -> Result<BatteryStatus, WinkitError> {
    let raw = system_power_status();
    let present_raw = raw.is_some();
    let mut unavailable = Vec::new();
    if raw.is_none() {
        unavailable.push(UnavailableReading::new(
            "system_power_status",
            "ac_line_battery",
            SensorAvailability::Unavailable,
            "GetSystemPowerStatus failed",
        ));
    }

    let (health, health_unavailable) = read_battery_health();
    let battery_present = health_unavailable
        .iter()
        .any(|u| u.availability == SensorAvailability::NotPresent);
    unavailable.extend(health_unavailable);
    let present = if battery_present {
        false
    } else {
        present_raw || health.is_some()
    };
    let percent = raw.as_ref().and_then(|r| r.battery_percent);
    let charging = raw.as_ref().map(|r| r.charging);
    let ac_online = raw.as_ref().and_then(|r| r.ac_online);

    Ok(BatteryStatus {
        status: if battery_present && !present {
            "not_present".into()
        } else if !unavailable.is_empty() && health.is_none() {
            "limited".into()
        } else {
            "ok".into()
        },
        timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
        present,
        percent,
        ac_online,
        charging,
        battery_state: battery_state_label(present, percent, charging.unwrap_or(false), ac_online),
        estimated_time_remaining_seconds: raw.as_ref().and_then(|r| r.remaining_seconds),
        health,
        unavailable,
    })
}

/// Power source report (tool `power_status`).
pub fn power_status() -> Result<PowerStatus, WinkitError> {
    let raw = system_power_status();
    let mut unavailable = Vec::new();
    if raw.is_none() {
        unavailable.push(UnavailableReading::new(
            "system_power_status",
            "ac_line_battery",
            SensorAvailability::Unavailable,
            "GetSystemPowerStatus failed",
        ));
    }
    // Determine battery presence the same way `power_state` does.
    let (info, _) = power_state();
    let present = info.battery_present;
    let percent = raw.as_ref().and_then(|r| r.battery_percent);
    let charging = raw.as_ref().map(|r| r.charging);
    let ac_online = raw.as_ref().and_then(|r| r.ac_online);
    let power_source = info.power_source;

    Ok(PowerStatus {
        status: if raw.is_some() { "ok" } else { "limited" }.into(),
        timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
        power_source,
        ac_online,
        battery_present: present,
        battery_percent: percent,
        battery_state: battery_state_label(present, percent, charging.unwrap_or(false), ac_online),
        charging,
        estimated_time_remaining_seconds: raw.as_ref().and_then(|r| r.remaining_seconds),
        unavailable,
    })
}

/// Compact battery summary for `hardware_snapshot`:
/// `(present, percent, ac_online, charging, remaining_seconds)`.
pub type BatterySummary = (bool, Option<u8>, Option<bool>, bool, Option<u64>);

pub fn battery_present_summary() -> Option<BatterySummary> {
    let (info, _) = power_state();
    Some((
        info.battery_present,
        info.battery_percent,
        info.ac_online,
        info.charging.unwrap_or(false),
        info.estimated_time_remaining_seconds,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_not_present_is_none() {
        assert_eq!(
            battery_state_label(false, Some(80), false, Some(true)),
            None
        );
        assert_eq!(battery_state_label(false, None, false, None), None);
    }

    #[test]
    fn label_critical_and_low_before_anything_else() {
        assert_eq!(
            battery_state_label(true, Some(8), false, None),
            Some("critical".into())
        );
        assert_eq!(
            battery_state_label(true, Some(10), false, Some(true)),
            Some("critical".into())
        );
        assert_eq!(
            battery_state_label(true, Some(25), true, Some(true)),
            Some("low".into())
        );
    }

    #[test]
    fn label_charging() {
        assert_eq!(
            battery_state_label(true, Some(60), true, Some(true)),
            Some("charging".into())
        );
        assert_eq!(
            battery_state_label(true, Some(77), true, Some(false)),
            Some("charging".into())
        );
    }

    #[test]
    fn label_plugged_full_battery() {
        assert_eq!(
            battery_state_label(true, Some(99), false, Some(true)),
            Some("full".into())
        );
        assert_eq!(
            battery_state_label(true, Some(100), false, Some(true)),
            Some("full".into())
        );
    }

    #[test]
    fn label_plugged_not_charging() {
        assert_eq!(
            battery_state_label(true, Some(77), false, Some(true)),
            Some("not_charging".into())
        );
        assert_eq!(
            battery_state_label(true, None, false, Some(true)),
            Some("not_charging".into())
        );
    }

    #[test]
    fn label_discharging_on_battery() {
        assert_eq!(
            battery_state_label(true, Some(77), false, Some(false)),
            Some("discharging".into())
        );
        assert_eq!(
            battery_state_label(true, Some(77), false, None),
            Some("discharging".into())
        );
    }
}
