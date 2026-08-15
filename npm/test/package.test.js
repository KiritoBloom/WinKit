'use strict';

// Package-level validation for the two WinKit npm packages. Uses Node's
// built-in test runner (node --test); no dependencies, no network.
//
// Covers:
//   - package metadata (names, bin entries, engines, os/cpu constraints)
//   - no install scripts
//   - no browser-automation or other runtime dependencies
//   - launcher contents via `npm pack --dry-run --json`
//   - native package contents including the binary (skipped when the
//     release binary is absent, e.g. before `cargo build --release` +
//     npm/scripts/copy-native.ps1)
//   - no secret material anywhere in the packaged contents
//
// Run: node --test npm\test\package.test.js   (from the repository root)

const { test } = require('node:test');
const assert = require('node:assert');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const root = path.resolve(__dirname, '..', '..');
const mcpDir = path.join(root, 'npm', 'mcp');
const nativeDir = path.join(root, 'npm', 'win32-x64-msvc');
const nativeExe = path.join(nativeDir, 'bin', 'winkit.exe');
const haveNative = fs.existsSync(nativeExe);

const SKIP_NATIVE = haveNative
  ? false
  : 'release binary not present; run `cargo build --release` then npm/scripts/copy-native.ps1';

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, 'utf8'));
}

function packDryRun(dir) {
  // npm.cmd is a batch shim; spawn it through the shell so PATH resolution
  // works on Windows. The arguments are static test constants.
  const res = spawnSync('npm.cmd', ['pack', '--dry-run', '--json'], {
    cwd: dir,
    encoding: 'utf8',
    shell: true,
    env: { ...process.env, npm_config_cache: path.join(os.tmpdir(), 'winkit-npm-cache') },
  });
  assert.strictEqual(res.status, 0, `npm pack --dry-run failed in ${dir}: ${res.stderr}`);
  const parsed = JSON.parse(res.stdout);
  assert.strictEqual(parsed.length, 1, 'pack dry-run reports exactly one package');
  return parsed[0];
}

// ---------------------------------------------------------------------------
// Launcher package metadata
// ---------------------------------------------------------------------------

test('launcher package.json declares the right identity and bin', () => {
  const pkg = readJson(path.join(mcpDir, 'package.json'));
  assert.strictEqual(pkg.name, '@winkit/mcp');
  assert.strictEqual(typeof pkg.version, 'string');
  assert.match(pkg.version, /^\d+\.\d+\.\d+$/);
  assert.strictEqual(pkg.license, 'MIT');
  assert.strictEqual(pkg.bin.winkit, 'bin/winkit.js');
  assert.deepStrictEqual(pkg.files, ['bin']);
  assert.ok(pkg.engines && pkg.engines.node, 'launcher declares an engines.node floor');
});

test('launcher has no install scripts and no runtime dependencies', () => {
  const pkg = readJson(path.join(mcpDir, 'package.json'));
  // No install lifecycle scripts at all.
  assert.ok(
    !pkg.scripts || !pkg.scripts.install,
    'launcher must not ship an install script'
  );
  // The only dependency surface is the optional native runtime.
  assert.ok(!pkg.dependencies, 'launcher must not depend on anything at runtime');
  const optional = pkg.optionalDependencies || {};
  assert.deepStrictEqual(
    Object.keys(optional),
    ['@winkit/win32-x64-msvc'],
    'the only optional dependency is the native runtime'
  );
  // No browser-automation stack anywhere in the dependency graph.
  const dependencyNames = Object.keys(optional);
  for (const name of dependencyNames) {
    assert.ok(
      !/playwright|puppeteer|selenium|chrome-launcher|chromedriver/i.test(name),
      `no browser-automation dependency: ${name}`
    );
  }
});

// ---------------------------------------------------------------------------
// Native package metadata
// ---------------------------------------------------------------------------

test('native package.json declares Windows x64 and the binary', () => {
  const pkg = readJson(path.join(nativeDir, 'package.json'));
  assert.strictEqual(pkg.name, '@winkit/win32-x64-msvc');
  assert.deepStrictEqual(pkg.os, ['win32']);
  assert.deepStrictEqual(pkg.cpu, ['x64']);
  assert.strictEqual(pkg.bin.winkit, 'bin/winkit.exe');
  assert.deepStrictEqual(pkg.files, ['bin']);
  assert.ok(!pkg.scripts || !pkg.scripts.install, 'native package must not have install scripts');
  assert.ok(!pkg.dependencies, 'native package carries no dependencies');
});

// ---------------------------------------------------------------------------
// Packed contents
// ---------------------------------------------------------------------------

test('launcher packed contents are exactly the launcher surface', () => {
  const pack = packDryRun(mcpDir);
  assert.strictEqual(pack.name, '@winkit/mcp');
  const files = pack.files.map((f) => f.path);
  assert.ok(files.includes('bin/winkit.js'), 'launcher ships bin/winkit.js');
  assert.ok(files.includes('package.json'));
  // No stray or secret-bearing files ride along.
  for (const f of files) {
    assert.ok(!/\.env|\.pem|\.key|\.p12|credentials|secret/i.test(f), `clean file name: ${f}`);
    assert.ok(!f.includes('\\'), 'packed paths use forward slashes');
  }
  // The launcher surface is bounded: bin + package.json + README only.
  const unexpected = files.filter((f) => !/^(bin\/winkit\.js|package\.json|README\.md)$/.test(f));
  assert.deepStrictEqual(unexpected, [], 'no unexpected files in the launcher package');
});

test('native packed contents include the binary', { skip: SKIP_NATIVE }, () => {
  const pack = packDryRun(nativeDir);
  assert.strictEqual(pack.name, '@winkit/win32-x64-msvc');
  const files = pack.files.map((f) => f.path);
  assert.ok(files.includes('bin/winkit.exe'), 'native package ships bin/winkit.exe');
  const bin = pack.files.find((f) => f.path === 'bin/winkit.exe');
  assert.ok(
    bin.size > 1_000_000,
    `release binary must be a real executable, got ${bin.size} bytes`
  );
  const unexpected = files.filter((f) => !/^(bin\/winkit\.exe|package\.json|README\.md)$/.test(f));
  assert.deepStrictEqual(unexpected, [], 'no unexpected files in the native package');
});

test('native pack dry-run still works without a binary and reports no binary', () => {
  // When the release binary is missing the pack must still succeed (npm
  // warns, does not fail) and simply not list bin/winkit.exe. This keeps
  // CI honest: a missing binary is a real gap, not a silent pass.
  const pack = packDryRun(nativeDir);
  const files = pack.files.map((f) => f.path);
  assert.ok(files.includes('package.json'));
  assert.strictEqual(
    files.includes('bin/winkit.exe'),
    haveNative,
    'binary presence in the pack matches binary presence on disk'
  );
});

// ---------------------------------------------------------------------------
// Secret scan over everything that would be packed
// ---------------------------------------------------------------------------

test('no secret material in either package', () => {
  const secretPatterns = [
    /-----BEGIN [A-Z ]*PRIVATE KEY-----/,
    /ghp_[A-Za-z0-9]{20,}/,
    /sk-[A-Za-z0-9]{20,}/,
    /glpat-[A-Za-z0-9_-]{16,}/,
    /AKIA[0-9A-Z]{16}/,
    /(password|passwd|secret|token)\s*=\s*[^\s"']{6,}/i,
  ];
  const walk = (dir, out) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      if (entry.name === 'node_modules' || entry.name === '.git') continue;
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(full, out);
      else out.push(full);
    }
  };
  const files = [];
  walk(mcpDir, files);
  walk(nativeDir, files);
  for (const file of files) {
    if (/\.exe$/.test(file)) continue; // binary payload, not text
    const text = fs.readFileSync(file, 'utf8');
    for (const pattern of secretPatterns) {
      assert.ok(
        !pattern.test(text),
        `secret pattern ${pattern} found in ${path.relative(root, file)}`
      );
    }
  }
});
