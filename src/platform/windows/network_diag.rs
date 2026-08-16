//! Bounded network diagnosis: per-interface gateway ICMP probing plus
//! Wi-Fi signal/link evidence, producing structured findings.
//!
//! Probing uses `icmp.dll` (`IcmpSendEcho`), which does not require
//! elevation for ICMPv4. Every probe is bounded (4 pings, 1 s timeout
//! each). The module never claims "the Internet is broken"; it reports only
//! what the evidence supports.

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho, ICMP_ECHO_REPLY, IP_OPTION_INFORMATION,
    IP_SUCCESS,
};

use crate::errors::WinkitError;
use crate::models::{
    EvidencePoint, NetworkDiagnosis, NetworkDiagnosticInterface, NetworkFinding,
    SensorAvailability, UnavailableReading, WifiAdapterStatus,
};

const PINGS: u32 = 4;
const PING_TIMEOUT_MS: u32 = 1_000;
const WEAK_SIGNAL_PERCENT: u8 = 40;
const LOW_LINK_SPEED_MBPS: f64 = 20.0;
const LOSS_WARNING_PERCENT: f64 = 5.0;
const LATENCY_WARNING_MS: f64 = 100.0;

/// Parse `a.b.c.d` into the network-byte-order `u32` `IcmpSendEcho` wants.
fn ipv4_to_u32(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut out = 0u32;
    for p in parts {
        let octet: u32 = p.trim().parse().ok()?;
        if octet > 255 {
            return None;
        }
        out = (out << 8) | octet;
    }
    Some(out)
}

/// One round of ICMP echo probes to `gateway`. Returns `(loss_percent,
/// avg_rtt_ms)`; both are `None` when the probe could not be attempted.
fn probe_gateway(gateway: &str) -> Option<(f64, f64)> {
    let dest = ipv4_to_u32(gateway)?;
    unsafe {
        let handle: HANDLE = IcmpCreateFile();
        if handle.is_null() {
            return None;
        }
        let data = b"winkit-probe";
        let reply_size = std::mem::size_of::<ICMP_ECHO_REPLY>() + data.len() + 16;
        let mut reply = vec![0u8; reply_size];
        let options = IP_OPTION_INFORMATION {
            Ttl: 64,
            Tos: 0,
            Flags: 0,
            OptionsSize: 0,
            OptionsData: std::ptr::null_mut(),
        };
        let mut received = 0u32;
        let mut total_rtt = 0u64;
        let mut lost = 0u32;
        for _ in 0..PINGS {
            let count = IcmpSendEcho(
                handle,
                dest,
                data.as_ptr() as *const _,
                data.len() as u16,
                &options,
                reply.as_mut_ptr() as *mut _,
                reply.len() as u32,
                PING_TIMEOUT_MS,
            );
            if count == 0 {
                lost += 1;
                continue;
            }
            let first = std::ptr::read(reply.as_ptr() as *const ICMP_ECHO_REPLY);
            if first.Status == IP_SUCCESS {
                received += 1;
                total_rtt += first.RoundTripTime as u64;
            } else {
                lost += 1;
            }
        }
        IcmpCloseHandle(handle);
        let loss = lost as f64 / PINGS as f64 * 100.0;
        let avg = if received > 0 {
            total_rtt as f64 / received as f64
        } else {
            f64::NAN
        };
        Some((loss, avg))
    }
}

/// Map Wi-Fi adapter statuses onto interfaces by description.
fn attach_wifi(interfaces: &mut [NetworkDiagnosticInterface], wifi: &[WifiAdapterStatus]) {
    for i in interfaces.iter_mut() {
        if !i.is_wifi {
            continue;
        }
        if let Some(w) = wifi.iter().find(|w| w.description == i.description) {
            i.signal_percent = w.signal_percent;
            i.rssi_dbm = w.rssi_dbm;
            i.link_speed_mbps = w.link_speed_mbps;
        }
    }
}

fn finding(
    id: &str,
    title: &str,
    severity: &str,
    confidence: &str,
    evidence: Vec<EvidencePoint>,
    detail: String,
    contradicting: Vec<String>,
) -> NetworkFinding {
    NetworkFinding {
        id: id.into(),
        title: title.into(),
        severity: severity.into(),
        confidence: confidence.into(),
        evidence,
        detail,
        contradicting,
    }
}

/// Per-interface diagnosis and findings.
pub fn network_diagnose(sample_window_ms: u64) -> Result<NetworkDiagnosis, WinkitError> {
    let started = std::time::Instant::now();
    let mut unavailable = Vec::new();
    let mut findings = Vec::new();

    let ifaces = match crate::platform::windows::network::list_network_interfaces() {
        Ok(list) => list,
        Err(e) => {
            unavailable.push(UnavailableReading::new(
                "interfaces",
                "connectivity",
                SensorAvailability::Unavailable,
                e.message.clone(),
            ));
            return Ok(NetworkDiagnosis {
                status: "unavailable".into(),
                timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
                duration_ms: started.elapsed().as_millis() as u64,
                summary: "network interfaces could not be enumerated".into(),
                interfaces: Vec::new(),
                findings: Vec::new(),
                completeness: "limited".into(),
                unavailable,
            });
        }
    };

    let wifi = crate::platform::windows::wifi::wifi_statuses();
    let mut interfaces: Vec<NetworkDiagnosticInterface> = Vec::new();

    for iface in ifaces.iter().filter(|i| !i.is_loopback) {
        let desc_lower = iface.description.to_ascii_lowercase();
        interfaces.push(NetworkDiagnosticInterface {
            description: iface.description.clone(),
            is_wifi: desc_lower.contains("wi-fi")
                || desc_lower.contains("wireless")
                || desc_lower.contains("wlan"),
            is_up: iface.is_up,
            gateway: iface.gateway.clone(),
            ..Default::default()
        });
    }
    attach_wifi(&mut interfaces, &wifi);

    // Probe the first up interface with a gateway.
    let probe_target = interfaces
        .iter()
        .find(|i| i.is_up && i.gateway.is_some())
        .and_then(|i| i.gateway.clone());

    if let Some(gateway) = probe_target {
        match probe_gateway(&gateway) {
            Some((loss, avg_rtt)) => {
                if !avg_rtt.is_nan() {
                    let idx = interfaces
                        .iter()
                        .position(|i| i.gateway.as_deref() == Some(gateway.as_str()));
                    if let Some(i) = idx {
                        interfaces[i].packet_loss_percent = Some(loss);
                        interfaces[i].gateway_latency_ms = Some(avg_rtt);
                    }
                }
                let evidence = vec![
                    EvidencePoint {
                        metric: "gateway".into(),
                        value: gateway.clone(),
                        detail: "default gateway".into(),
                    },
                    EvidencePoint {
                        metric: "packet_loss_percent".into(),
                        value: format!("{loss:.1}"),
                        detail: "ICMP echo loss over the probe window".into(),
                    },
                    EvidencePoint {
                        metric: "gateway_latency_ms".into(),
                        value: if avg_rtt.is_nan() {
                            "unreachable".into()
                        } else {
                            format!("{avg_rtt:.0}")
                        },
                        detail: "average round-trip to the gateway".into(),
                    },
                ];
                if loss >= LOSS_WARNING_PERCENT {
                    findings.push(finding(
                        "gateway-packet-loss",
                        "Packet loss to the gateway",
                        if loss >= 20.0 { "high" } else { "medium" },
                        "confirmed",
                        evidence,
                        format!("{loss:.1}% of ICMP probes to the gateway {gateway} were lost"),
                        Vec::new(),
                    ));
                } else if !avg_rtt.is_nan() && avg_rtt >= LATENCY_WARNING_MS {
                    findings.push(finding(
                        "gateway-high-latency",
                        "High latency to the gateway",
                        "medium",
                        "confirmed",
                        evidence,
                        format!("average round-trip to the gateway is {avg_rtt:.0} ms"),
                        Vec::new(),
                    ));
                }
            }
            None => {
                unavailable.push(UnavailableReading::new(
                    "gateway",
                    "connectivity",
                    SensorAvailability::Unavailable,
                    format!("ICMP probe to the gateway {gateway} could not be attempted"),
                ));
            }
        }
    }

    // Wi-Fi signal and link-quality findings.
    for i in interfaces.iter() {
        if !i.is_wifi {
            continue;
        }
        if let Some(signal) = i.signal_percent {
            if signal < WEAK_SIGNAL_PERCENT {
                findings.push(finding(
                    &format!("wifi-weak-signal-{}", slug(&i.description)),
                    "Weak Wi-Fi signal",
                    if signal < 25 { "high" } else { "medium" },
                    "confirmed",
                    vec![EvidencePoint {
                        metric: "wifi_signal_percent".into(),
                        value: signal.to_string(),
                        detail: "OS-reported signal quality".into(),
                    }],
                    format!(
                        "signal is {signal}% on {} (below the {WEAK_SIGNAL_PERCENT}% threshold)",
                        i.description
                    ),
                    Vec::new(),
                ));
            }
        }
        if let Some(speed) = i.link_speed_mbps {
            if speed < LOW_LINK_SPEED_MBPS {
                findings.push(finding(
                    &format!("wifi-low-link-speed-{}", slug(&i.description)),
                    "Low Wi-Fi link speed",
                    "medium",
                    "observed",
                    vec![EvidencePoint {
                        metric: "wifi_link_speed_mbps".into(),
                        value: format!("{speed:.0}"),
                        detail: "negotiated link rate".into(),
                    }],
                    format!(
                        "link speed is {speed:.0} Mbps on {} (below the {LOW_LINK_SPEED_MBPS} Mbps threshold)",
                        i.description
                    ),
                    Vec::new(),
                ));
            }
        }
        if i.is_wifi && i.is_up && i.signal_percent.is_none() {
            unavailable.push(UnavailableReading::new(
                slug(&i.description),
                "wifi_signal",
                SensorAvailability::Unavailable,
                "connected Wi-Fi adapter reported no signal quality",
            ));
        }
    }

    // Interface-level observations.
    for i in interfaces.iter() {
        if i.is_up && i.gateway.is_none() {
            findings.push(finding(
                &format!("no-gateway-{}", slug(&i.description)),
                "No default gateway",
                "info",
                "confirmed",
                vec![],
                format!(
                    "interface {} is up but has no default gateway",
                    i.description
                ),
                Vec::new(),
            ));
        }
        if !i.is_up {
            findings.push(finding(
                &format!("interface-down-{}", slug(&i.description)),
                "Network interface is down",
                "low",
                "confirmed",
                vec![],
                format!("interface {} is administratively down", i.description),
                Vec::new(),
            ));
        }
    }

    let issues: Vec<&NetworkFinding> = findings
        .iter()
        .filter(|f| f.severity == "high" || f.severity == "critical" || f.severity == "medium")
        .collect();
    let status = if unavailable.is_empty() && issues.is_empty() {
        "ok"
    } else if issues.is_empty() {
        "limited"
    } else {
        "issues_detected"
    };

    let summary = if issues.is_empty() {
        if status == "limited" {
            "network could not be fully assessed; no issues confirmed".to_string()
        } else {
            "no network issues detected".to_string()
        }
    } else {
        format!(
            "{} network issue(s) detected: {}",
            issues.len(),
            issues
                .iter()
                .map(|f| f.title.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )
    };

    Ok(NetworkDiagnosis {
        status: status.into(),
        timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
        duration_ms: (started.elapsed().as_millis() as u64).max(sample_window_ms.min(1_000)),
        summary,
        interfaces,
        findings,
        completeness: if unavailable.is_empty() {
            "full"
        } else {
            "limited"
        }
        .into(),
        unavailable,
    })
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// Bounded composite network snapshot for `network_snapshot`.
pub fn network_snapshot() -> Result<crate::models::NetworkSnapshot, WinkitError> {
    let started = std::time::Instant::now();
    let mut unavailable = Vec::new();

    let ifaces = match crate::platform::windows::network::list_network_interfaces() {
        Ok(list) => list,
        Err(e) => {
            unavailable.push(UnavailableReading::new(
                "interfaces",
                "status",
                SensorAvailability::Unavailable,
                e.message.clone(),
            ));
            Vec::new()
        }
    };
    let connections = match crate::platform::windows::network::list_connections(256) {
        Ok(list) => list,
        Err(e) => {
            unavailable.push(UnavailableReading::new(
                "tcp_connections",
                "status",
                SensorAvailability::Unavailable,
                e.message.clone(),
            ));
            Vec::new()
        }
    };
    let ports = match crate::platform::windows::network::list_listening_ports(256) {
        Ok(list) => list,
        Err(e) => {
            unavailable.push(UnavailableReading::new(
                "listening_ports",
                "status",
                SensorAvailability::Unavailable,
                e.message.clone(),
            ));
            Vec::new()
        }
    };
    let wifi = crate::platform::windows::wifi::wifi_statuses();

    Ok(crate::models::NetworkSnapshot {
        status: if unavailable.is_empty() {
            "ok"
        } else {
            "limited"
        }
        .into(),
        timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
        duration_ms: started.elapsed().as_millis() as u64,
        interfaces: ifaces,
        wifi,
        connections,
        listening_ports: ports,
        completeness: if unavailable.is_empty() {
            "full"
        } else {
            "limited"
        }
        .into(),
        unavailable,
    })
}
