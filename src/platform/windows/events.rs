//! Event log observability via the Windows Event Log API (wevtapi),
//! read-only. Queries are expressed as XPath against a channel; results are
//! rendered to XML and normalized into [`EventInfo`].

use crate::errors::WinkitError;
use crate::models::{EventInfo, EventLevel, EventQuery};
use crate::platform::windows::ffi::{self, EvtClose};
use crate::utils::time;
use crate::utils::{to_wide, wide_to_string};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::ptr::null_mut;

/// EvtQuery flags.
const EVT_QUERY_CHANNEL_PATH: u32 = 0x1;
const EVT_QUERY_REVERSE_DIRECTION: u32 = 0x200;

/// EvtRender mode: XML.
const EVT_RENDER_MODE_XML: u32 = 0x1;

/// EvtNext timeout in ms.
const EVT_NEXT_TIMEOUT_MS: u32 = 3_000;

/// Error codes.
const ERROR_NO_MORE_ITEMS: u32 = 259;
const ERROR_TIMEOUT: u32 = 1460;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

/// Build an XPath query from the request filters.
fn build_xpath(q: &EventQuery) -> String {
    let mut conditions: Vec<String> = Vec::new();
    if let Some(level) = q.min_level {
        conditions.push(format!("Level <= {level}"));
    }
    if let Some(id) = q.event_id {
        conditions.push(format!("EventID = {id}"));
    }
    if let Some(provider) = &q.provider {
        conditions.push(format!(
            "Provider[@Name='{}']",
            provider.replace('\'', "''")
        ));
    }
    if let Some(minutes) = q.since_minutes {
        // timediff(@SystemTime) is expressed in milliseconds.
        conditions.push(format!(
            "TimeCreated[timediff(@SystemTime) <= {}]",
            minutes * 60_000
        ));
    }
    let body = if conditions.is_empty() {
        "*".to_string()
    } else {
        conditions.join(" and ")
    };
    format!("*[System[({body})]]")
}

/// Query a channel, newest first, bounded by `max_results`.
pub fn get_recent_events(q: &EventQuery) -> Result<Vec<EventInfo>, WinkitError> {
    let xpath = build_xpath(q);
    let log_wide = to_wide(&q.log);
    let query_wide = to_wide(&xpath);
    let hquery = unsafe {
        ffi::EvtQuery(
            null_mut(),
            log_wide.as_ptr(),
            query_wide.as_ptr(),
            EVT_QUERY_CHANNEL_PATH | EVT_QUERY_REVERSE_DIRECTION,
        )
    };
    if hquery.is_null() {
        return Err(WinkitError::windows_api("EvtQuery"));
    }

    let mut events: Vec<EventInfo> = Vec::new();
    let mut handles = [null_mut::<std::ffi::c_void>(); 32];
    loop {
        if events.len() >= q.max_results {
            break;
        }
        let mut returned: u32 = 0;
        let ok = unsafe {
            ffi::EvtNext(
                hquery,
                handles.len() as u32,
                handles.as_mut_ptr(),
                EVT_NEXT_TIMEOUT_MS,
                0,
                &mut returned,
            )
        };
        if ok == 0 {
            let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            if err == ERROR_NO_MORE_ITEMS || err == ERROR_TIMEOUT {
                break;
            }
            break; // Other errors: stop cleanly rather than surfacing noise.
        }
        for &h in handles.iter().take(returned as usize) {
            if let Some(info) = render_event(h) {
                events.push(info);
                if events.len() >= q.max_results {
                    break;
                }
            }
            unsafe { EvtClose(h) };
        }
    }
    unsafe { EvtClose(hquery) };
    Ok(events)
}

fn render_event(handle: *mut std::ffi::c_void) -> Option<EventInfo> {
    let xml = render_event_xml(handle)?;
    parse_event_xml(&xml)
}

/// Render one event to its XML form.
fn render_event_xml(handle: *mut std::ffi::c_void) -> Option<String> {
    let mut used: u32 = 0;
    let mut property_count: u32 = 0;
    let mut ok = unsafe {
        ffi::EvtRender(
            null_mut(),
            handle,
            EVT_RENDER_MODE_XML,
            0,
            null_mut(),
            &mut used,
            &mut property_count,
        )
    };
    if ok == 0 {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        if err != ERROR_INSUFFICIENT_BUFFER {
            return None;
        }
    }
    // On the sizing probe EvtRender reports the required buffer size in
    // bytes through `used` (BufferUsed); `property_count` is unrelated to
    // buffer sizing and must not be used to allocate.
    let mut buf = vec![0u16; (used as usize).div_ceil(2)];
    used = 0;
    ok = unsafe {
        ffi::EvtRender(
            null_mut(),
            handle,
            EVT_RENDER_MODE_XML,
            (buf.len() * 2) as u32,
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            &mut used,
            &mut property_count,
        )
    };
    if ok == 0 {
        return None;
    }
    let chars = (used as usize).min(buf.len() * 2) / 2;
    Some(wide_to_string(&buf[..chars.min(buf.len())]))
}

/// Parse the wevtapi XML rendering into a normalized `EventInfo`.
///
/// The parser only extracts the fields WinKit is allowed to expose; it
/// never reads `EventData` payload content.
fn parse_event_xml(xml: &str) -> Option<EventInfo> {
    let mut info = EventInfo {
        record_id: None,
        event_id: None,
        level: EventLevel::Unknown,
        provider: None,
        channel: None,
        time_created: None,
        computer: None,
        process_id: None,
        message: None,
    };

    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut in_system = false;
    let mut in_rendering = false;
    let mut current_field: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                match name.as_str() {
                    "System" => in_system = true,
                    "RenderingInfo" => in_rendering = true,
                    "Message" if in_rendering => current_field = Some("Message".to_string()),
                    "EventID" | "Level" | "Channel" | "Computer" | "EventRecordID" if in_system => {
                        current_field = Some(name.clone());
                    }
                    "Provider" | "TimeCreated" | "Execution" => {
                        read_attributes(e, &mut info);
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if matches!(name.as_str(), "Provider" | "TimeCreated" | "Execution") {
                    read_attributes(e, &mut info);
                }
            }
            Ok(Event::Text(t)) => {
                if let Some(field) = &current_field {
                    let text = t.unescape().unwrap_or_default().trim().to_string();
                    match field.as_str() {
                        "Message" => info.message = Some(text),
                        "EventID" => info.event_id = text.parse::<u32>().ok(),
                        "Level" => info.level = EventLevel::from_u32(text.parse().unwrap_or(0)),
                        "Channel" => info.channel = Some(text),
                        "Computer" => info.computer = Some(text),
                        "EventRecordID" => info.record_id = text.parse::<u64>().ok(),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                match name.as_str() {
                    "System" => in_system = false,
                    "RenderingInfo" => in_rendering = false,
                    _ => {}
                }
                if current_field.as_deref() == Some(name.as_str()) {
                    current_field = None;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    if info.event_id.is_none() && info.provider.is_none() {
        return None; // Not a parsable event.
    }
    Some(info)
}

/// Read `Provider` / `TimeCreated` / `Execution` attributes (single-element
/// tags, either self-closing or with children).
fn read_attributes(e: quick_xml::events::BytesStart<'_>, info: &mut EventInfo) {
    for attr in e.attributes().flatten() {
        let value = String::from_utf8_lossy(attr.value.as_ref()).into_owned();
        match attr.key.as_ref() {
            b"Name" if info.provider.is_none() => info.provider = Some(value),
            b"SystemTime" if info.time_created.is_none() => info.time_created = Some(value),
            b"ProcessID" if info.process_id.is_none() => {
                info.process_id = value.parse::<u32>().ok()
            }
            _ => {}
        }
    }
}

/// Preset: application crash/error events (Windows Error Reporting).
pub fn application_error_query(max_results: usize, since_minutes: Option<u64>) -> EventQuery {
    EventQuery {
        log: "Application".to_string(),
        min_level: Some(2),
        since_minutes,
        provider: None,
        event_id: None,
        max_results,
    }
}

/// Preset: system error events (including BugCheck 1001, Kernel-Power 41).
pub fn system_error_query(max_results: usize, since_minutes: Option<u64>) -> EventQuery {
    EventQuery {
        log: "System".to_string(),
        min_level: Some(2),
        since_minutes,
        provider: None,
        event_id: None,
        max_results,
    }
}

/// RFC3339 helper re-export for consistency.
pub fn now_rfc3339() -> String {
    time::format_rfc3339(std::time::SystemTime::now())
}
