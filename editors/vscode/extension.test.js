'use strict';
const test = require('node:test');
const assert = require('node:assert');
const { state } = require('./extension.js');

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
  assert.strictEqual(state({ ...base, focused: false, document: doc('rust', '/a.rs') }).focused, false);
});
