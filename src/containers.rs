use crate::{
    api::{AppState, Result, ensure, missing},
    db::Host,
    jobs, logs, remote, ssh,
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
pub fn valid_id(id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        (12..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid container identifier"
    );
    Ok(())
}
pub fn host(s: &AppState, id: &str) -> Result<Host> {
    s.db.hosts()?
        .into_iter()
        .find(|h| h.id == id)
        .ok_or_else(missing)
}
pub async fn list(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let host = host(&s, &id)?;
    Ok(Json(
        remote::read(&s, &host, &remote::python(LIST, &json!({})), 512 * 1024).await?,
    ))
}
pub async fn inspect(
    State(s): State<Arc<AppState>>,
    Path((id, cid)): Path<(String, String)>,
) -> Result<Json<Value>> {
    valid_id(&cid)?;
    let host = host(&s, &id)?;
    Ok(Json(
        remote::read(
            &s,
            &host,
            &remote::python(INSPECT, &json!({"container":cid})),
            128 * 1024,
        )
        .await?,
    ))
}
#[derive(Deserialize)]
pub struct Action {
    pub action: String,
    pub confirm: String,
    #[serde(default)]
    pub checkpoint: String,
}
pub async fn action(
    State(s): State<Arc<AppState>>,
    Path((id, cid)): Path<(String, String)>,
    Json(body): Json<Action>,
) -> Result<Json<Value>> {
    valid_id(&cid)?;
    ensure(
        ["start", "stop", "restart", "remove", "update", "rollback"]
            .contains(&body.action.as_str()),
        "Unsupported container action",
    )?;
    let _gate = s.mutations.lock().await;
    let host = host(&s, &id)?;
    let checkpoint = if body.action == "rollback" {
        s.db.record("container-checkpoint", &body.checkpoint)?
            .filter(|v| v["host_id"] == id && v["new_id"] == cid)
            .ok_or_else(missing)?
    } else {
        Value::Null
    };
    let script = remote::python(
        &format!("{ENGINE}\n{ACTION}"),
        &json!({"container":cid,"action":body.action,"confirm":body.confirm,"host_id":id,"checkpoint":checkpoint,"snapshot":uuid::Uuid::new_v4().to_string()}),
    );
    let job = jobs::enqueue_script(
        s.clone(),
        host,
        None,
        format!("container-{}", body.action),
        script,
    )
    .await?;
    Ok(Json(json!({"job":job})))
}
#[derive(Deserialize)]
pub struct Create {
    pub name: String,
    pub image: String,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default)]
    pub mounts: Vec<String>,
    #[serde(default)]
    pub command: Vec<String>,
    pub restart: String,
    #[serde(default)]
    pub start: bool,
    pub confirm: String,
}
pub async fn create(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Create>,
) -> Result<Json<Value>> {
    ensure(
        !body.name.is_empty()
            && body.name.len() <= 100
            && !body.name.starts_with('-')
            && body
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "Invalid container name",
    )?;
    ensure(body.name == body.confirm, "Confirm the container name")?;
    ensure(
        !body.image.is_empty()
            && body.image.len() <= 300
            && !body.image.starts_with('-')
            && !body
                .image
                .chars()
                .any(|c| c.is_control() || c.is_whitespace()),
        "Invalid image reference",
    )?;
    ensure(
        ["no", "always", "unless-stopped", "on-failure"].contains(&body.restart.as_str()),
        "Invalid restart policy",
    )?;
    ensure(
        body.environment.len() <= 100 && body.ports.len() <= 30 && body.mounts.len() <= 30,
        "Too many environment values, ports, or mounts",
    )?;
    ensure(
        body.environment
            .iter()
            .all(|v| v.len() <= 8192 && !v.contains('\0') && v.contains('=')),
        "Environment must use NAME=value",
    )?;
    let _gate = s.mutations.lock().await;
    let host = host(&s, &id)?;
    let script = remote::python(
        &format!("{ENGINE}\n{CREATE}"),
        &json!({"name":body.name,"image":body.image,"environment":body.environment,"ports":body.ports,"mounts":body.mounts,"restart":body.restart,"start":body.start,"command":body.command}),
    );
    let job =
        jobs::enqueue_script(s.clone(), host, None, "container-create".into(), script).await?;
    Ok(Json(json!({"job":job})))
}
pub async fn checkpoints(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    host(&s, &id)?;
    Ok(Json(
        json!({"checkpoints":s.db.records("container-checkpoint",&id)?}),
    ))
}
pub async fn stream(
    State(s): State<Arc<AppState>>,
    Path((id, cid)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<
    axum::response::Sse<
        impl futures_core::Stream<
            Item = std::result::Result<axum::response::sse::Event, std::convert::Infallible>,
        >,
    >,
> {
    valid_id(&cid)?;
    let host = host(&s, &id)?;
    let remote = format!("docker logs --follow --tail 150 {}", ssh::quote(&cid));
    logs::open(s, host, remote, headers).await
}
const LIST: &str = r#"
result=execute(['docker','ps','--all','--no-trunc','--format','json']);rows=[]
for line in result.stdout.splitlines():
    item=json.loads(line);labels=item.get('Labels','')
    if 'dockmin.helper=true' in labels:continue
    item['compose']='com.docker.compose.project=' in labels;rows.append(item)
emit({'containers':rows,'checked':int(time.time())})
"#;
const INSPECT: &str = r#"
c=json.loads(execute(['docker','inspect',data['container']]).stdout)[0]
emit({'id':c['Id'],'name':c['Name'].lstrip('/'),'image':c['Config']['Image'],'state':c['State'],'ports':c['NetworkSettings']['Ports'],'mounts':c['Mounts'],'restart':c['HostConfig']['RestartPolicy'],'compose':bool((c['Config'].get('Labels') or {}).get('com.docker.compose.project'))})
"#;
pub const ENGINE: &str = r#"
import http.client,socket,urllib.parse
endpoint=json.loads(execute(['docker','context','inspect','--format','{{json .Endpoints.docker.Host}}']).stdout)
if not endpoint.startswith('unix://'):raise RuntimeError('Container recreation requires a local Unix-socket Docker context.')
class DockerConnection(http.client.HTTPConnection):
    def connect(self):
        self.sock=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);self.sock.settimeout(60);self.sock.connect(endpoint[7:])
def api(method,path,body=None):
    connection=DockerConnection('localhost');connection.request(method,path,body=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json'});response=connection.getresponse();raw=response.read();connection.close()
    if response.status>=400:
        try:message=json.loads(raw).get('message','Docker operation failed')
        except ValueError:message='Docker operation failed'
        raise RuntimeError(message)
    return json.loads(raw) if raw else None
version=api('GET','/version')['ApiVersion'];prefix='/v'+version
"#;
const ACTION: &str = r#"
c=api('GET',prefix+'/containers/'+data['container']+'/json');name=c['Name'].lstrip('/')
if (c['Config'].get('Labels') or {}).get('com.docker.compose.project'):raise RuntimeError('This container belongs to Compose. Manage its stack instead.')
if data['confirm']!=name:raise RuntimeError('Confirm the current container name.')
action=data['action'];cid=c['Id']
if action in ['start','stop','restart']:
    api('POST',prefix+'/containers/'+cid+'/'+action);print('Container '+action+' completed.',flush=True)
elif action=='remove':
    if c['State']['Running']:raise RuntimeError('Stop the container before removing it.')
    api('DELETE',prefix+'/containers/'+cid+'?v=false');print('Container removed. Volumes preserved.',flush=True)
elif action=='update':
    if c['HostConfig'].get('AutoRemove'):raise RuntimeError('Auto-remove containers cannot retain a previous version.')
    if any(e.get('IPAMConfig') for e in c['NetworkSettings']['Networks'].values()):raise RuntimeError('This container uses a static IP. Recreate it with an explicit network migration instead.')
    execute(['docker','pull',c['Config']['Image']]);was_running=c['State']['Running'];old_name='dockmin-prev-'+name[:60]+'-'+data['snapshot'][:8]
    config=dict(c['Config']);config['HostConfig']=dict(c['HostConfig']);config['Image']=c['Config']['Image']
    declared={m.get('Target') for m in config['HostConfig'].get('Mounts') or []}
    for bind in config['HostConfig'].get('Binds') or []:
        parts=bind.split(':')
        if len(parts)>1:declared.add(parts[1])
    existing=config['HostConfig'].get('Mounts') or []
    for m in c['Mounts']:
        if m['Type']=='volume' and m['Destination'] not in declared:existing.append({'Type':'volume','Source':m['Name'],'Target':m['Destination'],'ReadOnly':not m['RW']})
    config['HostConfig']['Mounts']=existing
    networks={}
    for network,settings in c['NetworkSettings']['Networks'].items():
        aliases=[a for a in settings.get('Aliases') or [] if a not in [cid,cid[:12]]]
        networks[network]={'Aliases':aliases}
    config['NetworkingConfig']={'EndpointsConfig':networks}
    new_id=None;renamed=False
    try:
        if was_running:api('POST',prefix+'/containers/'+cid+'/stop')
        api('POST',prefix+'/containers/'+cid+'/rename?name='+urllib.parse.quote(old_name));renamed=True
        new_id=api('POST',prefix+'/containers/create?name='+urllib.parse.quote(name),config)['Id']
        if was_running:
            api('POST',prefix+'/containers/'+new_id+'/start')
            deadline=time.monotonic()+120
            while True:
                state=api('GET',prefix+'/containers/'+new_id+'/json')['State']
                if not state['Running']:raise RuntimeError('Updated container exited.')
                health=state.get('Health',{}).get('Status')
                if not health or health=='healthy':break
                if health=='unhealthy' or time.monotonic()>deadline:raise RuntimeError('Updated container did not become healthy.')
                time.sleep(2)
    except BaseException:
        if new_id:
            try:api('DELETE',prefix+'/containers/'+new_id+'?force=true&v=false')
            except BaseException:pass
        if renamed:api('POST',prefix+'/containers/'+cid+'/rename?name='+urllib.parse.quote(name))
        if was_running:api('POST',prefix+'/containers/'+cid+'/start')
        print('Update failed; the original container was restored. Application data was preserved.',flush=True);raise
    checkpoint={'id':data['snapshot'],'host_id':data['host_id'],'old_id':cid,'new_id':new_id,'name':name,'was_running':was_running,'created':int(time.time())}
    print('DOCKMIN_RECORD container-checkpoint '+json.dumps(checkpoint),flush=True);print('Container updated; its previous version is retained for rollback.',flush=True)
elif action=='rollback':
    point=data['checkpoint'];old=api('GET',prefix+'/containers/'+point['old_id']+'/json')
    if (old['Config'].get('Labels') or {}).get('com.docker.compose.project'):raise RuntimeError('Invalid standalone rollback target.')
    failed_name='dockmin-replaced-'+name[:60]+'-'+data['snapshot'][:8]
    renamed=False
    try:
        if c['State']['Running']:api('POST',prefix+'/containers/'+cid+'/stop')
        api('POST',prefix+'/containers/'+cid+'/rename?name='+urllib.parse.quote(failed_name));renamed=True
        api('POST',prefix+'/containers/'+old['Id']+'/rename?name='+urllib.parse.quote(name))
        if point['was_running']:api('POST',prefix+'/containers/'+old['Id']+'/start')
    except BaseException:
        try:api('POST',prefix+'/containers/'+old['Id']+'/rename?name='+urllib.parse.quote(old['Name'].lstrip('/')))
        except BaseException:pass
        if renamed:api('POST',prefix+'/containers/'+cid+'/rename?name='+urllib.parse.quote(name))
        if c['State']['Running']:api('POST',prefix+'/containers/'+cid+'/start')
        raise
    print('Previous container restored. Application data was not rolled back.',flush=True)
"#;
const CREATE: &str = r#"
import re
config={'Image':data['image'],'Env':data['environment'],'HostConfig':{'RestartPolicy':{'Name':data['restart']},'PortBindings':{},'Mounts':[]},'ExposedPorts':{}}
if data.get('command'):config['Cmd']=data['command']
for value in data['ports']:
    match=re.fullmatch(r'(?:(127\.0\.0\.1|0\.0\.0\.0):)?(\d+):(\d+)(?:/(tcp|udp))?',value)
    if not match or any(not 1<=int(n)<=65535 for n in match.group(2,3)):raise RuntimeError('Ports must use [127.0.0.1:]host:container[/tcp|udp].')
    key=match.group(3)+'/'+(match.group(4) or 'tcp');config['ExposedPorts'][key]={};config['HostConfig']['PortBindings'].setdefault(key,[]).append({'HostIp':match.group(1) or '', 'HostPort':match.group(2)})
for value in data['mounts']:
    parts=value.split(':')
    if len(parts) not in [2,3] or not parts[0] or not parts[1].startswith('/') or (len(parts)==3 and parts[2]!='ro'):raise RuntimeError('Mounts must use source:/container/path[:ro].')
    config['HostConfig']['Mounts'].append({'Type':'bind' if parts[0].startswith('/') else 'volume','Source':parts[0],'Target':parts[1],'ReadOnly':len(parts)==3})
execute(['docker','pull',data['image']])
cid=api('POST',prefix+'/containers/create?name='+urllib.parse.quote(data['name']),config)['Id']
if data['start']:api('POST',prefix+'/containers/'+cid+'/start')
print('Container created'+(' and started.' if data['start'] else '. Start it when ready.'),flush=True)
"#;

/// Both sides of retained checkpoints are needed for rollback chains.
pub fn prune_script(checkpoints: &[Value]) -> anyhow::Result<String> {
    let mut protected = std::collections::BTreeSet::new();
    for checkpoint in checkpoints {
        for key in ["old_id", "new_id"] {
            let id = checkpoint[key]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Invalid retained container checkpoint"))?;
            valid_id(id)?;
            protected.insert(id.to_string());
        }
    }
    Ok(remote::python(PRUNE, &json!({"protected":protected})))
}
const PRUNE: &str = r#"
protected=data['protected'];removed=0;retained=0
rows=execute(['docker','container','ls','--all','--no-trunc','--format','{{json .}}']).stdout
for line in rows.splitlines():
    c=json.loads(line)
    if c['State'] not in ['created','exited','dead']:continue
    cid=c['ID'];name=c['Names']
    if any(cid.startswith(p) for p in protected) or name.startswith(('dockmin-prev-','dockmin-replaced-')):
        retained+=1;continue
    # Do not force removal: Docker must reject a container started since listing.
    result=execute(['docker','container','rm',cid],check=False)
    if result.returncode:raise RuntimeError(result.stderr[-1500:] or 'Container cleanup failed')
    removed+=1
print('Removed '+str(removed)+' stopped containers; preserved '+str(retained)+' rollback containers. Volumes preserved.',flush=True)
"#;

#[cfg(test)]
mod cleanup_tests {
    use super::*;

    #[test]
    fn retained_checkpoint_ids_are_validated_before_cleanup() {
        assert!(prune_script(&[]).is_ok());
        assert!(prune_script(&[json!({"old_id":"a".repeat(64),"new_id":"b".repeat(64)})]).is_ok());
        assert!(prune_script(&[json!({"old_id":"a".repeat(64)})]).is_err());
        assert!(prune_script(&[json!({"old_id":"invalid","new_id":"b".repeat(64)})]).is_err());
    }
}
