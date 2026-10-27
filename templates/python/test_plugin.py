#!/usr/bin/env python3
"""The plugin against its sample: run `python3 test_plugin.py`."""

import json
import os
import subprocess
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
PLUGIN = os.path.join(HERE, "keybind-find-toy")


def run(*args, stdin=""):
    return subprocess.run([sys.executable, PLUGIN, *args], input=stdin, capture_output=True, text=True)


class Plugin(unittest.TestCase):
    def test_the_sample_gives_the_expected_sheet(self):
        with open(os.path.join(HERE, "tests/keys.conf")) as f:
            request = {"version": 1, "context": {}, "inputs": [{"path": "keys.conf", "text": f.read()}]}
        out = run(stdin=json.dumps(request))
        self.assertEqual(out.returncode, 0, out.stderr)
        with open(os.path.join(HERE, "tests/expected.json")) as f:
            self.assertEqual(json.loads(out.stdout), json.load(f))

    def test_manifest(self):
        self.assertEqual(json.loads(run("--manifest").stdout)["name"], "toy")

    def test_another_version_no_inputs_and_a_bad_scope(self):
        out = run(stdin=json.dumps({"version": 2, "inputs": []}))
        self.assertEqual(out.returncode, 1)
        self.assertIn("request version 2 is not 1", out.stderr)
        out = run(stdin=json.dumps({"version": 1, "context": {}}))
        self.assertEqual((out.returncode, json.loads(out.stdout)["binds"]), (0, {"General": []}))
        out = run(stdin=json.dumps({"version": 1, "context": {}, "inputs": None}))
        self.assertEqual((out.returncode, json.loads(out.stdout)["binds"]), (0, {"General": []}))
        for text, want in [
            ("a = b ; mode\n", "a scope is `name=value`"),
            ("a = b ; mode=x,y\n", "a scope's name and value are printable ASCII words without , = [ or ]"),
            ("a = b ; mode=é\n", "a scope's name and value are printable ASCII words without , = [ or ]"),
            ("a = ; mode=insert\n", "no description"),
        ]:
            request = {"version": 1, "context": {}, "inputs": [{"path": "k", "text": text}]}
            self.assertEqual(run(stdin=json.dumps(request)).stderr, f"k: line 1: {want}\n")

    def test_inputs_join_and_errors_name_their_file(self):
        inputs = [{"path": "a", "text": "ctrl+s = Save\n"}, {"path": "b", "text": "ctrl+q = Quit\n"}]
        out = run(stdin=json.dumps({"version": 1, "context": {}, "inputs": inputs}))
        self.assertEqual([b["key"] for b in json.loads(out.stdout)["binds"]["General"]], ["ctrl+s", "ctrl+q"])
        inputs[1]["text"] = "oops\n"
        out = run(stdin=json.dumps({"version": 1, "context": {}, "inputs": inputs}))
        self.assertEqual(out.stderr, "b: line 1: not `key = description`\n")

    def test_a_bad_line_is_one_stderr_line_and_exit_1(self):
        request = {"version": 1, "context": {}, "inputs": [{"path": "k", "text": "ctrl+s = Save\nctrl+q\n"}]}
        out = run(stdin=json.dumps(request))
        self.assertEqual((out.returncode, out.stdout, out.stderr), (1, "", "k: line 2: not `key = description`\n"))


if __name__ == "__main__":
    unittest.main()
