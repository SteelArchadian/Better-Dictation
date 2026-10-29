"""Build a signed Better Dictation release and lay it out for GitHub.

  python -I release/release.py "what changed"            # next patch version (or the current one if not released yet)
  python -I release/release.py "what changed" --version 0.2.0

First it checks the source for private names and runs the tests; after the build it scans the installer with
Windows Defender and commits (unless --no-commit). Any failure stops it with a short reason.

The updater signing key is read from BETTER_DICTATION_SIGNING_KEY (a path), default
%USERPROFILE%\\.tauri\\better-dictation-updater.key; it never goes in the repo. Back it up: if it is lost,
installed copies can never update again.

Leaves release/<version>/ holding:
  Better-Dictation_<version>_x64-setup.exe   the installer (GitHub turns spaces in asset names into dots)
  Better-Dictation_<version>_x64-setup.exe.sig
  latest.json                                for the updater (points at the v<version> GitHub release)
  notes.md                                   the release notes
Publish them as the GitHub release tagged v<version>, marked Latest, with latest.json among its assets.
"""
import datetime
import json
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
TAURI = os.path.join(ROOT, 'src-tauri')
CONF = os.path.join(TAURI, 'tauri.conf.json')
CARGO = os.path.join(TAURI, 'Cargo.toml')
REPO = 'SteelArchadian/Better-Dictation'
ASSET = 'Better-Dictation'
DEFENDER = r'C:\Program Files\Windows Defender\MpCmdRun.exe'


def current_version():
    with open(CONF, encoding='utf-8') as f:
        return json.load(f)['version']


def set_version(v):
    with open(CONF, encoding='utf-8') as f:
        conf = json.load(f)
    conf['version'] = v
    with open(CONF, 'w', encoding='utf-8') as f:
        json.dump(conf, f, indent=2)
        f.write('\n')
    with open(CARGO, encoding='utf-8') as f:
        text = f.read()
    text = re.sub(r'(?m)^version = "[^"]*"', f'version = "{v}"', text, count=1)
    with open(CARGO, 'w', encoding='utf-8') as f:
        f.write(text)


def next_version():
    v = current_version()
    if not os.path.exists(os.path.join(HERE, v)):
        return v
    major, minor, patch = (int(x) for x in v.split('.'))
    return f'{major}.{minor}.{patch + 1}'


def private_words():
    """Names that must never go public: this repo's git identity, plus BETTER_DICTATION_PRIVATE_WORDS (comma-separated)."""
    words = [w.strip() for w in os.environ.get('BETTER_DICTATION_PRIVATE_WORDS', '').split(',') if w.strip()]
    for key in ('user.name', 'user.email'):
        value = subprocess.run(['git', 'config', key], cwd=ROOT, capture_output=True, text=True).stdout.strip()
        words.append(value.split('@')[0])
    return [w for w in words if len(w) > 2]


def checks():
    """Everything that must pass before a build: no private names in the shipped source, tests green."""
    words = private_words()
    for root in (os.path.join(TAURI, 'src'), os.path.join(ROOT, 'ui')):
        for dirpath, _, files in os.walk(root):
            for name in files:
                path = os.path.join(dirpath, name)
                with open(path, encoding='utf-8', errors='replace') as f:
                    text = f.read().lower()
                hit = next((w for w in words if w.lower() in text), None)
                if hit:
                    sys.exit(f'a private name is in {os.path.relpath(path, ROOT)}; take it out first')
    tests = subprocess.run(['cargo', 'test', '--quiet'], cwd=TAURI, capture_output=True, text=True)
    if tests.returncode != 0:
        sys.exit(f'tests failed:\n{(tests.stdout + tests.stderr)[-1500:]}')


def commit(version, notes):
    subprocess.run(['git', 'add', '-A'], cwd=ROOT, check=True)
    subprocess.run(['git', 'commit', '-q', '-m', f'{version}: {notes}'], cwd=ROOT, check=True)
    return subprocess.run(['git', 'log', '-1', '--format=%h'], cwd=ROOT, capture_output=True, text=True).stdout.strip()


def main():
    sys.stdout.reconfigure(encoding='utf-8', errors='replace')
    args = sys.argv[1:]
    version = None
    if '--version' in args:
        i = args.index('--version')
        version = args[i + 1]
        del args[i:i + 2]
    flags = [a for a in args if a.startswith('--')]
    args = [a for a in args if not a.startswith('--')] + flags
    if not args or args[0].startswith('--') or not args[0].strip():
        sys.exit('give the release notes: release.py "what changed"')
    notes = args[0].strip()
    version = version or next_version()
    if not re.fullmatch(r'\d+\.\d+\.\d+', version):
        sys.exit(f'not a version: {version}')
    out = os.path.join(HERE, version)
    if os.path.exists(out):
        sys.exit(f'release/{version} already exists')

    checks()
    key = os.environ.get('BETTER_DICTATION_SIGNING_KEY') or os.path.join(os.path.expanduser('~'), '.tauri', 'better-dictation-updater.key')
    if not os.path.isfile(key):
        sys.exit(f'no signing key at {key}')
    set_version(version)
    env = dict(os.environ, TAURI_SIGNING_PRIVATE_KEY=key, TAURI_SIGNING_PRIVATE_KEY_PASSWORD='')
    log = os.path.join(os.environ.get('TEMP', HERE), f'better-dictation-build-{version}.log')
    print(f'building {version} (log: {log})')
    with open(log, 'w', encoding='utf-8') as f:
        npx = shutil.which('npx') or 'npx'
        code = subprocess.run([npx, 'tauri', 'build'], cwd=ROOT, env=env, stdout=f, stderr=subprocess.STDOUT).returncode
    if code != 0:
        sys.exit(f'build failed (exit {code}); see {log}')

    nsis = os.path.join(TAURI, 'target', 'release', 'bundle', 'nsis')
    built = f'Better Dictation_{version}_x64-setup.exe'
    setup = f'{ASSET}_{version}_x64-setup.exe'
    for name in (built, built + '.sig'):
        if not os.path.isfile(os.path.join(nsis, name)):
            sys.exit(f'the build left no {name}')
    scan = subprocess.run([DEFENDER, '-Scan', '-ScanType', '3', '-File', os.path.join(nsis, built), '-DisableRemediation'],
                          capture_output=True, text=True)
    if scan.returncode != 0:
        sys.exit(f'Defender flagged or could not scan the installer:\n{scan.stdout[-600:]}')

    os.makedirs(out)
    shutil.copy2(os.path.join(nsis, built), os.path.join(out, setup))
    shutil.copy2(os.path.join(nsis, built + '.sig'), os.path.join(out, setup + '.sig'))
    with open(os.path.join(out, setup + '.sig'), encoding='utf-8') as f:
        signature = f.read().strip()
    latest = {
        'version': version,
        'notes': notes,
        'pub_date': datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ'),
        'platforms': {
            'windows-x86_64': {
                'signature': signature,
                'url': f'https://github.com/{REPO}/releases/download/v{version}/{setup}',
            },
        },
    }
    with open(os.path.join(out, 'latest.json'), 'w', encoding='utf-8') as f:
        json.dump(latest, f, indent=2)
        f.write('\n')
    with open(os.path.join(out, 'notes.md'), 'w', encoding='utf-8') as f:
        f.write(f'# Better Dictation {version}\n\n{notes}\n')
    head = commit(version, notes) if '--no-commit' not in args else '(not committed)'
    print(f'release/{version} ready and Defender-clean: commit {head}, tag v{version}, assets {setup}, {setup}.sig, latest.json')


if __name__ == '__main__':
    main()
