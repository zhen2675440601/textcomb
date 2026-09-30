"""Prepare a private prerelease candidate from a checked main commit; never publish."""
import argparse
import gzip
import hashlib
import json
import re
import subprocess
import tomllib
from pathlib import Path

REQUIRED = ('rust', 'web', 'migrations', 'container', 'release-tools', 'Rust dependency audit', 'Frontend dependency audit')

def validate_version(version, rust_version, web_version):
    match = re.fullmatch(r'v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-(alpha|beta|rc)\.(0|[1-9]\d*)', version)
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

def prepare(root, version, commit, checks, output):
    if not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('Use a full lowercase commit SHA')
    if git(root, 'rev-parse', 'HEAD').decode().strip() != commit:
        raise ValueError('Checkout does not match the requested commit')
    git(root, 'merge-base', '--is-ancestor', commit, 'origin/main')
    if git(root, 'status', '--porcelain').strip():
        raise ValueError('Candidate checkout must be clean')
    rust = tomllib.loads((root / 'Cargo.toml').read_text(encoding='utf-8'))['workspace']['package']['version']
    web = json.loads((root / 'web/package.json').read_text(encoding='utf-8'))['version']
    validate_version(version, rust, web)
    evidence = validate_checks(checks, commit)
    tracked = git(root, 'ls-tree', '-r', '--name-only', commit).decode().splitlines()
    for name in tracked:
        if name == '.env' or any(part in ('private-evaluation', 'target', 'node_modules') for part in Path(name).parts):
            raise ValueError('Disallowed tracked file in candidate')
    output.mkdir(parents=True, exist_ok=True)
    archive = output / ('textcomb-' + version + '.tar.gz')
    if archive.exists():
        raise ValueError('Candidate archive already exists; use a fresh output directory')
    tar = git(root, 'archive', '--format=tar', '--prefix=textcomb-' + version + '/', commit)
    with archive.open('wb') as target:
        with gzip.GzipFile(filename='', mode='wb', fileobj=target, mtime=0) as compressed:
            compressed.write(tar)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (output / 'SHA256SUMS').write_text(digest + '  ' + archive.name + '\n', encoding='utf-8')
    manifest = {'schema': 'textcomb.release-candidate.v1', 'version': version, 'commit': commit, 'source_sha256': digest, 'checks': evidence, 'published': False, 'quality_certified': False, 'review_required': ['human quality evidence and scoped claims', 'privacy/upgrade/restore evidence', 'known vulnerabilities and contributor authorization', 'release notes and exact commit approval']}
    (output / 'candidate.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
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
