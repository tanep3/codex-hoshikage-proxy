#!/usr/bin/env python3
"""Local read-only MCP; returns fixture text, never operates a browser."""
import json
import pathlib
import sys
catalog = json.loads(pathlib.Path(sys.argv[1]).read_text())['data'][0]['tools']
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request.get('method')
    if method == 'initialize':
        result = {'protocolVersion': request['params']['protocolVersion'], 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'isolated-read-fixture', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [catalog['browser_find']]}
    elif method == 'tools/call':
        result = {'content': [{'type': 'text', 'text': 'LOCAL_READ_RESULT'}], 'isError': False}
    else:
        result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
