#!/usr/bin/env python3
"""Read-only local MCP for live configuration reload acceptance."""
import json
import sys
for line in sys.stdin:
    r = json.loads(line)
    if 'id' not in r:
        continue
    method = r.get('method')
    if method == 'initialize':
        result = {'protocolVersion': r['params']['protocolVersion'], 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'reload-probe', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': 'reload_probe', 'description': 'Read-only local configuration reload test; returns RELOAD_OK.', 'inputSchema': {'type': 'object', 'properties': {}, 'additionalProperties': False}, 'annotations': {'readOnlyHint': True, 'destructiveHint': False, 'openWorldHint': False}}]}
    elif method == 'tools/call':
        result = {'content': [{'type': 'text', 'text': 'RELOAD_OK'}], 'isError': False}
    else:
        result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': r['id'], 'result': result}), flush=True)
