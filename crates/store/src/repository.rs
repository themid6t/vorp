use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use vorp_protocol::valid_subdomain;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("database operation failed: {0}")]
    Database(String),
    #[error("record not found")]
    NotFound,
    #[error("record already exists")]
    Conflict,
    #[error("invalid record: {0}")]
    Invalid(&'static str),
    #[error("secure random generation failed")]
    Random,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindPolicy {
    Any,
    Temporary,
    Reserved,
}
impl BindPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Temporary => "temporary",
            Self::Reserved => "reserved",
        }
    }
    fn parse(s: &str) -> Result<Self, RepositoryError> {
        match s {
            "any" => Ok(Self::Any),
            "temporary" => Ok(Self::Temporary),
            "reserved" => Ok(Self::Reserved),
            _ => Err(RepositoryError::Invalid("stored bind policy")),
        }
    }
}
#[derive(Clone, Debug)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub password_hash: String,
    pub is_admin: bool,
    pub assigned_subdomain: Option<String>,
    pub created_at_ms: i64,
}
#[derive(Clone)]
pub struct NewUser {
    pub email: String,
    pub password_hash: String,
    pub is_admin: bool,
}
#[derive(Clone)]
pub struct Session {
    pub id: String,
    pub user_id: i64,
    pub expires_at_ms: i64,
}
#[derive(Clone)]
pub struct NewSession {
    pub id: String,
    pub user_id: i64,
    pub expires_at_ms: i64,
}
#[derive(Clone)]
pub struct TokenRecord {
    pub id: i64,
    pub user_id: i64,
    pub token_hash: [u8; 32],
    pub bind_policy: BindPolicy,
    pub allowlist: Vec<String>,
    pub revoked_at_ms: Option<i64>,
}
#[derive(Clone)]
pub struct NewAgentToken {
    pub user_id: i64,
    pub token_hash: [u8; 32],
    pub bind_policy: BindPolicy,
    pub allowlist: Vec<String>,
}
#[derive(Debug, Clone)]
pub struct ReservedSubdomain {
    pub name: String,
    pub user_id: i64,
}

/// A single SQLite connection is serialized behind a mutex; each operation runs on Tokio's
/// blocking pool, so a busy transaction never holds an async executor worker.
#[derive(Clone)]
pub struct Repository {
    db: Arc<Mutex<Connection>>,
}
const SCHEMA:&str = "
CREATE TABLE IF NOT EXISTS users(id INTEGER PRIMARY KEY,email TEXT NOT NULL UNIQUE COLLATE NOCASE,password_hash TEXT NOT NULL,is_admin INTEGER NOT NULL CHECK(is_admin IN(0,1)),assigned_subdomain TEXT UNIQUE,created_at_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,expires_at_ms INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS sessions_user ON sessions(user_id); CREATE INDEX IF NOT EXISTS sessions_expiry ON sessions(expires_at_ms);
CREATE TABLE IF NOT EXISTS agent_tokens(id INTEGER PRIMARY KEY,user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,token_hash BLOB NOT NULL UNIQUE CHECK(length(token_hash)=32),bind_policy TEXT NOT NULL CHECK(bind_policy IN('any','temporary','reserved')),created_at_ms INTEGER NOT NULL,revoked_at_ms INTEGER);
CREATE INDEX IF NOT EXISTS tokens_user ON agent_tokens(user_id);
CREATE TABLE IF NOT EXISTS token_allowlist(token_id INTEGER NOT NULL REFERENCES agent_tokens(id) ON DELETE CASCADE,name TEXT NOT NULL,PRIMARY KEY(token_id,name));
CREATE TABLE IF NOT EXISTS reserved_subdomains(name TEXT PRIMARY KEY,user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,created_at_ms INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS reservations_user ON reserved_subdomains(user_id);
CREATE TABLE IF NOT EXISTS signup_invites(id INTEGER PRIMARY KEY,created_by INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,code_hash BLOB NOT NULL UNIQUE CHECK(length(code_hash)=32),created_at_ms INTEGER NOT NULL,used_at_ms INTEGER);
CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);";

fn map_db(e: rusqlite::Error) -> RepositoryError {
    match &e {
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            RepositoryError::Conflict
        }
        _ => RepositoryError::Database(e.to_string()),
    }
}
fn now_ms() -> Result<i64, RepositoryError> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RepositoryError::Invalid("clock"))?
            .as_millis(),
    )
    .map_err(|_| RepositoryError::Invalid("timestamp"))
}
fn user_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get(0)?,
        email: r.get(1)?,
        password_hash: r.get(2)?,
        is_admin: r.get(3)?,
        assigned_subdomain: r.get(4)?,
        created_at_ms: r.get(5)?,
    })
}
fn token_row(
    conn: &Connection,
    row: (i64, i64, Vec<u8>, String, Option<i64>),
) -> Result<TokenRecord, RepositoryError> {
    let hash = row
        .2
        .try_into()
        .map_err(|_| RepositoryError::Invalid("stored token hash"))?;
    let mut s = conn
        .prepare("SELECT name FROM token_allowlist WHERE token_id=? ORDER BY name")
        .map_err(map_db)?;
    let allowlist = s
        .query_map([row.0], |r| r.get::<_, String>(0))
        .map_err(map_db)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db)?;
    Ok(TokenRecord {
        id: row.0,
        user_id: row.1,
        token_hash: hash,
        bind_policy: BindPolicy::parse(&row.3)?,
        allowlist,
        revoked_at_ms: row.4,
    })
}
impl Repository {
    pub async fn open(path: &Path) -> Result<Self, RepositoryError> {
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            let c = Connection::open(path).map_err(map_db)?;
            c.busy_timeout(Duration::from_secs(5)).map_err(map_db)?;
            c.pragma_update(None, "foreign_keys", "ON")
                .map_err(map_db)?;
            c.pragma_update(None, "journal_mode", "WAL")
                .map_err(map_db)?;
            c.execute_batch(SCHEMA).map_err(map_db)?;
            Ok(Self {
                db: Arc::new(Mutex::new(c)),
            })
        })
        .await
        .map_err(|e| RepositoryError::Database(format!("database task: {e}")))?
    }
    async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, RepositoryError> + Send + 'static,
    ) -> Result<T, RepositoryError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = db
                .lock()
                .map_err(|_| RepositoryError::Database("database mutex poisoned".into()))?;
            f(&mut c)
        })
        .await
        .map_err(|e| RepositoryError::Database(format!("database task: {e}")))?
    }
    pub async fn create_user(&self, u: NewUser) -> Result<User, RepositoryError> {
        self.run(move |c| {
            let email = u.email.trim().to_ascii_lowercase();
            if email.is_empty() || !email.contains('@') {
                return Err(RepositoryError::Invalid("email"));
            }
            let now = now_ms()?;
            c.execute(
                "INSERT INTO users(email,password_hash,is_admin,created_at_ms) VALUES(?,?,?,?)",
                params![email, u.password_hash, u.is_admin, now],
            )
            .map_err(map_db)?;
            Ok(User {
                id: c.last_insert_rowid(),
                email,
                password_hash: u.password_hash,
                is_admin: u.is_admin,
                assigned_subdomain: None,
                created_at_ms: now,
            })
        })
        .await
    }
    pub async fn mint_invite(&self, admin_user_id: i64) -> Result<String, RepositoryError> {
        self.run(move |conn| {
            let tx = conn.transaction().map_err(map_db)?;
            let admin: Option<bool> = tx
                .query_row(
                    "SELECT is_admin FROM users WHERE id=?",
                    [admin_user_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(map_db)?;
            if admin != Some(true) {
                return Err(RepositoryError::NotFound);
            }
            let mut secret = [0u8; 32];
            getrandom::fill(&mut secret).map_err(|_| RepositoryError::Random)?;
            let placeholder = Sha256::digest(secret);
            tx.execute(
                "INSERT INTO signup_invites(created_by,code_hash,created_at_ms) VALUES(?,?,?)",
                params![admin_user_id, placeholder.as_slice(), now_ms()?],
            )
            .map_err(map_db)?;
            let id = tx.last_insert_rowid();
            let raw = format!(
                "vorp-invite_{id}_{}",
                secret
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            );
            let digest = Sha256::digest(raw.as_bytes());
            tx.execute(
                "UPDATE signup_invites SET code_hash=? WHERE id=?",
                params![digest.as_slice(), id],
            )
            .map_err(map_db)?;
            tx.commit().map_err(map_db)?;
            Ok(raw)
        })
        .await
    }
    pub async fn create_user_with_invite(
        &self,
        u: NewUser,
        raw: &str,
    ) -> Result<User, RepositoryError> {
        let raw = raw.to_owned();
        self.run(move |conn| {
            let id = raw
                .strip_prefix("vorp-invite_")
                .and_then(|s| s.split_once('_'))
                .and_then(|(id, _)| id.parse::<i64>().ok())
                .ok_or(RepositoryError::Invalid("invite code"))?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_db)?;
            let stored: Option<(Vec<u8>, Option<i64>)> = tx
                .query_row(
                    "SELECT code_hash,used_at_ms FROM signup_invites WHERE id=?",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(map_db)?;
            let (hash, used) = stored.ok_or(RepositoryError::Invalid("invite code"))?;
            let hash: [u8; 32] = hash
                .try_into()
                .map_err(|_| RepositoryError::Invalid("stored invite hash"))?;
            if used.is_some() || !bool::from(Sha256::digest(raw.as_bytes()).as_slice().ct_eq(&hash))
            {
                return Err(RepositoryError::Invalid("invite code"));
            }
            let email = u.email.trim().to_ascii_lowercase();
            if email.is_empty() || !email.contains('@') {
                return Err(RepositoryError::Invalid("email"));
            }
            let now = now_ms()?;
            tx.execute(
                "INSERT INTO users(email,password_hash,is_admin,created_at_ms) VALUES(?,?,0,?)",
                params![email, u.password_hash, now],
            )
            .map_err(map_db)?;
            let user_id = tx.last_insert_rowid();
            tx.execute(
                "UPDATE signup_invites SET used_at_ms=? WHERE id=? AND used_at_ms IS NULL",
                params![now, id],
            )
            .map_err(map_db)?;
            tx.commit().map_err(map_db)?;
            Ok(User {
                id: user_id,
                email,
                password_hash: u.password_hash,
                is_admin: false,
                assigned_subdomain: None,
                created_at_ms: now,
            })
        })
        .await
    }
    pub async fn user_by_email(&self, email: &str) -> Result<Option<User>, RepositoryError> {
        let e = email.to_owned();
        self.run(move|c|c.query_row("SELECT id,email,password_hash,is_admin,assigned_subdomain,created_at_ms FROM users WHERE email=?",[e],user_row).optional().map_err(map_db)).await
    }
    pub async fn user_by_id(&self, id: i64) -> Result<Option<User>, RepositoryError> {
        self.run(move|c|c.query_row("SELECT id,email,password_hash,is_admin,assigned_subdomain,created_at_ms FROM users WHERE id=?",[id],user_row).optional().map_err(map_db)).await
    }
    pub async fn user_count(&self) -> Result<i64, RepositoryError> {
        self.run(|c| {
            c.query_row("SELECT count(*) FROM users", [], |r| r.get(0))
                .map_err(map_db)
        })
        .await
    }
    pub async fn bootstrap_admin(
        &self,
        email: &str,
        password_hash: &str,
    ) -> Result<User, RepositoryError> {
        let (email, hash) = (email.to_owned(), password_hash.to_owned());
        self.run(move |c| {
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_db)?;
            let count: i64 = tx
                .query_row("SELECT count(*) FROM users", [], |r| r.get(0))
                .map_err(map_db)?;
            if count != 0 {
                return Err(RepositoryError::Conflict);
            }
            let email = email.trim().to_ascii_lowercase();
            if email.is_empty() || !email.contains('@') {
                return Err(RepositoryError::Invalid("email"));
            }
            let now = now_ms()?;
            tx.execute(
                "INSERT INTO users(email,password_hash,is_admin,created_at_ms) VALUES(?,?,1,?)",
                params![email, hash, now],
            )
            .map_err(map_db)?;
            let id = tx.last_insert_rowid();
            tx.commit().map_err(map_db)?;
            Ok(User {
                id,
                email,
                password_hash: hash,
                is_admin: true,
                assigned_subdomain: None,
                created_at_ms: now,
            })
        })
        .await
    }
    pub async fn get_or_create_assigned_subdomain(
        &self,
        user_id: i64,
    ) -> Result<String, RepositoryError> {
        self.run(move|c| { let tx=c.transaction_with_behavior(TransactionBehavior::Immediate).map_err(map_db)?; let existing:Option<String>=tx.query_row("SELECT assigned_subdomain FROM users WHERE id=?",[user_id],|r|r.get(0)).optional().map_err(map_db)?.ok_or(RepositoryError::NotFound)?; if let Some(n)=existing {return Ok(n);} for _ in 0..32 { let mut bytes=[0u8;10]; getrandom::fill(&mut bytes).map_err(|_|RepositoryError::Random)?; let name=bytes.iter().map(|b|format!("{b:02x}")).collect::<String>(); let reserved:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM reserved_subdomains WHERE name=?)",[&name],|r|r.get(0)).map_err(map_db)?; if reserved {continue;} match tx.execute("UPDATE users SET assigned_subdomain=? WHERE id=?",params![name,user_id]) { Ok(_)=>{tx.commit().map_err(map_db)?;return Ok(name)},Err(e) if matches!(&e,rusqlite::Error::SqliteFailure(code,_) if code.code==rusqlite::ErrorCode::ConstraintViolation)=>continue,Err(e)=>return Err(map_db(e)) } } Err(RepositoryError::Conflict) }).await
    }
    pub async fn assigned_subdomain_owner(
        &self,
        name: &str,
    ) -> Result<Option<i64>, RepositoryError> {
        let n = name.to_owned();
        self.run(move |c| {
            c.query_row(
                "SELECT id FROM users WHERE assigned_subdomain=?",
                [n],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_db)
        })
        .await
    }
    pub async fn create_session(&self, s: NewSession) -> Result<(), RepositoryError> {
        self.run(move |c| {
            c.execute(
                "INSERT INTO sessions(id,user_id,expires_at_ms) VALUES(?,?,?)",
                params![s.id, s.user_id, s.expires_at_ms],
            )
            .map_err(map_db)?;
            Ok(())
        })
        .await
    }
    pub async fn session_by_id(&self, id: &str) -> Result<Option<Session>, RepositoryError> {
        let id = id.to_owned();
        self.run(move |c| {
            c.query_row(
                "SELECT id,user_id,expires_at_ms FROM sessions WHERE id=? AND expires_at_ms>?",
                params![id, now_ms()?],
                |r| {
                    Ok(Session {
                        id: r.get(0)?,
                        user_id: r.get(1)?,
                        expires_at_ms: r.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(map_db)
        })
        .await
    }
    pub async fn delete_session(&self, id: &str) -> Result<(), RepositoryError> {
        let id = id.to_owned();
        self.run(move |c| {
            c.execute("DELETE FROM sessions WHERE id=?", [id])
                .map_err(map_db)?;
            Ok(())
        })
        .await
    }
    pub async fn delete_user_sessions(&self, user_id: i64) -> Result<(), RepositoryError> {
        self.run(move |c| {
            c.execute("DELETE FROM sessions WHERE user_id=?", [user_id])
                .map_err(map_db)?;
            Ok(())
        })
        .await
    }
    pub async fn update_password(&self, user_id: i64, hash: &str) -> Result<(), RepositoryError> {
        let hash = hash.to_owned();
        self.run(move |c| {
            let tx = c.transaction().map_err(map_db)?;
            if tx
                .execute(
                    "UPDATE users SET password_hash=? WHERE id=?",
                    params![hash, user_id],
                )
                .map_err(map_db)?
                == 0
            {
                return Err(RepositoryError::NotFound);
            }
            tx.execute("DELETE FROM sessions WHERE user_id=?", [user_id])
                .map_err(map_db)?;
            tx.commit().map_err(map_db)
        })
        .await
    }
    pub async fn create_token(&self, t: NewAgentToken) -> Result<TokenRecord, RepositoryError> {
        self.run(move |c| {
            if t.bind_policy != BindPolicy::Reserved && !t.allowlist.is_empty() {
                return Err(RepositoryError::Invalid("allowlist requires reserved policy"));
            }
            if t.allowlist.iter().any(|n| !valid_subdomain(n)) {
                return Err(RepositoryError::Invalid("allowlist name"));
            }
            let tx = c.transaction().map_err(map_db)?;
            tx.execute("INSERT INTO agent_tokens(user_id,token_hash,bind_policy,created_at_ms) VALUES(?,?,?,?)", params![t.user_id,t.token_hash.as_slice(),t.bind_policy.as_str(),now_ms()?]).map_err(map_db)?;
            let id = tx.last_insert_rowid();
            for name in &t.allowlist {
                let owner: Option<i64> = tx.query_row("SELECT user_id FROM reserved_subdomains WHERE name=?", [name], |r| r.get(0)).optional().map_err(map_db)?;
                if owner != Some(t.user_id) {
                    return Err(RepositoryError::Invalid("allowlist must contain owned reservations"));
                }
                tx.execute("INSERT INTO token_allowlist(token_id,name) VALUES(?,?)", params![id,name]).map_err(map_db)?;
            }
            tx.commit().map_err(map_db)?;
            Ok(TokenRecord { id, user_id:t.user_id, token_hash:t.token_hash, bind_policy:t.bind_policy, allowlist:t.allowlist, revoked_at_ms:None })
        }).await
    }
    pub async fn mint_token(
        &self,
        user_id: i64,
        policy: BindPolicy,
        allowlist: Vec<String>,
    ) -> Result<(TokenRecord, String), RepositoryError> {
        self.run(move|c|{
        if policy!=BindPolicy::Reserved&&!allowlist.is_empty(){return Err(RepositoryError::Invalid("allowlist requires reserved policy"))}
        if allowlist.iter().any(|n|!valid_subdomain(n)){return Err(RepositoryError::Invalid("allowlist name"))}
        let tx=c.transaction().map_err(map_db)?;
        for name in &allowlist {let owner:Option<i64>=tx.query_row("SELECT user_id FROM reserved_subdomains WHERE name=?",[name],|r|r.get(0)).optional().map_err(map_db)?;if owner!=Some(user_id){return Err(RepositoryError::Invalid("allowlist must contain owned reservations"))}}
        let mut secret=[0u8;32];getrandom::fill(&mut secret).map_err(|_|RepositoryError::Random)?;
        let placeholder=Sha256::digest(secret);
        tx.execute("INSERT INTO agent_tokens(user_id,token_hash,bind_policy,created_at_ms) VALUES(?,?,?,?)",params![user_id,placeholder.as_slice(),policy.as_str(),now_ms()?]).map_err(map_db)?;
        let id=tx.last_insert_rowid();let raw=format!("vorp_{id}_{}",secret.iter().map(|b|format!("{b:02x}")).collect::<String>());
        let token_hash:[u8;32]=Sha256::digest(raw.as_bytes()).into();
        tx.execute("UPDATE agent_tokens SET token_hash=? WHERE id=?",params![token_hash.as_slice(),id]).map_err(map_db)?;
        for name in &allowlist {tx.execute("INSERT INTO token_allowlist(token_id,name) VALUES(?,?)",params![id,name]).map_err(map_db)?;}
        tx.commit().map_err(map_db)?;
        Ok((TokenRecord{id,user_id,token_hash,bind_policy:policy,allowlist,revoked_at_ms:None},raw))
    }).await
    }
    pub async fn token_by_id(&self, id: i64) -> Result<Option<TokenRecord>, RepositoryError> {
        self.run(move|c|{let row=c.query_row("SELECT id,user_id,token_hash,bind_policy,revoked_at_ms FROM agent_tokens WHERE id=?",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(map_db)?;row.map(|r|token_row(c,r)).transpose()}).await
    }
    pub async fn token_for_user(
        &self,
        id: i64,
        user_id: i64,
    ) -> Result<Option<TokenRecord>, RepositoryError> {
        self.run(move|c|{let row=c.query_row("SELECT id,user_id,token_hash,bind_policy,revoked_at_ms FROM agent_tokens WHERE id=? AND user_id=?",params![id,user_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(map_db)?;row.map(|r|token_row(c,r)).transpose()}).await
    }
    pub async fn authenticate_token(
        &self,
        raw: &str,
    ) -> Result<Option<TokenRecord>, RepositoryError> {
        let Some(id) = raw
            .strip_prefix("vorp_")
            .and_then(|s| s.split_once('_'))
            .and_then(|(id, _)| id.parse::<i64>().ok())
        else {
            return Ok(None);
        };
        let Some(token) = self.token_by_id(id).await? else {
            return Ok(None);
        };
        let digest = Sha256::digest(raw.as_bytes());
        if token.revoked_at_ms.is_some() || !bool::from(digest.as_slice().ct_eq(&token.token_hash))
        {
            return Ok(None);
        }
        Ok(Some(token))
    }
    pub async fn tokens_for_user(&self, user_id: i64) -> Result<Vec<TokenRecord>, RepositoryError> {
        self.run(move|c|{let rows={let mut s=c.prepare("SELECT id,user_id,token_hash,bind_policy,revoked_at_ms FROM agent_tokens WHERE user_id=? ORDER BY id DESC").map_err(map_db)?;s.query_map([user_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(map_db)?.collect::<Result<Vec<_>,_>>().map_err(map_db)?};rows.into_iter().map(|r|token_row(c,r)).collect()}).await
    }
    pub async fn revoke_token(&self, id: i64, owner_user_id: i64) -> Result<bool, RepositoryError> {
        self.run(move|c|Ok(c.execute("UPDATE agent_tokens SET revoked_at_ms=? WHERE id=? AND user_id=? AND revoked_at_ms IS NULL",params![now_ms()?,id,owner_user_id]).map_err(map_db)?>0)).await
    }
    pub async fn reservation_owner(&self, name: &str) -> Result<Option<i64>, RepositoryError> {
        let n = name.to_owned();
        self.run(move |c| {
            c.query_row(
                "SELECT user_id FROM reserved_subdomains WHERE name=?",
                [n],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_db)
        })
        .await
    }
    pub async fn reserve_subdomain(
        &self,
        name: &str,
        owner_user_id: i64,
    ) -> Result<ReservedSubdomain, RepositoryError> {
        let name = name.to_owned();
        self.run(move |c| {
            if !valid_subdomain(&name) {
                return Err(RepositoryError::Invalid("subdomain name"));
            }
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_db)?;
            let assigned: Option<i64> = tx
                .query_row(
                    "SELECT id FROM users WHERE assigned_subdomain=?",
                    [&name],
                    |r| r.get(0),
                )
                .optional()
                .map_err(map_db)?;
            if assigned.is_some_and(|id| id != owner_user_id) {
                return Err(RepositoryError::Conflict);
            }
            tx.execute(
                "INSERT INTO reserved_subdomains(name,user_id,created_at_ms) VALUES(?,?,?)",
                params![name, owner_user_id, now_ms()?],
            )
            .map_err(map_db)?;
            tx.commit().map_err(map_db)?;
            Ok(ReservedSubdomain {
                name,
                user_id: owner_user_id,
            })
        })
        .await
    }
    pub async fn reservations_for_user(
        &self,
        user_id: i64,
    ) -> Result<Vec<ReservedSubdomain>, RepositoryError> {
        self.run(move |c| {
            let mut s = c
                .prepare(
                    "SELECT name,user_id FROM reserved_subdomains WHERE user_id=? ORDER BY name",
                )
                .map_err(map_db)?;
            s.query_map([user_id], |r| {
                Ok(ReservedSubdomain {
                    name: r.get(0)?,
                    user_id: r.get(1)?,
                })
            })
            .map_err(map_db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_db)
        })
        .await
    }
    pub async fn release_subdomain(
        &self,
        name: &str,
        owner_user_id: i64,
    ) -> Result<bool, RepositoryError> {
        let n = name.to_owned();
        self.run(move |c| {
            Ok(c.execute(
                "DELETE FROM reserved_subdomains WHERE name=? AND user_id=?",
                params![n, owner_user_id],
            )
            .map_err(map_db)?
                > 0)
        })
        .await
    }
    pub async fn setting(&self, key: &str) -> Result<Option<String>, RepositoryError> {
        let k = key.to_owned();
        self.run(move |c| {
            c.query_row("SELECT value FROM settings WHERE key=?", [k], |r| r.get(0))
                .optional()
                .map_err(map_db)
        })
        .await
    }
    pub async fn set_setting(&self, key: &str, value: &str) -> Result<(), RepositoryError> {
        let (k, v) = (key.to_owned(), value.to_owned());
        self.run(move|c|{c.execute("INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![k,v]).map_err(map_db)?;Ok(())}).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn repo() -> Repository {
        Repository::open(Path::new(":memory:")).await.expect("db")
    }
    async fn user(r: &Repository, email: &str) -> User {
        r.create_user(NewUser {
            email: email.into(),
            password_hash: "hash".into(),
            is_admin: false,
        })
        .await
        .expect("user")
    }
    #[tokio::test]
    async fn token_scope_and_revocation() {
        let r = repo().await;
        let a = user(&r, "a@x.test").await;
        let b = user(&r, "b@x.test").await;
        let t = r
            .create_token(NewAgentToken {
                user_id: a.id,
                token_hash: [7; 32],
                bind_policy: BindPolicy::Any,
                allowlist: vec![],
            })
            .await
            .expect("token");
        assert!(r.tokens_for_user(b.id).await.expect("list").is_empty());
        assert!(!r.revoke_token(t.id, b.id).await.expect("wrong owner"));
        assert!(r.revoke_token(t.id, a.id).await.expect("owner"));
        assert!(
            r.token_by_id(t.id)
                .await
                .expect("lookup")
                .expect("token")
                .revoked_at_ms
                .is_some()
        );
    }
    #[tokio::test]
    async fn reservation_and_sessions() {
        let r = repo().await;
        let a = user(&r, "a@x.test").await;
        let b = user(&r, "b@x.test").await;
        let n = r
            .get_or_create_assigned_subdomain(a.id)
            .await
            .expect("assigned");
        assert_eq!(
            r.get_or_create_assigned_subdomain(a.id)
                .await
                .expect("stable"),
            n
        );
        assert!(matches!(
            r.reserve_subdomain(&n, b.id).await,
            Err(RepositoryError::Conflict)
        ));
        r.create_session(NewSession {
            id: "s".into(),
            user_id: a.id,
            expires_at_ms: i64::MAX,
        })
        .await
        .expect("session");
        r.update_password(a.id, "new").await.expect("password");
        assert!(r.session_by_id("s").await.expect("lookup").is_none());
    }
    #[tokio::test]
    async fn minted_token_authenticates_only_until_revocation() {
        let r = repo().await;
        let a = user(&r, "a@x.test").await;
        let (t, raw) = r
            .mint_token(a.id, BindPolicy::Temporary, vec![])
            .await
            .expect("mint");
        assert_eq!(
            r.authenticate_token(&raw)
                .await
                .expect("auth")
                .map(|t| t.id),
            Some(t.id)
        );
        let replacement = if raw.ends_with('0') { '1' } else { '0' };
        let altered = format!("{}{replacement}", &raw[..raw.len() - 1]);
        assert!(
            r.authenticate_token(&altered)
                .await
                .expect("altered")
                .is_none()
        );
        r.revoke_token(t.id, a.id).await.expect("revoke");
        assert!(r.authenticate_token(&raw).await.expect("revoked").is_none());
    }
    #[tokio::test]
    async fn invite_is_admin_only_and_single_use() {
        let r = repo().await;
        let regular = user(&r, "regular@x.test").await;
        assert!(matches!(
            r.mint_invite(regular.id).await,
            Err(RepositoryError::NotFound)
        ));
        let admin = r.bootstrap_admin("admin@x.test", "hash").await;
        assert!(matches!(admin, Err(RepositoryError::Conflict)));
        // The first account can be elevated only through bootstrap, so use a fresh repository.
        let r = repo().await;
        let admin = r
            .bootstrap_admin("admin@x.test", "hash")
            .await
            .expect("admin");
        let invite = r.mint_invite(admin.id).await.expect("invite");
        let user = NewUser {
            email: "invited@x.test".into(),
            password_hash: "hash".into(),
            is_admin: false,
        };
        assert!(matches!(
            r.create_user_with_invite(user.clone(), "wrong").await,
            Err(RepositoryError::Invalid(_))
        ));
        r.create_user_with_invite(user.clone(), &invite)
            .await
            .expect("redeem");
        assert!(matches!(
            r.create_user_with_invite(user, &invite).await,
            Err(RepositoryError::Invalid(_))
        ));
    }
}
