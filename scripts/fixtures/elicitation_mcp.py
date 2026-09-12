#!/usr/bin/env python3
"""Local test-only MCP server. It performs no external action."""
import json
import sys


def send(message):
    print(json.dumps(message), flush=True)


pending = None
for line in sys.stdin:
    request = json.loads(line)
    method = request.get('method')
    request_id = request.get('id')
    if method is None and request_id == 'confirm-1' and pending is not None:
        result = request.get('result', {})
        ok = result.get('action') == 'accept' and result.get('content') == {'approved': True}
        send({'jsonrpc': '2.0', 'id': pending, 'result': {
            'content': [{'type': 'text', 'text': 'ELICITATION_ACCEPTED' if ok else 'ELICITATION_DECLINED'}],
            'isError': not ok}})
        pending = None
    elif method == 'initialize':
        send({'jsonrpc': '2.0', 'id': request_id, 'result': {
            'protocolVersion': request['params']['protocolVersion'],
            'capabilities': {'tools': {}},
            'serverInfo': {'name': 'hoshikage-interaction-test', 'version': '1.0'}}})
    elif method == 'tools/list':
        send({'jsonrpc': '2.0', 'id': request_id, 'result': {'tools': [{
            'name': 'confirm', 'description': 'Ask for a harmless local test confirmation. No external action.',
            'inputSchema': {'type': 'object', 'properties': {}, 'additionalProperties': False}}]}})
    elif method == 'tools/call':
        pending = request_id
        send({'jsonrpc': '2.0', 'id': 'confirm-1', 'method': 'elicitation/create', 'params': {
            'mode': 'form', 'message': 'Confirm this local integration test (no external action).',
            'requestedSchema': {'type': 'object', 'properties': {'approved': {'type': 'boolean'}},
                                'required': ['approved']}}})
    elif method == 'ping':
        send({'jsonrpc': '2.0', 'id': request_id, 'result': {}})
    elif method is not None and request_id is not None:
        send({'jsonrpc': '2.0', 'id': request_id, 'error': {'code': -32601, 'message': 'Unknown method'}})
