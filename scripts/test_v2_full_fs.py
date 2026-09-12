#!/usr/bin/env python3
"""Bounded Linux tmpfs exhaustion; no host mounts or production configuration."""
import json
import os
from pathlib import Path
import resource
import subprocess
import sys
import tempfile


def main():
    if len(sys.argv) == 4 and sys.argv[1] == '--inside':
        root, binary = Path(sys.argv[2]), Path(sys.argv[3])
        assert os.readlink('/proc/self/ns/mnt') != os.environ['HOSHIKAGE_PARENT_MNT']
        assert root.is_dir() and not any(root.iterdir())
        resource.setrlimit(resource.RLIMIT_AS, (512 * 1024**2, 512 * 1024**2))
        resource.setrlimit(resource.RLIMIT_CPU, (30, 30))
        subprocess.run(['mount', '--make-rprivate', '/'], check=True)
        subprocess.run(['mount', '-t', 'tmpfs', '-o', 'size=16m,nr_inodes=4096,mode=700,nodev,nosuid,noexec', 'hoshikage-test', str(root)], check=True)
        # Namespace teardown unmounts even on timeout or process failure.
        os.environ['HOSHIKAGE_FULL_FS_ROOT'] = str(root)
        os.execv(str(binary), [str(binary), '--ignored', '--exact', 'actual_full_filesystem_preserves_committed_state', '--nocapture'])
    repo = Path(__file__).resolve().parents[1]
    result = subprocess.run(['cargo', 'test', '--locked', '--test', 'v2_full_fs', '--no-run', '--message-format=json'], cwd=repo, check=True, capture_output=True, text=True)
    artifacts = [json.loads(line) for line in result.stdout.splitlines() if line.startswith('{')]
    binary = next(a['executable'] for a in artifacts if a.get('reason') == 'compiler-artifact' and a.get('executable') and a['target']['name'] == 'v2_full_fs')
    with tempfile.TemporaryDirectory(prefix='hoshikage-full-fs-') as root:
        env = dict(os.environ, HOSHIKAGE_PARENT_MNT=os.readlink('/proc/self/ns/mnt'))
        subprocess.run(['unshare', '--user', '--map-root-user', '--mount', '--fork', '--kill-child', sys.executable, str(Path(__file__).resolve()), '--inside', root, binary], env=env, check=True, timeout=60)
        assert not any(Path(root).iterdir()), 'Test mount must not be visible to parent'
    print('PASS: private mount removed; host namespace unchanged')


if __name__ == '__main__':
    main()
