use crate::{
    api::{AppState, Result, ensure, missing},
    auth, containers,
    db::{Job, now},
    remote, ssh,
};
use axum::{
    Json,
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::HeaderMap,
    response::Response,
};
use base64ct::{Base64, Encoding};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{process::Stdio, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn token_hash(token: &str) -> String {
    Base64::encode_string(&Sha256::digest(token.as_bytes()))
}
#[derive(Deserialize)]
pub struct Ticket {
    pub shell: String,
    pub confirm: String,
}
pub async fn ticket(
    State(s): State<Arc<AppState>>,
    Path((id, cid)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Ticket>,
) -> Result<Json<Value>> {
    containers::valid_id(&cid)?;
    ensure(
        ["/bin/sh", "/bin/bash", "/bin/ash"].contains(&body.shell.as_str()),
        "Choose sh, bash, or ash",
    )?;
    let host = containers::host(&s, &id)?;
    let inspected=remote::read(&s,&host,&remote::python("c=json.loads(execute(['docker','inspect',data['container']]).stdout)[0]\nemit({'name':c['Name'].lstrip('/'),'running':c['State']['Running']})",&json!({"container":cid})),8192).await?;
    ensure(
        inspected["running"] == true,
        "Start the container before opening a terminal",
    )?;
    ensure(
        inspected["name"].as_str() == Some(&body.confirm),
        "Confirm the current container name",
    )?;
    let key = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    s.db.put_record("terminal-ticket",&key,&id,&json!({"host_id":id,"container":cid,"shell":body.shell,"expires":now()+60,"session":token_hash(auth::token(&headers).unwrap_or_default())}))?;
    s.db.0.lock().unwrap().execute("DELETE FROM records WHERE kind='terminal-ticket' AND rowid NOT IN (SELECT rowid FROM records WHERE kind='terminal-ticket' ORDER BY rowid DESC LIMIT 64)",[]).map_err(anyhow::Error::from)?;
    Ok(Json(json!({"ticket":key})))
}
#[derive(Deserialize)]
pub struct Connect {
    pub ticket: String,
}
pub async fn connect(
    State(s): State<Arc<AppState>>,
    Path((id, cid)): Path<(String, String)>,
    Query(query): Query<Connect>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response> {
    ensure(
        auth::origin_valid(&s, &headers),
        "Terminal origin is not allowed",
    )?;
    containers::valid_id(&cid)?;
    let token = auth::token(&headers).unwrap_or_default().to_string();
    let point =
        s.db.take_record("terminal-ticket", &query.ticket)?
            .ok_or_else(missing)?;
    ensure(
        point["host_id"] == id
            && point["container"] == cid
            && point["session"] == token_hash(&token)
            && point["expires"].as_i64().is_some_and(|t| t > now()),
        "Terminal ticket expired or does not match this session",
    )?;
    let host = containers::host(&s, &id)?;
    let permit = s.terminal_slots.clone().try_acquire_owned().map_err(|_| {
        crate::api::ApiError(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "Four terminals are already open".into(),
        )
    })?;
    let shell = point["shell"].as_str().unwrap_or("/bin/sh").to_string();
    Ok(ws
        .max_message_size(32768)
        .max_frame_size(32768)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            serve(s, host, cid, shell, token, socket).await;
        }))
}
async fn serve(
    s: Arc<AppState>,
    host: crate::db::Host,
    cid: String,
    shell: String,
    token: String,
    mut socket: WebSocket,
) {
    let program = format!(
        "import os,sys,json,base64,subprocess,pathlib,hashlib,tempfile,shutil,time\ndata=json.loads(base64.b64decode('{}'))\n{}\n{}\n{}",
        Base64::encode_string(
            json!({"container":cid,"shell":shell})
                .to_string()
                .as_bytes()
        ),
        remote::COMMON,
        containers::ENGINE,
        BRIDGE
    );
    let original = ssh::stdin_command(&s.config, &host);
    let args: Vec<_> = original.as_std().get_args().map(|a| a.to_owned()).collect();
    let mut command = tokio::process::Command::new("ssh");
    command
        .args(&args[..args.len() - 1])
        .arg(format!(
            "exec python3 -c 'import base64;exec(base64.b64decode(\"{}\"))'",
            Base64::encode_string(program.as_bytes())
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(_) => {
            let _ = socket
                .send(Message::Text("Could not start SSH terminal".into()))
                .await;
            return;
        }
    };
    let job = Job {
        id: uuid::Uuid::new_v4().to_string(),
        host_id: host.id,
        stack_id: None,
        action: "terminal".into(),
        state: "running".into(),
        created: now(),
        finished: None,
        output: "Interactive terminal opened. Input and output are not recorded.".into(),
    };
    let _ = s.db.create_job(&job);
    let mut input = child.stdin.take().unwrap();
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let mut a = [0u8; 8192];
    let mut b = [0u8; 4096];
    let mut errors_open = true;
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(3600));
    tokio::pin!(deadline);
    loop {
        if s.stopping.load(std::sync::atomic::Ordering::SeqCst)
            || s.db.session(&token).ok().flatten().is_none()
        {
            break;
        }
        tokio::select! {
         _=&mut deadline=>{break;},_=interval.tick()=>{},_=s.shutdown_changed.notified()=>{break;},
         message=socket.recv()=>{match message {Some(Ok(Message::Text(text)))=>{let Ok(value)=serde_json::from_str::<Value>(&text) else{break;};let valid=match value["type"].as_str(){Some("input")=>value["data"].as_str().is_some_and(|v|v.len()<=16384),Some("resize")=>value["cols"].as_u64().is_some_and(|n|(20..=400).contains(&n))&&value["rows"].as_u64().is_some_and(|n|(5..=200).contains(&n)),_=>false};if !valid{break;}let framed=format!("{}\n",value);if input.write_all(framed.as_bytes()).await.is_err(){break;}},Some(Ok(Message::Ping(bytes)))=>{if socket.send(Message::Pong(bytes)).await.is_err(){break;}},Some(Ok(Message::Pong(_)))=>{},_=>break,}},
         read=out.read(&mut a)=>{match read{Ok(0)|Err(_)=>break,Ok(n)=>{if socket.send(Message::Binary(a[..n].to_vec().into())).await.is_err(){break;}}}},
         read=err.read(&mut b),if errors_open=>{match read{Ok(0)|Err(_)=>errors_open=false,Ok(n)=>{if socket.send(Message::Binary(b[..n].to_vec().into())).await.is_err(){break;}}}},
        }
    }
    let _ = child.kill().await;
    let _=s.db.update_job(&job.id,"succeeded","Terminal disconnected. Input and output were not recorded; commands may already have changed the container.",true);
    let _ = socket.send(Message::Close(None)).await;
}
const BRIDGE: &str = r#"
import selectors
info=api('GET',prefix+'/containers/'+data['container']+'/json')
if not info['State']['Running']:raise RuntimeError('Container is no longer running.')
eid=api('POST',prefix+'/containers/'+data['container']+'/exec',{'AttachStdin':True,'AttachStdout':True,'AttachStderr':True,'Tty':True,'Cmd':[data['shell']]})['Id']
wire=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);wire.settimeout(30);wire.connect(endpoint[7:]);body=json.dumps({'Detach':False,'Tty':True}).encode()
wire.sendall(('POST '+prefix+'/exec/'+eid+'/start HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nConnection: Upgrade\r\nUpgrade: tcp\r\nContent-Length: '+str(len(body))+'\r\n\r\n').encode()+body)
header=b''
while b'\r\n\r\n' not in header:
    chunk=wire.recv(4096)
    if not chunk or len(header)>65536:raise RuntimeError('Docker terminal handshake failed.')
    header+=chunk
head,initial=header.split(b'\r\n\r\n',1)
if head.split(b' ',2)[1] not in [b'101',b'200']:raise RuntimeError('Docker terminal start failed.')
if initial:os.write(1,initial)
wire.settimeout(None);selector=selectors.DefaultSelector();selector.register(wire,selectors.EVENT_READ,'docker');selector.register(sys.stdin.buffer,selectors.EVENT_READ,'input');pending=b''
try:
    while True:
        for key,_ in selector.select():
            if key.data=='docker':
                chunk=wire.recv(8192)
                if not chunk:sys.exit(0)
                os.write(1,chunk)
            else:
                chunk=os.read(0,32768)
                if not chunk:sys.exit(0)
                pending+=chunk
                while b'\n' in pending:
                    line,pending=pending.split(b'\n',1);value=json.loads(line)
                    if value['type']=='input':wire.sendall(value['data'].encode())
                    elif value['type']=='resize':api('POST',prefix+'/exec/'+eid+'/resize?h='+str(value['rows'])+'&w='+str(value['cols']))
                if len(pending)>65536:raise RuntimeError('Terminal input frame too large.')
finally:wire.close()
"#;
