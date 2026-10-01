import unittest
import hashlib
import io
import json
import subprocess
import tarfile
import tempfile
from pathlib import Path
from unittest import mock
import prepare

class ReleaseGates(unittest.TestCase):
    def repository(self, directory, files=None):
        root = Path(directory) / 'repo'
        root.mkdir()
        def git(*args, input=None):
            return subprocess.run(['git', *args], cwd=root, input=input, check=True, capture_output=True).stdout.decode('utf-8').strip()
        git('init', '-b', 'main')
        git('config', 'user.name', 'Synthetic test')
        git('config', 'user.email', 'fixture@example.invalid')
        git('config', 'core.autocrlf', 'false')
        git('config', 'core.symlinks', 'false')
        source = {'Cargo.toml': '[workspace.package]\nversion="0.1.0"\n', 'web/package.json': '{"version":"0.1.0"}'}
        source.update(files or {})
        for name, value in source.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(value, encoding='utf-8')
        git('add', '--all')
        git('commit', '-m', 'synthetic release fixture')
        sha = git('rev-parse', 'HEAD')
        git('update-ref', 'refs/remotes/origin/main', sha)
        checks = {'check_runs': [dict(id=i, name=name, head_sha=sha, app={'slug': 'github-actions'}, status='completed', conclusion='success') for i, name in enumerate(prepare.REQUIRED)]}
        return root, sha, checks, git

    def test_versions_reject_mismatch_stable_and_shell_text(self):
        prepare.validate_version('v0.1.0-alpha.1', '0.1.0', '0.1.0')
        for version in ['v0.1.0', 'v0.1.1-beta.1', 'v0.1.0-alpha.01', 'v0.1.0-alpha.1;echo secret', 'v0.1.0-alpha.1\N{ARABIC-INDIC DIGIT TWO}']:
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

    def test_nested_environment_state_and_generated_files_never_produce_a_bundle(self):
        names = ('.env', 'deploy/.env', 'deploy/.ENV.production', '.env.production', '.env.example.production',
                 'deploy/.env.production.example', '资料/.env', 'data/private-body.txt', 'deploy/data/body.txt',
                 'uploads/article.txt', 'tmp/body.txt', 'logs/model.json', 'backups/state.sql', '.sqlx/query.json',
                 'private-evaluation/labels.jsonl', 'nested/private-evaluation/report.json', 'nested/target/app',
                 'web/node_modules/dependency/index.js', 'web/dist/index.html', 'nested/build/generated.js',
                 'nested/__pycache__/module.pyc', 'coverage/result.json', 'archive.dump', 'nested/server.log')
        for name in names:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root, sha, checks, _ = self.repository(directory, {name: 'synthetic confidential fixture'})
                output = Path(directory) / 'candidate'
                with self.assertRaises(ValueError):
                    prepare.prepare(root, 'v0.1.0-alpha.1', sha, checks, output)
                self.assertFalse(output.exists())
                self.assertEqual({path.name for path in Path(directory).iterdir()}, {'repo'})

    def test_examples_and_redistributable_fixtures_are_preserved(self):
        files = {'.env.example': 'SYNTHETIC_EXAMPLE=replace-me\n', '.env.compose.example': 'SYNTHETIC_EXAMPLE=replace-me\n',
                 'deploy/.env.example': 'SYNTHETIC_EXAMPLE=replace-me\n', 'tests/fixtures/sample.txt': 'synthetic article',
                 'tests/fixtures/sample.pdf': 'synthetic PDF placeholder', 'tests/fixtures/sample.docx': 'synthetic DOCX placeholder',
                 'docs/资料/说明.md': 'synthetic Unicode filename fixture'}
        with tempfile.TemporaryDirectory() as directory:
            root, sha, checks, _ = self.repository(directory, files)
            output = Path(directory) / 'candidate'
            manifest = prepare.prepare(root, 'v0.1.0-alpha.1', sha, checks, output)
            archive = output / 'textcomb-v0.1.0-alpha.1.tar.gz'
            with tarfile.open(archive) as contents:
                for name in files:
                    self.assertIn('textcomb-v0.1.0-alpha.1/' + name, contents.getnames())
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            self.assertEqual(manifest['source_sha256'], digest)
            self.assertEqual((output / 'SHA256SUMS').read_text(), digest + '  ' + archive.name + '\n')
            self.assertEqual(json.loads((output / 'candidate.json').read_text())['source_sha256'], digest)
            self.assertEqual({path.name for path in output.iterdir()}, {archive.name, 'SHA256SUMS', 'candidate.json'})

    def test_git_nul_protocol_rejects_control_separators_without_quoting_bypass(self):
        with tempfile.TemporaryDirectory() as directory:
            root, sha, _, git = self.repository(directory)
            blob = git('hash-object', '-w', '--stdin', input=b'synthetic bytes')
            for name in ('public\n.env', 'public\t.env', 'private-evaluation\\article.txt'):
                with self.subTest(name=name):
                    tree = git('mktree', '-z', input=f'100644 blob {blob}\t{name}\0'.encode('utf-8'))
                    commit = git('commit-tree', tree, '-p', sha, '-m', 'synthetic unusual Git path')
                    with self.assertRaises(ValueError):
                        prepare.source_entries(root, commit)

    def test_symlinks_and_submodules_are_rejected_from_git_tree_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root, sha, _, git = self.repository(directory)
            blob = git('hash-object', '-w', '--stdin', input=b'../outside-synthetic-file')
            for metadata in (f'120000 blob {blob}', f'160000 commit {sha}'):
                with self.subTest(metadata=metadata.split(' ', 2)[:2]):
                    tree = git('mktree', '-z', input=(metadata + '\talias\0').encode('ascii'))
                    commit = git('commit-tree', tree, '-p', sha, '-m', 'synthetic non-file entry')
                    with self.assertRaises(ValueError):
                        prepare.source_entries(root, commit)

    def test_archive_attributes_cannot_remove_or_rewrite_checked_files(self):
        for attributes, body in (('README.md export-ignore\n', 'synthetic readme'), ('README.md export-subst\n', '$Format:%H$')):
            with self.subTest(attributes=attributes.strip()), tempfile.TemporaryDirectory() as directory:
                root, sha, checks, _ = self.repository(directory, {'.gitattributes': attributes, 'README.md': body})
                output = Path(directory) / 'candidate'
                with self.assertRaises(ValueError):
                    prepare.prepare(root, 'v0.1.0-alpha.1', sha, checks, output)
                self.assertFalse(output.exists())

    def test_archive_digest_ignores_host_eol_config_and_preserves_explicit_attributes(self):
        with tempfile.TemporaryDirectory() as directory:
            root, sha, checks, git = self.repository(directory, {'.gitattributes': '*.ps1 text eol=crlf\n', 'scripts/example.ps1': 'Write-Output "synthetic"\n'})
            git('config', 'core.autocrlf', 'true')
            first = prepare.prepare(root, 'v0.1.0-alpha.1', sha, checks, Path(directory) / 'windows')
            git('config', 'core.autocrlf', 'false')
            git('config', 'core.eol', 'lf')
            second = prepare.prepare(root, 'v0.1.0-alpha.1', sha, checks, Path(directory) / 'linux')
            self.assertEqual(first['source_sha256'], second['source_sha256'])
            with tarfile.open(Path(directory) / 'windows/textcomb-v0.1.0-alpha.1.tar.gz') as contents:
                self.assertEqual(contents.extractfile('textcomb-v0.1.0-alpha.1/scripts/example.ps1').read(), b'Write-Output "synthetic"\r\n')

    def test_unexpected_archive_file_is_rejected_before_any_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root, sha, checks, _ = self.repository(directory)
            original_archive = prepare.source_archive
            def inject_file(*args):
                result, crlf = original_archive(*args)
                buffer = io.BytesIO()
                with tarfile.open(fileobj=io.BytesIO(result), mode='r:') as source, tarfile.open(fileobj=buffer, mode='w:') as target:
                    for member in source:
                        target.addfile(member, source.extractfile(member) if member.isfile() else None)
                    unexpected = tarfile.TarInfo('textcomb-v0.1.0-alpha.1/unexpected.txt')
                    unexpected.size = 9
                    target.addfile(unexpected, io.BytesIO(b'synthetic'))
                return buffer.getvalue(), crlf
            output = Path(directory) / 'candidate'
            with mock.patch.object(prepare, 'source_archive', inject_file), self.assertRaises(ValueError):
                prepare.prepare(root, 'v0.1.0-alpha.1', sha, checks, output)
            self.assertFalse(output.exists())

    def test_untracked_and_global_attributes_and_tar_permissions_cannot_change_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            root, sha, checks, git = self.repository(directory, {'.gitattributes': '* text eol=lf\n', 'README.md': 'synthetic first line\nsecond line\n'})
            first = prepare.prepare(root, 'v0.1.0-alpha.1', sha, checks, Path(directory) / 'before')
            info_attributes = root / git('rev-parse', '--git-path', 'info/attributes')
            info_attributes.parent.mkdir(parents=True, exist_ok=True)
            info_attributes.write_text('* text eol=crlf\nREADME.md export-ignore\n')
            global_attributes = Path(directory) / 'host-attributes'
            global_attributes.write_text('* text eol=crlf\nREADME.md export-ignore\n')
            git('config', 'core.attributesFile', str(global_attributes))
            git('config', 'tar.umask', '0077')
            self.assertEqual(git('status', '--porcelain'), '')
            environment = {'GIT_CONFIG_COUNT': '1', 'GIT_CONFIG_KEY_0': 'tar.umask', 'GIT_CONFIG_VALUE_0': '0077'}
            with mock.patch.dict(prepare.os.environ, environment):
                second = prepare.prepare(root, 'v0.1.0-alpha.1', sha, checks, Path(directory) / 'after')
            self.assertEqual(first['source_sha256'], second['source_sha256'])
            with tarfile.open(Path(directory) / 'after/textcomb-v0.1.0-alpha.1.tar.gz') as contents:
                self.assertEqual(contents.extractfile('textcomb-v0.1.0-alpha.1/README.md').read(), b'synthetic first line\nsecond line\n')
            self.assertEqual(info_attributes.read_text(), '* text eol=crlf\nREADME.md export-ignore\n')
            self.assertEqual(git('config', 'tar.umask'), '0077')

    def test_crlf_rewrite_without_committed_crlf_attribute_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root, sha, checks, _ = self.repository(directory, {'.gitattributes': '* text eol=lf\n', 'README.md': 'synthetic first line\nsecond line\n'})
            original_archive = prepare.source_archive
            def rewrite_lines(*args):
                result, crlf = original_archive(*args)
                self.assertNotIn('README.md', crlf)
                buffer = io.BytesIO()
                with tarfile.open(fileobj=io.BytesIO(result), mode='r:') as source, tarfile.open(fileobj=buffer, mode='w:') as target:
                    for member in source:
                        if member.isfile():
                            contents = source.extractfile(member).read()
                            if member.name.endswith('/README.md'):
                                contents = contents.replace(b'\n', b'\r\n')
                                member.size = len(contents)
                            target.addfile(member, io.BytesIO(contents))
                        else:
                            target.addfile(member)
                return buffer.getvalue(), crlf
            output = Path(directory) / 'candidate'
            with mock.patch.object(prepare, 'source_archive', rewrite_lines), self.assertRaises(ValueError):
                prepare.prepare(root, 'v0.1.0-alpha.1', sha, checks, output)
            self.assertFalse(output.exists())

    def test_existing_bundles_and_partial_evidence_are_never_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            root, sha, checks, _ = self.repository(directory)
            for old_files in ({}, {'candidate.json': 'synthetic old manifest'}, {'SHA256SUMS': 'synthetic old digest'},
                              {'textcomb-v0.1.0-alpha.1.tar.gz': 'synthetic old archive'}):
                with self.subTest(old_files=tuple(old_files)), tempfile.TemporaryDirectory(dir=directory) as previous:
                    output = Path(previous)
                    for name, value in old_files.items():
                        (output / name).write_text(value)
                    with self.assertRaises(ValueError):
                        prepare.prepare(root, 'v0.1.0-alpha.2', sha, checks, output)
                    self.assertEqual({path.name: path.read_text() for path in output.iterdir()}, old_files)

    def test_failed_compression_or_manifest_write_leaves_no_success_artifact(self):
        for failure in ('compression', 'manifest', 'finalization'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root, sha, checks, _ = self.repository(directory)
                output = Path(directory) / 'candidate'
                write_text = Path.write_text
                def fail_manifest(path, *args, **kwargs):
                    if path.name == 'candidate.json':
                        raise OSError('synthetic disk write failure')
                    return write_text(path, *args, **kwargs)
                patches = {'compression': mock.patch.object(prepare.gzip.GzipFile, 'write', side_effect=OSError('synthetic compression failure')),
                           'manifest': mock.patch.object(Path, 'write_text', fail_manifest),
                           'finalization': mock.patch.object(prepare.os, 'rename', side_effect=OSError('synthetic finalization failure'))}
                patch = patches[failure]
                with patch, self.assertRaises(OSError):
                    prepare.prepare(root, 'v0.1.0-alpha.1', sha, checks, output)
                self.assertFalse(output.exists())
                self.assertEqual({path.name for path in Path(directory).iterdir()}, {'repo'})

    def test_concurrent_output_creation_does_not_replace_another_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            root, sha, checks, _ = self.repository(directory)
            output = Path(directory) / 'candidate'
            original_write = prepare.gzip.GzipFile.write
            def concurrent_write(compressed, source):
                output.mkdir()
                (output / 'candidate.json').write_text('synthetic concurrent evidence')
                return original_write(compressed, source)
            with mock.patch.object(prepare.gzip.GzipFile, 'write', concurrent_write), self.assertRaises(ValueError):
                prepare.prepare(root, 'v0.1.0-alpha.1', sha, checks, output)
            self.assertEqual({path.name: path.read_text() for path in output.iterdir()}, {'candidate.json': 'synthetic concurrent evidence'})
            self.assertEqual({path.name for path in Path(directory).iterdir()}, {'repo', 'candidate'})

    def test_requested_commit_must_match_head_and_belong_to_main(self):
        with tempfile.TemporaryDirectory() as directory:
            root, sha, checks, git = self.repository(directory)
            output = Path(directory) / 'candidate'
            with self.assertRaises(ValueError):
                prepare.prepare(root, 'v0.1.0-alpha.1', 'a' * 40, checks, output)
            (root / 'new.txt').write_text('synthetic unpublished change')
            git('add', 'new.txt')
            git('commit', '-m', 'synthetic commit outside main')
            new_sha = git('rev-parse', 'HEAD')
            for check in checks['check_runs']:
                check['head_sha'] = new_sha
            with self.assertRaises(subprocess.CalledProcessError):
                prepare.prepare(root, 'v0.1.0-alpha.1', new_sha, checks, output)
            self.assertFalse(output.exists())

    def test_version_is_read_from_archived_commit_even_when_index_hides_local_edits(self):
        with tempfile.TemporaryDirectory() as directory:
            root, sha, checks, git = self.repository(directory)
            git('update-index', '--assume-unchanged', 'Cargo.toml', 'web/package.json')
            (root / 'Cargo.toml').write_text('[workspace.package]\nversion="0.2.0"\n')
            (root / 'web/package.json').write_text('{"version":"0.2.0"}')
            self.assertEqual(git('status', '--porcelain'), '')
            output = Path(directory) / 'candidate'
            with self.assertRaises(ValueError):
                prepare.prepare(root, 'v0.2.0-alpha.1', sha, checks, output)
            self.assertFalse(output.exists())

if __name__ == '__main__': unittest.main()
