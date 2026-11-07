use crate::{
    api::{AppState, Result, ensure},
    db::Stack,
    jobs, remote, ssh,
};
use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub path: String,
    pub content: String,
    pub revision: String,
}
#[derive(Deserialize)]
pub struct Edit {
    pub files: Vec<File>,
    #[serde(default)]
    pub confirm: String,
}
fn validate(body: &Edit) -> anyhow::Result<()> {
    anyhow::ensure!(
        !body.files.is_empty() && body.files.len() <= 8,
        "Edit one to eight Compose files"
    );
    let mut paths = std::collections::HashSet::new();
    let mut total = 0;
    for f in &body.files {
        anyhow::ensure!(
            !f.path.chars().any(char::is_control)
                && f.path.starts_with('/')
                && paths.insert(&f.path),
            "Invalid or duplicate file path"
        );
        anyhow::ensure!(
            f.content.len() <= 256 * 1024 && !f.content.contains('\0'),
            "Each Compose file must be text smaller than 256 KiB"
        );
        total += f.content.len();
        anyhow::ensure!(
            f.revision.len() == 64 && f.revision.bytes().all(|b| b.is_ascii_hexdigit()),
            "Reload files before editing"
        );
    }
    anyhow::ensure!(total <= 1024 * 1024, "Combined files exceed 1 MiB");
    Ok(())
}
pub async fn get(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let (stack, host) = remote::context(&s, &id)?;
    Ok(Json(
        remote::read(
            &s,
            &host,
            &remote::python(READ, &json!({"stack":stack})),
            8 * 1024 * 1024,
        )
        .await?,
    ))
}
pub async fn preview(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Edit>,
) -> Result<Json<Value>> {
    validate(&body)?;
    let (stack, host) = remote::context(&s, &id)?;
    Ok(Json(
        remote::read(
            &s,
            &host,
            &remote::python(
                STAGE,
                &json!({"stack":stack,"files":body.files,"save":false}),
            ),
            8 * 1024 * 1024,
        )
        .await?,
    ))
}
pub async fn save(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Edit>,
) -> Result<Json<Value>> {
    validate(&body)?;
    let _gate = s.mutations.lock().await;
    let (stack, host) = remote::context(&s, &id)?;
    ensure(
        body.confirm == stack.name,
        "Confirm the stack name to save Compose files",
    )?;
    let script = remote::python(
        STAGE,
        &json!({"stack":stack,"files":body.files,"save":true,"snapshot":uuid::Uuid::new_v4().to_string()}),
    );
    let job =
        jobs::enqueue_script(s.clone(), host, Some(stack), "compose-save".into(), script).await?;
    Ok(Json(json!({"job":job})))
}
#[derive(Deserialize)]
pub struct Create {
    pub stack: Stack,
    pub content: String,
    pub confirm: String,
}
pub async fn create(
    State(s): State<Arc<AppState>>,
    Json(mut body): Json<Create>,
) -> Result<Json<Value>> {
    ssh::validate_stack(&body.stack)?;
    ensure(
        body.stack.id.is_empty(),
        "Use the existing-stack editor for registered stacks",
    )?;
    ensure(
        body.confirm == body.stack.name,
        "Confirm the new stack name",
    )?;
    ensure(
        body.stack.files.is_empty() || body.stack.files == vec!["compose.yaml"],
        "New stacks use compose.yaml; add override files after creation",
    )?;
    ensure(
        body.content.len() <= 256 * 1024 && !body.content.contains('\0'),
        "Compose content must be text smaller than 256 KiB",
    )?;
    let _gate = s.mutations.lock().await;
    let host =
        s.db.hosts()?
            .into_iter()
            .find(|h| h.id == body.stack.host_id)
            .ok_or_else(crate::api::missing)?;
    ensure(!s.db.active_on_host(&host.id)?, "Host is busy")?;
    let result = remote::read(
        &s,
        &host,
        &remote::python(CREATE, &json!({"stack":body.stack,"content":body.content})),
        8192,
    )
    .await?;
    ensure(result["created"] == true, "Compose stack was not created")?;
    body.stack.id = uuid::Uuid::new_v4().to_string();
    body.stack.files = vec!["compose.yaml".into()];
    s.db.save_stack(&body.stack)?;
    Ok(Json(json!({"stack":body.stack})))
}
const READ: &str = r#"
files=[]
for name in sources():
    p=pathlib.Path(name)
    if p.is_symlink() or not p.is_file():raise RuntimeError('Compose files must be regular files, not symlinks.')
    if p.stat().st_size>262144:raise RuntimeError('Compose file exceeds 256 KiB.')
    raw=p.read_bytes();files.append({'path':name,'content':raw.decode('utf-8'),'revision':digest(raw)})
emit({'files':files})
"#;
const STAGE: &str = r#"
import difflib
original=sources();edits=data['files'];staged={};backups=[];replaced=[]
try:
    for f in edits:
        name=f['path']
        if name not in original:raise RuntimeError('File is no longer part of this stack. Reload the editor.')
        p=pathlib.Path(name)
        if p.is_symlink() or digest(p.read_bytes())!=f['revision']:raise RuntimeError('File changed outside Dockmin. Reload before saving.')
        fd,temp=tempfile.mkstemp(prefix='.dockmin-edit-',suffix=p.suffix,dir=p.parent)
        with os.fdopen(fd,'w') as handle:handle.write(f['content'])
        os.chmod(temp,p.stat().st_mode&0o777);staged[name]=temp
    paths=[staged.get(name,name) for name in original]
    try:compose(['config','--quiet'],paths)
    except RuntimeError as error:
        if data['save']:raise
        emit({'valid':False,'error':str(error),'diff':''});sys.exit(0)
    diff=''.join(''.join(difflib.unified_diff(pathlib.Path(f['path']).read_text().splitlines(True),f['content'].splitlines(True),fromfile=f['path'],tofile=f['path']+' (edited)')) for f in edits)
    if not data['save']:emit({'valid':True,'diff':diff});sys.exit(0)
    remember_good_files()
    root=pathlib.Path(s['directory'])/'.dockmin'/'compose-history'/data['snapshot'];root.mkdir(parents=True,mode=0o700)
    for i,f in enumerate(edits):
        p=pathlib.Path(f['path'])
        if p.is_symlink() or digest(p.read_bytes())!=f['revision']:raise RuntimeError('File changed during validation. Reload before saving.')
        saved=root/str(i);shutil.copy2(p,saved);os.chmod(saved,0o600);backups.append({'path':str(p),'backup':str(saved)})
    (root/'manifest.json').write_text(json.dumps({'created':int(time.time()),'files':backups}));os.chmod(root/'manifest.json',0o600)
    try:
        for name,temp in staged.items():os.replace(temp,name);replaced.append(name)
    except BaseException:
        for entry in backups:
            if entry['path'] in replaced:shutil.copy2(entry['backup'],entry['path'])
        raise
    print('Compose files validated and saved. Previous files: '+str(root),flush=True)
    print('Containers are unchanged. Use Deploy saved configuration when ready.',flush=True)
finally:
    for temp in staged.values():
        try:os.unlink(temp)
        except FileNotFoundError:pass
"#;
const CREATE: &str = r#"
root=pathlib.Path(s['directory'])
if root.exists():raise RuntimeError('Choose a new, non-existing directory to create a stack.')
root.mkdir(parents=True,mode=0o700)
p=root/'compose.yaml';p.write_text(data['content']);os.chmod(p,0o600)
try:compose(['config','--quiet'])
except BaseException:
    p.unlink();root.rmdir();raise
emit({'created':True})
"#;
