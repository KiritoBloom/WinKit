#!/usr/bin/env node
'use strict';

// WinKit MCP launcher. Node built-ins only — no dependencies, no install
// scripts, no downloads. It resolves the platform-native binary and spawns
// it with the caller's arguments, inheriting stdio so the MCP protocol
// flows straight through. Tests can point at any binary via
// WINKIT_NATIVE_PATH.

const { spawn } = require('node:child_process');

const NATIVE_PACKAGE = '@winkit/win32-x64-msvc/bin/winkit.exe';

const WINDOWS_X64_MESSAGE =
  'WinKit currently ships only for Windows x64. Install the native package ' +
  '(@winkit/win32-x64-msvc) or build from source.';

// WINKIT_NATIVE_PATH overrides the packaged binary so tests can exercise
// the launcher without a real installation. Never set in production.
function resolveNativeBinary() {
  const override = process.env.WINKIT_NATIVE_PATH;
  if (override) {
    return override;
  }
  try {
    return require.resolve(NATIVE_PACKAGE);
  } catch (err) {
    return null;
  }
}

function isSupportedPlatform(platform) {
  return platform === 'win32';
}

function launch(binPath, args, platformOverride) {
  const platform = platformOverride || process.platform;
  if (!isSupportedPlatform(platform)) {
    console.error(WINDOWS_X64_MESSAGE);
    process.exit(1);
  }

  let child;
  try {
    child = spawn(binPath, args, { stdio: 'inherit', shell: false, windowsHide: true });
  } catch (err) {
    console.error(`Failed to launch WinKit native binary: ${err.message}`);
    process.exit(1);
  }

  child.on('error', (err) => {
    console.error(`Failed to launch WinKit native binary at ${binPath}: ${err.message}`);
    console.error(WINDOWS_X64_MESSAGE);
    process.exit(1);
  });

  child.on('exit', (code, signal) => {
    if (signal) {
      process.kill(process.pid, signal);
      return;
    }
    process.exit(code === null ? 0 : code);
  });

  // Forward SIGINT/SIGTERM to the child. On Windows, child.kill() sends the
  // process a termination request; stdio is inherited so the console also
  // delivers Ctrl+C to both processes.
  for (const signal of ['SIGINT', 'SIGTERM']) {
    process.on(signal, () => {
      if (child && !child.killed) {
        child.kill(signal);
      }
    });
  }
}

function main() {
  if (!isSupportedPlatform(process.platform)) {
    console.error(WINDOWS_X64_MESSAGE);
    process.exit(1);
  }
  const binPath = resolveNativeBinary();
  if (!binPath) {
    console.error(WINDOWS_X64_MESSAGE);
    process.exit(1);
  }
  launch(binPath, process.argv.slice(2));
}

if (require.main === module) {
  main();
}

module.exports = {
  WINDOWS_X64_MESSAGE,
  NATIVE_PACKAGE,
  resolveNativeBinary,
  isSupportedPlatform,
  launch,
  main,
};
