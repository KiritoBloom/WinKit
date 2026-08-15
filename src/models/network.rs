//! Network observability models.

use serde::{Deserialize, Serialize};

/// TCP connection states from the Windows MIB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
    DeleteTcb,
    Unknown,
}

impl TcpState {
    pub fn from_mib(state: u32) -> Self {
        match state {
            1 => Self::Closed,
            2 => Self::Listen,
            3 => Self::SynSent,
            4 => Self::SynReceived,
            5 => Self::Established,
            6 => Self::FinWait1,
            7 => Self::FinWait2,
            8 => Self::CloseWait,
            9 => Self::Closing,
            10 => Self::LastAck,
            11 => Self::TimeWait,
            12 => Self::DeleteTcb,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Listen => "listen",
            Self::SynSent => "syn_sent",
            Self::SynReceived => "syn_received",
            Self::Established => "established",
            Self::FinWait1 => "fin_wait1",
            Self::FinWait2 => "fin_wait2",
            Self::CloseWait => "close_wait",
            Self::Closing => "closing",
            Self::LastAck => "last_ack",
            Self::TimeWait => "time_wait",
            Self::DeleteTcb => "delete_tcb",
            Self::Unknown => "unknown",
        }
    }
}

/// A single listening port.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PortInfo {
    pub port: u16,
    pub protocol: String,
    pub pid: Option<u32>,
    pub process_name: Option<String>,
    pub state: Option<String>,
    /// Bound address, e.g. `0.0.0.0` or `127.0.0.1`.
    pub address: String,
}

/// A single TCP connection (v4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectionInfo {
    pub protocol: String,
    pub state: String,
    pub local_address: String,
    pub local_port: u16,
    pub remote_address: String,
    pub remote_port: u16,
    pub pid: Option<u32>,
    pub process_name: Option<String>,
}

/// A network interface as reported by the adapter table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkInterfaceInfo {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub mac_address: Option<String>,
    /// IPv4 addresses in dotted-quad form.
    pub ipv4_addresses: Vec<String>,
    pub ipv4_masks: Vec<String>,
    pub gateway: Option<String>,
    /// True for loopback interfaces.
    pub is_loopback: bool,
    pub is_up: bool,
}
