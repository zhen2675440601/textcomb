import unittest
import json
import subprocess
import tarfile
import tempfile
from pathlib import Path
import prepare

class ReleaseGates(unittest.TestCase):
    def test_versions_reject_mismatch_stable_and_shell_text(self):
        prepare.validate_version('v0.1.0-alpha.1', '0.1.0', '0.1.0')
        for version in ['v0.1.0', 'v0.1.1-beta.1', 'v0.1.0-alpha.01', 'v0.1.0-alpha.1;echo secret']:
            with self.assertRaises(ValueError):
                prepare.validate_version(version, '0.1.0', '0.1.0')
        with self.assertRaises(ValueError):
            prepare.validate_version('v0.1.0-alpha.1', '0.1.0', '0.2.0')

    def test_exact_commit_and_all_security_checks_are_required(self):
        sha = 'a' * 40
        checks = {'check_runs': [dict(id=i, name=name, head_sha=sha, app={'slug':'github-actions'}, status='completed', conclusion='success') for i, name in enumerate(prepare.REQUIRED)]}
        self.assertEqual(len(prepare.validate_checks(checks, sha)), len(prepare.REQUIRED))
        for field, value in [('head_sha','b'*40), ('app',{'slug':'other'}), ('status','queued'), ('conclusion','failure'), ('conclusion','skipped')]:
            broken = {'check_runs':[dict(c) for c in checks['check_runs']]}
            broken['check_runs'][-1][field] = value
            with self.assertRaises(ValueError): prepare.validate_checks(broken, sha)
        with self.assertRaises(ValueError): prepare.validate_checks({'check_runs':checks['check_runs'][:-1]}, sha)

    def test_newest_rerun_controls_gate(self):
        sha = 'a'*40
        checks = {'check_runs':[dict(id=i, name=name, head_sha=sha, app={'slug':'github-actions'}, status='completed', conclusion='success') for i,name in enumerate(prepare.REQUIRED)]}
        checks['check_runs'].append(dict(checks['check_runs'][0], id=99, conclusion='failure'))
        with self.assertRaises(ValueError): prepare.validate_checks(checks, sha)

    def test_clean_checked_source_archive_is_reproducible_and_never_published(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory); root = base / 'repo'; root.mkdir()
            def git(*args):
                return subprocess.run(['git', *args], cwd=root, check=True, capture_output=True).stdout.decode().strip()
            git('init', '-b', 'main'); git('config','user.name','Synthetic test'); git('config','user.email','fixture@example.invalid')
            (root/'Cargo.toml').write_text('[workspace.package]\nversion="0.1.0"\n')
            (root/'web').mkdir(); (root/'web/package.json').write_text('{"version":"0.1.0"}')
            git('add','.'); git('commit','-m','synthetic release fixture')
            sha = git('rev-parse','HEAD'); git('update-ref','refs/remotes/origin/main',sha)
            checks = {'check_runs':[dict(id=i,name=name,head_sha=sha,app={'slug':'github-actions'},status='completed',conclusion='success') for i,name in enumerate(prepare.REQUIRED)]}
            first = prepare.prepare(root,'v0.1.0-alpha.1',sha,checks,base/'one')
            second = prepare.prepare(root,'v0.1.0-alpha.1',sha,checks,base/'two')
            self.assertEqual(first['source_sha256'],second['source_sha256'])
            self.assertFalse(first['published']); self.assertFalse(first['quality_certified'])
            archive = base/'one/textcomb-v0.1.0-alpha.1.tar.gz'
            with tarfile.open(archive) as contents:
                self.assertIn('textcomb-v0.1.0-alpha.1/Cargo.toml',contents.getnames())
                self.assertFalse(any('/.git/' in n for n in contents.getnames()))
            self.assertEqual(json.loads((base/'one/candidate.json').read_text())['commit'],sha)
            (root/'dirty.txt').write_text('synthetic untracked change')
            with self.assertRaises(ValueError): prepare.prepare(root,'v0.1.0-alpha.1',sha,checks,base/'dirty-output')
            self.assertFalse((base/'dirty-output').exists())

if __name__ == '__main__': unittest.main()
