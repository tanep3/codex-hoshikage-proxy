#!/usr/bin/env python3
"""Opt-in real Codex image acceptance, isolated from installed Proxy/Gateway services."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.request
import uuid


def main():
    binary = Path(__file__).resolve().parents[1] / 'target/debug/codex-hoshikage-proxy'
    auth = Path(os.environ.get('CODEX_HOME', str(Path.home() / '.codex'))) / 'auth.json'
    with tempfile.TemporaryDirectory(prefix='hoshikage-v2-image-live-') as directory:
        root = Path(directory)
        work = root / 'work'
        work.mkdir()
        home = root / 'proxy/codex-home'
        home.mkdir(parents=True)
        shutil.copyfile(auth, home / 'auth.json')
        (home / 'auth.json').chmod(0o600)
        skill = Path(os.environ.get('HOSHIKAGE_IMAGE_SKILL', str(Path.home() / '.codex/skills/.system/imagegen')))
        if skill.is_dir():
            shutil.copytree(skill, home / 'skills/.system/imagegen')
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        key = str(uuid.uuid4())
        config = root / 'config.toml'
        config.write_text(f'''[server]
port = {port}
default_cwd = {json.dumps(str(work))}
turn_idle_timeout_seconds = 600
[codex]
inherit_global_config = false
command = {json.dumps(shutil.which('codex'))}
args = ["app-server", "--listen", "stdio://"]
[codex.sandbox]
mode = "workspace-write"
[security]
api_key = "{key}"
allowed_cwds = [{json.dumps(str(work))}]
[approval]
auto_approve_workspace = true
[providers.hoshikage]
codex_id = "hoshikage"
enabled = false
[providers.chatgpt]
codex_id = "openai"
enabled = true
''')
        config.chmod(0o600)
        env = os.environ.copy()
        env.update(CODEX_HOSHIKAGE_PROXY_HOME=str(root / 'proxy'),
                   CODEX_HOSHIKAGE_PROXY_CONFIG=str(config))
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        identity = {}

        def call(path, body=None, raw=False):
            headers = {'Authorization': f'Bearer {key}', 'Content-Type': 'application/json', **identity}
            if body is not None:
                headers['Idempotency-Key'] = str(uuid.uuid4())
            req = urllib.request.Request(f'http://127.0.0.1:{port}/v2/codex/{path}',
                data=None if body is None else json.dumps(body).encode(), headers=headers)
            with opener.open(req, timeout=40) as response:
                return response.read() if raw else json.load(response)

        def start():
            with open(root / 'proxy.log', 'ab') as log:
                process = subprocess.Popen([str(binary)], cwd=work, env=env, stdout=log, stderr=log)
            for _ in range(200):
                if process.poll() is not None:
                    raise RuntimeError('isolated Proxy startup failed')
                try:
                    cap = call('capabilities')
                    identity.update({'X-Proxy-Instance-Id': cap['instance_id'],
                                     'X-Proxy-Recovery-Generation': cap['recovery_generation']})
                    assert cap['features']['response_generated_images']
                    return process
                except OSError:
                    time.sleep(.1)
            process.terminate()
            process.wait(timeout=20)
            raise RuntimeError('isolated Proxy startup timeout')

        process = None
        try:
            process = start()
            op = call('conversations', {'workspace': {'mode': 'automatic'}, 'model': 'chatgpt/gpt-5.6-luna'})
            cid = op['resource']['id']
            op = call(f'conversations/{cid}/responses', {'input':
                '画像生成ツールで、白い背景に青い丸がひとつあるシンプルな画像を1枚作成してください。'
                'imagegenスキルを読み、組み込みのimage_genツールを実際に使用してください。コードやSVGによる代用は不要です。'})
            rid = op['resource']['id']
            print('Dedicated real image execution accepted:', rid, flush=True)
            deadline = time.monotonic() + 600
            while time.monotonic() < deadline:
                m = call(f'responses/{rid}/generated-images')
                if m['state'] == 'complete':
                    break
                time.sleep(2)
            else:
                raise RuntimeError('image inventory did not settle; no execution retry performed')
            assert m['items'], m
            assert all(i['state'] == 'ready' for i in m['items']), m
            blobs = {}
            for item in m['items']:
                aid = item['artifact_id']
                a = call(f'artifacts/{aid}')
                data = call(f'artifacts/{aid}/content', raw=True)
                assert a['media_type'] == 'image/png' and data.startswith(b'\x89PNG\r\n\x1a\n')
                assert a['size_bytes'] == len(data) and a['sha256'] == hashlib.sha256(data).hexdigest()
                blobs[aid] = data
                print('Real image artifact verified:', len(data), 'bytes', flush=True)
            process.terminate()
            process.wait(timeout=30)
            process = start()
            assert call(f'responses/{rid}/generated-images') == m
            for aid, data in blobs.items():
                assert call(f'artifacts/{aid}/content', raw=True) == data
            print('PASS: real generation, automatic capture, HTTP PNG/digest, restart identity and bytes', flush=True)
        except BaseException:
            # Keep diagnostics only for this dedicated test; never copy authentication/config files.
            diagnostic = Path(tempfile.mkdtemp(prefix='hoshikage-image-failure-'))
            if (root / 'proxy.log').exists():
                (diagnostic / 'proxy.log').write_text((root / 'proxy.log').read_text(errors='replace').replace(key, '[redacted]'))
                (diagnostic / 'proxy.log').chmod(0o600)
            if (home / 'sessions').exists():
                shutil.copytree(home / 'sessions', diagnostic / 'sessions')
            print('Private test diagnostics:', diagnostic, flush=True)
            raise
        finally:
            if process is not None and process.poll() is None:
                process.terminate()
                process.wait(timeout=30)


if __name__ == '__main__':
    main()
