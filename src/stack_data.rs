use crate::{
    api::{AppState, Result, ensure, missing},
    jobs, recovery, remote, ssh,
};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
pub const HELPER_IMAGE: &str = "debian:trixie-slim";
pub async fn mounts(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let (stack, host) = remote::context(&s, &id)?;
    let script = remote::python(
        &format!(
            "{FUNCTIONS}\nitems=inventory()\nemit({{'mounts':items,'helper_image':data['helper_image']}})"
        ),
        &json!({"stack":stack,"helper_image":HELPER_IMAGE}),
    );
    Ok(Json(remote::read(&s, &host, &script, 128 * 1024).await?))
}
pub async fn list(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> Result<Json<Value>> {
    remote::context(&s, &id)?;
    Ok(Json(json!({"backups":s.db.records("data-backup",&id)?})))
}
#[derive(Deserialize)]
pub struct Backup {
    pub mount_ids: Vec<String>,
    pub confirm: String,
}
pub async fn backup(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Backup>,
) -> Result<Json<Value>> {
    ensure(
        !body.mount_ids.is_empty()
            && body.mount_ids.len() <= 64
            && body
                .mount_ids
                .iter()
                .all(|v| v.len() == 64 && v.bytes().all(|c| c.is_ascii_hexdigit())),
        "Select one to 64 valid mounts",
    )?;
    let _gate = s.mutations.lock().await;
    let (stack, host) = remote::context(&s, &id)?;
    ensure(
        body.confirm == format!("BACKUP {}", stack.name),
        "Confirm BACKUP and the stack name",
    )?;
    let script = remote::python(
        &format!(
            "{}\n{}\n{}\n{}",
            crate::containers::ENGINE,
            recovery::FUNCTIONS,
            FUNCTIONS,
            BACKUP
        ),
        &json!({"stack":stack,"mount_ids":body.mount_ids,"backup_id":uuid::Uuid::new_v4().to_string(),"helper_image":HELPER_IMAGE}),
    );
    let job = jobs::enqueue_script(s.clone(), host, Some(stack), "backup".into(), script).await?;
    Ok(Json(json!({"job":job})))
}
#[derive(Deserialize)]
pub struct Restore {
    pub backup: String,
    pub confirm: String,
    #[serde(default)]
    pub configuration: bool,
}
pub async fn restore(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Restore>,
) -> Result<Json<Value>> {
    let _gate = s.mutations.lock().await;
    let (stack, host) = remote::context(&s, &id)?;
    ensure(
        body.confirm == format!("RESTORE {}", stack.name),
        "Confirm RESTORE and the stack name",
    )?;
    let point =
        s.db.record("data-backup", &body.backup)?
            .filter(|v| v["stack_id"] == id)
            .ok_or_else(missing)?;
    uuid::Uuid::parse_str(&body.backup).map_err(anyhow::Error::from)?;
    let script = remote::python(
        &format!(
            "{}\n{}\n{}\n{}",
            crate::containers::ENGINE,
            recovery::FUNCTIONS,
            FUNCTIONS,
            RESTORE
        ),
        &json!({"stack":stack,"backup_id":point["id"],"safety_id":uuid::Uuid::new_v4().to_string(),"configuration":body.configuration,"helper_image":HELPER_IMAGE,"health_timeout":recovery::policy(&s,&id)?.health_timeout}),
    );
    let job = jobs::enqueue_script(s.clone(), host, Some(stack), "restore".into(), script).await?;
    Ok(Json(json!({"job":job})))
}
pub async fn download(
    State(s): State<Arc<AppState>>,
    Path((id, bid)): Path<(String, String)>,
) -> Result<Response> {
    let (stack, host) = remote::context(&s, &id)?;
    uuid::Uuid::parse_str(&bid).map_err(anyhow::Error::from)?;
    s.db.record("data-backup", &bid)?
        .filter(|v| v["stack_id"] == id)
        .ok_or_else(missing)?;
    let permit = s.download_slots.clone().try_acquire_owned().map_err(|_| {
        crate::api::ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Two backup downloads are already active".into(),
        )
    })?;
    let script = remote::python(
        "import uuid\nuuid.UUID(data['backup_id'])\nroot=pathlib.Path.home()/'.local'/'share'/'dockmin'/'backups'/s['id']/data['backup_id']\nif not (root/'manifest.json').is_file():raise RuntimeError('Backup is not present on this host.')\nprint('DOCKMIN_BACKUP_READY',flush=True)\ncode=subprocess.call(['tar','-C',str(root),'-cf','-','.'])\nsys.exit(code)",
        &json!({"stack":stack,"backup_id":bid}),
    );
    let mut command = ssh::stdin_command(&s.config, &host);
    command.stderr(std::process::Stdio::null());
    let mut child = command.spawn().map_err(anyhow::Error::from)?;
    let mut input = child.stdin.take().unwrap();
    tokio::spawn(async move {
        let _ = input.write_all(script.as_bytes()).await;
    });
    let mut out = child.stdout.take().unwrap();
    let mut ready = vec![0u8; b"DOCKMIN_BACKUP_READY\n".len()];
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        out.read_exact(&mut ready),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Backup download timed out"))?
    .map_err(anyhow::Error::from)?;
    ensure(
        ready == b"DOCKMIN_BACKUP_READY\n",
        "Backup download could not be started",
    )?;
    let stream = async_stream::stream! {let _permit=permit;let mut buffer=[0u8;65536];loop{match out.read(&mut buffer).await{Ok(0)=>break,Ok(n)=>yield Ok::<Bytes,std::io::Error>(Bytes::copy_from_slice(&buffer[..n])),Err(e)=>{yield Err(e);break;}}}match child.wait().await{Ok(status)if status.success()=>{},_=>yield Err(std::io::Error::other("Backup download failed"))}};
    let mut response = Body::from_stream(stream).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/x-tar".parse().unwrap());
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"dockmin-backup-{bid}.tar\"")
            .parse()
            .unwrap(),
    );
    Ok(response)
}
pub const FUNCTIONS: &str = r#"
import stat,uuid
BLOCKED={'/','/etc','/usr','/var','/home','/root','/proc','/sys','/dev','/run','/var/run','/var/lib/docker'}
def inventory():
    ids=compose(['ps','--all','--quiet']).stdout.split()
    if not ids:return []
    owned=set(ids);items={}
    for c in json.loads(execute(['docker','inspect']+ids).stdout):
        for m in c['Mounts']:
            if m['Type'] not in ['bind','volume']:continue
            source=m.get('Name') if m['Type']=='volume' else m['Source'];kind='directory'
            if m['Type']=='bind':
                if source in BLOCKED:continue
                try:
                    mode=os.stat(source).st_mode
                    if stat.S_ISREG(mode):kind='file'
                    elif not stat.S_ISDIR(mode):continue
                except PermissionError:pass
                except FileNotFoundError:continue
            ident=hashlib.sha256((m['Type']+'\0'+source).encode()).hexdigest()
            item=items.setdefault(ident,{'id':ident,'type':m['Type'],'source':source,'kind':kind,'targets':[],'read_only':not m['RW'],'shared_with':[]})
            if m['Destination'] not in item['targets']:item['targets'].append(m['Destination'])
    others=execute(['docker','ps','--quiet']).stdout.split()
    if others:
        for c in json.loads(execute(['docker','inspect']+others).stdout):
            if c['Id'] in owned or c['Id'][:12] in owned:continue
            for m in c['Mounts']:
                source=m.get('Name') if m['Type']=='volume' else m.get('Source','')
                ident=hashlib.sha256((m['Type']+'\0'+source).encode()).hexdigest()
                if ident in items:items[ident]['shared_with'].append(c['Name'].lstrip('/'))
    return list(items.values())
def select_mounts(identifiers):
    current={m['id']:m for m in inventory()}
    if any(i not in current for i in identifiers):raise RuntimeError('A selected mount changed or is unavailable. Refresh the mount list.')
    selected=[current[i] for i in dict.fromkeys(identifiers)]
    reserved=(pathlib.Path.home()/'.local'/'share'/'dockmin').resolve()
    for mount in selected:
        if mount['type']=='bind' and (pathlib.Path(mount['source']).is_symlink() or pathlib.Path(mount['source']).resolve() in [reserved,*reserved.parents]):raise RuntimeError('Choose a dedicated data directory; this mount contains Dockmin backup storage or is a symlink.')
    if any(m['shared_with'] for m in selected):raise RuntimeError('Selected data is also used by another running container. Stop those consumers first.')
    return selected
def backup_root(ident):uuid.UUID(ident);return pathlib.Path.home()/'.local'/'share'/'dockmin'/'backups'/s['id']/ident
def file_hash(path):
    h=hashlib.sha256()
    with open(path,'rb') as handle:
        for block in iter(lambda:handle.read(1048576),b''):h.update(block)
    return h.hexdigest()
def ensure_helper():
    if execute(['docker','image','inspect',data['helper_image']],check=False).returncode:execute(['docker','pull',data['helper_image']])
def helper(mount,root,script,readonly):
    target={'Type':mount['type'],'Source':mount['source'],'Target':'/data','ReadOnly':readonly}
    if mount['type']=='bind':target['BindOptions']={'NonRecursive':True}
    config={'Image':data['helper_image'],'Cmd':['sh','-ec',script],'Labels':{'dockmin.helper':'true'},'HostConfig':{'NetworkMode':'none','ReadonlyRootfs':True,'CapDrop':['ALL'],'CapAdd':['DAC_OVERRIDE','DAC_READ_SEARCH','CHOWN','FOWNER','FSETID','SETFCAP'],'SecurityOpt':['no-new-privileges'],'Mounts':[target,{'Type':'bind','Source':str(root),'Target':'/backup','ReadOnly':False}]}}
    cid=api('POST',prefix+'/containers/create?name=dockmin-data-'+str(uuid.uuid4()),config)['Id'];last=time.monotonic()
    try:
        api('POST',prefix+'/containers/'+cid+'/start')
        while True:
            state=api('GET',prefix+'/containers/'+cid+'/json')['State']
            if not state['Running']:
                if state['ExitCode']!=0:raise RuntimeError(execute(['docker','logs',cid],check=False).stderr[-1200:] or 'Archive helper failed.')
                return
            if time.monotonic()-last>20:print('Processing selected stack data...',flush=True);last=time.monotonic()
            time.sleep(.5)
    finally:
        try:api('DELETE',prefix+'/containers/'+cid+'?force=true&v=false')
        except BaseException:pass
def archive_mount(mount,root,index):
    name='data-'+str(index)+'.tar.gz'
    args='tar --acls --xattrs --numeric-owner --sparse -czpf /backup/'+name+(' -C / data' if mount['kind']=='file' else ' -C /data .')
    args+=' && chown '+str(os.getuid())+':'+str(os.getgid())+' /backup/'+name+' && chmod 600 /backup/'+name
    helper(mount,root,args,True);path=root/name;return {**mount,'archive':name,'sha256':file_hash(path),'bytes':path.stat().st_size}
def create_backup(ident,mounts,running):
    root=backup_root(ident);root.mkdir(parents=True,mode=0o700,exist_ok=False)
    point=save_checkpoint(ident,require_containers=False)
    # Data is archived after stopping services. Preserve their pre-backup state
    # so a full restore starts exactly the services that were originally running.
    point['running']=list(running)
    for i,f in enumerate(point['files']):
        name='config-'+str(i);shutil.copy2(f['backup'],root/name);os.chmod(root/name,0o600);f['backup']=name
    archives=[]
    for i,m in enumerate(mounts):print('Archiving '+m['source'],flush=True);archives.append(archive_mount(m,root,i))
    manifest={'format':'dockmin-data','version':1,'id':ident,'stack_id':s['id'],'host_id':s['host_id'],'directory':s['directory'],'created':int(time.time()),'mounts':archives,'checkpoint':point}
    (root/'manifest.json').write_text(json.dumps(manifest));os.chmod(root/'manifest.json',0o600)
    record={'id':ident,'stack_id':s['id'],'host_id':s['host_id'],'created':manifest['created'],'mounts':len(archives),'bytes':sum(m['bytes'] for m in archives),'sources':[m['source'] for m in archives]}
    print('DOCKMIN_RECORD data-backup '+json.dumps(record),flush=True);return manifest

def verify_backup(ident):
    root=backup_root(ident);manifest=json.loads((root/'manifest.json').read_text())
    if manifest.get('format')!='dockmin-data' or manifest.get('version')!=1 or manifest['stack_id']!=s['id']:raise RuntimeError('Invalid stack backup.')
    for m in manifest['mounts']:
        name=m['archive']
        if pathlib.PurePath(name).name!=name or not name.startswith('data-'):raise RuntimeError('Invalid archive name.')
        if file_hash(root/name)!=m['sha256']:raise RuntimeError('Backup archive checksum mismatch.')
    allowed=set(sources()+[str(pathlib.Path(sources()[0]).parent/'.env')])
    for f in manifest['checkpoint']['files']:
        if f['path'] not in allowed or pathlib.PurePath(f['backup']).name!=f['backup']:raise RuntimeError('Backup configuration differs from the registered file paths.')
        if file_hash(root/f['backup'])!=f['sha256']:raise RuntimeError('Backup configuration checksum mismatch.')
    return root,manifest

def restore_mount(mount,root,index):
    archive='/backup/'+mount['archive']
    if mount['kind']=='file':
        script='mkdir -m 700 /backup/extracted-'+str(index)+'; tar --acls --xattrs --numeric-owner -xzpf '+archive+' -C /backup/extracted-'+str(index)+'; cat /backup/extracted-'+str(index)+'/data > /data; cp --attributes-only --preserve=all /backup/extracted-'+str(index)+'/data /data; rm -rf /backup/extracted-'+str(index)
    else:script='find /data -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +; tar --acls --xattrs --numeric-owner --same-owner -xzpf '+archive+' -C /data'
    helper(mount,root,script,False)
"#;
const BACKUP: &str = r#"
ensure_helper();selected=select_mounts(data['mount_ids']);running=running_services()
if running:compose(['stop']+running)
try:create_backup(data['backup_id'],selected,running);print('Stack data backup completed and verified.',flush=True)
finally:
    if running:compose(['start']+running)
"#;
const RESTORE: &str = r#"
ensure_helper();root,manifest=verify_backup(data['backup_id']);selected=select_mounts([m['id'] for m in manifest['mounts']]);current={m['id']:m for m in selected}
if any(m['kind']!=current[m['id']]['kind'] or m['type']!=current[m['id']]['type'] or m['source']!=current[m['id']]['source'] for m in manifest['mounts']):raise RuntimeError('Backup mount targets changed. Restore only to the original matching sources.')
if data['configuration']:ensure_checkpoint_images(manifest['checkpoint'])
running=running_services()
if running:compose(['stop']+running)
touched=False;success=False;configuration_restored=False
try:
    create_backup(data['safety_id'],selected,running);print('Safety backup completed. Restoring selected data...',flush=True)
    for i,m in enumerate(manifest['mounts']):
        touched=True;restore_mount(m,root,i)
    if data['configuration']:
        point=manifest['checkpoint']
        for f in point['files']:f['backup']=str(root/f['backup'])
        restore_checkpoint(point);configuration_restored=True
    success=True
    print('Stack data restored. Safety backup: '+data['safety_id'],flush=True)
finally:
    if running and not configuration_restored and (success or not touched):compose(['start']+running)
    elif not success and touched:print('Restore did not complete. Services remain stopped; restore the safety backup before restarting.',flush=True)
"#;

pub async fn import(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<Value>> {
    use http_body_util::BodyExt;
    const LIMIT: u64 = 100 * 1024 * 1024 * 1024;
    if let Some(length) = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
    {
        ensure(length <= LIMIT, "Backup upload exceeds 100 GiB")?;
    }
    let (stack, host) = remote::context(&s, &id)?;
    let _lock = s
        .host_lock(&host.id)
        .try_lock_owned()
        .map_err(|_| crate::api::ApiError(StatusCode::CONFLICT, "Host is busy".into()))?;
    let _slot = s.try_slot().ok_or_else(|| {
        crate::api::ApiError(StatusCode::TOO_MANY_REQUESTS, "Service is busy".into())
    })?;
    let script = remote::python_source(
        &format!("{FUNCTIONS}\n{IMPORT}"),
        &json!({"stack":stack,"new_id":uuid::Uuid::new_v4().to_string()}),
    );
    let mut child = ssh::python_command(&s.config, &host, &script)
        .spawn()
        .map_err(anyhow::Error::from)?;
    let mut input = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut body = request.into_body();
    let send = async {
        let mut length = 0u64;
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(anyhow::Error::from)?;
            if let Ok(bytes) = frame.into_data() {
                length += bytes.len() as u64;
                anyhow::ensure!(length <= LIMIT, "Backup upload exceeds 100 GiB");
                input.write_all(&bytes).await?;
            }
        }
        input.shutdown().await?;
        // Closing the pipe sends EOF to SSH; flushing alone keeps the remote
        // archive reader waiting for more bytes.
        drop(input);
        Ok::<(), anyhow::Error>(())
    };
    let (_, out, err) = tokio::time::timeout(
        std::time::Duration::from_secs(s.prefs().command_timeout_seconds),
        async {
            tokio::try_join!(
                send,
                ssh::read_limited(stdout, 128 * 1024),
                ssh::read_limited(stderr, 8192)
            )
        },
    )
    .await
    .map_err(|_| anyhow::anyhow!("Backup upload timed out"))??;
    let status = child.wait().await.map_err(anyhow::Error::from)?;
    ensure(
        status.success(),
        &format!(
            "Backup import failed: {}",
            String::from_utf8_lossy(&err)
                .chars()
                .take(1000)
                .collect::<String>()
        ),
    )?;
    let value: Value = serde_json::from_slice(&out).map_err(anyhow::Error::from)?;
    let bid = value["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid imported backup metadata"))?;
    ensure(
        value["stack_id"] == id
            && value["host_id"] == host.id
            && uuid::Uuid::parse_str(bid).is_ok(),
        "Invalid imported backup identity",
    )?;
    s.db.put_record("data-backup", bid, &id, &value)?;
    Ok(Json(json!({"backup":value})))
}
const IMPORT: &str = r#"
import tarfile,re
class BoundedTarInfo(tarfile.TarInfo):
    def _proc_pax(self,archive):
        if self.size>65536:raise RuntimeError('Archive extended header is too large.')
        return super()._proc_pax(archive)
    def _proc_gnulong(self,archive):
        if self.size>8192:raise RuntimeError('Archive long-name header is too large.')
        return super()._proc_gnulong(archive)
root=backup_root(data['new_id']);root.mkdir(parents=True,mode=0o700,exist_ok=False);seen=set();total=0
try:
    with tarfile.open(fileobj=sys.stdin.buffer,mode='r|',tarinfo=BoundedTarInfo) as archive:
        for member in archive:
            name=member.name.removeprefix('./')
            if member.isdir() and name in ['','.']:continue
            if not member.isfile() or not re.fullmatch(r'(manifest\.json|config-\d+|data-\d+\.tar\.gz)',name) or name in seen or len(seen)>=256:raise RuntimeError('Archive contains unsupported paths or file types.')
            if name=='manifest.json' and member.size>1048576:raise RuntimeError('Backup manifest exceeds 1 MiB.')
            if name.startswith('config-') and member.size>262144:raise RuntimeError('Backup configuration exceeds 256 KiB.')
            total+=member.size
            if total>100*1024*1024*1024:raise RuntimeError('Backup exceeds 100 GiB.')
            seen.add(name)
            with archive.extractfile(member) as source,(root/name).open('xb') as target:shutil.copyfileobj(source,target,1048576)
            os.chmod(root/name,0o600)
    for trailing in iter(lambda:sys.stdin.buffer.read(65536),b''):pass
    manifest=json.loads((root/'manifest.json').read_text())
    if manifest.get('format')!='dockmin-data' or manifest.get('version')!=1 or manifest.get('directory')!=s['directory']:raise RuntimeError('Choose the stack with this backup’s original Compose directory.')
    original=manifest['id'];manifest['id']=data['new_id'];manifest['stack_id']=s['id'];manifest['host_id']=s['host_id'];manifest['original_id']=original
    point=manifest['checkpoint'];point['id']=data['new_id'];point['stack_id']=s['id'];point['host_id']=s['host_id']
    if not isinstance(manifest['mounts'],list) or len(manifest['mounts'])>64 or not isinstance(point['images'],dict):raise RuntimeError('Invalid backup manifest.')
    (root/'manifest.json').write_text(json.dumps(manifest));verify_backup(data['new_id'])
    current={m['id']:m for m in inventory()}
    for mount in manifest['mounts']:
        if mount['id'] not in current or any(mount[k]!=current[mount['id']][k] for k in ['type','source','kind']):raise RuntimeError('Create matching containers and mounts on this host before importing this backup.')
    emit({'id':data['new_id'],'stack_id':s['id'],'host_id':s['host_id'],'created':manifest['created'],'mounts':len(manifest['mounts']),'bytes':sum(m['bytes'] for m in manifest['mounts']),'sources':[m['source'] for m in manifest['mounts']]})
except BaseException:shutil.rmtree(root);raise
"#;
