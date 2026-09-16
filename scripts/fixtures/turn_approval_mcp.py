#!/usr/bin/env python3
"""Local harmless MCP fixture. Never evaluates supplied code or accesses a browser."""
import json
import sys
for line in sys.stdin:
    r = json.loads(line)
    if 'id' not in r:
        continue
    if r.get('method') == 'initialize':
        result = {'protocolVersion': r['params']['protocolVersion'], 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'turn-approval-test', 'version': '1'}}
    elif r.get('method') == 'tools/list':
        result = {'tools': [{'name': name, 'description': 'Harmless approval test, returns a fixed string; code is never executed.', 'inputSchema': {'type': 'object', 'properties': {'function': {'type': 'string'}}, 'required': ['function'], 'additionalProperties': False}} for name in ['browser_evaluate', 'read_test', 'browser_run_code_unsafe']]}
    else:
        result = {'content': [{'type': 'text', 'text': 'LOCAL_TEST_ONLY'}], 'isError': False}
    print(json.dumps({'jsonrpc': '2.0', 'id': r['id'], 'result': result}), flush=True)
