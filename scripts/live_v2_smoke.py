#!/usr/bin/env python3
"""Opt-in isolated real Codex v2 acceptance. Never modifies the installed service."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import time
import urllib.request
import uuid


def main():
    binary = Path(__file__).resolve().parents[1] / 'target/debug/codex-hoshikage-proxy'
    auth = Path(os.environ.get('CODEX_HOME', str(Path.home() / '.codex'))) / 'auth.json'
    results = []
    with tempfile.TemporaryDirectory(prefix='hoshikage-v2-live-') as directory:
        root = Path(directory)
        work = root / 'work'
        work.mkdir()
        home = root / 'proxy/codex-home'
        home.mkdir(parents=True)
        shutil.copyfile(auth, home / 'auth.json')
        (home / 'auth.json').chmod(0o600)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        api_key = str(uuid.uuid4())
        config = root / 'config.toml'
        config.write_text(f'''[server]
v2_enabled = true
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
api_key = "{api_key}"
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
        env['CODEX_HOSHIKAGE_PROXY_HOME'] = str(root / 'proxy')
        env['CODEX_HOSHIKAGE_PROXY_CONFIG'] = str(config)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        identity = {}

        def call(path, body=None, key=None, binary=False):
            headers = {'Authorization': f'Bearer {api_key}', 'Content-Type': 'application/json', **identity}
            if key:
                headers['Idempotency-Key'] = key
            req = urllib.request.Request(f'http://127.0.0.1:{port}/v2/codex/{path}', data=None if body is None else json.dumps(body).encode(), headers=headers)
            with opener.open(req, timeout=150) as response:
                return response.read() if binary else json.load(response)

        def start():
            log = open(root / 'proxy.log', 'ab')
            process = subprocess.Popen([str(binary)], cwd=work, env=env, stdout=log, stderr=log, start_new_session=True)
            log.close()
            for _ in range(150):
                if process.poll() is not None:
                    diagnostics = (root / 'proxy.log').read_text(errors='replace')
                    diagnostic_file = Path('/tmp/hoshikage-v2-startup.log')
                    diagnostic_file.write_text(diagnostics.replace(api_key, '[redacted]'))
                    diagnostic_file.chmod(0o600)
                    raise RuntimeError('isolated Proxy exited during startup; private diagnostic: /tmp/hoshikage-v2-startup.log')
                try:
                    cap = call('capabilities')
                    identity.update({'X-Proxy-Instance-Id': cap['instance_id'], 'X-Proxy-Recovery-Generation': cap['recovery_generation']})
                    return process
                except OSError:
                    time.sleep(.1)
            process.terminate()
            process.wait(timeout=20)
            raise RuntimeError('isolated Proxy not ready')

        def run(cid, prompt, model=None):
            body = {'input': prompt}
            if model:
                body['model'] = model
            op = call(f'conversations/{cid}/responses', body, str(uuid.uuid4()))
            rid = op['resource']['id']
            for _ in range(3100):
                r = call(f'responses/{rid}')
                if r['output']['state'] == 'ready':
                    return rid, call(f'responses/{rid}/output')
                if r['phase'] in ('unknown', 'rejected', 'cancelled') or r['output']['state'] in ('failed', 'unavailable'):
                    raise RuntimeError(json.dumps({'phase': r['phase'], 'status': r['execution_status'], 'error': r.get('error'), 'output': r['output']}))
                time.sleep(.2)
            raise RuntimeError('execution deadline (620 seconds); terminal result was not confirmed')

        def admin(request):
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
                sock.settimeout(120)
                sock.connect(str(root / 'proxy/state/v2/admin.sock'))
                sock.sendall(json.dumps(request).encode() + b'\n')
                result = json.load(sock.makefile('rb'))
                if 'error' in result:
                    raise RuntimeError(json.dumps(result))
                return result

        process = start()
        try:
            c = call('conversations', {'workspace': {'mode': 'automatic'}, 'model': 'chatgpt/gpt-5.6-luna'}, 'create')
            cid = c['resource']['id']
            rid, output = run(cid, 'In this workspace create report.txt containing exactly V2_ARTIFACT_OK. Then call hoshikage_publish_artifact for report.txt. Reply exactly V2_DONE after successful publication. Do not access other directories or the network.')
            artifacts = call(f'conversations/{cid}/artifacts')['data']
            ready = [a for a in artifacts if a['state'] == 'ready']
            if not ready:
                with sqlite3.connect(root / 'proxy/state/v2/metadata.sqlite3') as db:
                    counts = db.execute('SELECT kind, count(*) FROM records GROUP BY kind').fetchall()
                    artifact_rows = [json.loads(row[0]) for row in db.execute("SELECT value FROM records WHERE kind='artifact'")]
                raise RuntimeError(json.dumps({'output': output, 'record_counts': counts, 'artifacts': [{k: a.get(k) for k in ('state', 'display_name', 'conversation_id')} for a in artifact_rows], 'source_exists': bool(list(work.rglob('report.txt')))}, ensure_ascii=False))
            a = ready[-1]
            data = call(f"artifacts/{a['artifact_id']}/content", binary=True)
            assert hashlib.sha256(data).hexdigest() == a['sha256']
            assert data.strip() == b'V2_ARTIFACT_OK'
            results.append({'test': 'real_model_artifact_registration', 'passed': True})
            process.terminate()
            process.wait(timeout=20)
            process = start()
            assert call(f'responses/{rid}/output') == output
            assert call(f"artifacts/{a['artifact_id']}/content", binary=True) == data
            results.append({'test': 'restart_same_output_and_artifact', 'passed': True})
            _, continued = run(cid, 'Without running any tools, state the exact content of report.txt you just created.', 'chatgpt/gpt-5.6-terra')
            text = ''.join(p.get('text', '') for item in continued['output'] for p in item.get('content', []))
            assert 'V2_ARTIFACT_OK' in text
            results.append({'test': 'same_conversation_model_change', 'passed': True})
            _, published_again = run(cid, 'Call hoshikage_publish_artifact for the existing report.txt, then reply V2_REPUBLISHED. Do not modify the file.', 'chatgpt/gpt-5.6-terra')
            assert 'V2_REPUBLISHED' in json.dumps(published_again)
            artifacts_after = call(f'conversations/{cid}/artifacts')['data']
            assert len([v for v in artifacts_after if v['state'] == 'ready']) > len(ready)
            results.append({'test': 'artifact_tool_after_resume_and_model_change', 'passed': True})
            stop_conv = call('conversations', {'workspace': {'mode': 'automatic'}, 'model': 'chatgpt/gpt-5.6-luna'}, 'stop-conversation')['resource']['id']
            stop_run = call(f'conversations/{stop_conv}/responses', {'input': 'Run sleep 30 in this workspace, then reply WAIT_FINISHED. Do not access the network or other directories.'}, 'stop-run')['resource']['id']
            for _ in range(300):
                state = call(f'responses/{stop_run}')
                if state['phase'] == 'started':
                    break
                time.sleep(.05)
            assert state['phase'] == 'started'
            stop = call('stops', {'target': {'response_id': stop_run}}, 'explicit-stop')
            for _ in range(300):
                state = call(f'responses/{stop_run}')
                if state['phase'] == 'finished':
                    break
                time.sleep(.1)
            assert state['execution_status'] == 'interrupted', state
            assert call(f"stops/{stop['stop_id']}")['stop_status'] == 'interrupted'
            _, continued_stop = run(stop_conv, 'Reply exactly STOP_CONTINUED without tools.')
            assert 'STOP_CONTINUED' in json.dumps(continued_stop)
            results.append({'test': 'real_interrupt_and_first_turn_continuation', 'passed': True})
            backup = root / 'backup'
            manifest = admin({'action': 'backup.create', 'destination': str(backup)})
            assert manifest['codex_history'] == 'included'
            previous_generation = identity['X-Proxy-Recovery-Generation']
            process.terminate()
            process.wait(timeout=20)
            restored = json.loads(subprocess.check_output([str(binary), 'admin', 'backup', 'restore', '--from', str(backup)], env=env))
            process = start()
            assert identity['X-Proxy-Recovery-Generation'] != previous_generation
            assert call('capabilities')['recovery_state'] == 'recovery_blocked'
            release = {'action': 'recovery.release', 'restore_id': restored['restore_id'], 'generation': restored['recovery_generation'], 'accept_risk': True, 'reason': 'isolated live acceptance; exact backup and stopped children verified'}
            first = admin(release)
            assert admin(release)['audit_id'] == first['audit_id']
            assert call(f'responses/{rid}/output') == output
            assert call(f"artifacts/{a['artifact_id']}/content", binary=True) == data
            _, restored_output = run(cid, 'Without tools, recall the exact content of the report.txt you published.')
            assert 'V2_ARTIFACT_OK' in json.dumps(restored_output)
            results.append({'test': 'formal_restore_history_generation_and_release_replay', 'passed': True})
        except Exception as error:
            results.append({'test': 'live_v2', 'passed': False, 'error': str(error)})
            # Raw Codex logs may contain prompts/credentials; do not print them.
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=20)
        print(json.dumps(results, ensure_ascii=False, indent=2))
        if any(not r['passed'] for r in results):
            raise SystemExit(1)


if __name__ == '__main__':
    main()
