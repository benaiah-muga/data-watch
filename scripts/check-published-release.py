#!/usr/bin/env python3
"""Verify that the public source, tag and uploaded binary match local artifacts."""
import hashlib
import json
import os
import pathlib
import subprocess
import sys


def main():
    repository, tag = sys.argv[1:]
    environment = dict(os.environ)
    environment.pop('DEBUG', None)
    environment.pop('GH_DEBUG', None)
    def api(route):
        return json.loads(subprocess.check_output(['gh', 'api', route], env=environment))
    project = pathlib.Path(__file__).resolve().parents[1]
    local = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=project, text=True).strip()
    repo = api(f'repos/{repository}')
    assert not repo['private'] and repo['license']['spdx_id'] == 'Apache-2.0', repo['html_url']
    main_ref = api(f'repos/{repository}/git/ref/heads/main')
    assert main_ref['object']['sha'] == local, 'remote main is behind the local commit'
    tag_ref = api(f'repos/{repository}/git/ref/tags/{tag}')['object']
    if tag_ref['type'] == 'tag':
        tag_ref = api(f'repos/{repository}/git/tags/{tag_ref["sha"]}')['object']
    assert tag_ref['sha'] == local, 'release tag does not match the reviewed source'
    release = api(f'repos/{repository}/releases/tags/{tag}')
    assert not release['draft'] and not release['prerelease'], release['html_url']
    assets = {asset['name']: asset for asset in release['assets']}
    for name in ('data-watch-linux-x86_64.tar.gz', 'SHA256SUMS'):
        asset = assets[name]
        local_file = project / 'target/release-dist' / name
        assert asset['state'] == 'uploaded' and asset['size'] == local_file.stat().st_size, asset
        expected = hashlib.sha256(local_file.read_bytes()).hexdigest()
        if asset.get('digest'):
            assert asset['digest'] == 'sha256:' + expected, asset['digest']
        else:
            remote_bytes = subprocess.check_output(['gh', 'api', f'repos/{repository}/releases/assets/{asset["id"]}', '-H', 'Accept: application/octet-stream'], env=environment)
            assert hashlib.sha256(remote_bytes).hexdigest() == expected, 'uploaded asset checksum mismatch'
    assert subprocess.check_output(['git', 'status', '--porcelain'], cwd=project, text=True).strip() == '', 'source checkout is dirty'
    print('RELEASE_PUBLISH_CHECK_PASS:', release['html_url'], local)


if __name__ == '__main__':
    main()
