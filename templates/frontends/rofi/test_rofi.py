#!/usr/bin/env python3
"""Feeds a sample request through keybind-find-rofi with a stub `rofi` on PATH. Run: python3 test_rofi.py"""

import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
FRONTEND = os.path.join(HERE, "keybind-find-rofi")

REQUEST = {
    "version": 1,
    "context": {"wm": "xmonad", "class": "Alacritty", "title": "t", "program": "nvim", "facts": {}},
    "entries": [
        {"id": 10, "key": "super+b", "desc": "Launch the browser", "program": "xmonad", "when": {}, "tags": []},
        {"id": 11, "key": "space f f", "desc": "Find <files> & more", "program": "nvim",
         "when": {"mode": "normal"}, "tags": ["[mode=normal]"]},
        {"id": 12, "key": "ctrl+b c", "desc": "new tab", "program": "tmux", "when": {}, "tags": []},
    ],
}


class Rofi(unittest.TestCase):
    def run_with(self, stub_body):
        with tempfile.TemporaryDirectory() as d:
            stub = os.path.join(d, "rofi")
            with open(stub, "w") as f:
                f.write(f'#!/bin/sh\nprintf "%s\\n" "$@" > "{d}/args"\ncat > "{d}/rows"\n{stub_body}\n')
            os.chmod(stub, 0o755)
            env = dict(os.environ, PATH=d + os.pathsep + os.environ.get("PATH", ""))
            out = subprocess.run([sys.executable, FRONTEND], input=json.dumps(REQUEST), capture_output=True,
                                 text=True, env=env)
            with open(f"{d}/args") as a, open(f"{d}/rows") as r:
                return out, a.read().split("\n"), r.read().split("\n")

    def test_prints_the_chosen_id(self):
        out, args, rows = self.run_with("echo 1")
        # Row 1's id, not the row number.
        self.assertEqual((out.returncode, out.stdout), (0, "11\n"))
        self.assertEqual(args[:8], ["-dmenu", "-i", "-format", "i", "-markup-rows", "-no-custom", "-p", "keys"])
        # Columns line up; markup is escaped; tags are dimmed.
        self.assertEqual(rows[0], "super+b    Launch the browser   xmonad")
        self.assertEqual(rows[1], "space f f  Find &lt;files&gt; &amp; more  nvim  <span alpha='50%'>[mode=normal]</span>")

    def test_escape_or_no_row_is_a_cancel(self):
        for body in ["exit 1", "echo -1", "echo 9", "true"]:
            out, _, _ = self.run_with(body)
            self.assertNotEqual(out.returncode, 0, body)
            self.assertEqual((out.stdout, out.stderr), ("", ""), body)

    def test_rofi_errors_reach_stderr_but_escape_stays_quiet(self):
        out, _, _ = self.run_with("echo 'cannot load theme' >&2; exit 2")
        self.assertEqual((out.returncode, out.stdout, out.stderr), (2, "", "cannot load theme\n"))
        # Escape (exit 1) with a warning: quiet.
        out, _, _ = self.run_with("echo 'deprecated option' >&2; exit 1")
        self.assertEqual((out.returncode, out.stdout, out.stderr), (1, "", ""))
        # Killed by a signal: said, not lost in an exit code.
        out, _, _ = self.run_with("kill -9 $$")
        self.assertEqual(out.returncode, 1)
        self.assertIn("killed by signal 9", out.stderr)

    def test_no_rofi_or_another_version_says_so(self):
        with tempfile.TemporaryDirectory() as d:
            env = dict(os.environ, PATH=d)
            out = subprocess.run([sys.executable, FRONTEND], input=json.dumps(REQUEST), capture_output=True,
                                 text=True, env=env)
            self.assertEqual((out.returncode, out.stderr), (1, "keybind-find-rofi: rofi is not installed\n"))
            out = subprocess.run([sys.executable, FRONTEND], input=json.dumps(dict(REQUEST, version=2)),
                                 capture_output=True, text=True, env=env)
            self.assertIn("request version 2 is not 1", out.stderr)


if __name__ == "__main__":
    unittest.main()
