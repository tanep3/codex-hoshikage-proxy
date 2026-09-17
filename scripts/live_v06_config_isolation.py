#!/usr/bin/env python3
"""Isolated Codex 0.153.4 config-switch proof; no model request or real MCP.

Leaves the temporary directory and result.json as evidence. Does not read or
change the operator's CODEX_HOME. Uses only a local fake MCP and injected local
history marker, no AI Turn and no interaction with the production service.
"""
import json, os, pathlib, subprocess, tempfile, threading, queue, time
root=pathlib.Path(tempfile.mkdtemp(prefix='proxy-v06-config-'))
(root/'config.toml').write_text('model = "test-model"\nmodel_provider = "probe"\n[model_providers.probe]\nname = "Probe"\nbase_url = "http://127.0.0.1:1/v1"\nwire_api = "responses"\nrequires_openai_auth = false\n')
mcp=root/'mcp.py'
mcp.write_text('''import sys,json
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r:continue
 m=r.get('method')
 v={'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'probe','version':'1'}} if m=='initialize' else {'tools':[{'name':sys.argv[1],'description':'harmless test','inputSchema':{'type':'object','properties':{}}}]} if m=='tools/list' else {}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':v}),flush=True)
''')
err=(root/'stderr').open('w')
p=subprocess.Popen([os.environ.get('CODEX_TEST_COMMAND', '/home/tane/.local/bin/codex'),'app-server'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=err,text=True,env={**os.environ,'CODEX_HOME':str(root)})
q=queue.Queue()
def reader():
 for line in p.stdout:
  try:q.put(json.loads(line))
  except ValueError:pass
threading.Thread(target=reader,daemon=True).start()
n=0
def call(method,params):
 global n
 n+=1;i=n;p.stdin.write(json.dumps({'id':i,'method':method,'params':params})+'\n');p.stdin.flush()
 deadline=time.monotonic()+45
 while time.monotonic()<deadline:
  try:r=q.get(timeout=max(.01,deadline-time.monotonic()))
  except queue.Empty:break
  if r.get('id')==i:
   if 'error' in r:raise RuntimeError((method,r['error']))
   return r['result']
 raise TimeoutError(method)
def config(tool):return {'mcp_servers.probe':{'command':'python3','args':[str(mcp),tool]}}
def names(t):
 r=call('mcpServerStatus/list',{'threadId':t,'detail':'toolsAndAuthOnly','limit':100})
 return {s['name']:sorted(s['tools']) for s in r['data']}
try:
 call('initialize',{'clientInfo':{'name':'hoshikage-isolated-probe','version':'1'},'capabilities':{'experimentalApi':True}})
 p.stdin.write('{"method":"initialized"}\n');p.stdin.flush()
 a=call('thread/start',{'cwd':str(root),'config':config('alpha'),'approvalPolicy':'on-request','approvalsReviewer':'user'})
 t=a['thread']['id'];before=names(t)
 call('thread/inject_items',{'threadId':t,'items':[{'type':'message','role':'user','content':[{'type':'input_text','text':'isolated history marker; do not execute'}]}]})
 b=call('thread/start',{'cwd':str(root),'config':config('beta'),'approvalPolicy':'on-request','approvalsReviewer':'user'})
 other=b['thread']['id'];other_before=names(other)
 unsub=call('thread/unsubscribe',{'threadId':t})
 resumed=call('thread/resume',{'threadId':t,'config':config('gamma'),'approvalPolicy':'never','approvalsReviewer':'user'})
 after=names(t);other_after=names(other)
 assert resumed['thread']['id']==t
 assert before=={'probe':['alpha']} and after=={'probe':['gamma']}
 assert other_before==other_after=={'probe':['beta']}
 assert resumed['approvalPolicy']=='never' and resumed['approvalsReviewer']=='user'
 call('thread/unsubscribe',{'threadId':t})
 cleared=call('thread/resume',{'threadId':t,'config':{},'approvalPolicy':'on-request','approvalsReviewer':'user'})
 cleared_names=names(t)
 assert cleared_names=={},cleared_names
 result={'same_thread':True,'before':before,'after':after,'other_unchanged':other_after,'cleared':cleared_names,'unsubscribe':unsub,'turns_started':0,'directory':str(root)}
 (root/'result.json').write_text(json.dumps(result,indent=2));print(json.dumps(result))
finally:
 p.terminate()
 try:p.wait(timeout=5)
 except subprocess.TimeoutExpired:p.kill();p.wait()
 err.close()
