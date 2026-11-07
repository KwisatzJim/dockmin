use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Mutex};
use uuid::Uuid;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Host {
    pub id: String,
    pub name: String,
    pub alias: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Stack {
    pub id: String,
    pub host_id: String,
    pub name: String,
    pub directory: String,
    pub project: String,
    pub files: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub host_id: String,
    pub stack_id: Option<String>,
    pub action: String,
    pub state: String,
    pub created: i64,
    pub finished: Option<i64>,
    pub output: String,
}
pub struct Database(pub Mutex<Connection>);
impl Database {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        let c = Connection::open(path)?;
        c.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS hosts(id TEXT PRIMARY KEY,name TEXT NOT NULL,alias TEXT NOT NULL UNIQUE);
        CREATE TABLE IF NOT EXISTS stacks(id TEXT PRIMARY KEY,host_id TEXT NOT NULL REFERENCES hosts(id),name TEXT NOT NULL,directory TEXT NOT NULL,project TEXT NOT NULL,files TEXT NOT NULL,UNIQUE(host_id,directory,project,files));
        CREATE TABLE IF NOT EXISTS stack_status(stack_id TEXT PRIMARY KEY REFERENCES stacks(id) ON DELETE CASCADE,value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY,host_id TEXT NOT NULL,stack_id TEXT,action TEXT NOT NULL,state TEXT NOT NULL,created INTEGER NOT NULL,finished INTEGER,output TEXT NOT NULL DEFAULT '');
        CREATE TABLE IF NOT EXISTS host_health(host_id TEXT PRIMARY KEY REFERENCES hosts(id) ON DELETE CASCADE,value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS records(kind TEXT NOT NULL,id TEXT NOT NULL,owner TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(kind,id));
        CREATE TABLE IF NOT EXISTS notifications(id TEXT PRIMARY KEY,created INTEGER NOT NULL,title TEXT NOT NULL,body TEXT NOT NULL,read INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS sessions(token TEXT PRIMARY KEY,csrf TEXT NOT NULL,expires INTEGER NOT NULL);")?;
        Ok(Self(Mutex::new(c)))
    }
    pub fn password(&self) -> anyhow::Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .0
            .lock()
            .unwrap()
            .query_row("SELECT value FROM settings WHERE key='password'", [], |r| {
                r.get(0)
            })
            .optional()?)
    }
    pub fn set_password(&self, hash: &str) -> anyhow::Result<()> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO settings VALUES('password',?1)",
            [hash],
        )?;
        tx.execute("DELETE FROM sessions", [])?;
        tx.commit()?;
        Ok(())
    }
    pub fn session(&self, token: &str) -> anyhow::Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT csrf FROM sessions WHERE token=?1 AND expires>?2",
                params![token, now()],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn new_session(&self) -> anyhow::Result<(String, String)> {
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let csrf = Uuid::new_v4().to_string();
        let c = self.0.lock().unwrap();
        c.execute("DELETE FROM sessions WHERE expires<=?1", [now()])?;
        c.execute("DELETE FROM sessions WHERE rowid NOT IN (SELECT rowid FROM sessions ORDER BY rowid DESC LIMIT 63)", [])?;
        c.execute(
            "INSERT INTO sessions VALUES(?1,?2,?3)",
            params![token, csrf, now() + 43200],
        )?;
        Ok((token, csrf))
    }
    pub fn logout(&self, token: &str) -> anyhow::Result<()> {
        self.0
            .lock()
            .unwrap()
            .execute("DELETE FROM sessions WHERE token=?1", [token])?;
        Ok(())
    }
    pub fn hosts(&self) -> anyhow::Result<Vec<Host>> {
        let c = self.0.lock().unwrap();
        let mut q = c.prepare("SELECT id,name,alias FROM hosts ORDER BY name")?;
        Ok(q.query_map([], |r| {
            Ok(Host {
                id: r.get(0)?,
                name: r.get(1)?,
                alias: r.get(2)?,
            })
        })?
        .collect::<Result<_, _>>()?)
    }
    pub fn save_host(&self, h: &Host) -> anyhow::Result<()> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute("INSERT INTO hosts VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET name=excluded.name,alias=excluded.alias",params![h.id,h.name,h.alias])?;
        tx.execute(
            "DELETE FROM stack_status WHERE stack_id IN (SELECT id FROM stacks WHERE host_id=?1)",
            [&h.id],
        )?;
        tx.execute("DELETE FROM host_health WHERE host_id=?1", [&h.id])?;
        let stack_ids: Vec<String> = {
            let mut query = tx.prepare("SELECT id FROM stacks WHERE host_id=?1")?;
            query
                .query_map([&h.id], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        for id in stack_ids {
            tx.execute("INSERT INTO jobs(id,host_id,stack_id,action,state,created,finished,output) VALUES(?1,?2,?3,'configure','succeeded',?4,?4,'Host configuration changed. Check images again.')",params![Uuid::new_v4().to_string(),h.id,id,now()])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn delete_host(&self, id: &str) -> anyhow::Result<()> {
        self.0
            .lock()
            .unwrap()
            .execute("DELETE FROM hosts WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn stacks(&self) -> anyhow::Result<Vec<Stack>> {
        let c = self.0.lock().unwrap();
        let mut q =
            c.prepare("SELECT id,host_id,name,directory,project,files FROM stacks ORDER BY name")?;
        Ok(q.query_map([], |r| {
            let files: String = r.get(5)?;
            Ok(Stack {
                id: r.get(0)?,
                host_id: r.get(1)?,
                name: r.get(2)?,
                directory: r.get(3)?,
                project: r.get(4)?,
                files: serde_json::from_str(&files).unwrap_or_default(),
            })
        })?
        .collect::<Result<_, _>>()?)
    }
    pub fn save_stack(&self, s: &Stack) -> anyhow::Result<()> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute("INSERT INTO jobs(id,host_id,stack_id,action,state,created,finished,output) SELECT ?1,host_id,id,'configure','succeeded',?2,?2,'Stack configuration changed. Check images again.' FROM stacks WHERE id=?3",params![Uuid::new_v4().to_string(),now(),s.id])?;
        tx.execute("INSERT INTO stacks VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET host_id=excluded.host_id,name=excluded.name,directory=excluded.directory,project=excluded.project,files=excluded.files",params![s.id,s.host_id,s.name,s.directory,s.project,serde_json::to_string(&s.files)?])?;
        tx.execute("DELETE FROM stack_status WHERE stack_id=?1", [&s.id])?;
        tx.commit()?;
        Ok(())
    }
    pub fn delete_stack(&self, id: &str) -> anyhow::Result<()> {
        self.0
            .lock()
            .unwrap()
            .execute("DELETE FROM stacks WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn create_job(&self, j: &Job) -> anyhow::Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT INTO jobs(id,host_id,stack_id,action,state,created) VALUES(?1,?2,?3,?4,?5,?6)",
            params![j.id, j.host_id, j.stack_id, j.action, j.state, j.created],
        )?;
        Ok(())
    }
    pub fn update_job(
        &self,
        id: &str,
        state: &str,
        output: &str,
        finished: bool,
    ) -> anyhow::Result<()> {
        self.0.lock().unwrap().execute(
            "UPDATE jobs SET state=?2,output=?3,finished=?4 WHERE id=?1",
            params![id, state, output, if finished { Some(now()) } else { None }],
        )?;
        Ok(())
    }
    pub fn jobs(&self) -> anyhow::Result<Vec<Job>> {
        let c = self.0.lock().unwrap();
        let mut q=c.prepare("SELECT id,host_id,stack_id,action,state,created,finished,output FROM jobs WHERE finished IS NULL OR id IN (SELECT id FROM jobs WHERE finished IS NOT NULL ORDER BY created DESC,rowid DESC LIMIT ?1) ORDER BY created DESC,rowid DESC")?;
        let limit = self
            .get_setting_locked(&c, "runtime")?
            .and_then(|v| v["history_limit"].as_u64())
            .unwrap_or(100);
        Ok(q.query_map([limit as i64], |r| {
            Ok(Job {
                id: r.get(0)?,
                host_id: r.get(1)?,
                stack_id: r.get(2)?,
                action: r.get(3)?,
                state: r.get(4)?,
                created: r.get(5)?,
                finished: r.get(6)?,
                output: r.get(7)?,
            })
        })?
        .collect::<Result<_, _>>()?)
    }
    pub fn job(&self, id: &str) -> anyhow::Result<Option<Job>> {
        use rusqlite::OptionalExtension;
        Ok(self.0.lock().unwrap().query_row("SELECT id,host_id,stack_id,action,state,created,finished,output FROM jobs WHERE id=?1",[id],|r| Ok(Job{id:r.get(0)?,host_id:r.get(1)?,stack_id:r.get(2)?,action:r.get(3)?,state:r.get(4)?,created:r.get(5)?,finished:r.get(6)?,output:r.get(7)?})).optional()?)
    }
    pub fn recover(&self) -> anyhow::Result<()> {
        self.0.lock().unwrap().execute("UPDATE jobs SET state='interrupted',finished=?1,output=output || char(10) || 'Service restarted. Refresh status before retrying; remote outcome may be unknown.' WHERE state IN ('queued','running')",[now()])?;
        Ok(())
    }
    pub fn set_status(&self, id: &str, value: &serde_json::Value) -> anyhow::Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT OR REPLACE INTO stack_status VALUES(?1,?2)",
            params![id, serde_json::to_string(value)?],
        )?;
        Ok(())
    }
    pub fn statuses(&self) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
        let c = self.0.lock().unwrap();
        let mut q = c.prepare("SELECT stack_id,value FROM stack_status")?;
        let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut result = serde_json::Map::new();
        for row in rows {
            let (id, value) = row?;
            result.insert(id, serde_json::from_str(&value)?);
        }
        Ok(result)
    }
    fn get_setting_locked(
        &self,
        c: &Connection,
        key: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        use rusqlite::OptionalExtension;
        let raw: Option<String> = c
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        raw.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }
    pub fn get_setting(&self, key: &str) -> anyhow::Result<Option<serde_json::Value>> {
        self.get_setting_locked(&self.0.lock().unwrap(), key)
    }
    pub fn set_setting(&self, key: &str, value: &serde_json::Value) -> anyhow::Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT OR REPLACE INTO settings VALUES(?1,?2)",
            params![key, serde_json::to_string(value)?],
        )?;
        Ok(())
    }
    pub fn health(
        &self,
    ) -> anyhow::Result<std::collections::HashMap<String, crate::monitor::Health>> {
        let c = self.0.lock().unwrap();
        let mut q = c.prepare("SELECT host_id,value FROM host_health")?;
        let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut values = std::collections::HashMap::new();
        for row in rows {
            let (id, value) = row?;
            values.insert(id, serde_json::from_str(&value)?);
        }
        Ok(values)
    }
    pub fn set_health(&self, id: &str, value: &crate::monitor::Health) -> anyhow::Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT OR REPLACE INTO host_health VALUES(?1,?2)",
            params![id, serde_json::to_string(value)?],
        )?;
        Ok(())
    }
    pub fn import_config(
        &self,
        data: &crate::data::Export,
        apply_settings: bool,
    ) -> anyhow::Result<(usize, usize)> {
        use rusqlite::OptionalExtension;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let mut mapping = std::collections::HashMap::new();
        let mut host_count = 0;
        let mut stack_count = 0;
        for host in &data.hosts {
            let existing: Option<String> = tx
                .query_row("SELECT id FROM hosts WHERE alias=?1", [&host.alias], |r| {
                    r.get(0)
                })
                .optional()?;
            let id = if let Some(id) = existing {
                id
            } else {
                let id = Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO hosts VALUES(?1,?2,?3)",
                    params![id, host.name, host.alias],
                )?;
                host_count += 1;
                id
            };
            mapping.insert(&host.id, id);
        }
        let mut stack_mapping = std::collections::HashMap::new();
        for stack in &data.stacks {
            let host_id = &mapping[&stack.host_id];
            let files = serde_json::to_string(&stack.files)?;
            let existing:Option<String>=tx.query_row("SELECT id FROM stacks WHERE host_id=?1 AND directory=?2 AND project=?3 AND files=?4",params![host_id,stack.directory,stack.project,files],|r|r.get(0)).optional()?;
            let id = if let Some(id) = existing {
                id
            } else {
                let id = Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO stacks VALUES(?1,?2,?3,?4,?5,?6)",
                    params![
                        id,
                        host_id,
                        stack.name,
                        stack.directory,
                        stack.project,
                        files
                    ],
                )?;
                stack_count += 1;
                id
            };
            stack_mapping.insert(&stack.id, id);
        }
        for connection in &data.connections {
            let original = data
                .hosts
                .iter()
                .find(|h| h.alias == connection.alias)
                .ok_or_else(|| anyhow::anyhow!("Connection has no imported host"))?;
            tx.execute(
                "INSERT OR IGNORE INTO records VALUES('ssh-host',?1,'',?2)",
                params![mapping[&original.id], serde_json::to_string(connection)?],
            )?;
        }
        if apply_settings {
            tx.execute(
                "INSERT OR REPLACE INTO settings VALUES('runtime',?1)",
                [serde_json::to_string(&data.settings)?],
            )?;
        }
        if apply_settings {
            if let Some(original) = &data.automation {
                let mut schedule = original.clone();
                schedule.stack_ids = original
                    .stack_ids
                    .iter()
                    .map(|id| stack_mapping[id].clone())
                    .collect();
                tx.execute(
                    "INSERT OR REPLACE INTO settings VALUES('automation',?1)",
                    [serde_json::to_string(&schedule)?],
                )?;
            }
            tx.execute("DELETE FROM jobs WHERE finished IS NOT NULL AND id NOT IN (SELECT id FROM jobs WHERE finished IS NOT NULL ORDER BY created DESC,rowid DESC LIMIT ?1)",[data.settings.history_limit as i64])?;
        }
        tx.commit()?;
        Ok((host_count, stack_count))
    }
    pub fn backup_bytes(&self, path: &Path) -> anyhow::Result<Vec<u8>> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let destination = parent.join(format!(".backup-{}.sqlite", Uuid::new_v4()));
        let result = (|| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&destination)?;
            }
            let c = self.0.lock().unwrap();
            c.backup("main", &destination, None)?;
            drop(c);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o600))?;
            }
            let backup = Connection::open(&destination)?;
            backup.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA secure_delete=ON; DELETE FROM sessions; DELETE FROM records WHERE kind='terminal-ticket'; VACUUM;")?;
            drop(backup);
            let metadata = std::fs::metadata(&destination)?;
            anyhow::ensure!(
                metadata.len() <= 128 * 1024 * 1024,
                "Backup exceeds the 128 MiB download limit"
            );
            Ok(std::fs::read(&destination)?)
        })();
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", destination.display()));
        }
        result
    }
    pub fn add_managed_host(&self, h: &Host, value: &serde_json::Value) -> anyhow::Result<()> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO hosts VALUES(?1,?2,?3)",
            params![h.id, h.name, h.alias],
        )?;
        tx.execute(
            "INSERT INTO records VALUES('ssh-host',?1,'',?2)",
            params![h.id, serde_json::to_string(value)?],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn active_on_host(&self, id: &str) -> anyhow::Result<bool> {
        Ok(self.0.lock().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE host_id=?1 AND finished IS NULL)",
            [id],
            |r| r.get(0),
        )?)
    }
    pub fn record(&self, kind: &str, id: &str) -> anyhow::Result<Option<serde_json::Value>> {
        use rusqlite::OptionalExtension;
        let raw: Option<String> = self
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT value FROM records WHERE kind=?1 AND id=?2",
                params![kind, id],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|v| serde_json::from_str(&v).map_err(Into::into))
            .transpose()
    }
    pub fn take_record(&self, kind: &str, id: &str) -> anyhow::Result<Option<serde_json::Value>> {
        use rusqlite::OptionalExtension;
        let raw: Option<String> = self
            .0
            .lock()
            .unwrap()
            .query_row(
                "DELETE FROM records WHERE kind=?1 AND id=?2 RETURNING value",
                params![kind, id],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|v| serde_json::from_str(&v).map_err(Into::into))
            .transpose()
    }
    pub fn put_record(
        &self,
        kind: &str,
        id: &str,
        owner: &str,
        value: &serde_json::Value,
    ) -> anyhow::Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT OR REPLACE INTO records VALUES(?1,?2,?3,?4)",
            params![kind, id, owner, serde_json::to_string(value)?],
        )?;
        Ok(())
    }
    pub fn records(&self, kind: &str, owner: &str) -> anyhow::Result<Vec<serde_json::Value>> {
        let c = self.0.lock().unwrap();
        let mut q =
            c.prepare("SELECT value FROM records WHERE kind=?1 AND owner=?2 ORDER BY rowid DESC")?;
        let raw = q
            .query_map(params![kind, owner], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raw.into_iter()
            .map(|v| serde_json::from_str(&v).map_err(Into::into))
            .collect()
    }
    pub fn notify_change(
        &self,
        key: &str,
        signal: &str,
        title: &str,
        body: &str,
    ) -> anyhow::Result<()> {
        use rusqlite::OptionalExtension;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let previous: Option<String> = tx
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        if previous.as_deref() != Some(signal) {
            tx.execute(
                "INSERT OR REPLACE INTO settings VALUES(?1,?2)",
                params![key, signal],
            )?;
            if !signal.is_empty() {
                let id = Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO notifications VALUES(?1,?2,?3,?4,0)",
                    params![id, now(), title, body],
                )?;
                let ntfy: Option<String> = tx
                    .query_row("SELECT value FROM settings WHERE key='ntfy'", [], |r| {
                        r.get(0)
                    })
                    .optional()?;
                if let Some(p) = ntfy
                    .and_then(|v| serde_json::from_str::<serde_json::Value>(&v).ok())
                    .filter(|p| p["enabled"] == true)
                {
                    let delivery = serde_json::json!({"id":id,"title":title,"state":"pending","attempts":0,"next_attempt":0});
                    tx.execute(
                        "INSERT INTO records(kind,id,owner,value) VALUES('ntfy-delivery',?1,?2,?3)",
                        params![
                            id,
                            p["generation"].as_str().unwrap_or_default(),
                            delivery.to_string()
                        ],
                    )?;
                }
            }
        }
        tx.execute("DELETE FROM notifications WHERE id NOT IN (SELECT id FROM notifications ORDER BY created DESC,rowid DESC LIMIT 200)",[])?;
        tx.execute("DELETE FROM records WHERE kind='ntfy-delivery' AND id NOT IN (SELECT id FROM notifications)",[])?;
        tx.commit()?;
        Ok(())
    }
    pub fn notifications(&self) -> anyhow::Result<Vec<serde_json::Value>> {
        let c = self.0.lock().unwrap();
        let mut q=c.prepare("SELECT id,created,title,body,read FROM notifications ORDER BY created DESC,rowid DESC LIMIT 200")?;
        Ok(q.query_map([],|r|Ok(serde_json::json!({"id":r.get::<_,String>(0)?,"created":r.get::<_,i64>(1)?,"title":r.get::<_,String>(2)?,"body":r.get::<_,String>(3)?,"read":r.get::<_,bool>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn read_notifications(&self) -> anyhow::Result<()> {
        self.0
            .lock()
            .unwrap()
            .execute("UPDATE notifications SET read=1", [])?;
        Ok(())
    }
    pub fn trim_jobs(&self, limit: usize) -> anyhow::Result<()> {
        self.0.lock().unwrap().execute("DELETE FROM jobs WHERE finished IS NOT NULL AND id NOT IN (SELECT id FROM jobs WHERE finished IS NOT NULL ORDER BY created DESC,rowid DESC LIMIT ?1)",[limit as i64])?;
        Ok(())
    }
}
