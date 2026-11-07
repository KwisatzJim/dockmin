"""Exercise the actual embedded deployment script with simulated Docker results."""
import contextlib
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

SCRIPT = (Path(__file__).resolve().parents[1] / 'src/recovery.rs').read_text().split('const UPDATE: &str = r#"', 1)[1].split('"#;', 1)[0]


class UpdateImages(unittest.TestCase):
    def run_update(self, actual='old', pull=True, recreate_works=True, recreate_fails=False, auto_rollback=False):
        calls, remembered, restored = [], [], []
        containers = [
            {'Config': {'Labels': {'com.docker.compose.service': service}}, 'Image': image}
            for service, image in [('db', actual), ('app', 'current-app')]
        ]

        def compose(args, files=None):
            calls.append(args)
            output = ''
            if args == ['config', '--format', 'json']:
                output = json.dumps({'services': {'db': {'image': 'postgres:14'}, 'app': {'image': 'app:latest'}}})
            if args == ['ps', '-q']:
                output = 'db-container app-container'
            if '--force-recreate' in args:
                if recreate_fails:
                    raise RuntimeError('health check failed')
                if recreate_works:
                    containers[0]['Image'] = 'current-db'
            return SimpleNamespace(stdout=output)

        def execute(args):
            if args[1] == 'inspect':
                output = containers
            else:
                output = [{'Id': 'current-db' if args[-1] == 'postgres:14' else 'current-app'}]
            return SimpleNamespace(stdout=json.dumps(output))

        with tempfile.TemporaryDirectory() as directory:
            context = dict(json=json, pathlib=__import__('pathlib'), s={'directory': directory},
                           data={'snapshot': 'test', 'pull': pull, 'health_timeout': 120, 'auto_rollback': auto_rollback},
                           compose=compose, execute=execute, sources=lambda: [],
                           save_checkpoint=lambda *a, **kw: {'images': {'db': {}}},
                           public_checkpoint=lambda p: p, remember_good_files=lambda **kw: remembered.append(True),
                           restore_checkpoint=lambda p: restored.append(True))
            with contextlib.redirect_stdout(io.StringIO()):
                try:
                    exec(SCRIPT, context)
                except RuntimeError as error:
                    return calls, remembered, restored, str(error)
        return calls, remembered, restored, None

    def test_recreates_only_stale_service(self):
        calls, remembered, _, error = self.run_update()
        recreate = [c for c in calls if '--force-recreate' in c]
        self.assertEqual(len(recreate), 1)
        self.assertEqual(recreate[0][-1], 'db')
        self.assertIn('--no-deps', recreate[0])
        self.assertTrue(remembered)
        self.assertIsNone(error)

    def test_current_images_are_not_recreated(self):
        calls, _, _, error = self.run_update(actual='current-db')
        self.assertFalse(any('--force-recreate' in c for c in calls))
        self.assertIsNone(error)

    def test_configuration_deploy_does_not_apply_new_images(self):
        calls, _, _, error = self.run_update(pull=False)
        self.assertNotIn(['config', '--format', 'json'], calls)
        self.assertFalse(any('--force-recreate' in c for c in calls))
        self.assertIsNone(error)

    def test_failed_replacement_does_not_report_success(self):
        _, remembered, _, error = self.run_update(recreate_works=False)
        self.assertFalse(remembered)
        self.assertIn('still use older images', error)

    def test_replacement_failure_preserves_automatic_rollback(self):
        _, remembered, restored, error = self.run_update(recreate_fails=True, auto_rollback=True)
        self.assertFalse(remembered)
        self.assertTrue(restored)
        self.assertEqual(error, 'health check failed')


if __name__ == '__main__':
    unittest.main()
