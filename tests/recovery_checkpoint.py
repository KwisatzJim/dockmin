#!/usr/bin/env python3
"""Exercise embedded rollback code with temporary files and simulated Docker."""
import hashlib
import json
import os
from pathlib import Path
import pathlib
import re
import shutil
import tempfile
import time
from types import SimpleNamespace
import unittest

ROOT = Path(__file__).resolve().parents[1]

def embedded(file, name):
    text = (ROOT / 'src' / file).read_text()
    return re.search(r'(?:pub )?const ' + name + r': &str = r#"(.*?)"#;', text, re.S)[1]

class RollbackCheckpointTests(unittest.TestCase):
    def exercise(self, fail_health=False):
        with tempfile.TemporaryDirectory(prefix='dockmin-rollback-test-') as folder:
            root = Path(folder)
            source = root / 'compose.yaml'
            source.write_text('configuration B')
            stack = dict(id='s', host_id='h', directory=folder,
                         project='test', files=['compose.yaml'])
            env = dict(os=os, json=json, pathlib=pathlib, hashlib=hashlib,
                       tempfile=tempfile, shutil=shutil, time=time,
                       data={'stack': stack})
            exec(embedded('remote.rs', 'COMMON'), env)
            exec(embedded('recovery.rs', 'FUNCTIONS'), env)
            env['remember_good_files'](overwrite=True)
            saved = root / 'saved-A'
            saved.write_text('configuration A')
            def compose(args, *unused, **kwargs):
                if fail_health and args[0] == 'up':
                    raise RuntimeError('Simulated health failure')
                return SimpleNamespace(stdout='')
            env['compose'] = compose
            env['ensure_checkpoint_images'] = lambda point: None
            point = dict(files=[dict(backup=str(saved), path=str(source),
                         sha256=env['digest'](saved.read_bytes()))],
                         images={}, running=['web'])
            if fail_health:
                with self.assertRaisesRegex(RuntimeError, 'health failure'):
                    env['restore_checkpoint'](point)
            else:
                env['restore_checkpoint'](point)
            env['checkpoint_root'] = lambda: root / 'points'
            next_point = env['save_checkpoint']('next', require_containers=False)
            self.assertEqual(source.read_text(), 'configuration A')
            self.assertEqual(Path(next_point['files'][0]['backup']).read_text(),
                             'configuration B' if fail_health else 'configuration A')

    def test_successful_rollback_updates_next_checkpoint(self):
        self.exercise()

    def test_failed_rollback_does_not_mark_configuration_good(self):
        self.exercise(fail_health=True)

if __name__ == '__main__':
    unittest.main()
