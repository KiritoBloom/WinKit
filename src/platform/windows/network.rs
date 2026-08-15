//! Network observability via Win32/IP Helper (read-only, local only).
//!
//! v1 scope: IPv4 TCP tables via `GetExtendedTcpTable`, IPv4 UDP tables via
//! `GetExtendedUdpTable`, and adapter enumeration via `GetAdaptersInfo`.
//! IPv6 connection detail is deferred to v0.2 (documented limitation, no
//! fabricated data).

use crate::errors::WinkitError;
use crate::models::{ConnectionInfo, NetworkInterfaceInfo, PortInfo, ProcessOnPort, TcpState};
use crate::platform::windows::ffi::{AdapterInfo, IpAddrString};
use crate::platform::windows::processes::pid_to_name;
use crate::utils::fixed_bytes_to_string;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{
    ERROR_BUFFER_OVERFLOW, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
    MIB_UDPROW_OWNER_PID, MIB_UDPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
};
use windows_sys::Win32::Networking::WinSock::AF_INET;

/// TCP listen state constant (MIB_TCP_STATE_LISTEN).
const MIB_TCP_STATE_LISTEN: u32 = 2;

/// Convert a network-byte-order port into host order.
fn port_to_host(nbo: u32) -> u16 {
    u16::from_be(nbo as u16)
}

/// Convert a network-byte-order IPv4 address into dotted-quad form.
///
/// The table APIs store the address as four bytes in network byte order.
/// Reading the field as a `u32` on a little-endian host yields the
/// byte-swapped value, so `to_le_bytes()` (which recovers the original
/// memory order) is correct here — the same compensation `port_to_host`
/// applies via `from_be`.
fn ipv4_to_string(addr: u32) -> String {
    let bytes = addr.to_le_bytes();
    format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3])
}

fn tcp_table(af: u16, class: i32) -> Result<Vec<MIB_TCPROW_OWNER_PID>, WinkitError> {
    let mut size: u32 = 0;
    let mut err = unsafe { GetExtendedTcpTable(null_mut(), &mut size, 0, af as u32, class, 0) };
    if err != ERROR_INSUFFICIENT_BUFFER {
        return Err(WinkitError::windows_api("GetExtendedTcpTable"));
    }
    let mut buf = vec![0u8; size as usize];
    err = unsafe {
        GetExtendedTcpTable(
            buf.as_mut_ptr() as *mut _,
            &mut size,
            0,
            af as u32,
            class,
            0,
        )
    };
    if err != ERROR_SUCCESS {
        return Err(WinkitError::windows_api("GetExtendedTcpTable"));
    }
    let table = unsafe { &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID) };
    let count = table.dwNumEntries as usize;
    Ok(unsafe { std::slice::from_raw_parts(table.table.as_ptr(), count).to_vec() })
}

/// IPv4 UDP table via `GetExtendedUdpTable` (`UDP_TABLE_OWNER_PID`).
///
/// Dedicated to IPv4 because the AF_INET6 variant requires a different table
/// type (`MIB_UDP6TABLE_OWNER_PID`); v1 has no IPv6 callers.
fn udp_table_v4() -> Result<Vec<MIB_UDPROW_OWNER_PID>, WinkitError> {
    let mut size: u32 = 0;
    let mut err = unsafe {
        GetExtendedUdpTable(
            null_mut(),
            &mut size,
            0,
            AF_INET as u32,
            UDP_TABLE_OWNER_PID,
            0,
        )
    };
    if err != ERROR_INSUFFICIENT_BUFFER {
        return Err(WinkitError::windows_api("GetExtendedUdpTable"));
    }
    let mut buf = vec![0u8; size as usize];
    err = unsafe {
        GetExtendedUdpTable(
            buf.as_mut_ptr() as *mut _,
            &mut size,
            0,
            AF_INET as u32,
            UDP_TABLE_OWNER_PID,
            0,
        )
    };
    if err != ERROR_SUCCESS {
        return Err(WinkitError::windows_api("GetExtendedUdpTable"));
    }
    let table = unsafe { &*(buf.as_ptr() as *const MIB_UDPTABLE_OWNER_PID) };
    let count = table.dwNumEntries as usize;
    Ok(unsafe { std::slice::from_raw_parts(table.table.as_ptr(), count).to_vec() })
}

/// All listening TCP ports plus all UDP bindings, bounded by `limit`.
pub fn list_listening_ports(limit: usize) -> Result<Vec<PortInfo>, WinkitError> {
    let mut out: Vec<PortInfo> = Vec::new();
    for row in tcp_table(AF_INET, TCP_TABLE_OWNER_PID_ALL)? {
        if row.dwState != MIB_TCP_STATE_LISTEN {
            continue;
        }
        out.push(PortInfo {
            port: port_to_host(row.dwLocalPort),
            protocol: "tcp".to_string(),
            pid: (row.dwOwningPid != 0).then_some(row.dwOwningPid),
            process_name: None,
            state: Some(TcpState::from_mib(row.dwState).as_str().to_string()),
            address: ipv4_to_string(row.dwLocalAddr),
        });
        if out.len() >= limit {
            break;
        }
    }
    if out.len() < limit {
        for row in udp_table_v4()? {
            out.push(PortInfo {
                port: port_to_host(row.dwLocalPort),
                protocol: "udp".to_string(),
                pid: (row.dwOwningPid != 0).then_some(row.dwOwningPid),
                process_name: None,
                state: None,
                address: ipv4_to_string(row.dwLocalAddr),
            });
            if out.len() >= limit {
                break;
            }
        }
    }
    // Resolve process names lazily (only for the bounded result set).
    for info in &mut out {
        if let Some(pid) = info.pid {
            info.process_name = pid_to_name(pid);
        }
    }
    Ok(out)
}

/// Find the process owning a listening port.
pub fn process_on_port(port: u16) -> Result<Option<ProcessOnPort>, WinkitError> {
    let mut candidates = Vec::new();
    for row in tcp_table(AF_INET, TCP_TABLE_OWNER_PID_ALL)? {
        if row.dwState == MIB_TCP_STATE_LISTEN && port_to_host(row.dwLocalPort) == port {
            candidates.push((
                "tcp".to_string(),
                row.dwOwningPid,
                Some(TcpState::from_mib(row.dwState).as_str().to_string()),
            ));
        }
    }
    for row in udp_table_v4()? {
        if port_to_host(row.dwLocalPort) == port {
            candidates.push(("udp".to_string(), row.dwOwningPid, None));
        }
    }
    let (protocol, pid, state) = match candidates.into_iter().next() {
        Some(c) => c,
        None => return Ok(None),
    };
    let process_name = (pid != 0).then(|| pid_to_name(pid)).flatten();
    Ok(Some(ProcessOnPort {
        port,
        protocol,
        pid: (pid != 0).then_some(pid),
        process_name,
        state,
    }))
}

/// All TCP v4 connections (UDP bindings are excluded: `ConnectionInfo`
/// carries remote-address and connection-state fields that are TCP-only),
/// bounded by `limit`.
pub fn list_connections(limit: usize) -> Result<Vec<ConnectionInfo>, WinkitError> {
    let mut out: Vec<ConnectionInfo> = Vec::new();
    for row in tcp_table(AF_INET, TCP_TABLE_OWNER_PID_ALL)? {
        out.push(ConnectionInfo {
            protocol: "tcp".to_string(),
            state: TcpState::from_mib(row.dwState).as_str().to_string(),
            local_address: ipv4_to_string(row.dwLocalAddr),
            local_port: port_to_host(row.dwLocalPort),
            remote_address: ipv4_to_string(row.dwRemoteAddr),
            remote_port: port_to_host(row.dwRemotePort),
            pid: (row.dwOwningPid != 0).then_some(row.dwOwningPid),
            process_name: None,
        });
        if out.len() >= limit {
            break;
        }
    }
    for info in &mut out {
        if let Some(pid) = info.pid {
            info.process_name = pid_to_name(pid);
        }
    }
    Ok(out)
}

/// Enumerate network interfaces (IPv4 view).
pub fn list_network_interfaces() -> Result<Vec<NetworkInterfaceInfo>, WinkitError> {
    let mut size: u32 = 0;
    let mut err = unsafe { crate::platform::windows::ffi::GetAdaptersInfo(null_mut(), &mut size) };
    // The sizing probe may report either constant depending on the Windows
    // version (both mean "buffer too small"); treat both as success.
    if err != ERROR_INSUFFICIENT_BUFFER && err != ERROR_BUFFER_OVERFLOW {
        return Err(WinkitError::windows_api("GetAdaptersInfo"));
    }
    let mut buf = vec![0u8; size as usize];
    err = unsafe {
        crate::platform::windows::ffi::GetAdaptersInfo(
            buf.as_mut_ptr() as *mut AdapterInfo,
            &mut size,
        )
    };
    if err != ERROR_SUCCESS {
        return Err(WinkitError::windows_api("GetAdaptersInfo"));
    }

    let mut out = Vec::new();
    let mut current = buf.as_ptr() as *const AdapterInfo;
    while !current.is_null() {
        let adapter = unsafe { &*current };
        let mut ipv4 = Vec::new();
        let mut masks = Vec::new();
        let mut addr = &adapter.ip_address_list as *const IpAddrString;
        while !addr.is_null() {
            let a = unsafe { &*addr };
            ipv4.push(fixed_bytes_to_string(&a.ip_address));
            masks.push(fixed_bytes_to_string(&a.ip_mask));
            addr = a.next;
        }
        let gateway = {
            let gw = &adapter.gateway_list;
            let s = fixed_bytes_to_string(&gw.ip_address);
            (!s.is_empty() && s != "0.0.0.0").then_some(s)
        };
        let mac = if adapter.address_length >= 1 && adapter.address_length <= 6 {
            Some(
                adapter.address[..adapter.address_length as usize]
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(":"),
            )
        } else {
            None
        };
        let is_loopback = ipv4.iter().any(|ip| ip == "127.0.0.1");
        out.push(NetworkInterfaceInfo {
            index: adapter.index,
            name: fixed_bytes_to_string(&adapter.adapter_name),
            description: fixed_bytes_to_string(&adapter.description),
            mac_address: mac,
            ipv4_addresses: ipv4,
            ipv4_masks: masks,
            gateway,
            is_loopback,
            is_up: true, // GetAdaptersInfo only returns active adapters
        });
        current = adapter.next;
    }
    Ok(out)
}

/// Get the connection count (cheap summary for `snapshot`).
pub fn connection_count() -> usize {
    tcp_table(AF_INET, TCP_TABLE_OWNER_PID_ALL)
        .map(|t| t.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_addresses_are_decoded_in_dotted_quad_order() {
        // Values are the raw u32 fields as read on a little-endian host
        // (network byte order in memory). See `ipv4_to_string`.
        assert_eq!(ipv4_to_string(0), "0.0.0.0");
        assert_eq!(ipv4_to_string(0x0100_007F), "127.0.0.1");
        assert_eq!(ipv4_to_string(0x0901_A8C0), "192.168.1.9");
        assert_eq!(ipv4_to_string(0x7F00_0001), "1.0.0.127");
    }

    #[test]
    fn ports_are_converted_to_host_order() {
        // 135 in network byte order: memory bytes [0x00, 0x87]; read as a
        // little-endian u16 that is 0x8700, which from_be converts to 135.
        assert_eq!(port_to_host(0x8700), 135);
        assert_eq!(port_to_host(0x0908), 0x809); // 2057
    }

    // ABI documentation, not a substitute for calling the correct API: these
    // are the exact strides Windows uses for the owner-PID table rows. If a
    // future Windows version changes them, these assertions fail loudly
    // instead of silently misreading the table.
    #[test]
    fn owner_pid_row_layout_matches_windows_abi() {
        assert_eq!(std::mem::size_of::<MIB_UDPROW_OWNER_PID>(), 12);
        assert_eq!(std::mem::size_of::<MIB_TCPROW_OWNER_PID>(), 24);
    }
}

/// Live Windows regression tests (opt-in): `WINKIT_LIVE_WINDOWS=1 cargo test
/// --features live-windows`. Guards the corrected UDP path — the bound socket
/// must appear in the `GetExtendedUdpTable` result as a clean 12-byte row,
/// never as a TCP row from `GetExtendedTcpTable`.
#[cfg(all(test, feature = "live-windows"))]
mod live_windows {
    use super::*;

    fn live_enabled() -> bool {
        std::env::var("WINKIT_LIVE_WINDOWS")
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    fn live_skip(reason: &str) {
        eprintln!("SKIP: {reason}");
    }

    #[test]
    fn udp_table_reports_bound_udp_socket() {
        if !live_enabled() {
            live_skip("live diagnostic harness not enabled; run with WINKIT_LIVE_WINDOWS=1");
            return;
        }
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind loopback UDP socket");
        let port = socket.local_addr().expect("resolved local addr").port();

        let rows = udp_table_v4().expect("GetExtendedUdpTable succeeds");
        let row = rows
            .iter()
            .find(|r| port_to_host(r.dwLocalPort) == port)
            .unwrap_or_else(|| panic!("bound UDP port {port} missing from UDP table"));

        // A UDP row has no TCP state and no remote endpoint; the port is in
        // range and the owning PID is nonzero because this process owns it.
        assert_eq!(port_to_host(row.dwLocalPort), port);
        assert_ne!(row.dwOwningPid, 0, "bound socket must have an owning PID");
        let addr = ipv4_to_string(row.dwLocalAddr);
        assert_eq!(addr, "127.0.0.1", "loopback binding reported as {addr}");

        let owner = process_on_port(port)
            .expect("process_on_port succeeds")
            .expect("bound socket has an owner");
        assert_eq!(
            owner.protocol, "udp",
            "bound socket reported as {}",
            owner.protocol
        );
        assert_eq!(owner.port, port);
        assert_eq!(owner.state, None, "UDP must not carry a TCP state");

        let all = list_listening_ports(usize::MAX).expect("list_listening_ports succeeds");
        let udp = all
            .iter()
            .find(|p| p.protocol == "udp" && p.port == port)
            .unwrap_or_else(|| panic!("UDP port {port} missing from listening ports"));
        assert_eq!(udp.pid, Some(row.dwOwningPid));
        assert_eq!(udp.state, None, "UDP must not carry a TCP state");
        assert!(udp.address.contains('.'), "UDP address looked malformed");

        drop(socket);
    }
}
