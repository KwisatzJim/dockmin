use crate::{
    api::{ApiError, AppState, Result, missing},
    db::{Host, Stack},
    ssh,
};
use base64ct::{Base64, Encoding};
use serde_json::Value;
use std::sync::Arc;
pub fn python_source(body: &str, data: &Value) -> String {
    format!(
        "import os,sys,json,base64,subprocess,pathlib,hashlib,tempfile,shutil,time\ndata=json.loads(base64.b64decode('{}'))\n{}\n{}\n",
        Base64::encode_string(serde_json::to_string(data).unwrap().as_bytes()),
        COMMON,
        body
    )
}
pub fn python(body: &str, data: &Value) -> String {
    format!(
        "exec python3 - <<'DOCKMIN_PYTHON'\n{}DOCKMIN_PYTHON\n",
        python_source(body, data)
    )
}
pub const COMMON: &str = r#"
def execute(args,cwd=None,check=True):
    result=subprocess.run(args,cwd=cwd,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    if check and result.returncode:raise RuntimeError(result.stderr[-1500:] or 'Command failed')
    return result
s=data.get('stack')
def sources():
    if s['files']:return [str((pathlib.Path(s['directory'])/p).absolute()) for p in s['files']]
    for name in ['compose.yaml','compose.yml','docker-compose.yml','docker-compose.yaml']:
        p=pathlib.Path(s['directory'])/name
        if p.is_file():return [str(p)]
    raise RuntimeError('No Compose file found in the registered directory; configure explicit file paths.')
def compose(extra,files=None):
    args=['docker','compose']
    if s['project']:args+=['--project-name',s['project']]
    chosen=sources() if files is None else files
    for p in chosen:args+=['--file',p]
    active=pathlib.Path(s['directory'])/'.dockmin'/'active-images.json'
    if files is None and active.is_file():args+=['--file',str(active)]
    args+=['--project-directory',str(pathlib.Path(sources()[0]).parent)]
    return execute(args+extra,cwd=s['directory'])
def remember_good_files(overwrite=False):
    root=pathlib.Path(s['directory'])/'.dockmin'/'last-good'
    if root.exists() and not overwrite:return
    root.parent.mkdir(parents=True,mode=0o700,exist_ok=True)
    temp=root.parent/('good-'+str(__import__('uuid').uuid4()));temp.mkdir(mode=0o700)
    names=sources();dotenv=pathlib.Path(names[0]).parent/'.env';all_names=names+([str(dotenv)] if dotenv.is_file() else []);files=[]
    for i,name in enumerate(all_names):
        source=pathlib.Path(name);saved=temp/str(i);shutil.copy2(source,saved);os.chmod(saved,0o600);files.append({'path':name,'file':str(i),'mode':source.stat().st_mode&0o777})
    (temp/'manifest.json').write_text(json.dumps({'sources':names,'project':s['project'],'files':files}));os.chmod(temp/'manifest.json',0o600)
    old=root.parent/('old-good-'+str(__import__('uuid').uuid4()))
    if root.exists():os.rename(root,old)
    os.rename(temp,root)
    if old.exists():shutil.rmtree(old)
def digest(raw):return hashlib.sha256(raw).hexdigest()
def emit(value):print(json.dumps(value),flush=True)
"#;
pub fn context(s: &AppState, id: &str) -> Result<(Stack, Host)> {
    let stack =
        s.db.stacks()?
            .into_iter()
            .find(|v| v.id == id)
            .ok_or_else(missing)?;
    let host =
        s.db.hosts()?
            .into_iter()
            .find(|v| v.id == stack.host_id)
            .ok_or_else(missing)?;
    Ok((stack, host))
}
pub async fn read(s: &Arc<AppState>, host: &Host, script: &str, limit: usize) -> Result<Value> {
    let _lock = s.host_lock(&host.id).try_lock_owned().map_err(|_| {
        ApiError(
            axum::http::StatusCode::CONFLICT,
            "Host is busy; try again shortly".into(),
        )
    })?;
    let _slot = s.try_slot().ok_or_else(|| {
        ApiError(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "Service is busy; try again shortly".into(),
        )
    })?;
    let capture = ssh::capture_script(&s.config, host, script, 60, limit).await?;
    if !capture.success {
        return Err(ApiError(
            axum::http::StatusCode::BAD_REQUEST,
            capture
                .stderr
                .chars()
                .rev()
                .take(1500)
                .collect::<String>()
                .chars()
                .rev()
                .collect(),
        ));
    }
    Ok(serde_json::from_str(&capture.stdout).map_err(anyhow::Error::from)?)
}
