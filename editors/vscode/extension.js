// Writes the focused window's language to `$XDG_RUNTIME_DIR/keybind-find/vscode/<pid>.json` for keybind-find's
// VS Code sheet. No network, no programs; nothing is read beyond the VS Code API and `XDG_RUNTIME_DIR`.
'use strict';
const fs = require('fs');
const path = require('path');

/**
 * The state file's object, from the window's values; `document` is the active text editor's, or undefined. Only what
 * the API shows exactly: the language (`editorLangId`) and the file's extension. `resourceScheme` and the other
 * `resource*` keys follow whichever view has focus, which no API reports, so they are left out.
 */
function state({ pid, focused, time, appName, document }) {
  const facts = {};
  if (document) {
    facts.filetype = document.languageId;
    const ext = path.posix.extname(document.uri.path);
    // As keybind-find's title guess: 1 to 16 characters.
    if (ext.length > 1 && ext.length <= 17) facts.extension = ext.slice(1);
  }
  return { version: 1, pid, focused, time, appName, facts };
}

let file;

/**
 * Writes the window's state file. `opened` is a document just (re)opened: a language change reopens the document, and
 * the event comes before the active editor holds the new one, so that one is taken when it is the active editor's.
 */
function write(vscode, opened) {
  const run = process.env.XDG_RUNTIME_DIR;
  if (!run) return;
  const editor = vscode.window.activeTextEditor;
  let document = editor && editor.document;
  if (opened) {
    // Documents open in the background all the time (git, language servers); only the active one matters.
    if (!document || opened.uri.toString() !== document.uri.toString()) return;
    document = opened;
  }
  const dir = path.join(run, 'keybind-find', 'vscode');
  file = path.join(dir, `${process.pid}.json`);
  const s = state({
    pid: process.pid,
    focused: vscode.window.state.focused,
    time: Date.now(),
    appName: vscode.env.appName,
    document,
  });
  try {
    fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
    fs.writeFileSync(`${file}.tmp`, JSON.stringify(s), { mode: 0o600 });
    fs.renameSync(`${file}.tmp`, file);
  } catch {
    // Nowhere to report it; keybind-find falls back to the window title.
  }
}

function activate(context) {
  const vscode = require('vscode');
  const update = () => write(vscode);
  context.subscriptions.push(
    vscode.window.onDidChangeWindowState(update),
    vscode.window.onDidChangeActiveTextEditor(update),
    // A language change closes and reopens the document.
    vscode.workspace.onDidOpenTextDocument((d) => write(vscode, d)),
  );
  update();
}

function deactivate() {
  try {
    if (file) fs.unlinkSync(file);
  } catch {
    // Already gone.
  }
}

module.exports = { activate, deactivate, state, write };
