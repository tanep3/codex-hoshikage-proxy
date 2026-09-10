#!/usr/bin/env python3
"""Opt-in live smoke test. Uses the current Codex login and consumes model quota.

Build first: cargo build --bin codex-hoshikage-proxy
Run: python3 scripts/live_codex_smoke.py
Only temporary workspaces/processes are used; copied credentials are removed.
"""
import base64
from datetime import datetime, timezone
import json
import os
import queue
import threading
from pathlib import Path
import shutil
import signal
import socket
import struct
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import zlib


def red_png():
    def chunk(kind, data):
        return struct.pack('!I', len(data)) + kind + data + struct.pack('!I', zlib.crc32(kind + data))
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('!2I5B', 64, 64, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress((b'\x00' + b'\xff\x00\x00' * 64) * 64)) + chunk(b'IEND', b'')


def main():
    binary = Path(__file__).resolve().parents[1] / 'target/debug/codex-hoshikage-proxy'
    auth = Path(os.environ.get('CODEX_HOME', str(Path.home() / '.codex'))) / 'auth.json'
    if not auth.is_file():
        raise RuntimeError('Codex auth.json is required; log in to Codex first')
    results = []
    selected = set(filter(None, os.environ.get("LIVE_CODEX_TESTS", "").split(",")))
    with tempfile.TemporaryDirectory(prefix='hoshikage-live-') as directory:
        root = Path(directory)
        workspace = root / 'workspace'
        workspace.mkdir()
        home = root / 'proxy/codex-home'
        home.mkdir(parents=True)
        shutil.copyfile(auth, home / 'auth.json')
        (home / 'auth.json').chmod(0o600)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        config = root / 'config.toml'
        config.write_text(f'''[server]
port = {port}
default_cwd = {json.dumps(str(workspace))}
turn_idle_timeout_seconds = 120
turn_heartbeat_seconds = 2
[codex]
command = {json.dumps(shutil.which('codex'))}
args = ["app-server", "--listen", "stdio://"]
[codex.sandbox]
mode = "read-only"
[security]
allowed_cwds = [{json.dumps(str(workspace))}]
[approval]
auto_approve_workspace = false
timeout_seconds = 10
[providers.hoshikage]
codex_id = "hoshikage"
enabled = false
[providers.chatgpt]
codex_id = "openai"
enabled = true
''')
        env = os.environ.copy()
        env['CODEX_HOSHIKAGE_PROXY_CONFIG'] = str(config)
        env['CODEX_HOSHIKAGE_PROXY_HOME'] = str(root / 'proxy')
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        def request(path, body=None):
            data = None if body is None else json.dumps(body).encode()
            req = urllib.request.Request(f'http://127.0.0.1:{port}{path}', data=data, headers={'Content-Type': 'application/json'})
            return opener.open(req, timeout=150)

        def call(path, body=None):
            with request(path, body) as response:
                return json.load(response)

        def text(response):
            return ''.join(part.get('text', '') for item in response['output'] for part in item.get('content', []))

        log = (root / 'proxy.log').open('w')
        process = subprocess.Popen([str(binary)], env=env, cwd=workspace, stdout=log, stderr=log, start_new_session=True)
        try:
            for _ in range(100):
                if process.poll() is not None:
                    raise RuntimeError('Proxy startup failed: ' + (root / 'proxy.log').read_text()[-2500:])
                try:
                    if call('/readyz')['status'] == 'ready':
                        break
                except (OSError, urllib.error.HTTPError):
                    pass
                time.sleep(.1)
            else:
                raise RuntimeError('Proxy did not become ready')
            models = call('/v1/models')['data']
            ids = [m['id'] for m in models]
            model = os.environ.get('LIVE_CODEX_MODEL', 'chatgpt/gpt-5.6-luna')
            if model not in ids:
                raise RuntimeError(f'Requested model {model} not in live catalog: {ids}')
            results.append({'test': 'initialize_and_catalog', 'passed': True, 'models': len(ids), 'model': model})
            print(json.dumps(results[-1]), flush=True)

            def check(name, operation):
                if selected and name not in selected:
                    return
                started = time.monotonic()
                try:
                    detail = operation()
                    result = {'test': name, 'passed': True, 'detail': detail}
                except urllib.error.HTTPError as error:
                    result = {'test': name, 'passed': False, 'error': error.read().decode()[:1500]}
                except Exception as error:
                    result = {'test': name, 'passed': False, 'error': str(error)[:1500]}
                result['seconds'] = round(time.monotonic() - started, 2)
                results.append(result)
                print(json.dumps(result, ensure_ascii=False), flush=True)

            def basic():
                response = call('/v1/responses', {'model': model, 'input': 'Do not use tools. Remember the word MAPLE. Reply exactly OK.'})
                assert text(response).strip() == 'OK', text(response)
                followup = call('/v1/responses', {'model': model, 'previous_response_id': response['id'], 'input': 'What word did I ask you to remember? Reply with only that word. Do not use tools.'})
                assert 'MAPLE' in text(followup), text(followup)
                return 'text and previous_response_id verified'
            check('responses_and_continuation', basic)

            schema = {'type': 'object', 'properties': {'color': {'type': 'string', 'enum': ['red', 'green', 'blue']}}, 'required': ['color'], 'additionalProperties': False}
            image = 'data:image/png;base64,' + base64.b64encode(red_png()).decode()
            def vision():
                response = call('/v1/responses', {'model': model, 'input': [{'role': 'user', 'content': [{'type': 'input_text', 'text': 'Identify the solid image color. Do not use tools.'}, {'type': 'input_image', 'image_url': image, 'detail': 'high'}]}], 'text': {'format': {'type': 'json_schema', 'name': 'color', 'strict': True, 'schema': schema}}})
                value = json.loads(text(response))
                assert value == {'color': 'red'}, value
                return value
            check('responses_image_and_schema', vision)

            def chat(detail='low'):
                response = call('/v1/chat/completions', {'model': model, 'messages': [{'role':'user', 'content': [{'type':'text', 'text':'Identify the solid image color. Do not use tools.'}, {'type':'image_url', 'image_url': {'url':image, 'detail':detail}}]}], 'reasoning_effort':'low', 'response_format': {'type':'json_schema', 'json_schema': {'name':'color', 'strict':True, 'schema':schema}}})
                value = json.loads(response['choices'][0]['message']['content'])
                assert value == {'color': 'red'}, value
                return value
            check('chat_image_schema_reasoning', chat)
            check('chat_image_schema_high', lambda: chat('high'))

            def streaming(endpoint):
                body = {'model':model, 'stream':True}
                if endpoint == '/v1/responses':
                    body['input'] = 'Do not use tools. Reply exactly STREAM_OK.'
                else:
                    body['messages'] = [{'role':'user', 'content':'Do not use tools. Reply exactly STREAM_OK.'}]
                with request(endpoint, body) as response:
                    stream = response.read().decode()
                parts = []
                for line in stream.splitlines():
                    if not line.startswith('data: ') or line == 'data: [DONE]':
                        continue
                    event = json.loads(line[6:])
                    if endpoint == '/v1/responses':
                        parts.append(event.get('delta', ''))
                    else:
                        parts.extend(choice.get('delta', {}).get('content', '') for choice in event.get('choices', []))
                assert ''.join(parts).strip() == 'STREAM_OK', stream[:1000]
                assert 'response.completed' in stream if endpoint == '/v1/responses' else '[DONE]' in stream, stream[:1000]
                assert 'response.failed' not in stream, stream[:1000]
                return 'text delta and completion verified'
            check('responses_stream', lambda: streaming('/v1/responses'))
            check('chat_stream', lambda: streaming('/v1/chat/completions'))

            def direct_image(detail):
                direct_env = env.copy()
                direct_env['CODEX_HOME'] = str(home)
                child = subprocess.Popen([shutil.which('codex'), 'app-server'], env=direct_env,
                    cwd=workspace, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log,
                    text=True, start_new_session=True)
                messages = queue.Queue()
                def read_messages():
                    for line in child.stdout:
                        messages.put(json.loads(line))
                    messages.put(None)
                reader = threading.Thread(target=read_messages, daemon=True)
                reader.start()
                def send(value):
                    child.stdin.write(json.dumps(value) + '\n')
                    child.stdin.flush()
                def rpc(identifier, method, params):
                    send({'id':identifier, 'method':method, 'params':params})
                    while True:
                        value = messages.get(timeout=120)
                        assert value is not None, 'Direct Codex exited'
                        if value.get('id') == identifier:
                            assert 'error' not in value, value.get('error')
                            return value['result']
                try:
                    rpc(1, 'initialize', {'clientInfo':{'name':'live-test', 'version':'1.0'}, 'capabilities':{}})
                    send({'method':'initialized', 'params':{}})
                    thread = rpc(2, 'thread/start', {'model':model.split('/',1)[1], 'modelProvider':'openai',
                        'cwd':str(workspace), 'ephemeral':True, 'approvalPolicy':'never', 'sandbox':'read-only'})
                    send({'id':3, 'method':'turn/start', 'params':{'threadId':thread['thread']['id'],
                        'input':[{'type':'text', 'text':'[user]\n'}, {'type':'text', 'text':'Identify the solid image color. Do not use tools.'}, {'type':'image','url':image,'detail':detail}],
                        'model':model.split('/',1)[1], 'effort':'low', 'outputSchema':schema}})
                    output = ''
                    while True:
                        value = messages.get(timeout=120)
                        assert value is not None, 'Direct Codex exited'
                        assert 'error' not in value, value.get('error')
                        if value.get('method') == 'item/agentMessage/delta':
                            output += value['params']['delta']
                        if value.get('method') == 'turn/completed':
                            assert value['params']['turn']['status'] == 'completed', value['params']['turn']
                            break
                    color = json.loads(output)
                    assert color == {'color':'red'}, f'direct App Server, detail={detail}: {color}'
                    return {'detail':detail, 'color':color['color'], 'proxy_bypassed':True}
                finally:
                    if child.poll() is None:
                        os.killpg(child.pid, signal.SIGTERM)
                    child.wait(timeout=10)
                    child.stdin.close()
                    reader.join(timeout=2)
                    child.stdout.close()
            check('direct_image_low', lambda: direct_image('low'))
            check('direct_image_high', lambda: direct_image('high'))

            def active_stream(prompt):
                response = request('/v1/responses', {'model':model, 'stream':True, 'input':prompt,
                    'metadata':{'codex.approval_capability':'interactive'}})
                try:
                    while True:
                        line = response.readline().decode()
                        if not line:
                            raise AssertionError('Turn completed before lifecycle test could observe it')
                        if line.startswith('data: '):
                            event = json.loads(line[6:])
                            if event.get('turn_id'):
                                return response, event['turn_id']
                except BaseException:
                    response.close()
                    raise

            def disconnect():
                response, turn_id = active_stream('Run the shell command sleep 20 once, then reply DONE. Do not read or write any files.')
                response.close()
                for _ in range(50):
                    status = call(f'/v1/codex/turns/{turn_id}/status')['status']
                    if status == 'interrupted':
                        return 'client disconnect interrupted the real Codex turn'
                    time.sleep(.2)
                raise AssertionError(f'Expected interrupted; got {status}')
            check('disconnect_interrupt', disconnect)

            def approval(expire=False):
                marker = workspace / ('timeout-marker' if expire else 'decline-marker')
                prompt = f'Run exactly this shell command: printf approved > {marker}. The filesystem is read-only: request approval using require_escalated. If approval is declined or cancelled, do not retry or use another method. Reply DENIED. Do not read other files.'
                response, turn_id = active_stream(prompt)
                try:
                    approval_id = None
                    for _ in range(50):
                        for index in range(1, 10):
                            candidate = f'approval_{index}'
                            try:
                                view = call('/v1/codex/approvals/' + candidate)
                            except urllib.error.HTTPError as error:
                                if error.code == 404:
                                    break
                                raise
                            if view['details'].get('turnId') == turn_id and view['state'] == 'pending':
                                approval_id = candidate
                                break
                        if approval_id:
                            break
                        time.sleep(.2)
                    assert approval_id, 'Real Codex did not issue a pending approval'
                    path = '/v1/codex/approvals/' + approval_id
                    if not expire:
                        allowed = view['available_decisions']
                        decision = 'decline' if 'decline' in allowed else 'cancel'
                        assert decision in allowed, allowed
                        expected = 'denied' if decision == 'decline' else 'cancelled'
                        try:
                            assert call(path, {'decision':decision})['state'] == expected
                        except urllib.error.HTTPError as error:
                            current = call(path)
                            raise AssertionError(f'decision={decision}, offered={allowed}, current_state={current["state"]}, http={error.code}') from error
                        try:
                            call(path, {'decision':'accept'})
                            raise AssertionError('Second approval decision unexpectedly accepted')
                        except urllib.error.HTTPError as error:
                            assert error.code == 409, error.code
                    else:
                        for _ in range(75):
                            if call(path)['state'] == 'expired':
                                break
                            time.sleep(.2)
                        else:
                            raise AssertionError('Approval did not expire')
                    response.read()
                    assert not marker.exists(), 'Declined/expired approval wrote a file'
                    return 'approval expired without writing' if expire else f'{decision} and duplicate rejection verified without writing; offered={allowed}'
                finally:
                    response.close()
            check('approval_decline', approval)
            check('approval_timeout', lambda: approval(True))

            def crash():
                response, _ = active_stream('Run sleep 20 once, then reply DONE. Do not read or write files.')
                children = set()
                def descendants(pid):
                    for path in Path(f'/proc/{pid}/task').glob('*/children'):
                        for value in path.read_text().split():
                            child = int(value)
                            if child not in children:
                                children.add(child)
                                descendants(child)
                descendants(process.pid)
                assert children, 'Cannot identify isolated Codex child process'
                started = time.monotonic()
                for child in children:
                    try:
                        if os.getpgid(child) == process.pid:
                            os.kill(child, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                with response:
                    tail = response.read().decode()
                assert time.monotonic() - started < 5, 'HTTP response did not fail promptly'
                assert 'runtime_disconnected' in tail, tail[:1000]
                return 'isolated Codex crash promptly produced runtime_disconnected'
            check('codex_crash', crash)
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            log.close()
    report = Path(__file__).resolve().parents[1] / 'docs/live-codex-results.json'
    history = json.loads(report.read_text()).get('history', []) + [json.loads(report.read_text()).get('results', [])] if report.exists() else []
    report.write_text(json.dumps({'tested_at': datetime.now(timezone.utc).isoformat(), 'history': history, 'codex_version': subprocess.check_output(['codex','--version'], text=True).strip(), 'results':results}, ensure_ascii=False, indent=2) + '\n')
    if not all(r['passed'] for r in results):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
