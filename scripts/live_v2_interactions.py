#!/usr/bin/env python3
"""Opt-in isolated real-Codex MCP form relay test; no Discord posts or production changes."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request
import uuid


def main():
    repo = Path(__file__).resolve().parents[1]
    binary = repo / 'target/debug/codex-hoshikage-proxy'
    mcp = repo / 'scripts/fixtures/elicitation_mcp.py'
    with tempfile.TemporaryDirectory(prefix='hoshikage-relay-live-') as temporary:
        root = Path(temporary)
        work = root / 'work'
        work.mkdir()
        home = root / 'proxy/codex-home'
        home.mkdir(parents=True)
        auth = Path(os.environ.get('CODEX_HOME', str(Path.home() / '.codex'))) / 'auth.json'
        shutil.copyfile(auth, home / 'auth.json')
        (home / 'auth.json').chmod(0o600)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        key = str(uuid.uuid4())
        args = ['-c', 'mcp_servers.interaction_test.command="python3"',
                '-c', 'mcp_servers.interaction_test.args=' + json.dumps([str(mcp)]),
                'app-server', '--listen', 'stdio://']
        config = root / 'config.toml'
        config.write_text(f'''[server]
port = {port}
default_cwd = {json.dumps(str(work))}
[codex]
inherit_global_config = false
command = {json.dumps(shutil.which('codex'))}
args = {json.dumps(args)}
[codex.sandbox]
mode = "workspace-write"
[security]
api_key = "{key}"
allowed_cwds = [{json.dumps(str(work))}]
[approval]
auto_approve_workspace = false
[providers.hoshikage]
enabled = false
codex_id = "hoshikage"
[providers.chatgpt]
enabled = true
codex_id = "openai"
''')
        config.chmod(0o600)
        env = dict(os.environ, CODEX_HOSHIKAGE_PROXY_HOME=str(root / 'proxy'),
                   CODEX_HOSHIKAGE_PROXY_CONFIG=str(config))
        identity = {}
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        def call(path, body=None, operation=None):
            headers = {'Authorization': 'Bearer ' + key, **identity}
            if operation:
                headers['Idempotency-Key'] = operation
            if body is not None:
                headers['Content-Type'] = 'application/json'
            request = urllib.request.Request(f'http://127.0.0.1:{port}/v2/codex/' + path,
                                             headers=headers,
                                             data=None if body is None else json.dumps(body).encode())
            with opener.open(request, timeout=20) as response:
                return json.load(response)

        process = None
        try:
            with (root / 'proxy.log').open('wb') as log:
                process = subprocess.Popen([str(binary)], env=env, cwd=work, stdout=log, stderr=log,
                                           start_new_session=True)
            for _ in range(150):
                if process.poll() is not None:
                    raise RuntimeError('isolated Proxy exited during startup')
                try:
                    cap = call('capabilities')
                    identity.update({'X-Proxy-Instance-Id': cap['instance_id'],
                                     'X-Proxy-Recovery-Generation': cap['recovery_generation']})
                    break
                except OSError:
                    time.sleep(.1)
            else:
                raise RuntimeError('isolated Proxy startup timeout')
            c = call('conversations', {'workspace': {'mode': 'automatic'},
                                      'model': 'chatgpt/gpt-5.6-luna'}, 'live-conversation')
            cid = c['resource']['id']
            r = call(f'conversations/{cid}/responses', {
                'input': 'Call the interaction_test MCP confirm tool exactly once. It only asks for a harmless local test confirmation. Do not use shell commands, edit files, or perform external actions. After the tool succeeds, reply ELICITATION_ACCEPTED.',
                'interaction_capabilities': ['mcp_form']}, 'live-response')
            rid = r['resource']['id']
            deadline = time.monotonic() + 180
            answered = set()
            for stage in ('tool_approval', 'form'):
                interaction = None
                while time.monotonic() < deadline:
                    items = call(f'responses/{rid}/interactions')['data']
                    pending = [i for i in items if i['state'] == 'pending' and i['interaction_id'] not in answered]
                    if pending:
                        interaction = pending[0]
                        break
                    state = call(f'responses/{rid}')
                    if state['phase'] in ('finished', 'unknown', 'rejected', 'cancelled'):
                        raise RuntimeError(f'No MCP form before terminal state: {state["phase"]}')
                    time.sleep(.25)
                if interaction is None:
                    raise RuntimeError('MCP form was not received; request is not retried')
                assert interaction['kind'] == 'mcp_form'
                request = interaction['request']
                assert request['serverName'] == 'interaction_test'
                if stage == 'tool_approval':
                    assert request['_meta']['codex_approval_kind'] == 'mcp_tool_call'
                    assert request['message'] == 'Allow the interaction_test MCP server to run tool "confirm"?'
                    assert request['requestedSchema']['properties'] == {}
                    content = {}
                else:
                    assert request['requestedSchema']['properties']['approved']['type'] == 'boolean'
                    content = {'approved': True}
                iid = interaction['interaction_id']
                answer = {'expected_revision': interaction['revision'],
                          'response': {'action': 'accept', 'content': content}}
                op = call(f'interactions/{iid}/reply', answer, 'live-answer-' + stage)
                replay = call(f'interactions/{iid}/reply', answer, 'live-answer-' + stage)
                assert op['operation_id'] == replay['operation_id']
                answered.add(iid)
            while time.monotonic() < deadline:
                state = call(f'responses/{rid}')
                if state['output']['state'] == 'ready':
                    break
                if state['phase'] == 'unknown':
                    raise RuntimeError('execution result unknown; request is not retried')
                time.sleep(.25)
            else:
                raise RuntimeError('Output not ready before deadline')
            output = call(f'responses/{rid}/output')
            assert 'ELICITATION_ACCEPTED' in json.dumps(output)
            final = call(f'interactions/{iid}')
            assert final['state'] == 'resolved' and final['request'] is None
            print(json.dumps({'result': 'PASS', 'kind': 'mcp_form', 'reply_status': final['reply_status'],
                              'same_operation_on_replay': True, 'output_sha256': hashlib.sha256(json.dumps(output, sort_keys=True).encode()).hexdigest()}))
        except Exception:
            log_path = Path('/tmp/hoshikage-relay-live-failure.log')
            if (root / 'proxy.log').exists():
                log_path.touch(mode=0o600, exist_ok=True)
                log_path.chmod(0o600)
                log_path.write_text((root / 'proxy.log').read_text(errors='replace').replace(key, '[redacted]'))
            raise
        finally:
            if process is not None and process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()


if __name__ == '__main__':
    main()
