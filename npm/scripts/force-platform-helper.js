'use strict';

// Forces the unsupported-platform branch of the launcher by overriding
// process.platform before requiring it. Lives outside npm/test so Node's
// test runner does not auto-discover it. Node lets tests redefine these
// process properties; production runs never touch this file.

Object.defineProperty(process, 'platform', { value: 'linux' });

const launcher = require('../mcp/bin/winkit.js');
launcher.main();