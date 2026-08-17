//! Wi-Fi observability via the native WLAN API (`wlanapi.dll`).
//!
//! `wifi_scan` is gated by `[hardware] wifi_scan_enabled` because a BSS scan
//! can be seen as a probe of nearby networks. When disabled the scan reports
//! explicitly unavailable, never an empty result. Connected-adapter status is
//! always read.

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::NetworkManagement::WiFi::*;

use crate::errors::{ErrorKind, WinkitError};
use crate::log_warn;
use crate::models::{
    SensorAvailability, UnavailableReading, WifiAdapterStatus, WifiNetwork, WifiScan,
};
use crate::platform::windows::hardware::HardwareOptions;

const WLAN_CLIENT_VERSION_V1: u32 = 1;
const MAX_SCAN_RESULTS: usize = 64;

fn guid_to_string(g: &windows_sys::core::GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7]
    )
}

fn ssid_string(ssid: &DOT11_SSID) -> Option<String> {
    let len = ssid.uSSIDLength as usize;
    if len == 0 {
        return None;
    }
    let bytes = &ssid.ucSSID[..len.min(32)];
    String::from_utf8_lossy(bytes).into_owned().into()
}

fn mac_string(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Rate in kbps -> Mbps.
fn rate_mbps(rate_kbps: u32) -> f64 {
    rate_kbps as f64 / 1000.0
}

fn channel_from_freq(freq_mhz: u64) -> Option<u32> {
    if (2412..=2484).contains(&freq_mhz) {
        return Some(((freq_mhz - 2412) / 5 + 1) as u32);
    }
    if (5180..=5825).contains(&freq_mhz) {
        return Some(((freq_mhz - 5180) / 5 + 36) as u32);
    }
    if (5955..=7115).contains(&freq_mhz) {
        return Some(((freq_mhz - 5955) / 5 + 1) as u32);
    }
    None
}

fn band_from_freq(freq_mhz: u64) -> Option<String> {
    if (2412..=2484).contains(&freq_mhz) {
        Some("2.4ghz".into())
    } else if (4900..=5900).contains(&freq_mhz) {
        Some("5ghz".into())
    } else if (5925..=7125).contains(&freq_mhz) {
        Some("6ghz".into())
    } else {
        None
    }
}

fn auth_string(auth: i32) -> String {
    match auth {
        1 => "open".into(),
        2 => "shared_key".into(),
        3 => "wpa".into(),
        4 => "wpa_psk".into(),
        5 => "wpa_none".into(),
        6 => "wpa2".into(),
        7 => "wpa2_psk".into(),
        8 => "wpa3".into(),
        9 => "wpa3_sae".into(),
        10 => "owe".into(),
        11 => "wpa3_enterprise".into(),
        _ => format!("algorithm_{auth}"),
    }
}

fn cipher_string(cipher: i32) -> String {
    match cipher {
        0 => "none".into(),
        1 => "wep40".into(),
        2 => "tkip".into(),
        4 => "ccmp".into(),
        5 => "wep104".into(),
        6 => "bip".into(),
        8 => "gcmp".into(),
        9 => "gcmp256".into(),
        10 => "ccmp256".into(),
        11 => "bip_gmac128".into(),
        12 => "bip_gmac256".into(),
        13 => "bip_cmac256".into(),
        257 => "wep".into(),
        _ => format!("cipher_{cipher}"),
    }
}

struct WlanSession {
    handle: HANDLE,
}

impl WlanSession {
    fn open() -> Result<Self, WinkitError> {
        unsafe {
            let mut negotiated = 0u32;
            let mut handle: HANDLE = std::ptr::null_mut();
            let code = WlanOpenHandle(
                WLAN_CLIENT_VERSION_V1,
                std::ptr::null(),
                &mut negotiated,
                &mut handle,
            );
            if code != 0 || handle.is_null() {
                return Err(WinkitError::new(
                    ErrorKind::WindowsApiError,
                    format!("WlanOpenHandle failed with error {code}"),
                ));
            }
            Ok(Self { handle })
        }
    }
}

impl Drop for WlanSession {
    fn drop(&mut self) {
        unsafe { WlanCloseHandle(self.handle, std::ptr::null()) };
    }
}

fn interface_list(session: &WlanSession) -> Result<Vec<WLAN_INTERFACE_INFO>, WinkitError> {
    unsafe {
        let mut raw: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        let code = WlanEnumInterfaces(session.handle, std::ptr::null(), &mut raw);
        if code != 0 || raw.is_null() {
            return Err(WinkitError::new(
                ErrorKind::WindowsApiError,
                format!("WlanEnumInterfaces failed with error {code}"),
            ));
        }
        let count = (*raw).dwNumberOfItems as usize;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            out.push(*(*raw).InterfaceInfo.as_ptr().add(i));
        }
        WlanFreeMemory(raw as *const _);
        Ok(out)
    }
}

/// Connection attributes for one adapter (SSID, signal, link rates).
fn connection_attributes(
    session: &WlanSession,
    guid: &windows_sys::core::GUID,
) -> Option<WLAN_CONNECTION_ATTRIBUTES> {
    unsafe {
        let mut size = 0u32;
        let mut data: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut value_type = wlan_opcode_value_type_query_only;
        let code = WlanQueryInterface(
            session.handle,
            guid,
            wlan_intf_opcode_current_connection,
            std::ptr::null(),
            &mut size,
            &mut data,
            &mut value_type,
        );
        if code != 0 || data.is_null() {
            return None;
        }
        let attrs = std::ptr::read(data as *const WLAN_CONNECTION_ATTRIBUTES);
        WlanFreeMemory(data);
        Some(attrs)
    }
}

fn adapter_status(session: &WlanSession, info: &WLAN_INTERFACE_INFO) -> WifiAdapterStatus {
    let mut status = WifiAdapterStatus {
        adapter_id: guid_to_string(&info.InterfaceGuid),
        description: wide_string(&info.strInterfaceDescription),
        state: if info.isState == wlan_interface_state_connected {
            "connected"
        } else if info.isState == wlan_interface_state_disconnected {
            "disconnected"
        } else {
            "not_ready"
        }
        .into(),
        is_up: info.isState == wlan_interface_state_connected,
        ..Default::default()
    };

    if status.state == "connected" {
        if let Some(attrs) = connection_attributes(session, &info.InterfaceGuid) {
            let assoc = &attrs.wlanAssociationAttributes;
            status.ssid = ssid_string(&assoc.dot11Ssid);
            let signal = assoc.wlanSignalQuality.min(100);
            status.signal_percent = Some(signal as u8);
            status.link_speed_mbps = Some(rate_mbps(assoc.ulRxRate.max(assoc.ulTxRate)));
            status.authentication =
                Some(auth_string(attrs.wlanSecurityAttributes.dot11AuthAlgorithm));
            status.cipher = Some(cipher_string(
                attrs.wlanSecurityAttributes.dot11CipherAlgorithm,
            ));
            // The association carries the connected AP's BSSID; enrich the
            // status with the matching BSS entry (RSSI, channel, band) when
            // the scan is readable. Missing data stays `None` — never inferred.
            let connected_bssid = &assoc.dot11Bssid;
            if let Ok(entries) = bss_scan(session, &info.InterfaceGuid) {
                if let Some(entry) = entries.iter().find(|e| &e.dot11Bssid == connected_bssid) {
                    let freq_mhz = entry.ulChCenterFrequency as u64 / 1_000;
                    status.rssi_dbm = Some(entry.lRssi);
                    status.channel = channel_from_freq(freq_mhz);
                    status.frequency_mhz = (freq_mhz > 0).then_some(freq_mhz);
                    status.band = band_from_freq(freq_mhz);
                }
            }
        }
    }
    status
}

fn wide_string(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    crate::utils::wide_to_string(&wide[..end])
}

/// Connected Wi-Fi adapter statuses (never gated).
pub fn wifi_statuses() -> Vec<WifiAdapterStatus> {
    match WlanSession::open() {
        Ok(session) => match interface_list(&session) {
            Ok(ifaces) => ifaces
                .iter()
                .filter(|i| i.isState != wlan_interface_state_not_ready)
                .map(|i| adapter_status(&session, i))
                .collect(),
            Err(e) => {
                log_warn!("wifi status unavailable: {}", e.message);
                Vec::new()
            }
        },
        Err(e) => {
            log_warn!("wifi status unavailable: {}", e.message);
            Vec::new()
        }
    }
}

/// Scan nearby BSS entries for one adapter; empty when the scan is refused.
fn bss_scan(
    session: &WlanSession,
    guid: &windows_sys::core::GUID,
) -> Result<Vec<WLAN_BSS_ENTRY>, WinkitError> {
    unsafe {
        let mut raw: *mut WLAN_BSS_LIST = std::ptr::null_mut();
        let code = WlanGetNetworkBssList(
            session.handle,
            guid,
            std::ptr::null(),
            dot11_BSS_type_any,
            false as _,
            std::ptr::null(),
            &mut raw,
        );
        if code != 0 || raw.is_null() {
            return Err(WinkitError::new(
                ErrorKind::WindowsApiError,
                format!("WlanGetNetworkBssList failed with error {code}"),
            ));
        }
        let count = (*raw).dwNumberOfItems as usize;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            out.push(*(*raw).wlanBssEntries.as_ptr().add(i));
        }
        WlanFreeMemory(raw as *const _);
        Ok(out)
    }
}

fn network_from_bss(entry: &WLAN_BSS_ENTRY) -> WifiNetwork {
    let freq_mhz = entry.ulChCenterFrequency as u64 / 1_000;
    WifiNetwork {
        ssid: ssid_string(&entry.dot11Ssid),
        bssid: Some(mac_string(&entry.dot11Bssid)),
        signal_percent: Some(entry.uLinkQuality.min(100) as u8),
        rssi_dbm: Some(entry.lRssi),
        channel: channel_from_freq(freq_mhz),
        frequency_mhz: (freq_mhz > 0).then_some(freq_mhz),
        band: band_from_freq(freq_mhz),
        security: None,
        link_quality: Some(entry.uLinkQuality.min(100) as u8),
    }
}

/// `wifi_scan`: nearby networks. Gated by `wifi_scan_enabled`.
pub fn wifi_scan(opts: &HardwareOptions) -> Result<WifiScan, WinkitError> {
    let mut unavailable = Vec::new();
    if !opts.wifi_scan_enabled {
        unavailable.push(UnavailableReading::new(
            "wifi_scan",
            "nearby_networks",
            SensorAvailability::Unavailable,
            "Wi-Fi scanning is disabled in configuration ([hardware] wifi_scan_enabled = false); \
             enable it to scan for nearby networks",
        ));
        return Ok(WifiScan {
            status: "unavailable".into(),
            timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
            adapter_id: None,
            networks: Vec::new(),
            truncated: false,
            unavailable,
        });
    }

    let session = match WlanSession::open() {
        Ok(s) => s,
        Err(e) => {
            unavailable.push(UnavailableReading::new(
                "wifi_scan",
                "nearby_networks",
                SensorAvailability::Unavailable,
                format!("Wireless AutoConfig service unreachable: {}", e.message),
            ));
            return Ok(WifiScan {
                status: "unavailable".into(),
                timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
                adapter_id: None,
                networks: Vec::new(),
                truncated: false,
                unavailable,
            });
        }
    };

    let ifaces = match interface_list(&session) {
        Ok(list) => list,
        Err(e) => {
            unavailable.push(UnavailableReading::new(
                "wifi_scan",
                "nearby_networks",
                SensorAvailability::Unavailable,
                e.message.clone(),
            ));
            return Ok(WifiScan {
                status: "unavailable".into(),
                timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
                adapter_id: None,
                networks: Vec::new(),
                truncated: false,
                unavailable,
            });
        }
    };

    // Scan the first adapter that reports connected or ready.
    let mut scanned: Option<windows_sys::core::GUID> = None;
    let mut networks = Vec::new();
    for iface in ifaces.iter() {
        if iface.isState == wlan_interface_state_not_ready {
            continue;
        }
        if scanned.is_some() {
            break;
        }
        scanned = Some(iface.InterfaceGuid);
        match bss_scan(&session, &iface.InterfaceGuid) {
            Ok(entries) => {
                for e in entries.iter() {
                    networks.push(network_from_bss(e));
                }
            }
            Err(e) => {
                unavailable.push(UnavailableReading::new(
                    guid_to_string(&iface.InterfaceGuid),
                    "nearby_networks",
                    SensorAvailability::Unavailable,
                    e.message.clone(),
                ));
            }
        }
    }

    // Deterministic order: strongest signal first, then SSID, then BSSID.
    networks.sort_by(|a, b| {
        b.signal_percent
            .cmp(&a.signal_percent)
            .then_with(|| a.ssid.cmp(&b.ssid))
            .then_with(|| a.bssid.cmp(&b.bssid))
    });
    let truncated = networks.len() > MAX_SCAN_RESULTS;
    networks.truncate(MAX_SCAN_RESULTS);

    Ok(WifiScan {
        status: if networks.is_empty() && !unavailable.is_empty() {
            "limited".into()
        } else if networks.is_empty() {
            "unavailable".into()
        } else {
            "ok".into()
        },
        timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
        adapter_id: scanned.map(|g| guid_to_string(&g)),
        networks,
        truncated,
        unavailable,
    })
}

/// Wi-Fi adapters for `network_snapshot`.
pub fn wifi_adapter_statuses() -> Vec<WifiAdapterStatus> {
    wifi_statuses()
}
