'use strict';

// Launcher tests for npm/mcp/bin/winkit.js using Node's built-in test
// runner (node --test). No dependencies; never touches the network.
//
// Tests that need the native binary use the real release build when it is
// present (npm/win32-x64-msvc/bin/winkit.exe, produced by
// npm/scripts/copy-native.ps1 after `cargo build --release`) and skip
// cleanly otherwise. Everything is driven through the WINKIT_NATIVE_PATH
// env override so no installation is required.

const { test } = require('node:test');
const assert = require('node:assert');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const launcher = path.resolve(__dirname, '..', 'mcp', 'bin', 'winkit.js');
const nativeExe = path.resolve(__dirname, '..', 'win32-x64-msvc', 'bin', 'winkit.exe');
const haveNative = fs.existsSync(nativeExe);

const SKIP_NATIVE = haveNative
  ? false
  : 'release binary not present; run `cargo build --release` then npm/scripts/copy-native.ps1';

function runWithInput(args, input, env) {
  return spawnSync(process.execPath, [launcher, ...args], {
    input,
    encoding: 'utf8',
    env: { ...process.env, ...env },
  });
}

test('launcher module exposes helpers and a Windows-x64-only message', () => {
  const mod = require(launcher);
  assert.strictEqual(typeof mod.resolveNativeBinary, 'function');
  assert.strictEqual(typeof mod.launch, 'function');
  assert.strictEqual(mod.isSupportedPlatform('win32'), true);
  assert.strictEqual(mod.isSupportedPlatform('darwin'), false);
  assert.strictEqual(mod.isSupportedPlatform('linux'), false);
  assert.match(mod.WINDOWS_X64_MESSAGE, /Windows x64/);
  assert.match(mod.WINDOWS_X64_MESSAGE, /build from source/);
});

test('unsupported platform prints an actionable error and exits non-zero', () => {
  const helper = path.resolve(__dirname, '..', 'scripts', 'force-platform-helper.js');
  const res = spawnSync(process.execPath, [helper], { encoding: 'utf8' });
  assert.notStrictEqual(res.status, 0);
  assert.match(res.stderr, /Windows x64/);
  assert.match(res.stderr, /build from source/);
});

test('missing binary prints an actionable error and exits non-zero', () => {
  const missing = path.join(
    os.tmpdir(),
    `winkit-missing-${process.pid}-${Date.now()}.exe`
  );
  const res = runWithInput(['--version'], undefined, { WINKIT_NATIVE_PATH: missing });
  assert.notStrictEqual(res.status, 0);
  assert.match(res.stderr, /Windows x64|native|Failed to launch/i);
});

test('--version propagates output and exit code 0', { skip: SKIP_NATIVE }, () => {
  const res = runWithInput(['--version'], undefined, { WINKIT_NATIVE_PATH: nativeExe });
  assert.strictEqual(res.status, 0);
  assert.match(res.stdout, /^winkit \d+\.\d+\.\d+/m);
});

test('--help exits 0', { skip: SKIP_NATIVE }, () => {
  const res = runWithInput(['--help'], undefined, { WINKIT_NATIVE_PATH: nativeExe });
  assert.strictEqual(res.status, 0);
  assert.match(res.stdout, /WinKit/);
});

test('exit code propagates from a failing doctor', { skip: SKIP_NATIVE }, () => {
  const missingConfig = path.join(
    os.tmpdir(),
    `winkit-doctor-${process.pid}-${Date.now()}.toml`
  );
  const res = runWithInput(
    ['doctor', '--json', '--config', missingConfig],
    undefined,
    { WINKIT_NATIVE_PATH: nativeExe }
  );
  assert.notStrictEqual(res.status, 0, 'doctor with a missing config must fail');
  const report = JSON.parse(res.stdout);
  assert.strictEqual(report.ok, false);
  assert.ok(report.failed_checks.includes('config'));
});

test('launcher resolves the local native package without env override', { skip: SKIP_NATIVE }, () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'winkit-resolve-'));
  try {
    const pkgMcp = path.join(root, 'node_modules', '@winkit', 'mcp');
    const pkgNative = path.join(root, 'node_modules', '@winkit', 'win32-x64-msvc');
    fs.mkdirSync(path.join(pkgMcp, 'bin'), { recursive: true });
    fs.mkdirSync(path.join(pkgNative, 'bin'), { recursive: true });
    fs.copyFileSync(launcher, path.join(pkgMcp, 'bin', 'winkit.js'));
    fs.copyFileSync(nativeExe, path.join(pkgNative, 'bin', 'winkit.exe'));
    fs.writeFileSync(
      path.join(pkgNative, 'package.json'),
      JSON.stringify({ name: '@winkit/win32-x64-msvc', version: '0.1.0' })
    );

    const res = spawnSync(process.execPath, [path.join(pkgMcp, 'bin', 'winkit.js'), '--version'], {
      encoding: 'utf8',
      env: { ...process.env, WINKIT_NATIVE_PATH: '' },
    });
    assert.strictEqual(res.status, 0);
    assert.match(res.stdout, /^winkit \d+\.\d+\.\d+/m);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test('MCP handshake over stdio is protocol-clean', { skip: SKIP_NATIVE }, () => {
  const initialize = JSON.stringify({
    jsonrpc: '2.0',
    id: 1,
    method: 'initialize',
    params: {
      protocolVersion: '2024-11-05',
      capabilities: {},
      clientInfo: { name: 'winkit-test', version: '0.0.0' },
    },
  });
  const input = [initialize, '{"jsonrpc":"2.0","method":"exit"}'].join('\n') + '\n';
  const res = runWithInput([], input, { WINKIT_NATIVE_PATH: nativeExe });

  assert.strictEqual(res.status, 0);
  const lines = res.stdout
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean);
  assert.ok(lines.length >= 1, 'initialize must produce a reply');

  for (const line of lines) {
    assert.doesNotThrow(() => JSON.parse(line), `stdout must contain only JSON frames, got: ${line}`);
  }

  const reply = JSON.parse(lines[0]);
  assert.strictEqual(reply.id, 1);
  assert.strictEqual(reply.result.serverInfo.name, 'winkit');
  assert.strictEqual(reply.result.protocolVersion, '2024-11-05');
});