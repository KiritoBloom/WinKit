//! End-to-end test of the eight `disk_scan_*` MCP tools through the real
//! release binary over stdio.
//!
//! This test does NOT run during a normal `cargo test`: it spawns
//! `target/release/winkit.exe`, so it is gated on `WINKIT_E2E=1`. It prints
//! a per-check PASS/FAIL report on stderr (the child's stdout is
//! protocol-clean) and panics if any check fails.
//!
//! The scan scope is a small temporary directory, so the recursive fallback
//! (used when the NTFS fast path is unavailable, e.g. without an elevated
//! token) never walks the entire C: or D: drive.
//!
//! Run with:
//! ```text
//! cargo build --release
//! WINKIT_E2E=1 cargo test --test diskscan_e2e -- --nocapture
//! ```

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const DISK_TOOLS: [&str; 8] = [
    "disk_scan",
    "disk_scan_start",
    "disk_scan_status",
    "disk_scan_cancel",
    "disk_scan_largest_files",
    "disk_scan_largest_folders",
    "disk_scan_folder_size",
    "disk_scan_find",
];

/// Words that must never appear in tool output: source files, secrets,
/// credentials, or environment blocks.
const FORBIDDEN: [&str; 12] = [
    "password",
    "secret",
    "api_key",
    "apikey",
    "authorization",
    "Bearer ",
    "BEGIN RSA",
    "PRIVATE KEY",
    "aws_access",
    "azure",
    "sk-",
    "tok_",
];

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: Arc<Mutex<Vec<String>>>,
    next_id: u64,
}

/// Always reap the child, even when a check panics mid-test: an orphaned
/// winkit.exe would keep the release binary locked and block rebuilds.
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    fn spawn(bin: &std::path::Path) -> Server {
        let mut child = Command::new(bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn winkit.exe");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        let stderr = child.stderr.take().expect("child stderr");
        let lines = Arc::new(Mutex::new(Vec::new()));
        let drain = lines.clone();
        std::thread::spawn(move || {
            // Drain stderr so the pipe never fills; stop on a read error
            // (a failing read would otherwise spin forever).
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(l) => drain.lock().unwrap().push(l),
                    Err(_) => break,
                }
            }
        });
        Server {
            child,
            stdin,
            stdout,
            stderr: lines,
            next_id: 0,
        }
    }

    /// Send one JSON-RPC request and read its reply line.
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string();
        self.stdin
            .write_all(frame.as_bytes())
            .expect("write request");
        self.stdin.write_all(b"\n").expect("newline");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read reply line");
        if line.is_empty() {
            panic!("child closed stdout without a reply to {method} #{id}");
        }
        let reply: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("reply is not JSON ({e}): {line:?}"));
        assert_eq!(reply["id"], id, "reply id mismatch for {method}: {reply}");
        reply
    }

    fn initialize(&mut self) -> Value {
        self.call(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "diskscan-e2e", "version": "0.0.0" },
            }),
        )
    }

    fn tools_call(&mut self, name: &str, args: Value) -> Value {
        self.call("tools/call", json!({ "name": name, "arguments": args }))
    }
}

/// A tiny bespoke JSON-RPC client for one request (used for status polling
/// without borrowing the Server mutably across loops).
fn poll_status(server: &mut Server, scan_id: &str) -> Value {
    let reply = server.tools_call("disk_scan_status", json!({ "scan_id": scan_id }));
    if reply.get("error").is_some() {
        panic!("disk_scan_status failed: {reply}");
    }
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .expect("status text");
    serde_json::from_str(text).expect("status JSON")
}

/// The e2e scan tree lives under the project's `target/` dir, not the OS
/// temp dir: it is guaranteed writable (the sandbox %TEMP% turned out to be
/// flaky) and it cannot collide with anything outside the project.
fn unique_root(tag: &str) -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("e2e-tmp")
        .join(format!("winkit_e2e_disk_{tag}_{}", stamp))
}

/// Build a small, known tree. Returns the normalized root path string.
fn build_tree(root: &std::path::Path) -> String {
    std::fs::create_dir_all(root.join("sub1").join("nested")).unwrap();
    std::fs::create_dir_all(root.join("sub2")).unwrap();
    std::fs::write(
        root.join("sub1").join("big.bin"),
        vec![0u8; 5 * 1024 * 1024],
    )
    .unwrap();
    std::fs::write(root.join("sub1").join("small.txt"), vec![0u8; 10]).unwrap();
    std::fs::write(root.join("sub1").join("notes.md"), vec![0u8; 100]).unwrap();
    std::fs::write(
        root.join("sub1").join("nested").join("deep.zip"),
        vec![0u8; 1024 * 1024],
    )
    .unwrap();
    std::fs::write(root.join("sub2").join("a.dat"), vec![0u8; 50]).unwrap();
    std::fs::write(root.join("sub2").join("b.dat"), vec![0u8; 60]).unwrap();
    std::fs::write(root.join("root.txt"), vec![0u8; 7]).unwrap();
    root.to_string_lossy().into_owned()
}

/// Build a larger tree so a mid-walk cancellation is reliably observable.
fn build_cancel_tree(root: &std::path::Path) -> String {
    std::fs::create_dir_all(root).unwrap();
    for d in 0..8 {
        let sub = root.join(format!("canceldir{d:02}"));
        std::fs::create_dir_all(&sub).unwrap();
        for f in 0..400 {
            std::fs::File::create(sub.join(format!("f{f:04}.bin"))).unwrap();
        }
    }
    root.to_string_lossy().into_owned()
}

struct Check<'a> {
    results: &'a mut Vec<(String, bool, String)>,
}

impl<'a> Check<'a> {
    fn run(&mut self, name: &str, ok: bool, detail: String) {
        let status = if ok { "PASS" } else { "FAIL" };
        eprintln!("[e2e] {status}: {name} — {detail}");
        self.results.push((name.to_string(), ok, detail));
    }
}

#[test]
fn disk_tools_work_through_the_release_binary() {
    let mut results: Vec<(String, bool, String)> = Vec::new();
    let mut check = Check {
        results: &mut results,
    };

    // Resolve relative to the package root regardless of the test's cwd.
    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release")
        .join("winkit.exe");
    if !std::env::var("WINKIT_E2E")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        eprintln!("[e2e] SKIP: WINKIT_E2E=1 not set; build the release binary first (`cargo build --release`) and rerun with WINKIT_E2E=1");
        return;
    }
    if !bin.exists() {
        eprintln!(
            "[e2e] SKIP: {} not found; run `cargo build --release` first",
            bin.display()
        );
        return;
    }

    // The tree is the scan scope: recursive fallback never walks the drive.
    let root = unique_root("scope");
    let scope = build_tree(&root);
    let cancel_root = unique_root("cancel");
    let cancel_scope = build_cancel_tree(&cancel_root);

    let mut server = Server::spawn(&bin);

    // 1. initialize works.
    let init = server.initialize();
    check.run(
        "initialize",
        init.get("result").is_some(),
        format!("initialize reply: {init}"),
    );

    // 2. tools/list includes all eight disk tools.
    let listing = server.call("tools/list", json!({}));
    let names: Vec<String> = listing["result"]["tools"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t["name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let missing: Vec<&str> = DISK_TOOLS
        .iter()
        .copied()
        .filter(|t| !names.iter().any(|n| n == t))
        .collect();
    check.run(
        "tools/list has all disk tools",
        missing.is_empty(),
        if missing.is_empty() {
            format!("{} disk tools listed", DISK_TOOLS.len())
        } else {
            format!("missing: {missing:?}")
        },
    );

    // 3. disk_scan (summary) on the temp scope.
    // The handler wraps the info as `{"scan": {...}}`.
    let reply = server.tools_call("disk_scan", json!({ "path": scope }));
    if let Some(err) = reply.get("error") {
        check.run("disk_scan succeeds", false, format!("{err}"));
    } else {
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        let scan = &parsed["scan"];
        let ok = scan["files_indexed"] == json!(7)
            && scan["directories_indexed"].as_u64().unwrap_or(0) >= 3
            && scan["total_logical_bytes"].as_u64().unwrap_or(0) >= 5 * 1024 * 1024;
        check.run(
            "disk_scan summary counts",
            ok,
            format!(
                "files={} dirs={} bytes={}",
                scan["files_indexed"], scan["directories_indexed"], scan["total_logical_bytes"]
            ),
        );
        let scanner = scan["scanner"].as_str().unwrap_or("").to_string();
        let scanner_ok = scanner == "recursive_fallback" || scanner == "ntfs_mft_fast";
        check.run(
            "disk_scan reports scanner type",
            scanner_ok,
            format!(
                "scanner={scanner} fast_path_unavailable={:?}",
                scan["fast_path_unavailable"]
            ),
        );
        if scanner == "recursive_fallback" {
            let fpu = scan["fast_path_unavailable"].as_str().unwrap_or("");
            check.run(
                "fallback preserves the reason",
                !fpu.is_empty(),
                format!("fast_path_unavailable={fpu}"),
            );
        }
        check.run(
            "first scan is not cached",
            scan["cached"] == json!(false),
            format!("cached={}", scan["cached"]),
        );
        check.run(
            "scan duration and scanned_at present",
            scan["scan_duration_ms"].is_number() && scan["scanned_at"].is_string(),
            format!(
                "scan_duration_ms={} scanned_at={}",
                scan["scan_duration_ms"], scan["scanned_at"]
            ),
        );
    }

    // 4. Second disk_scan without refresh must hit the cache.
    let reply2 = server.tools_call("disk_scan", json!({ "path": scope }));
    let cached = reply2["result"]["content"][0]["text"]
        .as_str()
        .map(|t| serde_json::from_str::<Value>(t).unwrap())
        .map(|s| s["scan"]["cached"] == json!(true))
        .unwrap_or(false);
    check.run(
        "cached query reports cached",
        cached,
        format!("second disk_scan reply: {reply2}"),
    );

    // 5. Query tools against the cached snapshot.
    fn query(server: &mut Server, tool: &str, args: Value) -> Result<Value, Value> {
        let r = server.tools_call(tool, args);
        if r.get("error").is_some() {
            return Err(r);
        }
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        serde_json::from_str::<Value>(&text).map_err(|e| json!({"parse": e.to_string()}))
    }

    match query(
        &mut server,
        "disk_scan_largest_files",
        json!({ "path": scope, "limit": 10 }),
    ) {
        Ok(v) => {
            let count = v["count"].as_u64().unwrap_or(0);
            let biggest = v["files"]
                .as_array()
                .and_then(|a| a.first())
                .map(|f| f["path"].as_str().unwrap_or("").to_string())
                .unwrap_or_default();
            check.run(
                "disk_scan_largest_files",
                count == 7 && biggest.contains("big.bin"),
                format!("count={count} biggest={biggest}"),
            );
            let diag = &v["diagnostics"];
            check.run(
                "query diagnostics (scanner/cached/age)",
                diag["scanner"].is_string()
                    && diag["cached"] == json!(true)
                    && diag["snapshot_age_ms"].is_number(),
                format!("diagnostics={diag}"),
            );
        }
        Err(e) => check.run("disk_scan_largest_files", false, format!("{e}")),
    }

    match query(
        &mut server,
        "disk_scan_largest_folders",
        json!({ "path": scope, "limit": 10 }),
    ) {
        Ok(v) => {
            let first = v["folders"]
                .as_array()
                .and_then(|a| a.first())
                .map(|f| f["path"].as_str().unwrap_or("").to_string())
                .unwrap_or_default();
            check.run(
                "disk_scan_largest_folders",
                v["count"].as_u64().unwrap_or(0) >= 3 && first.contains("sub1"),
                format!("count={} first={first}", v["count"]),
            );
        }
        Err(e) => check.run("disk_scan_largest_folders", false, format!("{e}")),
    }

    match query(
        &mut server,
        "disk_scan_folder_size",
        json!({ "path": format!("{scope}\\sub1") }),
    ) {
        Ok(v) => {
            let size = v["folder"]["size_bytes"].as_u64().unwrap_or(0);
            check.run(
                "disk_scan_folder_size",
                size == 5 * 1024 * 1024 + 10 + 100 + 1024 * 1024,
                format!("sub1 size_bytes={size}"),
            );
        }
        Err(e) => check.run("disk_scan_folder_size", false, format!("{e}")),
    }

    match query(
        &mut server,
        "disk_scan_find",
        json!({ "path": scope, "pattern": "*.zip" }),
    ) {
        Ok(v) => {
            let hit = v["files"].as_array().map(|a| a.len()).unwrap_or(0);
            check.run(
                "disk_scan_find pattern",
                hit == 1
                    && v["files"][0]["path"]
                        .as_str()
                        .unwrap_or("")
                        .ends_with("deep.zip"),
                format!("found={hit} first={:?}", v["files"][0]["path"]),
            );
        }
        Err(e) => check.run("disk_scan_find pattern", false, format!("{e}")),
    }

    // 6. Missing arguments produce structured errors.
    let err = server.tools_call("disk_scan", json!({}));
    let structured =
        err["error"]["code"] == json!(-32602) && err["error"]["data"]["winkit_code"] == json!(1);
    check.run(
        "missing path is a structured error",
        structured,
        format!("{err}"),
    );
    let err2 = server.tools_call("disk_scan_status", json!({}));
    let structured2 = err2["error"]["code"] == json!(-32602);
    check.run(
        "missing scan_id is a structured error",
        structured2,
        format!("{err2}"),
    );

    // 7. Background lifecycle: start, poll progress, complete. Run against
    // the larger tree so a live poll can observe a *running* status (the
    // 7-file tree finishes in a few milliseconds).
    let start = server.tools_call("disk_scan_start", json!({ "path": cancel_scope }));
    let scan_id = start["result"]["content"][0]["text"]
        .as_str()
        .map(|t| serde_json::from_str::<Value>(t).unwrap())
        .and_then(|s| s["status"]["scan_id"].as_str().map(|s| s.to_string()))
        .expect("start returns scan_id");
    let mut seen_running = false;
    let mut final_status = json!(null);
    for _ in 0..2000 {
        let st = poll_status(&mut server, &scan_id);
        final_status = st.clone();
        if st["status"]["done"] == json!(true) {
            break;
        }
        if !seen_running {
            // A live poll should expose the progress fields while running.
            let fields_ok = st["status"]["records_so_far"].is_number()
                && st["status"]["files_so_far"].is_number()
                && st["status"]["directories_so_far"].is_number()
                && st["status"]["phase"].is_string()
                && st["status"]["elapsed_ms"].is_number();
            seen_running = fields_ok;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let st = &final_status["status"];
    let completed = st["done"] == json!(true) && st["phase"] == "done" && st["result"].is_object();
    check.run(
        "background scan completes",
        completed,
        format!("status={final_status}"),
    );
    check.run(
        "status exposes progress fields",
        seen_running,
        format!("progress fields observed while running: {final_status}"),
    );
    let dirs = st["directories_so_far"].as_u64().unwrap_or(0);
    check.run(
        "directory progress is nonzero",
        dirs > 0,
        format!("directories_so_far={dirs}"),
    );
    let scanner = st["result"]["scanner"].as_str().unwrap_or("").to_string();
    check.run(
        "completed result reports scanner + age",
        (scanner == "recursive_fallback" || scanner == "ntfs_mft_fast")
            && st["result"]["scanned_at"].is_string()
            && st["result"]["scan_duration_ms"].is_number(),
        format!(
            "scanner={scanner} scanned_at={}",
            st["result"]["scanned_at"]
        ),
    );

    // 8. A completed scan can be followed by another scan for the same scope.
    let start2 = server.tools_call("disk_scan_start", json!({ "path": cancel_scope }));
    let scan2 = start2["result"]["content"][0]["text"]
        .as_str()
        .map(|t| serde_json::from_str::<Value>(t).unwrap())
        .and_then(|s| s["status"]["scan_id"].as_str().map(|s| s.to_string()))
        .unwrap_or_default();
    let different = !scan2.is_empty() && scan2 != scan_id;
    check.run(
        "a new scan starts after completion",
        different,
        format!("first={scan_id} second={scan2}"),
    );
    let mut done2 = false;
    for _ in 0..2000 {
        let st2 = poll_status(&mut server, &scan2);
        if st2["status"]["done"] == json!(true) {
            done2 = st2["status"]["phase"] == "done";
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    check.run("second scan completes", done2, "scan2 done".to_string());

    // 9. Cancellation: cancel mid-walk on the larger tree.
    let start3 = server.tools_call("disk_scan_start", json!({ "path": cancel_scope }));
    let scan3 = start3["result"]["content"][0]["text"]
        .as_str()
        .map(|t| serde_json::from_str::<Value>(t).unwrap())
        .and_then(|s| s["status"]["scan_id"].as_str().map(|s| s.to_string()))
        .expect("third scan id");
    // Wait until the walk is mid-flight, then cancel.
    let mut observed = false;
    for _ in 0..2000 {
        let st3 = poll_status(&mut server, &scan3);
        if st3["status"]["records_so_far"].as_u64().unwrap_or(0) > 0 {
            observed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let cancel_reply = server.tools_call("disk_scan_cancel", json!({ "scan_id": scan3 }));
    let cancel_ok = cancel_reply.get("error").is_none()
        && cancel_reply["result"]["content"][0]["text"]
            .as_str()
            .map(|t| t.contains("\"cancelled\":true"))
            .unwrap_or(false);
    check.run(
        "cancellation acknowledged",
        observed && cancel_ok,
        format!("observed_mid_walk={observed} cancel={cancel_reply}"),
    );
    let mut cancelled = false;
    let mut cancel_terminal = json!(null);
    for _ in 0..2000 {
        let st3 = poll_status(&mut server, &scan3);
        cancel_terminal = st3.clone();
        if st3["status"]["done"] == json!(true) {
            cancelled =
                st3["status"]["phase"] == "cancelled" && st3["status"]["cancelled"] == json!(true);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    check.run(
        "cancelled scan terminates as cancelled",
        cancelled,
        format!("phase={}", cancel_terminal["status"]["phase"]),
    );

    // 10. Outputs stay bounded and carry no source files / secrets / env
    // blocks. Only the disk-tool *result payloads* are scanned: the
    // tools/list metadata legitimately contains words like "secret" in the
    // workspace_snapshot description, but a scan result must never.
    let mut disk_outputs = String::new();
    let mut total_len = 0usize;
    for t in DISK_TOOLS {
        let r = server.tools_call(t, json!({ "path": scope }));
        let s = format!("{r}");
        total_len += s.len();
        assert!(
            s.len() < 200_000,
            "tool {t} response too large: {} bytes",
            s.len()
        );
        // Only the payload text is checked for leaks, not the JSON-RPC shell.
        if let Some(text) = r["result"]["content"][0]["text"].as_str() {
            disk_outputs.push_str(text);
        }
    }
    let lower = disk_outputs.to_ascii_lowercase();
    let leaked: Vec<&str> = FORBIDDEN
        .iter()
        .copied()
        .filter(|w| lower.contains(&w.to_ascii_lowercase()))
        .collect();
    check.run(
        "no source files / secrets / credentials / env blocks",
        leaked.is_empty(),
        if leaked.is_empty() {
            format!("no forbidden content across {total_len} bytes of disk-tool output")
        } else {
            format!("leaked terms: {leaked:?}")
        },
    );
    check.run(
        "outputs stay bounded",
        total_len < 2_000_000,
        format!("total captured ~{total_len} bytes"),
    );
    let stderr_count = server.stderr.lock().unwrap().len();
    check.run(
        "stdout stayed protocol-clean",
        true,
        format!("all frames parsed as JSON-RPC (any log line would have broken framing); {stderr_count} log lines went to stderr as intended"),
    );

    // 11. The status of the completed scan stays readable afterwards.
    let again = server.tools_call("disk_scan_status", json!({ "scan_id": scan_id }));
    let readable = again.get("error").is_none()
        && again["result"]["content"][0]["text"]
            .as_str()
            .map(|t| t.contains("\"phase\":\"done\""))
            .unwrap_or(false);
    check.run(
        "completed status stays readable",
        readable,
        format!("{again}"),
    );

    // Cleanup: `Server`'s Drop kills and reaps this test's own child (never
    // any unrelated process); then remove the scan trees.
    drop(server);
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&cancel_root).ok();

    let failed: Vec<&(String, bool, String)> = results.iter().filter(|(_, ok, _)| !ok).collect();
    eprintln!(
        "[e2e] SUMMARY: {} passed, {} failed",
        results.len() - failed.len(),
        failed.len()
    );
    if !failed.is_empty() {
        for (name, _, detail) in &failed {
            eprintln!("[e2e]   FAILED: {name} — {detail}");
        }
        panic!("{} e2e check(s) failed", failed.len());
    }
}
