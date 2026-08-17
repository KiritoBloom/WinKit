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
    let mut info = parse_event_xml(&render_event_xml(handle)?)?;
    if info.message.is_none() {
        // The XML render never carries a `<Message>` element; ask the provider
        // manifest directly, exactly like Event Viewer / Get-WinEvent do.
        if let Some(provider) = &info.provider {
            info.message = format_message(provider, handle);
        }
    }
    Some(info)
}

/// Render the provider-formatted message for an event handle (parameter
/// substitution included). Returns `None` when the provider publishes no
/// message template (its payload lives only in `EventData`).
fn format_message(provider: &str, handle: *mut std::ffi::c_void) -> Option<String> {
    // Classic sources (SCM, Application Error, NetBT, ...) are only
    // resolvable through an explicit publisher-metadata handle; a null
    // metadata handle fails with ERROR_EVT_MESSAGE_NOT_FOUND for them.
    let provider_wide = to_wide(provider);
    let metadata = unsafe {
        ffi::EvtOpenPublisherMetadata(
            null_mut(),
            provider_wide.as_ptr(),
            null_mut(),
            0,
            0,
        )
    };
    if metadata.is_null() {
        return None;
    }
    let result = format_message_with_metadata(metadata, handle);
    unsafe { ffi::EvtClose(metadata) };
    result
}

fn format_message_with_metadata(
    metadata: *mut std::ffi::c_void,
    handle: *mut std::ffi::c_void,
) -> Option<String> {
    let mut used: u32 = 0;
    let probe = unsafe {
        ffi::EvtFormatMessage(
            metadata,
            handle,
            0,
            0,
            null_mut(),
            ffi::EVT_FORMAT_MESSAGE_EVENT,
            0,
            null_mut(),
            &mut used,
        )
    };
    if probe == 0 {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        if err != ERROR_INSUFFICIENT_BUFFER || used == 0 {
            return None;
        }
    }
    let mut buf = vec![0u16; used as usize];
    let mut written: u32 = 0;
    let ok = unsafe {
        ffi::EvtFormatMessage(
            metadata,
            handle,
            0,
            0,
            null_mut(),
            ffi::EVT_FORMAT_MESSAGE_EVENT,
            buf.len() as u32,
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            &mut written,
        )
    };
    if ok == 0 {
        return None;
    }
    Some(wide_to_string(&buf[..(written as usize).min(buf.len())]))
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
    // The rendered `Message` can arrive as several text chunks (surrounding
    // whitespace, entity boundaries); accumulate and flush once so a trailing
    // whitespace chunk can never blank an already-captured message.
    let mut message_buf = String::new();

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
                    let text = t.unescape().unwrap_or_default();
                    match field.as_str() {
                        "Message" => message_buf.push_str(&text),
                        "EventID" => info.event_id = text.trim().parse::<u32>().ok(),
                        "Level" => info.level = EventLevel::from_u32(text.trim().parse().unwrap_or(0)),
                        "Channel" => info.channel = Some(text.trim().to_string()),
                        "Computer" => info.computer = Some(text.trim().to_string()),
                        "EventRecordID" => info.record_id = text.trim().parse::<u64>().ok(),
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
                    if name == "Message" {
                        let message = message_buf.trim();
                        if !message.is_empty() {
                            info.message = Some(message.to_string());
                        }
                        message_buf.clear();
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative wevtapi rendered-event XML: `System` metadata plus a
    /// `RenderingInfo` section with the formatted message.
    fn rendered_event_xml() -> &'static str {
        r#"<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'>
  <System>
    <Provider Name='Application Error'/>
    <EventID>1000</EventID>
    <Level>2</Level>
    <TimeCreated SystemTime='2026-08-15T19:27:00.000Z'/>
    <EventRecordID>12345</EventRecordID>
    <Execution ProcessID='1234' ThreadID='0'/>
    <Channel>Application</Channel>
    <Computer>HOST</Computer>
  </System>
  <EventData>
    <Data Name='AppName'>chrome.exe</Data>
  </EventData>
  <RenderingInfo Culture='en-US'>
    <Message>Faulting application name: chrome.exe &amp; helper, version: 126.0.6478.127, time stamp: 0x667b7e6c
Faulting module name: chrome.dll, version: 126.0.6478.127, time stamp: 0x667b7d99
Exception code: 0xc0000005
  </Message>
    <Level>Error</Level>
    <Task>Application Crashing Events</Task>
    <Opcode>Info</Opcode>
    <Channel>Application</Channel>
    <Provider>Application Error</Provider>
  </RenderingInfo>
</Event>"#
    }

    #[test]
    fn parse_rendered_event_extracts_fields() {
        let info = parse_event_xml(rendered_event_xml()).expect("representative event should parse");
        let message = info.message.as_deref().unwrap_or("");
        assert!(
            message.contains("Faulting application name"),
            "message should keep the faulting app line, got: {message:?}"
        );
        assert!(
            message.contains("chrome.exe & helper"),
            "message should decode XML entities, got: {message:?}"
        );
        assert_eq!(info.process_id, Some(1234));
        assert_eq!(info.event_id, Some(1000));
        assert_eq!(info.provider.as_deref(), Some("Application Error"));
        assert_eq!(info.channel.as_deref(), Some("Application"));
        assert_eq!(info.record_id, Some(12345));
        assert_eq!(info.level, EventLevel::Error);
        assert!(
            info.time_created.as_deref().unwrap_or("").starts_with("2026-08-15"),
            "time_created was {:?}",
            info.time_created
        );
    }

    #[test]
    fn parse_without_rendering_info_leaves_message_none() {
        let xml = r#"<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'>
  <System>
    <Provider Name='Application Error'/>
    <EventID>1000</EventID>
    <Level>2</Level>
    <TimeCreated SystemTime='2026-08-15T19:27:00.000Z'/>
    <EventRecordID>12345</EventRecordID>
    <Execution ProcessID='1234' ThreadID='0'/>
    <Channel>Application</Channel>
    <Computer>HOST</Computer>
  </System>
  <EventData>
    <Data Name='AppName'>chrome.exe</Data>
  </EventData>
</Event>"#;
        let info = parse_event_xml(xml).expect("raw event should parse");
        assert_eq!(info.message, None, "no RenderingInfo means no fabricated message");
        assert_eq!(info.process_id, Some(1234));
        assert_eq!(info.event_id, Some(1000));
        assert_eq!(info.provider.as_deref(), Some("Application Error"));
        assert_eq!(info.record_id, Some(12345));
    }

    #[test]
    fn whitespace_only_message_stays_none() {
        let xml = "<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'>\
            <System><Provider Name='P'/><EventID>1000</EventID></System>\
            <RenderingInfo Culture='en-US'><Message>  </Message></RenderingInfo>\
            </Event>";
        let info = parse_event_xml(xml).expect("event should parse");
        assert_eq!(info.message, None, "whitespace-only Message must not become an empty string");
    }
}
