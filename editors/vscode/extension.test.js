'use strict';
const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { state, write, deactivate } = require('./extension.js');

const base = { pid: 7, focused: true, time: 1, appName: 'VSCodium' };
const doc = (languageId, p, scheme = 'file') => ({ languageId, uri: { scheme, path: p } });

test('language and extension from the active document', () => {
  assert.deepStrictEqual(state({ ...base, document: doc('rust', '/src/main.rs') }), {
    version: 1, pid: 7, focused: true, time: 1, appName: 'VSCodium',
    facts: { filetype: 'rust', extension: 'rs' },
  });
  assert.deepStrictEqual(state({ ...base, document: doc('makefile', '/p/Makefile') }).facts, { filetype: 'makefile' });
  assert.deepStrictEqual(state({ ...base, document: doc('x', '/a.abcdefghijklmnopq') }).facts, { filetype: 'x' });
  assert.deepStrictEqual(state({ ...base, document: doc('shellscript', '/home/u/.bashrc') }).facts, { filetype: 'shellscript' });
  // The scheme isn't reported (it follows the focused view).
  assert.deepStrictEqual(state({ ...base, document: doc('plaintext', 'Untitled-1', 'untitled') }).facts, { filetype: 'plaintext' });
});

test('no editor: no facts', () => {
  assert.deepStrictEqual(state(base).facts, {});
});

test('an unfocused window says so', () => {
  assert.deepStrictEqual(state({ ...base, focused: false, document: doc('rust', '/a.rs') }), {
    ...base, focused: false, version: 1, facts: { filetype: 'rust', extension: 'rs' },
  });
});

test('the file: written in a 0700 directory through a rename, the reopened language, deleted on deactivate', () => {
  const run = fs.mkdtempSync(path.join(os.tmpdir(), 'kbf-vscode-'));
  const uri = (p) => ({ scheme: 'file', path: p, toString: () => `file://${p}` });
  const vscode = {
    window: { state: { focused: true }, activeTextEditor: { document: { languageId: 'plaintext', uri: uri('/a.rs') } } },
    env: { appName: 'Visual Studio Code' },
  };
  const saved = process.env.XDG_RUNTIME_DIR;
  try {
    delete process.env.XDG_RUNTIME_DIR;
    write(vscode);
    process.env.XDG_RUNTIME_DIR = '';
    write(vscode);
    assert.deepStrictEqual(fs.readdirSync(run), []);
    process.env.XDG_RUNTIME_DIR = run;
    // The language changed to rust: the reopened document is taken, through a temporary file and a rename.
    const renames = [];
    const rename = fs.renameSync;
    fs.renameSync = (a, b) => { renames.push([path.basename(a), path.basename(b)]); rename(a, b); };
    try {
      write(vscode, { languageId: 'rust', uri: uri('/a.rs') });
    } finally {
      fs.renameSync = rename;
    }
    assert.deepStrictEqual(renames, [[`${process.pid}.json.tmp`, `${process.pid}.json`]]);
    const dir = path.join(run, 'keybind-find', 'vscode');
    assert.strictEqual(fs.statSync(dir).mode & 0o777, 0o700);
    assert.deepStrictEqual(fs.readdirSync(dir), [`${process.pid}.json`]);
    const file = path.join(dir, `${process.pid}.json`);
    const read = () => JSON.parse(fs.readFileSync(file, 'utf8'));
    assert.strictEqual(read().facts.filetype, 'rust');
    assert.strictEqual(fs.statSync(file).mode & 0o777, 0o600);
    assert.strictEqual(read().appName, 'Visual Studio Code');
    // Another document's opening isn't written.
    write(vscode, { languageId: 'go', uri: uri('/b.go') });
    assert.strictEqual(read().facts.filetype, 'rust');
    deactivate();
    assert.deepStrictEqual(fs.readdirSync(dir), []);
  } finally {
    if (saved === undefined) delete process.env.XDG_RUNTIME_DIR;
    else process.env.XDG_RUNTIME_DIR = saved;
    fs.rmSync(run, { recursive: true, force: true });
  }
});

test('activate: writes at once and on focus, editor and document events', () => {
  const Module = require('node:module');
  const run = fs.mkdtempSync(path.join(os.tmpdir(), 'kbf-vscode-'));
  const handlers = {};
  const on = (name) => (f) => { handlers[name] = f; return { dispose() {} }; };
  const uri = (p) => ({ scheme: 'file', path: p, toString: () => `file://${p}` });
  const fake = {
    window: {
      state: { focused: true },
      activeTextEditor: { document: { languageId: 'plaintext', uri: uri('/a.rs') } },
      onDidChangeWindowState: on('window'),
      onDidChangeActiveTextEditor: on('editor'),
    },
    workspace: { onDidOpenTextDocument: on('open') },
    env: { appName: 'VSCodium' },
  };
  const load = Module._load;
  Module._load = function (request, ...rest) {
    return request === 'vscode' ? fake : load.call(this, request, ...rest);
  };
  const saved = process.env.XDG_RUNTIME_DIR;
  process.env.XDG_RUNTIME_DIR = run;
  try {
    const { activate } = require('./extension.js');
    const context = { subscriptions: [] };
    activate(context);
    assert.strictEqual(context.subscriptions.length, 3);
    const file = path.join(run, 'keybind-find', 'vscode', `${process.pid}.json`);
    const read = () => JSON.parse(fs.readFileSync(file, 'utf8'));
    assert.strictEqual(read().facts.filetype, 'plaintext');
    fake.window.state = { focused: false };
    handlers.window(fake.window.state);
    assert.strictEqual(read().focused, false);
    fake.window.activeTextEditor = { document: { languageId: 'go', uri: uri('/b.go') } };
    handlers.editor(fake.window.activeTextEditor);
    assert.deepStrictEqual(read().facts, { filetype: 'go', extension: 'go' });
    handlers.open({ languageId: 'templ', uri: uri('/b.go') });
    assert.strictEqual(read().facts.filetype, 'templ');
  } finally {
    Module._load = load;
    if (saved === undefined) delete process.env.XDG_RUNTIME_DIR;
    else process.env.XDG_RUNTIME_DIR = saved;
    fs.rmSync(run, { recursive: true, force: true });
  }
});

test('package.json is what the README installs', () => {
  const pkg = require('./package.json');
  assert.strictEqual(pkg.main, './extension.js');
  assert.deepStrictEqual(pkg.activationEvents, ['onStartupFinished']);
  assert.strictEqual(pkg.contributes, undefined);
  assert.ok(pkg.engines.vscode);
  const readme = fs.readFileSync(path.join(__dirname, '..', '..', 'README.md'), 'utf8');
  assert.ok(readme.includes(`extensions/${pkg.publisher}.${pkg.name}-${pkg.version}`));
});
