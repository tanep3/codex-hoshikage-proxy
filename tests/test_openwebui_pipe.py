import asyncio
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import unittest

import httpx
from pydantic import SecretStr
from openwebui.codex_hoshikage_pipe import Pipe


class Events(httpx.AsyncByteStream):
    def __init__(self, events):
        self.events = events

    async def __aiter__(self):
        for name, value in self.events:
            yield f'event: {name}\ndata: {json.dumps(value)}\n\n'.encode()
        await asyncio.Future()


class ApprovalTests(unittest.IsolatedAsyncioTestCase):
    def test_proxy_key_and_provider_authentication_errors_are_distinct_and_redacted(self):
        self.assertIn(
            'Proxy APIキー',
            Pipe._proxy_error(401, b'{"error":{"code":"invalid_api_key","message":"secret"}}'),
        )
        provider = Pipe._proxy_error(
            401,
            b'{"error":{"code":"provider_authentication_required","message":"token abc"}}',
        )
        self.assertIn('Codexのログイン', provider)
        self.assertNotIn('token abc', provider)
        unknown = Pipe._proxy_error(
            500,
            b'{"error":{"code":"upstream_failed","message":"Bearer highly-secret"}}',
        )
        self.assertEqual(unknown, 'Proxyでエラーが発生しました（HTTP 500, upstream_failed）。')
        self.assertNotIn('highly-secret', unknown)

    def test_proxy_api_key_is_a_secret_and_only_sent_as_bearer_header(self):
        pipe = Pipe()
        pipe.valves.PROXY_API_KEY = SecretStr('secret-key')
        headers = pipe._headers()
        self.assertEqual(headers['authorization'], 'Bearer secret-key')
        self.assertNotIn('secret-key', repr(pipe.valves.PROXY_API_KEY))

    async def test_overlapping_approvals_are_both_presented(self):
        pending = ['a', 'b']
        shown = []
        posted = []
        first_shown = asyncio.Event()
        allow_first = asyncio.Event()
        all_done = asyncio.Event()

        def event(a):
            return dict(approval_id=a, turnId='turn', threadId='thread', availableDecisions=['accept', 'cancel'])

        async def handler(req):
            if req.url.path.endswith('/events/stream'):
                return httpx.Response(200, stream=Events([('approval_requested', event(a)) for a in pending]))
            if req.url.path.endswith('/approvals'):
                return httpx.Response(200, json={'data': [event(a) for a in pending]})
            a = req.url.path.split('/')[-1]
            if req.method == 'GET':
                return httpx.Response(200, json={'id': a, 'state': 'pending', 'available_decisions': ['accept', 'cancel'], 'details': {'turnId': 'turn', 'threadId': 'thread', 'command': 'echo '+a}})
            posted.append(json.loads(req.content))
            pending.remove(a)
            if not pending:
                all_done.set()
            return httpx.Response(200, json={'state': 'approved'})

        async def confirm(value):
            shown.append(value)
            if len(shown) == 1:
                first_shown.set()
                await allow_first.wait()
            return True

        async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
            task = asyncio.create_task(Pipe()._watch_approvals(client, 'turn', confirm))
            try:
                await asyncio.wait_for(first_shown.wait(), 1)
                await asyncio.sleep(.02)  # Second event arrives while the first dialog is open.
                allow_first.set()
                await asyncio.wait_for(all_done.wait(), 2)
                self.assertEqual(len(shown), 2)
                self.assertTrue(all(p.get('expected_turn_id') == 'turn' for p in posted))
                self.assertIn('echo a', shown[0]['data']['message'])
            finally:
                task.cancel()
                await asyncio.gather(task, return_exceptions=True)

    async def test_approval_monitor_error_reaches_generation(self):
        async def handler(req):
            if req.method == 'POST':
                return httpx.Response(200, headers={'x-codex-turn-id': 'turn'}, stream=Events([('response.created', {'id': 'r'})]))
            return httpx.Response(503, json={'error': 'monitor unavailable'})

        async def confirm(_):
            return True

        async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
            async def consume():
                return [s async for s in Pipe()._stream_responses(client, {}, 'chat', 'model', confirm, None)]
            with self.assertRaisesRegex(RuntimeError, 'Approval monitoring failed'):
                await asyncio.wait_for(consume(), 1)

    async def test_expired_dialog_is_cancelled_without_post(self):
        shown = asyncio.Event()
        cancelled = asyncio.Event()
        requests = 0

        async def handler(req):
            nonlocal requests
            self.assertEqual(req.method, 'GET')
            if req.url.path.endswith('/approvals'):
                requests += 1
                pending = [{'approval_id': 'a'}] if requests == 1 else []
                return httpx.Response(200, json={'data': pending})
            return httpx.Response(200, json={'id': 'a', 'state': 'pending',
                'available_decisions': ['accept', 'cancel'], 'details': {'turnId': 'turn'}})

        async def confirm(_):
            shown.set()
            try:
                await asyncio.Future()
            finally:
                cancelled.set()

        async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
            task = asyncio.create_task(Pipe()._watch_approvals(client, 'turn', confirm))
            try:
                await asyncio.wait_for(shown.wait(), 1)
                await asyncio.wait_for(cancelled.wait(), 1)
            finally:
                task.cancel()
                await asyncio.gather(task, return_exceptions=True)

    async def test_completed_stream_closes_monitor(self):
        async def handler(req):
            if req.method == 'POST':
                return httpx.Response(200, headers={'x-codex-turn-id': 'turn'},
                    stream=Events([('response.created', {'id': 'r'}),
                        ('response.output_text.delta', {'delta': 'OK'}),
                        ('response.completed', {'id': 'r'})]))
            return httpx.Response(200, json={'data': []})

        async def confirm(_):
            self.fail('no approval was requested')

        pipe = Pipe()
        async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
            async def consume():
                return [s async for s in pipe._stream_responses(client, {}, 'chat', 'model', confirm, None)]
            self.assertEqual(await asyncio.wait_for(consume(), 1), ['OK'])
        self.assertEqual(pipe._previous_response_id('chat', 'model'), 'r')

    async def test_actual_proxy_approval_round_trip(self):
        repo = Path(__file__).resolve().parents[1]
        binary = repo / 'target/debug/codex-hoshikage-proxy'
        fake = repo / 'target/debug/fake_codex'
        if not binary.exists() or not fake.exists():
            self.skipTest('cargo build --bins is required for the HTTP integration test')
        with tempfile.TemporaryDirectory(prefix='pipe-approval-') as temporary:
            root = Path(temporary)
            work = root / 'work'
            work.mkdir()
            with socket.socket() as listener:
                listener.bind(('127.0.0.1', 0))
                port = listener.getsockname()[1]
            config = root / 'config.toml'
            config.write_text(f'''[server]
port = {port}
default_cwd = {json.dumps(str(work))}
[codex]
command = {json.dumps(str(fake))}
args = ["--approval"]
[security]
api_key = "isolated-test-key"
allowed_cwds = [{json.dumps(str(work))}]
[defaults]
model = "chatgpt/gpt-test-first"
[approval]
auto_approve_workspace = false
[providers.hoshikage]
codex_id = "hoshikage"
enabled = false
[providers.chatgpt]
enabled = true
codex_id = "openai"
''')
            env = dict(os.environ, CODEX_HOSHIKAGE_PROXY_CONFIG=str(config),
                CODEX_HOSHIKAGE_PROXY_HOME=str(root / 'proxy'))
            log_file = (root / "proxy.log").open("w")
            process = subprocess.Popen([str(binary)], env=env, cwd=work,
                stdout=log_file, stderr=log_file)
            pipe = Pipe()
            pipe.valves.PROXY_BASE_URL = f'http://127.0.0.1:{port}'
            pipe.valves.PROXY_API_KEY = 'isolated-test-key'
            shown = []

            async def confirm(event):
                shown.append(event)
                return True

            try:
                async with httpx.AsyncClient() as client:
                    for _ in range(100):
                        self.assertIsNone(process.poll(), (root / 'proxy.log').read_text())
                        try:
                            response = await client.get(pipe._base_url()+'/readyz', headers=pipe._headers())
                            if response.status_code == 200:
                                break
                        except (httpx.ConnectError, httpx.ReadError):
                            pass
                        await asyncio.sleep(.05)
                    else:
                        self.fail('isolated Proxy did not become ready')
                async def consume():
                    return ''.join([text async for text in pipe.pipe(
                        {'model': 'codex/chatgpt/gpt-test-first',
                         'messages': [{'role': 'user', 'content': 'test approval'}]},
                        __chat_id__='isolated-chat', __user__={'id': 'test-user'},
                        __event_call__=confirm)])
                output = await asyncio.wait_for(consume(), 10)
                self.assertIn('approved response', output)
                self.assertEqual(len(shown), 1)
                self.assertIn('echo approval', shown[0]['data']['message'])
            finally:
                process.terminate()
                await asyncio.to_thread(process.wait, timeout=10)
                log_file.close()

    def test_multimodal_input_survives_first_and_continued_turns(self):
        pipe = Pipe()
        image = {'type': 'image_url', 'image_url': {'url': 'data:image/png;base64,AA=='}}
        body = {'messages': [{'role': 'user', 'content': [{'type': 'text', 'text': 'What is this?'}, image]}]}
        for continued in [False, True]:
            if continued:
                pipe._response_ids['chat'] = ('model', 'previous')
            parts = pipe._responses_input(body, {}, 'chat', 'model')
            self.assertIn(image, parts)
            self.assertTrue(any(p.get('text') == 'What is this?' for p in parts))

    async def test_uploaded_file_image_resolves_once_with_user_context(self):
        pipe = Pipe()
        calls = []
        async def read(file_id, user):
            calls.append((file_id, user))
            return 'data:image/png;base64,AA==', 'image/png'
        pipe._file_image = read
        user = {'id': 'owner'}
        parts = [{'type': 'input_image', 'image_url': '/api/v1/files/abc/content'}]
        files = [{'id': 'abc', 'content_type': 'image/png'}]
        result = await pipe._resolve_images(parts, files, user)
        self.assertEqual(calls, [('abc', user)])
        self.assertEqual(result, [{'type': 'input_image', 'image_url': 'data:image/png;base64,AA=='}])

    async def test_file_only_upload_is_added_and_access_failure_is_not_ignored(self):
        pipe = Pipe()
        async def read(file_id, user):
            if user['id'] != 'owner':
                raise PermissionError('not allowed')
            return 'data:image/png;base64,AA==', 'image/png'
        pipe._file_image = read
        files = [{'id': 'abc', 'content_type': 'image/png'}]
        result = await pipe._resolve_images([{'type': 'text', 'text': 'describe'}], files, {'id': 'owner'})
        self.assertEqual(result[-1]['type'], 'input_image')
        with self.assertRaises(PermissionError):
            await pipe._resolve_images([], files, {'id': 'another-user'})


if __name__ == '__main__':
    unittest.main()
