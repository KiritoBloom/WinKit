// Phase 9 user-experience smoke: drives the real winkit.exe over MCP stdio
// with managed Chrome enabled, exercising the default request (no headless),
// an explicit headed request, and an explicit headless request.
'use strict';
const { spawn } = require('child_process');
const http = require('http');
const fs = require('fs');
const os = require('os');
const path = require('path');

let failures = 0;
function assert(cond, label) {
  if (cond) {
    console.log('  ok:', label);
  } else {
    failures += 1;
    console.log('  FAIL:', label);
  }
}
function assertEq(actual, expected, label) {
  assert(actual === expected, `${label} (got ${JSON.stringify(actual)}, want ${JSON.stringify(expected)})`);
}

const server = http.createServer((req, res) => {
  res.writeHead(200, { 'Content-Type': 'text/html' });
  res.end('<html><head><title>Phase9 Page</title></head><body><h1>Phase9 Page</h1><p>phase9-marker</p></body></html>');
});
server.listen(0, '127.0.0.1', () => {
  const port = server.address().port;
  run(port);
});

function run(port) {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'winkit-phase9-'));
  const cfgPath = path.join(tmp, 'winkit.toml');
  fs.writeFileSync(cfgPath, `
[server]
log_level = "warn"
[permissions]
mode = "unrestricted"
[tools]
profile = "full"
[chrome.managed]
enabled = true
startup_timeout_ms = 25000
`);
  const child = spawn('target/debug/winkit.exe', ['--config', cfgPath], {
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  let buf = '';
  let id = 0;
  const pending = new Map();
  child.stdout.on('data', (d) => {
    buf += d.toString('utf8');
    let idx;
    while ((idx = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, idx).trim();
      buf = buf.slice(idx + 1);
      if (!line) continue;
      let msg;
      try {
        msg = JSON.parse(line);
      } catch {
        continue;
      }
      if (msg.id !== undefined && pending.has(msg.id)) {
        const { resolve } = pending.get(msg.id);
        pending.delete(msg.id);
        resolve(msg);
      }
    }
  });
  child.stderr.on('data', () => {});
  const call = (method, params) =>
    new Promise((resolve) => {
      const rid = ++id;
      pending.set(rid, { resolve });
      child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: rid, method, params }) + '\n');
    });
  const notify = (method, params) => {
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method, params }) + '\n');
  };
  const toolCall = async (name, arguments_) => {
    const resp = await call('tools/call', { name, arguments: arguments_ });
    if (resp.error) throw new Error(JSON.stringify(resp.error));
    return JSON.parse(resp.result.content[0].text);
  };

  async function main() {
    const init = await call('initialize', {
      protocolVersion: '2024-11-05',
      capabilities: {},
      clientInfo: { name: 'phase9', version: '1.0.0' },
    });
    assert(!!init.result, 'initialize succeeds');
    notify('notifications/initialized', {});

    const url = `http://127.0.0.1:${port}/`;

    console.log('--- 1. default request (headless not specified) ---');
    const s1 = await toolCall('chrome_start_managed_session', { url, wait_for_ready_ms: 20000 });
    console.log('  headless:', s1.headless, 'window_mode:', s1.window_mode, 'launch_mode:', s1.launch_mode, 'state:', s1.state);
    assertEq(s1.headless, false, 'default request reports headless=false');
    assertEq(s1.window_mode, 'headed', 'default request window_mode=headed');
    assertEq(s1.state, 'ready', 'default request state=ready');
    const sid1 = s1.session_id;
    const summary1 = await toolCall('chrome_get_page_summary', { session_id: sid1, observe_ms: 0 });
    assertEq(summary1.title, 'Phase9 Page', 'default: page summary title');
    const shot1 = await toolCall('chrome_capture_screenshot', { session_id: sid1 });
    assert(shot1.bytes > 0, `default: screenshot non-empty (${shot1.bytes} bytes)`);
    const closed1 = await toolCall('chrome_stop_managed_session', { session_id: sid1 });
    assertEq(closed1.state, 'closed', 'default: stop -> closed');

    console.log('--- 2. explicit headed request (headless:false) ---');
    const s2 = await toolCall('chrome_start_managed_session', { url, headless: false, wait_for_ready_ms: 20000 });
    console.log('  headless:', s2.headless, 'window_mode:', s2.window_mode, 'launch_mode:', s2.launch_mode);
    assertEq(s2.window_mode, 'headed', 'explicit headed window_mode=headed');
    const sid2 = s2.session_id;
    const summary2 = await toolCall('chrome_get_page_summary', { session_id: sid2, observe_ms: 0 });
    assertEq(summary2.title, 'Phase9 Page', 'explicit headed: page summary title');
    await toolCall('chrome_stop_managed_session', { session_id: sid2 });

    console.log('--- 3. explicit headless request (headless:true) ---');
    const s3 = await toolCall('chrome_start_managed_session', { url, headless: true, wait_for_ready_ms: 20000 });
    console.log('  headless:', s3.headless, 'window_mode:', s3.window_mode, 'launch_mode:', s3.launch_mode);
    assertEq(s3.headless, true, 'explicit headless reports headless=true');
    assertEq(s3.window_mode, 'headless', 'explicit headless window_mode=headless');
    const sid3 = s3.session_id;
    const summary3 = await toolCall('chrome_get_page_summary', { session_id: sid3, observe_ms: 0 });
    assertEq(summary3.title, 'Phase9 Page', 'headless: page summary title');
    const shot3 = await toolCall('chrome_capture_screenshot', { session_id: sid3 });
    assert(shot3.bytes > 0, `headless: screenshot non-empty (${shot3.bytes} bytes)`);
    const closed3 = await toolCall('chrome_stop_managed_session', { session_id: sid3 });
    assertEq(closed3.state, 'closed', 'headless: stop -> closed');

    console.log(failures === 0 ? 'PHASE9-ALL-PASS' : `PHASE9-FAIL (${failures} assertion failures)`);
    child.kill();
    server.close();
    fs.rmSync(tmp, { recursive: true, force: true });
    process.exit(failures === 0 ? 0 : 1);
  }

  main().catch((e) => {
    console.error('PHASE9-FAIL', e.message);
    child.kill();
    server.close();
    process.exit(1);
  });
}
