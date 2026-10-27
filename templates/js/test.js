// The plugin against its sample: run `node test.js`.
"use strict";
const assert = require("node:assert/strict");
const { execFileSync, spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const plugin = path.join(__dirname, "keybind-find-toy");
const run = (args, input) => spawnSync(process.execPath, [plugin, ...args], { input, encoding: "utf8" });

const request = {
  version: 1,
  context: {},
  inputs: [{ path: "keys.conf", text: fs.readFileSync(path.join(__dirname, "tests/keys.conf"), "utf8") }],
};
const out = run([], JSON.stringify(request));
assert.equal(out.status, 0, out.stderr);
assert.deepEqual(JSON.parse(out.stdout), JSON.parse(fs.readFileSync(path.join(__dirname, "tests/expected.json"))));

assert.equal(JSON.parse(execFileSync(process.execPath, [plugin, "--manifest"], { encoding: "utf8" })).name, "toy");

const bad = run([], JSON.stringify({ version: 1, context: {}, inputs: [{ path: "k", text: "ctrl+s = Save\nctrl+q\n" }] }));
assert.deepEqual([bad.status, bad.stdout, bad.stderr], [1, "", "k: line 2: not `key = description`\n"]);

const v2 = run([], JSON.stringify({ version: 2, inputs: [] }));
assert.equal(v2.status, 1);
assert.match(v2.stderr, /request version 2 is not 1/);
const none = run([], JSON.stringify({ version: 1, context: {} }));
assert.deepEqual([none.status, JSON.parse(none.stdout).binds], [0, { General: [] }]);
const scope = run([], JSON.stringify({ version: 1, context: {}, inputs: [{ path: "k", text: "a = b ; mode\n" }] }));
assert.equal(scope.stderr, "k: line 1: a scope is `name=value`\n");
const comma = run([], JSON.stringify({ version: 1, context: {}, inputs: [{ path: "k", text: "a = b ; mode=x,y\n" }] }));
assert.equal(comma.stderr, "k: line 1: a scope's name and value are printable ASCII words without , = [ or ]\n");
const nodesc = run([], JSON.stringify({ version: 1, context: {}, inputs: [{ path: "k", text: "a = ; mode=insert\n" }] }));
assert.equal(nodesc.stderr, "k: line 1: no description\n");

console.log("ok");
