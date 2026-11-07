#!/usr/bin/env python3
"""Opt-in real-Docker integration tests. Only unique dockmin-test-* resources
are mutated. Requires the local service host to be reachable through ssh pi2.
Existing containers, stacks, volumes, and configuration are never modified.
"""
import base64,hashlib,http.client,http.cookiejar,json,os,re,shutil,signal,socket,sqlite3,struct,subprocess,sys,tempfile,time,urllib.request,urllib.error
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
BINARY=Path(sys.argv[1]).resolve() if len(sys.argv)>1 else ROOT/'target/debug/dockmin'
PREFIX='dockmin-test-'+os.urandom(5).hex()
IMAGE='debian:trixie-slim'
SOURCE=Path('/tmp')/PREFIX
SOURCE.mkdir(mode=0o700)
STATE=SOURCE/'state';STATE.mkdir(mode=0o700)
jar=http.cookiejar.CookieJar();client=urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar));csrf='';process=None;stack=None;host=None;project=PREFIX;output=None

def remote(*args,check=True):
    result=subprocess.run(['ssh','pi2','sh','-s'],input='exec '+shlex_join(args)+'\n',text=True,capture_output=True)
    if check and result.returncode:raise AssertionError('Remote test command failed: '+result.stderr[-2000:])
    return result

def shlex_join(args):
    import shlex
    return shlex.join(str(a) for a in args)

def request(path,body=None,expected=200,method=None):
    req=urllib.request.Request(origin+'/api'+path,data=json.dumps(body).encode() if body is not None else None,method=method,headers={'Origin':origin,'Content-Type':'application/json','X-CSRF-Token':csrf})
    try:
        with client.open(req,timeout=90) as response:code=response.status;data=response.read()
    except urllib.error.HTTPError as error:code=error.code;data=error.read()
    assert code==expected,(path,code,expected,data[:1500])
    if data[:1] in [b'{',b'[']:return json.loads(data)
    return data

def wait(job,success=True):
    if 'job' in job:job=job['job']
    for _ in range(1200):
        value=request('/jobs/'+job['id'])
        if value['finished']:
            if success:assert value['state']=='succeeded',(value['action'],value['state'],value['output'][-4000:])
            return value
        time.sleep(.2)
    raise AssertionError('Operation timed out')

def idle():
    for _ in range(200):
        if all(j['finished'] for j in request('/overview')['jobs']):return
        time.sleep(.1)
    raise AssertionError('Operation queue remained busy')

def docker_inspect(cid):return json.loads(remote('docker','inspect',cid).stdout)[0]

def terminal_test(cid,name):
    endpoint=f'/hosts/{host["id"]}/containers/{cid}'
    request(endpoint+'/terminal-ticket',{'shell':'/bin/sh -c evil','confirm':name},expected=400)
    ticket=request(endpoint+'/terminal-ticket',{'shell':'/bin/sh','confirm':name})['ticket']
    raw=socket.create_connection(('127.0.0.1',port),timeout=15)
    cookie='; '.join(f'{c.name}={c.value}' for c in jar)
    key=base64.b64encode(os.urandom(16)).decode()
    raw.sendall((f'GET /api{endpoint}/terminal?ticket={ticket} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: {origin}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nCookie: {cookie}\r\n\r\n').encode())
    pending=b''
    while b'\r\n\r\n' not in pending:pending+=raw.recv(4096)
    header,pending=pending.split(b'\r\n\r\n',1);assert b' 101 ' in header,header
    def send(opcode,payload):
        mask=os.urandom(4);n=len(payload);length=bytes([0x80|n]) if n<126 else bytes([0x80|126])+struct.pack('!H',n)
        raw.sendall(bytes([0x80|opcode])+length+mask+bytes(b^mask[i%4] for i,b in enumerate(payload)))
    def take(n):
        nonlocal pending
        while len(pending)<n:
            block=raw.recv(8192)
            if not block:raise AssertionError('Terminal disconnected unexpectedly')
            pending+=block
        result,pending=pending[:n],pending[n:];return result
    send(1,json.dumps({'type':'resize','cols':100,'rows':30}).encode())
    marker='DOCKMIN_TERMINAL_OK';octal=''.join('\\%03o'%ord(c) for c in marker)
    send(1,json.dumps({'type':'input','data':f"printf '{octal}\\n'\n"}).encode())
    seen=b''
    for _ in range(100):
        first,second=take(2);opcode=first&15;n=second&127
        if n==126:n=struct.unpack('!H',take(2))[0]
        elif n==127:n=struct.unpack('!Q',take(8))[0]
        payload=take(n)
        if opcode==9:send(10,payload)
        elif opcode in [1,2]:seen+=payload
        if marker.encode() in seen:break
    assert marker.encode() in seen,'Interactive command did not execute'
    send(8,struct.pack('!H',1000));raw.close();idle()
    # A used ticket cannot be reused, and terminal output is not persisted.
    for value in request('/overview')['jobs']:assert marker not in request('/jobs/'+value['id'])['output']

try:
    # Pull only the official, temporary-test/helper image; no existing images
    # or production container tags are replaced.
    remote('docker','pull',IMAGE)
    listener=socket.socket();listener.bind(('127.0.0.1',0));port=listener.getsockname()[1];listener.close();origin=f'http://127.0.0.1:{port}'
    config=STATE/'dockmin.toml';config.write_text(f'''listen="127.0.0.1:{port}"
origins=["{origin}"]
database="{STATE}/db.sqlite"
static_dir="{ROOT}/web/dist"
ssh_config="/home/jim/.ssh/config"
known_hosts="/home/jim/.ssh/known_hosts"
secure_cookie=false
concurrency=3
health_refresh_seconds=0
command_timeout_seconds=180
''')
    subprocess.run([str(BINARY),'--config',str(config),'set-password'],input='temporary-test-password-123\n',text=True,check=True,capture_output=True)
    with sqlite3.connect(STATE/'db.sqlite') as db:db.execute("INSERT INTO settings VALUES('automation',?)",[json.dumps({'enabled':False,'interval_minutes':1440,'stack_ids':[]})])
    output=(STATE/'server.log').open('w');process=subprocess.Popen([str(BINARY),'--config',str(config)],stdout=output,stderr=output)
    for _ in range(100):
        try:request('/health');break
        except OSError:
            if process.poll() is not None:raise AssertionError((STATE/'server.log').read_text())
            time.sleep(.1)
    csrf=request('/login',{'password':'temporary-test-password-123'})['csrf']
    host=request('/hosts',{'id':'','name':'Isolated test host','alias':'pi2'})
    bind_directory=SOURCE/'bind-data';bind_directory.mkdir();(bind_directory/'value').write_text('original-bind')
    bind_file=SOURCE/'bind-file';bind_file.write_text('original-file');bind_file.chmod(0o640);os.utime(bind_file,(1700000000,1700000000))
    content=f'''services:
  app:
    image: {IMAGE}
    init: true
    command: ["sh", "-c", "echo COMPOSE_READY; exec sleep infinity"]
    volumes:
      - data:/data
      - {bind_directory}:/bind-data
      - {bind_file}:/bind-file:ro
volumes:
  data: {{}}
'''
    directory=SOURCE/'compose'
    stack=request('/stacks/create',{'stack':{'id':'','host_id':host['id'],'name':PREFIX,'directory':str(directory),'project':project,'files':[]},'content':content,'confirm':PREFIX})['stack']
    assert (directory/'compose.yaml').read_text()==content
    request('/stacks/create',{'stack':{**stack,'id':''},'content':content,'confirm':PREFIX},expected=400)
    invalid=SOURCE/'invalid'
    request('/stacks/create',{'stack':{**stack,'id':'','directory':str(invalid),'files':[]},'content':'services: [invalid','confirm':PREFIX},expected=400);assert not invalid.exists()
    wait(request('/stacks/'+stack['id']+'/deploy',{'confirm':'DEPLOY '+PREFIX}));idle()
    app=json.loads(remote('docker','ps','--filter','label=com.docker.compose.project='+project,'--format','json').stdout)
    cid=app['ID'];remote('docker','exec',cid,'sh','-c','printf original > /data/value')
    # Stale edit and invalid YAML never replace the original.
    files=request('/stacks/'+stack['id']+'/compose')['files']
    request('/stacks/'+stack['id']+'/compose/preview',{'files':[{**files[0],'revision':'0'*64}]},expected=400)
    invalid_preview=request('/stacks/'+stack['id']+'/compose/preview',{'files':[{**files[0],'content':'services: [invalid'}]});assert not invalid_preview['valid'];assert (directory/'compose.yaml').read_text()==content
    # A failed saved deployment must restore both the previous image and the
    # previous working command, rather than checkpointing the edited file.
    bad=content.replace('echo COMPOSE_READY; exec sleep infinity','exit 7')
    edited=[{**files[0],'content':bad}]
    assert request('/stacks/'+stack['id']+'/compose/preview',{'files':edited})['valid']
    wait(request('/stacks/'+stack['id']+'/compose',{'files':edited,'confirm':PREFIX}));idle()
    assert docker_inspect(cid)['State']['Running'],'Saving Compose restarted the stack'
    request('/stacks/'+stack['id']+'/deployment-policy',{'auto_rollback':True,'health_timeout':15})
    result=wait(request('/stacks/'+stack['id']+'/deploy',{'confirm':'DEPLOY '+PREFIX}),success=False);assert result['state']=='failed' and 'were restored' in result['output'],result['output'];idle()
    cid=remote('docker','compose','--project-name',project,'--file',str(directory/'compose.yaml'),'ps','--quiet').stdout.strip()
    assert docker_inspect(cid)['State']['Running'];assert remote('docker','exec',cid,'cat','/data/value').stdout=='original';assert (directory/'compose.yaml').read_text()==content
    assert request('/overview')['notifications'],'Failed deployment did not notify'
    terminal_test(cid,project+'-app-1')
    # Back up real named-volume data, then restore it with a safety backup.
    mounts=request('/stacks/'+stack['id']+'/data')['mounts'];assert len(mounts)==3
    wait(request('/stacks/'+stack['id']+'/data-backups',{'mount_ids':[m['id'] for m in mounts],'confirm':'BACKUP '+PREFIX}));idle()
    backups=request('/stacks/'+stack['id']+'/data-backups')['backups'];assert len(backups)==1
    archive=request('/stacks/'+stack['id']+'/data-backups/'+backups[0]['id']+'/download');assert archive[:2]!=b'\x1f\x8b' and len(archive)>1024
    import io,tarfile
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:assert './manifest.json' in tar.getnames()
    # Stream a downloaded archive back without changing application data.
    req=urllib.request.Request(origin+'/api/stacks/'+stack['id']+'/data-backups/import',data=archive,headers={'Origin':origin,'Content-Type':'application/x-tar','X-CSRF-Token':csrf})
    with client.open(req,timeout=90) as response:imported=json.load(response)['backup']
    assert imported['id']!=backups[0]['id'] and imported['sources']==backups[0]['sources']
    assert remote('docker','exec',cid,'cat','/data/value').stdout=='original'
    count_before_restore=len(request('/stacks/'+stack['id']+'/data-backups')['backups'])
    (bind_directory/'value').write_text('changed-bind');bind_file.write_text('changed-file');bind_file.chmod(0o600)
    remote('docker','exec',cid,'sh','-c','printf changed > /data/value')
    wait(request('/stacks/'+stack['id']+'/restore',{'backup':backups[0]['id'],'confirm':'RESTORE '+PREFIX,'configuration':False}));idle()
    assert remote('docker','exec',cid,'cat','/data/value').stdout=='original'
    assert (bind_directory/'value').read_text()=='original-bind' and bind_file.read_text()=='original-file'
    assert bind_file.stat().st_mode&0o777==0o640 and int(bind_file.stat().st_mtime)==1700000000
    assert len(request('/stacks/'+stack['id']+'/data-backups')['backups'])==count_before_restore+1,'Safety backup missing'
    # Corruption is detected before services are stopped or any data changes.
    backup_path=Path.home()/'.local/share/dockmin/backups'/stack['id']/backups[0]['id']/'data-0.tar.gz'
    saved=backup_path.read_bytes();backup_path.write_bytes(saved+b'corruption')
    failed=wait(request('/stacks/'+stack['id']+'/restore',{'backup':backups[0]['id'],'confirm':'RESTORE '+PREFIX}),success=False);assert failed['state']=='failed' and 'checksum' in failed['output'];idle();assert docker_inspect(cid)['State']['Running'];backup_path.write_bytes(saved)
    # Restore the matching configuration/images as well as application data.
    wait(request('/stacks/'+stack['id']+'/restore',{'backup':backups[0]['id'],'confirm':'RESTORE '+PREFIX,'configuration':True}));idle()
    cid=remote('docker','compose','--project-name',project,'--file',str(directory/'compose.yaml'),'ps','--quiet').stdout.strip()
    assert docker_inspect(cid)['State']['Running'] and (directory/'compose.yaml').read_text()==content
    assert remote('docker','exec',cid,'cat','/data/value').stdout=='original'
    # Create, recreate, and roll back a standalone container, preserving mounts,
    # environment, port bindings, restart policy, and data.
    standalone=PREFIX+'-solo';volume=PREFIX+'-solo-data'
    port_socket=socket.socket();port_socket.bind(('127.0.0.1',0));published=port_socket.getsockname()[1];port_socket.close()
    body={'name':standalone,'image':IMAGE,'command':['sh','-c',"trap 'exit 0' TERM; echo SOLO_READY; while :; do sleep .2; done"],'environment':['DOCKMIN_TEST_VALUE=kept'],'ports':[f'127.0.0.1:{published}:8080'],'mounts':[volume+':/data'],'restart':'unless-stopped','start':True,'confirm':standalone}
    wait(request('/hosts/'+host['id']+'/containers',body));idle();row=next(c for c in request('/hosts/'+host['id']+'/containers')['containers'] if c['Names']==standalone);original=docker_inspect(row['ID']);remote('docker','exec',original['Id'],'sh','-c','printf preserved > /data/value')
    wait(request('/hosts/'+host['id']+'/containers/'+original['Id']+'/actions',{'action':'update','confirm':standalone}));idle();row=next(c for c in request('/hosts/'+host['id']+'/containers')['containers'] if c['Names']==standalone);updated=docker_inspect(row['ID']);assert updated['Id']!=original['Id'];assert updated['Config']['Env']==original['Config']['Env'];assert updated['HostConfig']['PortBindings']==original['HostConfig']['PortBindings'];assert updated['HostConfig']['RestartPolicy']==original['HostConfig']['RestartPolicy'];assert remote('docker','exec',updated['Id'],'cat','/data/value').stdout=='preserved'
    point=request('/hosts/'+host['id']+'/container-checkpoints')['checkpoints'][0]
    wait(request('/hosts/'+host['id']+'/containers/'+updated['Id']+'/actions',{'action':'rollback','confirm':standalone,'checkpoint':point['id']}));idle();assert docker_inspect(original['Id'])['Name']=='/'+standalone and docker_inspect(original['Id'])['State']['Running']
    # Compose containers cannot be mutated through standalone endpoints.
    bad_action=wait(request('/hosts/'+host['id']+'/containers/'+cid+'/actions',{'action':'stop','confirm':project+'-app-1'}),success=False);assert bad_action['state']=='failed';assert docker_inspect(cid)['State']['Running']
    request('/logout',{})
    print('PASS: real Docker stack creation, stale-edit protection, validation, file backups, failed-deployment rollback, persistent notifications, interactive terminal/resize, volume backup and restore, safety backup, checksums, streaming download/import, bind-file/directory restore, matching image/configuration restore, standalone recreation and rollback, Compose ownership protection')
finally:
    if process and process.poll() is None:process.send_signal(signal.SIGTERM);process.wait(timeout=20)
    if output:output.close()
    # Every deletion is guarded by the unique test prefix or stack identifier.
    listed=remote('docker','ps','--all','--filter','name='+PREFIX,'--format','{{.ID}}',check=False).stdout.split()
    for cid in listed:
        details=docker_inspect(cid)
        if PREFIX in details['Name']:remote('docker','rm','--force',cid,check=False)
    volumes=remote('docker','volume','ls','--filter','name='+PREFIX,'--format','{{.Name}}',check=False).stdout.split()
    for name in volumes:
        if name.startswith(PREFIX):remote('docker','volume','rm',name,check=False)
    networks=remote('docker','network','ls','--filter','name='+PREFIX,'--format','{{.Name}}',check=False).stdout.split()
    for name in networks:
        if name.startswith(PREFIX):remote('docker','network','rm',name,check=False)
    if stack:
        image_prefix='dockmin-rollback/'+hashlib.sha256(stack['id'].encode()).hexdigest()[:16]
        tags=remote('docker','image','ls',image_prefix,'--format','{{.Repository}}:{{.Tag}}',check=False).stdout.split()
        for tag in tags:
            if tag.startswith(image_prefix+':'):remote('docker','image','rm',tag,check=False)
        for kind in ['backups','checkpoints']:
            folder=Path.home()/'.local/share/dockmin'/kind/stack['id']
            if folder.exists():shutil.rmtree(folder)
    # Keep only a sanitized service log on failure for debugging. It contains
    # test inputs, never production Compose contents or account credentials.
    if sys.exc_info()[0] is not None:
        print('Test resources cleaned. Service diagnostics: '+str(STATE/'server.log'),file=sys.stderr)
    else:shutil.rmtree(SOURCE)
