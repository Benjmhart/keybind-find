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
    assert.deepStrictEqual(fs.readdirSync(run), []);
    process.env.XDG_RUNTIME_DIR = run;
    // The language changed to rust: the reopened document is taken; another document's opening isn't.
    write(vscode, { languageId: 'rust', uri: uri('/a.rs') });
    const dir = path.join(run, 'keybind-find', 'vscode');
    assert.strictEqual(fs.statSync(dir).mode & 0o777, 0o700);
    assert.deepStrictEqual(fs.readdirSync(dir), [`${process.pid}.json`]);
    const file = path.join(dir, `${process.pid}.json`);
    const read = () => JSON.parse(fs.readFileSync(file, 'utf8'));
    assert.strictEqual(read().facts.filetype, 'rust');
    assert.strictEqual(read().appName, 'Visual Studio Code');
    write(vscode, { languageId: 'go', uri: uri('/b.go') });
    assert.strictEqual(read().facts.filetype, 'plaintext');
    deactivate();
    assert.deepStrictEqual(fs.readdirSync(dir), []);
  } finally {
    if (saved === undefined) delete process.env.XDG_RUNTIME_DIR;
    else process.env.XDG_RUNTIME_DIR = saved;
    fs.rmSync(run, { recursive: true, force: true });
  }
});
