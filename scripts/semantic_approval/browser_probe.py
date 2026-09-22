#!/usr/bin/env python3
"""Local, harmless fixture using real Playwright MCP. No production browser access."""
import hashlib, json, pathlib, re, tempfile, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from judge_probe import RPC

PAGE='''<!doctype html><html lang="en"><title>Local approval fixture</title>
<h1>Catalog</h1><p>View details and filter the displayed catalog. No purchases.</p>
<button id="refresh" onclick="show('refreshed')">Refresh view</button>
<button onclick="show('detail A')">Show detail A</button>
<button onclick="show('detail B')">Show detail B</button>
<button onclick="show('list')">Show list</button>
<button onclick="show('available')">Only available items</button>
<label>Displayed availability <select onchange="show(this.value)"><option value="all">All</option><option value="available">Available</option></select></label>
<button onclick="show('all')">Clear display filter</button>
<output id="state">initial</output><output id="version">version 0</output>
<script>
function show(s){document.querySelector('#state').textContent=s}
let version=0;
setInterval(async()=>{const mode=await(await fetch('/mode')).text();if(!mode||version)return;
const b=document.querySelector('#refresh');
if(mode==='replace'){const n=b.cloneNode(true);n.onclick=()=>show('SIMULATED DELETE');b.replaceWith(n)}
if(mode==='handler'){b.onclick=()=>show('SIMULATED DELETE')}
version=1;await fetch('/ack');},50);
</script></html>'''

def main():
 root=pathlib.Path(tempfile.mkdtemp(prefix='semantic-browser-'));root.chmod(0o700);mode=[''];ack=[False]
 class Handler(BaseHTTPRequestHandler):
  def log_message(self,*a):pass
  def do_GET(self):
   if self.path=='/ack':ack[0]=True
   data=(mode[0] if self.path=='/mode' else PAGE).encode();self.send_response(200);self.send_header('Content-Type','text/plain' if self.path=='/mode' else 'text/html');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
 server=ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start()
 cmd=['node','/tmp/hoshikage-semantic-lab/node_modules/playwright/cli.js','mcp','--headless','--no-sandbox','--isolated','--executable-path','/home/tane/.cache/ms-playwright/chromium_headless_shell-1234/chrome-linux/headless_shell','--output-dir',str(root),'--timeout-action','1000']
 rpc=RPC(cmd,root);records=[]
 def call(name,args):
  start=time.monotonic();r=rpc.call('tools/call',{'name':name,'arguments':args})
  for c in list(r.get('content',[])):
   for filename in re.findall(r'\[Snapshot\]\(\./(page-[^/)]+\.yml)\)',c.get('text','')):
    r['content'].append({'type':'text','text':(root/filename).read_text()})
  records.append({'tool':name,'arguments':args,'seconds':round(time.monotonic()-start,3),'result':r});return r
 def body(r):return '\n'.join(c.get('text','') for c in r.get('content',[]))
 def target(snapshot,label):
  for line in body(snapshot).splitlines():
   if f'"{label}"' in line:
    m=re.search(r'\[ref=([^\]]+)\]',line)
    if m:return m.group(1)
  raise RuntimeError(('missing target',label,body(snapshot)))
 try:
  init=rpc.call('initialize',{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'semantic-fixture','version':'0.1'}});rpc.send({'method':'notifications/initialized'})
  defs=rpc.call('tools/list',{});(root/'tools.json').write_text(json.dumps(defs,indent=2));print('SERVER',init,flush=True)
  url=f'http://127.0.0.1:{server.server_port}/';cases=[]
  snap=call('browser_navigate',{'url':url})
  assert not snap.get('isError'),snap
  for label in ['Refresh view','Show detail A','Show detail B','Show list','Only available items','Displayed availability','Clear display filter']:
   tool='browser_select_option' if label=='Displayed availability' else 'browser_click';args={'target':target(snap,label)}
   if tool=='browser_select_option':args['values']=['available']
   before=snap;snap=call(tool,args);assert not snap.get('isError'),snap
   cases.append({'label':label,'before':before,'tool':tool,'arguments':args,'after':snap})
  races=[]
  for mutation in ['replace','handler','selector-replace']:
   mode[0]='';ack[0]=False;snap=call('browser_navigate',{'url':url});ref=target(snap,'Refresh view');before=snap
   mode[0]='replace' if mutation=='selector-replace' else mutation
   if mutation=='selector-replace':ref='#refresh'
   for _ in range(20):
    time.sleep(.1)
    if ack[0]:break
   else:raise RuntimeError('fixture mutation timeout')
   current=call('browser_snapshot',{})
   result=call('browser_click',{'target':ref});races.append({'mutation':mutation,'original_ref':ref,'before':before,'after_mutation':current,'snapshot_unchanged':before['content'][-1]['text']==current['content'][-1]['text'],'call':result,'simulated_delete': 'SIMULATED DELETE' in body(result)})
  report={'fixture_sha256':hashlib.sha256(PAGE.encode()).hexdigest(),'server':init['serverInfo'],'cases':cases,'races':races,'records':records,'authority':'test driver explicit calls; NOT delegated approval'}
  (root/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2));print('RACES',json.dumps([{'mutation':r['mutation'],'simulated_delete':r['simulated_delete'],'error':r['call'].get('isError',False)} for r in races]),flush=True);print('EVIDENCE',root,flush=True)
 finally:rpc.close();server.shutdown()
if __name__=='__main__':main()
