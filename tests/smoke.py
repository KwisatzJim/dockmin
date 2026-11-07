#!/usr/bin/env python3
"""Exercise the real HTTP service against disposable, simulated SSH hosts.
No Docker daemon or LAN machines are contacted. Uses only Python's stdlib.
"""
import http.client
import json
import os
from pathlib import Path
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.request
import urllib.error
import http.cookiejar

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT / 'target/debug/dockmin'

FAKE_SSH = r'''#!/usr/bin/env python3
import sys,time,json,os
args=sys.argv[1:]
if '-G' in args:
    print('hostname 127.0.0.1\nport 22\nuser test\nidentityfile /nonexistent/dockmin-identity');sys.exit(0)
assert 'StrictHostKeyChecking=yes'  in args and 'BatchMode=yes' in args
assert 'GlobalKnownHostsFile=/dev/null' in args
remote=args[-1]
if remote=="exec sh -s":remote=sys.stdin.read()
if "data=json.loads(base64.b64decode('" in remote:
    import base64,re
    encoded=re.search(r"data=json.loads\(base64.b64decode\('([^']+)'",remote).group(1)
    payload=json.loads(base64.b64decode(encoded))
    if 'pull' in payload:
        remote="cd -- '"+payload['stack']['directory']+"' && docker compose pull && docker compose up -d"
if remote.startswith("printf %s "):
    import base64
    remote=base64.b64decode(remote.split()[2]).decode()
with open(os.environ['DOCKMIN_FAKE_LOG'],'a') as log: log.write(json.dumps({'command':remote,'start':time.time()})+'\n')
if "'slow'" in remote or '/srv/slow' in remote:
    time.sleep(30)
elif '/srv/disconnected' in remote:
    print('SSH transport lost',file=sys.stderr);sys.exit(255)
elif 'ps --all --format json' in remote:
    time.sleep(.15)
    print('Compose warning on stderr',file=sys.stderr)
    print(json.dumps([{'Service':'web','Name':'test-web-1','State':'running','Health':'healthy','Image':'nginx:latest','Publishers':[{'PublishedPort':8080,'TargetPort':80,'Protocol':'tcp'}]}]))
elif 'imagetools inspect' in remote:
    marker=os.environ['DOCKMIN_FAKE_LOG']+'.updated'
    digest='old' if '/srv/available' in remote and not os.path.exists(marker) else 'current'
    print('DOCKMIN_IMAGE nginx:latest\nLOCAL\n'+json.dumps({'Os':'linux','Architecture':'amd64','RepoDigests':['nginx@sha256:'+digest]})+'\nREMOTE\n{\n"digest":"sha256:current"\n}\nEND_IMAGE')
elif '--follow' in remote:
    open(os.environ['DOCKMIN_FAKE_LOG']+'.follow-pid','w').write(str(os.getpid()))
    for i in range(1000):print('live container line '+str(i),flush=True);time.sleep(.1)
elif 'docker inspect --format' in remote:
    print(json.dumps({'project':'discovered','directory':'/srv/discovered','files':'compose.yml','name':'/discovered-web-1'}))
elif ' logs ' in remote:
    print('x'*160000)
elif 'pull &&' in remote:
    if '/srv/available' in remote:open(os.environ['DOCKMIN_FAKE_LOG']+'.updated','w').close()
    print('Pulling images',flush=True);time.sleep(.4);print('Stack recreated',flush=True)
else:
    print('Docker 28 · Compose v2')
'''

with tempfile.TemporaryDirectory(prefix='dockmin-smoke-') as folder:
    temp = Path(folder)
    (temp/'bin').mkdir()
    ssh = temp/'bin/ssh';ssh.write_text(FAKE_SSH);ssh.chmod(0o755)
    (temp/'ssh_config').write_text('Host pc1 pc2\n  HostName 127.0.0.1\nHost *\n  User test\n')
    subprocess.run(['ssh-keygen','-q','-t','ed25519','-N','','-f',str(temp/'scan-key')],check=True)
    subprocess.run(['ssh-keygen','-q','-t','ed25519','-N','','-f',str(temp/'replacement-key')],check=True)
    scan=temp/'bin/ssh-keyscan';scan.write_text("#!/usr/bin/env python3\nimport os\nprint('127.0.0.1 '+' '.join(open(os.environ['DOCKMIN_SCAN_PUBLIC']).read().split()[:2]))\n");scan.chmod(0o755)
    scanned_public=temp/'scanned.pub';scanned_public.write_text((temp/'scan-key.pub').read_text())
    log=temp/'ssh.log'  
    sock=socket.socket();sock.bind(('127.0.0.1',0));port=sock.getsockname()[1];sock.close()
    origin=f'http://127.0.0.1:{port}'
    config=temp/'dockmin.toml'
    config.write_text(f'''listen = "127.0.0.1:{port}"
origins = ["{origin}"]
database = "{temp}/db.sqlite"
static_dir = "{ROOT}/web/dist"
ssh_config = "{temp}/ssh_config"
known_hosts = "{temp}/known_hosts"
service_identity = "{temp}/scan-key"
secure_cookie = false
concurrency = 2
command_timeout_seconds = 10
health_refresh_seconds = 0
''')
    env={**os.environ,'PATH':f'{temp}/bin:'+os.environ['PATH'],'DOCKMIN_FAKE_LOG':str(log),'DOCKMIN_SCAN_PUBLIC':str(scanned_public)}
    subprocess.run([str(BINARY),'--config',str(config),'set-password'],input='test-only-password-123\n',text=True,env=env,check=True,capture_output=True)
    with sqlite3.connect(temp/'db.sqlite') as db:db.execute("INSERT OR REPLACE INTO settings VALUES('automation',?)",[json.dumps({'enabled':False,'interval_minutes':1440,'stack_ids':[]})])
    output=open(temp/'service.log','w')
    process=None
    def start():
        p=subprocess.Popen([str(BINARY),'--config',str(config)],env=env,stdout=output,stderr=output)
        for _ in range(100):
            try:
                urllib.request.urlopen(origin+'/api/health',timeout=.2).close();return p
            except OSError:
                if p.poll() is not None: raise RuntimeError('Service failed: '+(temp/'service.log').read_text())
                time.sleep(.1)
        raise RuntimeError('Service did not start')
    jar=http.cookiejar.CookieJar();client=urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
    csrf=''
    def request(path,body=None,headers=None,expected=200,opener=client):
        h={'Origin':origin,'Content-Type':'application/json','X-CSRF-Token':csrf};h.update(headers or {})
        req=urllib.request.Request(origin+path,data=json.dumps(body).encode() if body is not None else None,headers=h)
        try:
            with opener.open(req,timeout=5) as r: code=r.status;data=r.read()
        except urllib.error.HTTPError as e:code=e.code;data=e.read()
        assert code==expected,(path,code,expected,data)
        if data and data[:1] in (b'{',b'['):return json.loads(data)
        return data
    def wait(job):
        for _ in range(150):
            value=request('/api/jobs/'+job['id'])
            if value['finished']:return value
            time.sleep(.1)
        raise AssertionError('Job did not finish')
    def idle():
        for _ in range(150):
            if all(j['finished'] for j in request('/api/overview')['jobs']):return
            time.sleep(.1)
        raise AssertionError('Queue remained active')
    try:
        process=start()
        assert b'dockmin' in request('/').lower()
        request('/api/overview',expected=401,opener=urllib.request.build_opener())
        request('/api/login',{'password':'test-only-password-123'},headers={'Origin':'https://evil.example'},expected=403)
        request('/api/login',{'password':'wrong'},expected=401)
        session=request('/api/login',{'password':'test-only-password-123'});csrf=session['csrf']
        request('/api/hosts',{'id':'','name':'PC1','alias':'pc1'},headers={'X-CSRF-Token':'wrong'},expected=403)
        request('/api/hosts',{'id':'','name':'PC1','alias':'-oBad'},expected=400)
        host=request('/api/hosts',{'id':'','name':'PC1','alias':'pc1'})
        host2=request('/api/hosts',{'id':'','name':'PC2','alias':'pc2'})
        stack=request('/api/stacks',{'id':'','host_id':host['id'],'name':'Web','directory':"/srv/it's $(touch /tmp/dockmin-smoke-unsafe)",'project':'web','files':['compose.yml','prod.yml']})
        job=request('/api/stacks/'+stack['id']+'/actions',{'action':'refresh'})
        assert wait(job)['state']=='succeeded'
        view=request('/api/overview');assert view['statuses'][stack['id']]['services'][0]['State']=='running'
        assert 'error' not in view['statuses'][stack['id']], 'stderr warning corrupted status'
        request('/api/stacks/'+stack['id']+'/actions',{'action':'stop'},expected=400)
        request('/api/stacks/'+stack['id']+'/actions',{'action':'down --volumes','confirm':'Web'},expected=400)
        update=request('/api/stacks/'+stack['id']+'/actions',{'action':'update','confirm':'Web'})
        second=request('/api/stacks/'+stack['id']+'/actions',{'action':'restart','confirm':'Web'})
        assert second['state']=='queued'
        request('/api/hosts',host,expected=400)
        # Subscribe to SSE briefly, then disconnect. The server-owned update must finish.
        cookie='; '.join(f'{c.name}={c.value}' for c in jar)
        conn=http.client.HTTPConnection('127.0.0.1',port,timeout=5)
        conn.request('GET',f"/api/jobs/{update['id']}/events",headers={'Cookie':cookie})
        response=conn.getresponse();assert response.status==200
        line=response.readline();assert line.startswith(b'event: job')
        conn.close()
        assert wait(update)['state']=='succeeded';assert wait(second)['state']=='succeeded';idle()
        checked=request('/api/stacks/'+stack['id']+'/actions',{'action':'check-updates'})
        assert 'up to date' in wait(checked)['output']
        checked_all=request('/api/bulk',{'stack_ids':[stack['id']],'action':'check-updates'})
        assert len(checked_all['jobs'])==1
        assert 'up to date' in wait(checked_all['jobs'][0])['output']
        summary=next(j for j in request('/api/overview')['jobs'] if j['id']==checked_all['jobs'][0]['id'])
        assert 'up to date' in summary['result']
        assert request('/api/overview')['updates'][stack['id']]['state']=='current'
        available=request('/api/stacks',{'id':'','host_id':host2['id'],'name':'Available','directory':'/srv/available','project':'available','files':[]})
        check=request('/api/stacks/'+available['id']+'/actions',{'action':'check-updates'})
        wait(check)
        assert request('/api/overview')['updates'][available['id']]['state']=='available'
        count=len(request('/api/overview')['jobs'])
        request('/api/bulk',{'stack_ids':[available['id'],stack['id']],'action':'update','only_available':True,'confirm':'UPDATE SELECTED'},expected=400)
        assert len(request('/api/overview')['jobs'])==count, 'Stale eligibility queued a partial update'
        request('/api/bulk',{'stack_ids':[available['id']],'action':'update','only_available':True},expected=400)
        upgraded=request('/api/bulk',{'stack_ids':[available['id']],'action':'update','only_available':True,'confirm':'UPDATE SELECTED'})
        wait(upgraded['jobs'][0]);idle()
        assert request('/api/overview')['updates'][available['id']]['state']=='current', 'Completed updates were not checked again'

        logs=request('/api/stacks/'+stack['id']+'/actions',{'action':'logs'})
        result=wait(logs);assert len(result['output'])<132000 and 'truncated' in result['output']
        stack['directory']='/srv/web'
        stack=request('/api/stacks',stack)
        assert stack['id'] not in request('/api/overview')['statuses'], 'Edited stack retained status from previous target'
        assert request('/api/overview')['updates'][stack['id']]['state']=='unchecked'
        # Persisted settings, sanitized backup, import idempotence, and live logs.
        request('/api/automation',{'enabled':True,'interval_minutes':0,'stack_ids':[]},expected=400)
        request('/api/automation',{'enabled':True,'interval_minutes':60,'stack_ids':['missing']},expected=400)
        prefs=request('/api/settings');assert prefs['health_refresh_seconds']==0
        invalid={**prefs,'concurrency':0};request('/api/settings',invalid,expected=400)
        prefs['browser_poll_seconds']=7;request('/api/settings',prefs)
        assert request('/api/overview')['settings']['browser_poll_seconds']==7
        assert request('/api/ssh/aliases')['aliases']==['pc1','pc2']
        identity=request('/api/hosts/'+host['id']+'/identity');assert identity['hostname']=='127.0.0.1' and not identity['public_keys']
        scanned=request('/api/hosts/'+host['id']+'/keys',{});assert not scanned['has_trusted_keys']
        candidate=scanned['keys'][0]
        request('/api/hosts/'+host['id']+'/trust',{**candidate,'confirm':'wrong'},expected=400)
        request('/api/hosts/'+host['id']+'/trust',{**candidate,'fingerprint':'SHA256:wrong','confirm':'pc1'},expected=400)
        request('/api/hosts/'+host['id']+'/trust',{**candidate,'confirm':'pc1'})
        assert (temp/'known_hosts').stat().st_mode & 0o777==0o600
        scanned=request('/api/hosts/'+host['id']+'/keys',{});assert scanned['keys'][0]['trusted']
        known_bytes=(temp/'known_hosts').read_bytes()
        scanned_public.write_text((temp/'replacement-key.pub').read_text())
        replacement=request('/api/hosts/'+host['id']+'/keys',{})
        request('/api/hosts/'+host['id']+'/trust',{**replacement['keys'][0],'confirm':'pc1'},expected=400)
        assert (temp/'known_hosts').read_bytes()==known_bytes,'Existing trust was silently replaced'
        scanned_public.write_text((temp/'scan-key.pub').read_text())
        # Web-managed host connections and one-time bootstrap credentials.
        new_host=request('/api/hosts/new',{'name':'New machine','alias':'pc3','address':'127.0.0.1','user':'test','port':22})['host']
        request('/api/hosts/new',{'name':'Duplicate','alias':'pc3','address':'127.0.0.1','user':'test','port':22},expected=400)
        assert 'pc3' in request('/api/ssh/aliases')['aliases']
        connection=request('/api/hosts/'+new_host['id']+'/connection')['connection'];assert connection['address']=='127.0.0.1'
        configuration=request('/api/config/export');assert configuration['version']==2 and configuration['connections'][0]['alias']=='pc3'
        boot=request('/api/hosts/'+new_host['id']+'/bootstrap',{'password':'temporary-remote-password','confirm':'pc3'});assert boot['authorized'] and boot['docker_ready']
        assert not list(temp.glob('.bootstrap-*')),'One-time credentials remained on disk'
        assert b'temporary-remote-password' not in (temp/'db.sqlite').read_bytes()
        assert 'temporary-remote-password' not in log.read_text()
        managed=temp/'managed_ssh_config';assert managed.stat().st_mode&0o777==0o600
        assert 'Include ' in managed.read_text()
        req=urllib.request.Request(origin+'/api/hosts/'+new_host['id'],method='DELETE',headers={'Origin':origin,'X-CSRF-Token':csrf})
        with client.open(req,timeout=5) as response:assert response.status==200
        assert 'pc3' not in request('/api/ssh/aliases')['aliases']
        before_jobs=request('/api/overview')['jobs']
        request('/api/health/refresh',{})
        for _ in range(100):
            health=request('/api/overview')['host_health']
            if len(health)==2 and all(h['state']=='online' for h in health.values()):break
            time.sleep(.1)
        assert len(health)==2 and all(h['state']=='online' for h in health.values()),health
        assert request('/api/overview')['jobs']==before_jobs,'Background health polluted operation history'
        discovered=request('/api/hosts/' +host['id']+'/discover',{});assert discovered['stacks'][0]['directory']=='/srv/discovered'
        exported=request('/api/config/export');assert exported['format']=='dockmin' and 'password' not in exported
        imported=request('/api/config/import',{'configuration':exported,'confirm':'IMPORT CONFIGURATION'});assert imported=={'hosts_added':0,'stacks_added':0}
        dockman=[{'id':'legacy','ssh_alias':'pc1','label':'PC1','compose_dirs':[{'id':'/srv/legacy','path':'/srv/legacy','label':'Legacy'}]}]
        request('/api/config/import',{'configuration':dockman,'confirm':'WRONG'},expected=400)
        imported=request('/api/config/import',{'configuration':dockman,'confirm':'IMPORT CONFIGURATION'});assert imported['hosts_added']==0 and imported['stacks_added']==1
        assert request('/api/config/import',{'configuration':dockman,'confirm':'IMPORT CONFIGURATION'})['stacks_added']==0
        before=request('/api/overview')
        broken={**exported,'stacks':[{**exported['stacks'][0],'host_id':'orphan'}]}
        request('/api/config/import',{'configuration':broken,'confirm':'IMPORT CONFIGURATION'},expected=400)
        assert len(request('/api/overview')['stacks'])==len(before['stacks'])
        request('/api/backups',{'password':'wrong'},expected=403)
        backup=request('/api/backups',{'password':'test-only-password-123'});assert backup.startswith(b'SQLite format 3')
        (temp/'backup.sqlite').write_bytes(backup)
        backup_db=sqlite3.connect(temp/'backup.sqlite');assert backup_db.execute('SELECT count(*) FROM sessions').fetchone()[0]==0;assert backup_db.execute('PRAGMA integrity_check').fetchone()[0]=='ok';backup_db.close()
        for cookie_value in jar:assert cookie_value.value.encode() not in backup
        # Live log streams leave the host available for ordinary actions.
        cookie='; '.join(f'{c.name}={c.value}' for c in jar)
        live=http.client.HTTPConnection('127.0.0.1',port,timeout=5)
        live.request('GET',f"/api/stacks/{stack['id']}/logs",headers={'Cookie':cookie})
        response=live.getresponse();assert response.status==200
        assert response.readline().startswith(b'event: log');assert b'live container line' in response.readline()
        extras=[]
        for _ in range(3):
            extra=http.client.HTTPConnection('127.0.0.1',port,timeout=5)
            extra.request('GET',f"/api/stacks/{stack['id']}/logs",headers={'Cookie':cookie})
            stream_response=extra.getresponse();assert stream_response.status==200
            assert stream_response.readline().startswith(b'event: log')
            extras.append((extra,stream_response))
        request('/api/stacks/'+stack['id']+'/logs',expected=429)
        assert wait(request('/api/stacks/'+stack['id']+'/actions',{'action':'refresh'}))['state']=='succeeded'
        for extra,stream_response in extras:stream_response.close();extra.close()
        response.close();live.close()
        pid=int(Path(str(log)+'.follow-pid').read_text())
        for _ in range(50):
            if not Path(f'/proc/{pid}').exists():break
            time.sleep(.1)
        assert not Path(f'/proc/{pid}').exists(),'Disconnected SSH log process was not reaped'

        slow=request('/api/stacks',{'id':'','host_id':host2['id'],'name':'Slow','directory':'/srv/slow','project':'slow','files':[]})
        timeout_job=request('/api/stacks/'+slow['id']+'/actions',{'action':'refresh'})
        result=wait(timeout_job);assert result['state']=='interrupted' and 'Timeout' in result['output']
        assert 'error' in request('/api/overview')['statuses'][slow['id']]
        disconnected=request('/api/stacks',{'id':'','host_id':host['id'],'name':'Disconnected','directory':'/srv/disconnected','project':'disconnected','files':[]})
        failed=request('/api/stacks/'+disconnected['id']+'/actions',{'action':'refresh'})
        assert wait(failed)['state']=='interrupted'
        bulk=request('/api/bulk',{'stack_ids':[stack['id']],'action':'refresh'});assert len(bulk['jobs'])==1;idle()
        request('/api/bulk',{'stack_ids':[stack['id'],stack['id']],'action':'refresh'},expected=400)
        request('/api/bulk',{'stack_ids':[stack['id']],'action':'update'},expected=400)
        interrupted=request('/api/stacks/'+slow['id']+'/actions',{'action':'refresh'})
        time.sleep(.2);process.kill();process.wait();process=start()
        recovered=request('/api/jobs/'+interrupted['id']);assert recovered['state']=='interrupted'
        assert 'Service restarted' in recovered['output']
        view=request('/api/overview');assert len(view['hosts'])==2 and view['statuses'][stack['id']]['services']
        assert request('/api/settings')['browser_poll_seconds']==7,'Settings did not survive service restart'
        second_jar=http.cookiejar.CookieJar();second_client=urllib.request.build_opener(urllib.request.HTTPCookieProcessor(second_jar))
        request('/api/login',{'password':'test-only-password-123'},opener=second_client)
        request('/api/password',{'current_password':'wrong','new_password':'changed-test-password-123'},expected=403)
        changed=request('/api/password',{'current_password':'test-only-password-123','new_password':'changed-test-password-123'});csrf=changed['csrf']
        request('/api/overview',expected=401,opener=second_client)
        request('/api/overview')
        request('/api/login',{'password':'test-only-password-123'},expected=401,opener=second_client)
        request('/api/logout',{});request('/api/overview',expected=401)
        database=sqlite3.connect(temp/'db.sqlite');assert database.execute('SELECT count(*) FROM sessions').fetchone()[0]==0
        assert ((temp/'db.sqlite').stat().st_mode & 0o777)==0o600
        print('PASS: login, origin/CSRF, input validation, Compose status, confirmation, jobs, SSE disconnect, bounded output, image checks, timeout, SSH failure, bulk operations, settings persistence, health without history changes, discovery, verified key enrollment, import idempotence, session-free backup, live log disconnect, password/session revocation, restart recovery, logout, private database permissions')
    finally:
        if process and process.poll() is None:process.send_signal(signal.SIGINT);process.wait(timeout=10)
        output.close()
