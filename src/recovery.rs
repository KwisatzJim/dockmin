use crate::{
    api::{AppState, Result, ensure},
    db::Stack,
    jobs, remote,
};
use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub auto_rollback: bool,
    pub health_timeout: u64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            auto_rollback: false,
            health_timeout: 120,
        }
    }
}
pub fn policy(s: &AppState, id: &str) -> anyhow::Result<Policy> {
    Ok(s.db
        .record("deployment-policy", id)?
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default())
}
pub async fn list(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> Result<Json<Value>> {
    remote::context(&s, &id)?;
    Ok(Json(
        json!({"checkpoints":s.db.records("checkpoint",&id)?,"policy":policy(&s,&id)?}),
    ))
}
pub async fn save_policy(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Policy>,
) -> Result<Json<Value>> {
    remote::context(&s, &id)?;
    ensure(
        (10..=600).contains(&body.health_timeout),
        "Health timeout must be 10–600 seconds",
    )?;
    s.db.put_record(
        "deployment-policy",
        &id,
        &id,
        &serde_json::to_value(&body).map_err(anyhow::Error::from)?,
    )?;
    Ok(Json(json!({"policy":body})))
}
#[derive(Deserialize)]
pub struct Action {
    pub confirm: String,
    #[serde(default)]
    pub checkpoint: String,
}
pub async fn rollback(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Action>,
) -> Result<Json<Value>> {
    let _gate = s.mutations.lock().await;
    let (stack, host) = remote::context(&s, &id)?;
    ensure(
        body.confirm == format!("ROLLBACK {}", stack.name),
        "Confirm ROLLBACK and the stack name",
    )?;
    let point =
        s.db.record("checkpoint", &body.checkpoint)?
            .filter(|v| v["stack_id"] == id)
            .ok_or_else(crate::api::missing)?;
    let script = remote::python(
        &format!(
            "{FUNCTIONS}\npoint=load_checkpoint(data['checkpoint']['id'])\nrestore_checkpoint(point)\nprint('Previous images and configuration restored. Application data was not reverted.',flush=True)"
        ),
        &json!({"stack":stack,"checkpoint":point,"health_timeout":policy(&s,&id)?.health_timeout}),
    );
    let job = jobs::enqueue_script(s.clone(), host, Some(stack), "rollback".into(), script).await?;
    Ok(Json(json!({"job":job})))
}
pub async fn deploy(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Action>,
) -> Result<Json<Value>> {
    let _gate = s.mutations.lock().await;
    let (stack, host) = remote::context(&s, &id)?;
    ensure(
        body.confirm == format!("DEPLOY {}", stack.name),
        "Confirm DEPLOY and the stack name",
    )?;
    let script = update_script(&s, &stack, false)?;
    let job = jobs::enqueue_script(s.clone(), host, Some(stack), "deploy".into(), script).await?;
    Ok(Json(json!({"job":job})))
}
pub fn update_script(s: &AppState, stack: &Stack, pull: bool) -> anyhow::Result<String> {
    let policy = policy(s, &stack.id)?;
    Ok(remote::python(
        &format!("{FUNCTIONS}\n{UPDATE}"),
        &json!({"stack":stack,"pull":pull,"snapshot":uuid::Uuid::new_v4().to_string(),"auto_rollback":policy.auto_rollback,"health_timeout":policy.health_timeout}),
    ))
}
pub const FUNCTIONS: &str = r#"
import signal
def interrupted(signum,frame):raise SystemExit("Remote operation interrupted.")
for sig in [signal.SIGTERM,signal.SIGHUP]:signal.signal(sig,interrupted)
# Checkpoints remain on the managed host; neither environment values nor file
# contents are sent back to operation history.
def checkpoint_root():return pathlib.Path.home()/'.local'/'share'/'dockmin'/'checkpoints'/s['id']
def load_checkpoint(ident):
    import uuid
    uuid.UUID(ident);root=checkpoint_root()/ident;point=json.loads((root/'manifest.json').read_text())
    if point['stack_id']!=s['id']:raise RuntimeError('Checkpoint belongs to another stack.')
    return point
def running_services():return compose(['ps','--status','running','--services']).stdout.split()
def save_checkpoint(ident,require_containers=True):
    root=checkpoint_root()/ident;root.mkdir(parents=True,mode=0o700,exist_ok=False);files=[]
    good=pathlib.Path(s['directory'])/'.dockmin'/'last-good';known=None
    if (good/'manifest.json').is_file():
        candidate=json.loads((good/'manifest.json').read_text())
        if candidate['sources']==sources() and candidate['project']==s['project']:known=candidate
    names=sources();dotenv=pathlib.Path(names[0]).parent/'.env'
    entries=known['files'] if known else [{'path':name,'file':None} for name in names+([str(dotenv)] if dotenv.is_file() else [])]
    for i,entry in enumerate(entries):
        name=entry['path'];source=good/entry['file'] if known else pathlib.Path(name)
        if source.is_symlink():raise RuntimeError('Checkpointing symlink Compose files is not supported.')
        saved=root/('file-'+str(i));shutil.copy2(source,saved);os.chmod(saved,0o600)
        files.append({'path':name,'backup':str(saved),'sha256':digest(saved.read_bytes()),'mode':entry.get('mode',source.stat().st_mode&0o777)})
    ids=compose(['ps','--all','--quiet']).stdout.split();images={};running=[]
    if ids:
        for c in json.loads(execute(['docker','inspect']+ids).stdout):
            service=(c['Config'].get('Labels') or {}).get('com.docker.compose.service')
            if not service:continue
            image=c['Image'];tag='dockmin-rollback/'+hashlib.sha256(s['id'].encode()).hexdigest()[:16]+':'+ident[:8]+'-'+hashlib.sha256(service.encode()).hexdigest()[:12]
            execute(['docker','image','tag',image,tag]);details=json.loads(execute(['docker','image','inspect',image]).stdout)[0];refs=details.get('RepoDigests') or [];images[service]={'image':tag,'registry':refs[0] if refs else None};
            if c['State']['Running']:running.append(service)
    if require_containers and not images:raise RuntimeError('No existing containers to checkpoint. Deploy this new stack first.')
    point={'id':ident,'stack_id':s['id'],'host_id':s['host_id'],'created':int(time.time()),'files':files,'images':images,'running':list(set(running))}
    (root/'manifest.json').write_text(json.dumps(point));os.chmod(root/'manifest.json',0o600)
    return point
def public_checkpoint(point):return {k:point[k] for k in ['id','stack_id','host_id','created','images','running']}
def ensure_checkpoint_images(point):
    for service,image in point['images'].items():
        if execute(['docker','image','inspect',image['image']],check=False).returncode:
            if not image.get('registry'):raise RuntimeError('A checkpoint image is missing and has no registry digest. Restore that image first.')
            execute(['docker','pull',image['registry']]);execute(['docker','image','tag',image['registry'],image['image']])
def restore_checkpoint(point):
    # Check every saved file and image before replacing anything.
    for f in point['files']:
        if digest(pathlib.Path(f['backup']).read_bytes())!=f['sha256']:raise RuntimeError('Checkpoint file checksum mismatch.')
        if pathlib.Path(f['path']).is_symlink():raise RuntimeError('Refusing to replace a symlink during rollback.')
    ensure_checkpoint_images(point)
    for f in point['files']:
        target=pathlib.Path(f['path']);fd,temp=tempfile.mkstemp(prefix='.dockmin-rollback-',dir=target.parent);os.close(fd);shutil.copy2(f['backup'],temp);os.chmod(temp,f.get('mode',0o600));os.replace(temp,target)
    active=pathlib.Path(s['directory'])/'.dockmin'/'active-images.json';active.parent.mkdir(parents=True,mode=0o700,exist_ok=True)
    active.write_text(json.dumps({'services':{name:{'image':item['image']} for name,item in point['images'].items()}}));os.chmod(active,0o600)
    currently=running_services();stop=[name for name in currently if name not in point['running']]
    if stop:compose(['stop']+stop)
    if point['running']:compose(['up','-d','--no-build','--pull','never','--wait','--wait-timeout',str(data.get('health_timeout',120))]+point['running'])
    remember_good_files(overwrite=True)
"#;
const UPDATE: &str = r#"
compose(['config','--quiet'],sources())
point=save_checkpoint(data['snapshot'],require_containers=data['pull'])
print('DOCKMIN_RECORD checkpoint '+json.dumps(public_checkpoint(point)),flush=True)
active=pathlib.Path(s['directory'])/'.dockmin'/'active-images.json'
previous_active=active.read_bytes() if active.is_file() else None
try:active.unlink()
except FileNotFoundError:pass
started=False
try:
    if data['pull']:
        print('Pulling stack images...',flush=True);result=compose(['pull']);print(result.stdout,flush=True)
    started=True
    print('Applying configuration and waiting for health...',flush=True)
    result=compose(['up','-d','--wait','--wait-timeout',str(data['health_timeout'])]);print(result.stdout,flush=True)
    if data['pull']:
        # Compose can retain a stale container despite a successful pull/up.
        # Compare actual image IDs rather than trusting Compose's image label.
        config=json.loads(compose(['config','--format','json']).stdout)
        def stale_services():
            ids=compose(['ps','-q']).stdout.split()
            stale=set()
            if ids:
                for container in json.loads(execute(['docker','inspect']+ids).stdout):
                    service=container['Config']['Labels'].get('com.docker.compose.service')
                    image=config['services'].get(service,{}).get('image')
                    if image:
                        expected=json.loads(execute(['docker','image','inspect',image]).stdout)[0]['Id']
                        if container['Image']!=expected:stale.add(service)
            return stale
        stale=stale_services()
        if stale:
            print('Replacing containers still using older images: '+', '.join(sorted(stale)),flush=True)
            result=compose(['up','-d','--force-recreate','--no-deps','--wait','--wait-timeout',str(data['health_timeout'])]+sorted(stale));print(result.stdout,flush=True)
            remaining=stale_services()
            if remaining:raise RuntimeError('Containers still use older images after recreation: '+', '.join(sorted(remaining)))
    remember_good_files(overwrite=True)
    print('Stack deployment succeeded.',flush=True)
except BaseException:
    if not started and previous_active is not None:active.write_bytes(previous_active)
    if data['auto_rollback'] and started and point['images']:
        try:restore_checkpoint(point);print('Deployment failed; previous images and configuration were restored. Application data was preserved.',flush=True)
        except BaseException as error:print('Automatic rollback failed: '+str(error),flush=True)
    elif point['images']:print('Deployment failed. A checkpoint is available for manual rollback.',flush=True)
    raise
"#;
