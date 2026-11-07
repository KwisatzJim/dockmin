#!/usr/bin/env python3
"""Test cleanup selection without a Docker daemon or real containers."""
import contextlib
import io
import json
from pathlib import Path
import re
from types import SimpleNamespace
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = re.search(r'const PRUNE: &str = r#"(.*?)"#;',
                   (ROOT / 'src/containers.rs').read_text(), re.S)[1]

class CleanupTests(unittest.TestCase):
    def run_cleanup(self, rows, protected=(), fail=False):
        removed = []
        def execute(args, check=True):
            if args[:3] == ['docker', 'container', 'ls']:
                return SimpleNamespace(stdout='\n'.join(json.dumps(r) for r in rows))
            self.assertEqual(args[:3], ['docker', 'container', 'rm'])
            self.assertEqual(len(args), 4)  # No force or volume-removal flags.
            self.assertFalse(check)
            removed.append(args[3])
            return SimpleNamespace(returncode=1 if fail else 0, stderr='Container is running')
        with contextlib.redirect_stdout(io.StringIO()):
            exec(SCRIPT, dict(data={'protected': protected}, execute=execute, json=json))
        return removed

    def row(self, ident, name, state='exited'):
        return dict(ID=ident, Names=name, State=state)

    def test_retains_both_checkpoint_sides_and_unrecorded_previous_versions(self):
        rows = [self.row('a'*64, 'original'), self.row('b'*64, 'updated'),
                self.row('c'*64, 'dockmin-prev-app-12345678'),
                self.row('d'*64, 'dockmin-replaced-app-12345678'),
                self.row('e'*64, 'ordinary')]
        self.assertEqual(self.run_cleanup(rows, ['a'*64, 'b'*64]), ['e'*64])

    def test_only_removes_stopped_states(self):
        states = ['created', 'exited', 'dead', 'running', 'paused', 'restarting']
        rows = [self.row(str(i)*64, 'app'+str(i), state) for i, state in enumerate(states)]
        self.assertEqual(self.run_cleanup(rows), ['0'*64, '1'*64, '2'*64])

    def test_empty_host(self):
        self.assertEqual(self.run_cleanup([]), [])

    def test_running_race_reports_failure_without_force(self):
        with self.assertRaisesRegex(RuntimeError, 'Container is running'):
            self.run_cleanup([self.row('a'*64, 'started-after-listing')], fail=True)

if __name__ == '__main__':
    unittest.main()
