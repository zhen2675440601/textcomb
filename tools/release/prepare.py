"""Prepare a private prerelease candidate from a checked main commit; never publish."""
import argparse
import gzip
import hashlib
import io
import json
import os
import re
import subprocess
import tarfile
import tempfile
import tomllib
from pathlib import Path, PurePosixPath

REQUIRED = ('rust', 'web', 'migrations', 'container', 'release-tools', 'Rust dependency audit', 'Frontend dependency audit')
# These names are reserved for local state or generated artifacts throughout the
# source tree. Licensed TXT/PDF/DOCX fixtures and explicit env examples remain valid.
PRIVATE_DIRECTORIES = frozenset(('private-evaluation', 'data', 'uploads', 'tmp', 'logs', 'backups', '.sqlx'))
BUILD_DIRECTORIES = frozenset(('target', 'node_modules', 'dist', 'build', '__pycache__', 'coverage'))
ENV_EXAMPLES = frozenset(('.env.example', '.env.compose.example'))
GENERATED_SUFFIXES = ('.log', '.tmp', '.dump', '.backup', '.pyc', '.pyo')

def validate_version(version, rust_version, web_version):
    match = re.fullmatch(r'v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-(alpha|beta|rc)\.(0|[1-9][0-9]*)', version)
    if not match:
        raise ValueError('Only explicit alpha/beta/rc prerelease versions are accepted')
    base = '.'.join(match.group(n) for n in (1, 2, 3))
    if rust_version != base or web_version != base:
        raise ValueError('Candidate version must match both workspace and web versions')

def validate_checks(payload, commit):
    evidence = []
    for name in REQUIRED:
        checks = [c for c in payload['check_runs'] if c['name'] == name and c.get('head_sha') == commit and c.get('app', {}).get('slug') == 'github-actions']
        if not checks:
            raise ValueError('Missing trusted check: ' + name)
        check = max(checks, key=lambda item: item['id'])
        if check['status'] != 'completed' or check.get('conclusion') != 'success':
            raise ValueError('Required check did not succeed: ' + name)
        evidence.append({'name': name, 'id': check['id'], 'head_sha': commit, 'conclusion': check['conclusion'], 'details_url': check.get('details_url')})
    return evidence

def git(root, *args):
    return subprocess.run(['git', *args], cwd=root, check=True, capture_output=True).stdout

def validate_source_path(name):
    # Git tree paths always use '/', even on Windows. Do not interpret quoted
    # ls-tree output or let Windows separators disguise a private path segment.
    parts = name.split('/')
    if any(part in ('', '.', '..') for part in parts) or '\\' in name or ':' in name or any(ord(char) < 32 or ord(char) == 127 for char in name):
        raise ValueError('Candidate paths must be portable relative POSIX paths')
    folded = tuple(part.casefold() for part in PurePosixPath(name).parts)
    if any(part in PRIVATE_DIRECTORIES or part in BUILD_DIRECTORIES for part in folded):
        raise ValueError('Disallowed local state or build artifact in candidate')
    if any((part == '.env' or part.startswith('.env.')) and part not in ENV_EXAMPLES for part in folded):
        raise ValueError('Disallowed environment configuration in candidate')
    if folded[-1].endswith(GENERATED_SUFFIXES):
        raise ValueError('Disallowed generated log, backup or temporary file in candidate')

def source_entries(root, commit):
    entries = {}
    # NUL-delimited records preserve tabs, newlines and Unicode without Git's
    # presentation quoting. Metadata is separated at only the first tab.
    for record in git(root, 'ls-tree', '-r', '-z', commit).split(b'\0'):
        if not record:
            continue
        metadata, raw_name = record.split(b'\t', 1)
        mode, kind, blob = metadata.decode('ascii').split(' ')
        try:
            name = raw_name.decode('utf-8')
        except UnicodeDecodeError as error:
            raise ValueError('Candidate paths must use UTF-8') from error
        validate_source_path(name)
        if kind != 'blob' or mode not in ('100644', '100755'):
            raise ValueError('Candidate source must not contain symlinks or submodules')
        entries[name] = blob
    return entries

def source_archive(root, commit, prefix, entries):
    # `git archive` also reads untracked .git/info/attributes, global attributes
    # and host config. Use a throwaway bare repository sharing only objects so
    # every archive decision comes from the checked tree and fixed settings.
    objects = git(root, 'rev-parse', '--path-format=absolute', '--git-path', 'objects').decode('utf-8').strip()
    with tempfile.TemporaryDirectory(prefix='textcomb-archive-') as directory:
        base = Path(directory)
        empty = base / 'empty-config'
        empty.write_text('', encoding='utf-8')
        environment = {key: value for key, value in os.environ.items() if not key.upper().startswith('GIT_')}
        environment.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=str(empty))
        def isolated_git(repo, *args, input=None):
            return subprocess.run(['git', *args], cwd=repo, input=input, env=environment, check=True, capture_output=True).stdout
        isolated_git(base, 'init', '--bare', '--template=', 'source.git')
        repository = base / 'source.git'
        (repository / 'objects/info/alternates').write_text(json.dumps(Path(objects).as_posix(), ensure_ascii=False) + '\n', encoding='utf-8', newline='\n')
        options = ('-c', 'core.attributesFile=' + str(empty), '-c', 'core.autocrlf=false', '-c', 'core.eol=lf', '-c', 'tar.umask=0002')
        attributes = isolated_git(repository, *options, 'check-attr', '--source=' + commit, '-z', '--stdin', 'text', 'eol', input=b'\0'.join(name.encode('utf-8') for name in entries) + b'\0')
        attributes = attributes.split(b'\0')[:-1]
        values = {}
        for offset in range(0, len(attributes), 3):
            name, attribute, value = attributes[offset:offset + 3]
            values.setdefault(name.decode('utf-8'), {})[attribute.decode('ascii')] = value.decode('ascii')
        crlf = {name for name, attributes in values.items() if attributes.get('eol') == 'crlf' and attributes.get('text') != 'unset'}
        archive = isolated_git(repository, *options, 'archive', '--format=tar', '--prefix=' + prefix + '/', commit)
        return archive, crlf

def validate_archive(tar, prefix, entries, crlf_entries=frozenset()):
    directories = {prefix}
    for name in entries:
        directories.update(prefix + '/' + str(parent) for parent in PurePosixPath(name).parents if str(parent) != '.')
    seen = set()
    files = set()
    with tarfile.open(fileobj=io.BytesIO(tar), mode='r:') as contents:
        for member in contents:
            name = member.name.rstrip('/') if member.isdir() else member.name
            if name in seen:
                raise ValueError('Duplicate candidate archive entry')
            seen.add(name)
            if member.isdir() and name in directories:
                continue
            if not member.isfile() or not name.startswith(prefix + '/'):
                raise ValueError('Unsafe candidate archive entry')
            relative = name[len(prefix) + 1:]
            if relative not in entries:
                raise ValueError('Unexpected candidate archive file')
            # export-ignore/export-subst attributes must not silently drop or
            # change checked source files. Check the actual archive against Git.
            source = contents.extractfile(member).read()
            blob = hashlib.sha1(b'blob ' + str(len(source)).encode('ascii') + b'\0' + source).hexdigest()
            if blob != entries[relative]:
                if relative not in crlf_entries:
                    raise ValueError('Candidate archive does not match checked source')
                # Git honors committed eol=crlf attributes (e.g. PowerShell
                # scripts). Permit only that representation change, not an
                # export-subst rewrite or unrelated archive content.
                normalized = source.replace(b'\r\n', b'\n')
                normalized_blob = hashlib.sha1(b'blob ' + str(len(normalized)).encode('ascii') + b'\0' + normalized).hexdigest()
                if normalized_blob != entries[relative]:
                    raise ValueError('Candidate archive does not match checked source')
            files.add(relative)
    if files != entries.keys():
        raise ValueError('Candidate archive is missing checked source files')

def prepare(root, version, commit, checks, output):
    output = output.absolute()
    # A directory is a single candidate bundle. Never mix a new manifest with
    # an old archive, or overwrite evidence from an earlier invocation.
    if os.path.lexists(output):
        raise ValueError('Use a fresh candidate output directory')
    if not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('Use a full lowercase commit SHA')
    if git(root, 'rev-parse', 'HEAD').decode().strip() != commit:
        raise ValueError('Checkout does not match the requested commit')
    git(root, 'merge-base', '--is-ancestor', commit, 'origin/main')
    if git(root, 'status', '--porcelain').strip():
        raise ValueError('Candidate checkout must be clean')
    # Bind version metadata to the archived commit, including when index flags
    # hide a local version-file edit from `git status`.
    rust = tomllib.loads(git(root, 'show', commit + ':Cargo.toml').decode('utf-8'))['workspace']['package']['version']
    web = json.loads(git(root, 'show', commit + ':web/package.json').decode('utf-8'))['version']
    validate_version(version, rust, web)
    evidence = validate_checks(checks, commit)
    entries = source_entries(root, commit)
    prefix = 'textcomb-' + version
    tar, crlf_entries = source_archive(root, commit, prefix, entries)
    validate_archive(tar, prefix, entries, crlf_entries)
    output.parent.mkdir(parents=True, exist_ok=True)
    # Write privately in a sibling directory, then expose all three consistent
    # files together. Failed compression/writes leave no apparent success bundle.
    with tempfile.TemporaryDirectory(prefix='.' + output.name + '.', dir=output.parent) as temporary:
        staging = Path(temporary)
        archive = staging / (prefix + '.tar.gz')
        with archive.open('wb') as target:
            with gzip.GzipFile(filename='', mode='wb', fileobj=target, mtime=0) as compressed:
                compressed.write(tar)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        (staging / 'SHA256SUMS').write_text(digest + '  ' + archive.name + '\n', encoding='utf-8', newline='\n')
        manifest = {'schema': 'textcomb.release-candidate.v1', 'version': version, 'commit': commit, 'source_sha256': digest, 'checks': evidence, 'published': False, 'quality_certified': False, 'review_required': ['human quality evidence and scoped claims', 'privacy/upgrade/restore evidence', 'known vulnerabilities and contributor authorization', 'release notes and exact commit approval']}
        (staging / 'candidate.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8', newline='\n')
        if os.path.lexists(output):
            raise ValueError('Candidate output directory was created by another invocation')
        os.rename(staging, output)
    return manifest

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--checks', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    prepare(root, args.version, args.commit, json.loads(args.checks.read_text(encoding='utf-8')), args.output)
    print('Prerelease candidate prepared for maintainer review; nothing published.')
