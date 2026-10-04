use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use vorp_protocol::valid_subdomain;

use crate::{
    LimitOverrides, UserLimitEntry, UserLimits,
    names::{NameAction, NameConflict, NameHolders, name_conflict},
};

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
    #[error("subdomain name unavailable: {}", .0.message())]
    NameTaken(NameConflict),
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
    /// Set for accounts an admin created or a host reset; cleared when the
    /// user picks their own password.
    pub must_change_password: bool,
    /// Reservations take effect at once instead of waiting for an admin.
    pub can_reserve_directly: bool,
}
#[derive(Clone)]
pub struct NewUser {
    pub email: String,
    pub password_hash: String,
    pub is_admin: bool,
    pub must_change_password: bool,
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
#[derive(Debug, Clone)]
pub struct ReservationRequest {
    pub name: String,
    pub user_id: i64,
    pub email: String,
    pub created_at_ms: i64,
}

/// A single SQLite connection is serialized behind a mutex; each operation runs on Tokio's
/// blocking pool, so a busy transaction never holds an async executor worker.
#[derive(Clone)]
pub struct Repository {
    db: Arc<Mutex<Connection>>,
}
const SCHEMA:&str = "
CREATE TABLE IF NOT EXISTS users(id INTEGER PRIMARY KEY,email TEXT NOT NULL UNIQUE COLLATE NOCASE,password_hash TEXT NOT NULL,is_admin INTEGER NOT NULL CHECK(is_admin IN(0,1)),assigned_subdomain TEXT UNIQUE,created_at_ms INTEGER NOT NULL,must_change_password INTEGER NOT NULL DEFAULT 0 CHECK(must_change_password IN(0,1)),can_reserve_directly INTEGER NOT NULL DEFAULT 0 CHECK(can_reserve_directly IN(0,1)));
CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,expires_at_ms INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS sessions_user ON sessions(user_id); CREATE INDEX IF NOT EXISTS sessions_expiry ON sessions(expires_at_ms);
CREATE TABLE IF NOT EXISTS agent_tokens(id INTEGER PRIMARY KEY,user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,token_hash BLOB NOT NULL UNIQUE CHECK(length(token_hash)=32),bind_policy TEXT NOT NULL CHECK(bind_policy IN('any','temporary','reserved')),created_at_ms INTEGER NOT NULL,revoked_at_ms INTEGER);
CREATE INDEX IF NOT EXISTS tokens_user ON agent_tokens(user_id);
CREATE TABLE IF NOT EXISTS token_allowlist(token_id INTEGER NOT NULL REFERENCES agent_tokens(id) ON DELETE CASCADE,name TEXT NOT NULL,PRIMARY KEY(token_id,name));
CREATE TABLE IF NOT EXISTS reserved_subdomains(name TEXT PRIMARY KEY,user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,created_at_ms INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS reservations_user ON reserved_subdomains(user_id);
CREATE TABLE IF NOT EXISTS reservation_requests(name TEXT PRIMARY KEY,user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,created_at_ms INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS reservation_requests_user ON reservation_requests(user_id);
DROP TABLE IF EXISTS signup_invites;
CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS user_limits(user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,max_tunnels INTEGER CHECK(max_tunnels>0),bandwidth_bytes_per_sec INTEGER CHECK(bandwidth_bytes_per_sec>0),max_concurrent_requests INTEGER CHECK(max_concurrent_requests>0));";

/// `users` columns added after the first release, with their definitions.
/// `CREATE TABLE IF NOT EXISTS` leaves an older table as it was, so `open`
/// adds each missing column in place.
const ADDED_USER_COLUMNS: [(&str, &str); 2] = [
    (
        "must_change_password",
        "INTEGER NOT NULL DEFAULT 0 CHECK(must_change_password IN(0,1))",
    ),
    (
        "can_reserve_directly",
        "INTEGER NOT NULL DEFAULT 0 CHECK(can_reserve_directly IN(0,1))",
    ),
];
/// Every `users` column, in the order `user_row` reads them.
const USER_COLUMNS: &str = "id,email,password_hash,is_admin,assigned_subdomain,created_at_ms,must_change_password,can_reserve_directly";

const LIMIT_KEYS: [&str; 3] = [
    "limits.max_tunnels",
    "limits.bandwidth_bytes_per_sec",
    "limits.max_concurrent_requests",
];

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
/// Sessions are stored by SHA-256 of the cookie value, so a copy of the
/// database cannot be replayed as live logins. The id is 256 random bits, so
/// an unsalted fast hash suffices, as for agent tokens.
fn session_key(id: &str) -> String {
    Sha256::digest(id.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
        must_change_password: r.get(6)?,
        can_reserve_directly: r.get(7)?,
    })
}
fn add_missing_user_columns(c: &Connection) -> Result<(), RepositoryError> {
    let existing = {
        let mut s = c
            .prepare("SELECT name FROM pragma_table_info('users')")
            .map_err(map_db)?;
        s.query_map([], |r| r.get::<_, String>(0))
            .map_err(map_db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_db)?
    };
    for (name, definition) in ADDED_USER_COLUMNS {
        if !existing.iter().any(|column| column == name) {
            c.execute_batch(&format!("ALTER TABLE users ADD COLUMN {name} {definition}"))
                .map_err(map_db)?;
        }
    }
    Ok(())
}
fn name_holders_in(c: &Connection, name: &str) -> Result<NameHolders, RepositoryError> {
    let owner = |sql: &str| -> Result<Option<i64>, RepositoryError> {
        c.query_row(sql, [name], |r| r.get(0))
            .optional()
            .map_err(map_db)
    };
    Ok(NameHolders {
        reserved_by: owner("SELECT user_id FROM reserved_subdomains WHERE name=?")?,
        requested_by: owner("SELECT user_id FROM reservation_requests WHERE name=?")?,
        assigned_to: owner("SELECT id FROM users WHERE assigned_subdomain=?")?,
    })
}
/// Stores a reservation and drops the owner's pending request for the name.
fn insert_reservation(c: &Connection, name: &str, user_id: i64) -> Result<(), RepositoryError> {
    c.execute(
        "DELETE FROM reservation_requests WHERE name=? AND user_id=?",
        params![name, user_id],
    )
    .map_err(map_db)?;
    c.execute(
        "INSERT INTO reserved_subdomains(name,user_id,created_at_ms) VALUES(?,?,?)",
        params![name, user_id, now_ms()?],
    )
    .map_err(map_db)?;
    Ok(())
}
fn check_name(
    c: &Connection,
    name: &str,
    requester: i64,
    action: NameAction,
) -> Result<(), RepositoryError> {
    if !valid_subdomain(name) {
        return Err(RepositoryError::Invalid("subdomain name"));
    }
    match name_conflict(name_holders_in(c, name)?, requester, action) {
        Some(conflict) => Err(RepositoryError::NameTaken(conflict)),
        None => Ok(()),
    }
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
fn default_limits_in(c: &Connection) -> Result<UserLimits, RepositoryError> {
    let mut limits = UserLimits::DEFAULT;
    let mut s = c
        .prepare("SELECT key,value FROM settings WHERE key IN (?,?,?)")
        .map_err(map_db)?;
    let rows = s
        .query_map(LIMIT_KEYS, |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(map_db)?;
    for row in rows {
        let (key, value) = row.map_err(map_db)?;
        let invalid = || RepositoryError::Invalid("stored default limit");
        match key.as_str() {
            "limits.max_tunnels" => limits.max_tunnels = value.parse().map_err(|_| invalid())?,
            "limits.bandwidth_bytes_per_sec" => {
                limits.bandwidth_bytes_per_sec = value.parse().map_err(|_| invalid())?;
            }
            _ => limits.max_concurrent_requests = value.parse().map_err(|_| invalid())?,
        }
    }
    limits.validate()?;
    Ok(limits)
}
fn overrides_in(c: &Connection, user_id: i64) -> Result<LimitOverrides, RepositoryError> {
    c.query_row(
        "SELECT max_tunnels,bandwidth_bytes_per_sec,max_concurrent_requests FROM user_limits WHERE user_id=?",
        [user_id],
        |r| overrides_at(r, 0),
    )
    .optional()
    .map_err(map_db)
    .map(Option::unwrap_or_default)
}
/// Reads the three `user_limits` value columns starting at column `first`.
fn overrides_at(r: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<LimitOverrides> {
    Ok(LimitOverrides {
        max_tunnels: r.get(first)?,
        // The column CHECK keeps stored bandwidth positive.
        bandwidth_bytes_per_sec: r.get::<_, Option<i64>>(first + 1)?.map(i64::cast_unsigned),
        max_concurrent_requests: r.get(first + 2)?,
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
            add_missing_user_columns(&c)?;
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
                "INSERT INTO users(email,password_hash,is_admin,created_at_ms,must_change_password) VALUES(?,?,?,?,?)",
                params![email, u.password_hash, u.is_admin, now, u.must_change_password],
            )
            .map_err(map_db)?;
            Ok(User {
                id: c.last_insert_rowid(),
                email,
                password_hash: u.password_hash,
                is_admin: u.is_admin,
                assigned_subdomain: None,
                created_at_ms: now,
                must_change_password: u.must_change_password,
                can_reserve_directly: false,
            })
        })
        .await
    }
    pub async fn user_by_email(&self, email: &str) -> Result<Option<User>, RepositoryError> {
        let e = email.to_owned();
        self.run(move |c| {
            c.query_row(
                &format!("SELECT {USER_COLUMNS} FROM users WHERE email=?"),
                [e],
                user_row,
            )
            .optional()
            .map_err(map_db)
        })
        .await
    }
    pub async fn user_by_id(&self, id: i64) -> Result<Option<User>, RepositoryError> {
        self.run(move |c| {
            c.query_row(
                &format!("SELECT {USER_COLUMNS} FROM users WHERE id=?"),
                [id],
                user_row,
            )
            .optional()
            .map_err(map_db)
        })
        .await
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
                must_change_password: false,
                can_reserve_directly: false,
            })
        })
        .await
    }
    pub async fn get_or_create_assigned_subdomain(
        &self,
        user_id: i64,
    ) -> Result<String, RepositoryError> {
        self.run(move|c| { let tx=c.transaction_with_behavior(TransactionBehavior::Immediate).map_err(map_db)?; let existing:Option<String>=tx.query_row("SELECT assigned_subdomain FROM users WHERE id=?",[user_id],|r|r.get(0)).optional().map_err(map_db)?.ok_or(RepositoryError::NotFound)?; if let Some(n)=existing {return Ok(n);} for _ in 0..32 { let mut bytes=[0u8;10]; getrandom::fill(&mut bytes).map_err(|_|RepositoryError::Random)?; let name=bytes.iter().map(|b|format!("{b:02x}")).collect::<String>(); let reserved:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM reserved_subdomains WHERE name=?1) OR EXISTS(SELECT 1 FROM reservation_requests WHERE name=?1)",[&name],|r|r.get(0)).map_err(map_db)?; if reserved {continue;} match tx.execute("UPDATE users SET assigned_subdomain=? WHERE id=?",params![name,user_id]) { Ok(_)=>{tx.commit().map_err(map_db)?;return Ok(name)},Err(e) if matches!(&e,rusqlite::Error::SqliteFailure(code,_) if code.code==rusqlite::ErrorCode::ConstraintViolation)=>continue,Err(e)=>return Err(map_db(e)) } } Err(RepositoryError::Conflict) }).await
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
        let key = session_key(&s.id);
        self.run(move |c| {
            c.execute(
                "INSERT INTO sessions(id,user_id,expires_at_ms) VALUES(?,?,?)",
                params![key, s.user_id, s.expires_at_ms],
            )
            .map_err(map_db)?;
            Ok(())
        })
        .await
    }
    pub async fn session_by_id(&self, id: &str) -> Result<Option<Session>, RepositoryError> {
        let (id, key) = (id.to_owned(), session_key(id));
        self.run(move |c| {
            c.query_row(
                "SELECT user_id,expires_at_ms FROM sessions WHERE id=? AND expires_at_ms>?",
                params![key, now_ms()?],
                |r| {
                    Ok(Session {
                        id,
                        user_id: r.get(0)?,
                        expires_at_ms: r.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(map_db)
        })
        .await
    }
    pub async fn delete_session(&self, id: &str) -> Result<(), RepositoryError> {
        let key = session_key(id);
        self.run(move |c| {
            c.execute("DELETE FROM sessions WHERE id=?", [key])
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
    /// Replaces the password and ends every session. `must_change` makes the
    /// next login ask for a new password, as after an admin or host reset.
    pub async fn update_password(
        &self,
        user_id: i64,
        hash: &str,
        must_change: bool,
    ) -> Result<(), RepositoryError> {
        let hash = hash.to_owned();
        self.run(move |c| {
            let tx = c.transaction().map_err(map_db)?;
            if tx
                .execute(
                    "UPDATE users SET password_hash=?,must_change_password=? WHERE id=?",
                    params![hash, must_change, user_id],
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
    /// Takes a free name at once, replacing the owner's own pending request.
    pub async fn reserve_subdomain(
        &self,
        name: &str,
        owner_user_id: i64,
    ) -> Result<ReservedSubdomain, RepositoryError> {
        let name = name.to_owned();
        self.run(move |c| {
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_db)?;
            check_name(&tx, &name, owner_user_id, NameAction::Reserve)?;
            insert_reservation(&tx, &name, owner_user_id)?;
            tx.commit().map_err(map_db)?;
            Ok(ReservedSubdomain {
                name,
                user_id: owner_user_id,
            })
        })
        .await
    }
    /// Asks an admin for a free name; it holds the name against other users
    /// until approved or rejected.
    pub async fn request_subdomain(&self, name: &str, user_id: i64) -> Result<(), RepositoryError> {
        let name = name.to_owned();
        self.run(move |c| {
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_db)?;
            check_name(&tx, &name, user_id, NameAction::Request)?;
            tx.execute(
                "INSERT INTO reservation_requests(name,user_id,created_at_ms) VALUES(?,?,?)",
                params![name, user_id, now_ms()?],
            )
            .map_err(map_db)?;
            tx.commit().map_err(map_db)
        })
        .await
    }
    /// The user's pending request names.
    pub async fn requests_for_user(&self, user_id: i64) -> Result<Vec<String>, RepositoryError> {
        self.run(move |c| {
            let mut s = c
                .prepare("SELECT name FROM reservation_requests WHERE user_id=? ORDER BY name")
                .map_err(map_db)?;
            s.query_map([user_id], |r| r.get(0))
                .map_err(map_db)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(map_db)
        })
        .await
    }
    pub async fn cancel_subdomain_request(
        &self,
        name: &str,
        user_id: i64,
    ) -> Result<bool, RepositoryError> {
        let name = name.to_owned();
        self.run(move |c| {
            Ok(c.execute(
                "DELETE FROM reservation_requests WHERE name=? AND user_id=?",
                params![name, user_id],
            )
            .map_err(map_db)?
                > 0)
        })
        .await
    }
    /// Every pending request, oldest first. The caller must have checked that
    /// the requester is an admin.
    pub async fn reservation_requests(&self) -> Result<Vec<ReservationRequest>, RepositoryError> {
        self.run(|c| {
            let mut s = c
                .prepare(
                    "SELECT r.name,r.user_id,u.email,r.created_at_ms FROM reservation_requests r
                     JOIN users u ON u.id=r.user_id ORDER BY r.created_at_ms,r.name",
                )
                .map_err(map_db)?;
            s.query_map([], |r| {
                Ok(ReservationRequest {
                    name: r.get(0)?,
                    user_id: r.get(1)?,
                    email: r.get(2)?,
                    created_at_ms: r.get(3)?,
                })
            })
            .map_err(map_db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_db)
        })
        .await
    }
    /// Turns a pending request into a reservation for its requester.
    pub async fn approve_subdomain_request(
        &self,
        name: &str,
    ) -> Result<ReservedSubdomain, RepositoryError> {
        let name = name.to_owned();
        self.run(move |c| {
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_db)?;
            let user_id: i64 = tx
                .query_row(
                    "SELECT user_id FROM reservation_requests WHERE name=?",
                    [&name],
                    |r| r.get(0),
                )
                .optional()
                .map_err(map_db)?
                .ok_or(RepositoryError::NotFound)?;
            // Re-check: an assigned subdomain may have taken the name since.
            check_name(&tx, &name, user_id, NameAction::Reserve)?;
            insert_reservation(&tx, &name, user_id)?;
            tx.commit().map_err(map_db)?;
            Ok(ReservedSubdomain { name, user_id })
        })
        .await
    }
    pub async fn reject_subdomain_request(&self, name: &str) -> Result<bool, RepositoryError> {
        let name = name.to_owned();
        self.run(move |c| {
            Ok(
                c.execute("DELETE FROM reservation_requests WHERE name=?", [name])
                    .map_err(map_db)?
                    > 0,
            )
        })
        .await
    }
    /// Sets a user's role and reservation permission. Refuses to remove the
    /// last admin, which would leave nobody able to manage the relay.
    pub async fn set_user_access(
        &self,
        user_id: i64,
        is_admin: bool,
        can_reserve_directly: bool,
    ) -> Result<(), RepositoryError> {
        self.run(move |c| {
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_db)?;
            let was_admin: bool = tx
                .query_row("SELECT is_admin FROM users WHERE id=?", [user_id], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(map_db)?
                .ok_or(RepositoryError::NotFound)?;
            tx.execute(
                "UPDATE users SET is_admin=?,can_reserve_directly=? WHERE id=?",
                params![is_admin, can_reserve_directly, user_id],
            )
            .map_err(map_db)?;
            if was_admin && !is_admin {
                let admins: i64 = tx
                    .query_row("SELECT count(*) FROM users WHERE is_admin=1", [], |r| {
                        r.get(0)
                    })
                    .map_err(map_db)?;
                if admins == 0 {
                    return Err(RepositoryError::Invalid("at least one admin must remain"));
                }
            }
            tx.commit().map_err(map_db)
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
    /// The deployment-wide defaults, falling back to `UserLimits::DEFAULT` for
    /// any value an admin has not stored.
    pub async fn default_limits(&self) -> Result<UserLimits, RepositoryError> {
        self.run(|c| default_limits_in(c)).await
    }
    pub async fn set_default_limits(&self, limits: UserLimits) -> Result<(), RepositoryError> {
        limits.validate()?;
        self.run(move |c| {
            let tx = c.transaction().map_err(map_db)?;
            let values = [
                limits.max_tunnels.to_string(),
                limits.bandwidth_bytes_per_sec.to_string(),
                limits.max_concurrent_requests.to_string(),
            ];
            for (key, value) in LIMIT_KEYS.iter().zip(values) {
                tx.execute(
                    "INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![key, value],
                )
                .map_err(map_db)?;
            }
            tx.commit().map_err(map_db)
        })
        .await
    }
    /// Replaces a user's overrides; all-`None` removes them so the user
    /// follows the defaults again.
    pub async fn set_limit_overrides(
        &self,
        user_id: i64,
        overrides: LimitOverrides,
    ) -> Result<(), RepositoryError> {
        overrides.validate()?;
        self.run(move |c| {
            let exists: bool = c
                .query_row("SELECT EXISTS(SELECT 1 FROM users WHERE id=?)", [user_id], |r| {
                    r.get(0)
                })
                .map_err(map_db)?;
            if !exists {
                return Err(RepositoryError::NotFound);
            }
            if overrides.is_empty() {
                c.execute("DELETE FROM user_limits WHERE user_id=?", [user_id])
                    .map_err(map_db)?;
                return Ok(());
            }
            // validate() bounded bandwidth to i64.
            let bandwidth = overrides.bandwidth_bytes_per_sec.map(u64::cast_signed);
            c.execute(
                "INSERT INTO user_limits(user_id,max_tunnels,bandwidth_bytes_per_sec,max_concurrent_requests) VALUES(?,?,?,?)
                 ON CONFLICT(user_id) DO UPDATE SET max_tunnels=excluded.max_tunnels,bandwidth_bytes_per_sec=excluded.bandwidth_bytes_per_sec,max_concurrent_requests=excluded.max_concurrent_requests",
                params![user_id, overrides.max_tunnels, bandwidth, overrides.max_concurrent_requests],
            )
            .map_err(map_db)?;
            Ok(())
        })
        .await
    }
    /// The limits that apply to a user, or `None` for an admin, who is exempt
    /// from every quota.
    pub async fn effective_limits(
        &self,
        user_id: i64,
    ) -> Result<Option<UserLimits>, RepositoryError> {
        self.run(move |c| {
            let is_admin: bool = c
                .query_row("SELECT is_admin FROM users WHERE id=?", [user_id], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(map_db)?
                .ok_or(RepositoryError::NotFound)?;
            if is_admin {
                return Ok(None);
            }
            Ok(Some(
                default_limits_in(c)?.with_overrides(&overrides_in(c, user_id)?),
            ))
        })
        .await
    }
    /// Every user with their overrides, for the admin dashboard. The caller
    /// must have checked that the requester is an admin.
    pub async fn users_with_limits(&self) -> Result<Vec<UserLimitEntry>, RepositoryError> {
        self.run(|c| {
            let mut s = c
                .prepare(
                    "SELECT u.id,u.email,u.is_admin,u.created_at_ms,u.must_change_password,u.can_reserve_directly,l.max_tunnels,l.bandwidth_bytes_per_sec,l.max_concurrent_requests
                     FROM users u LEFT JOIN user_limits l ON l.user_id=u.id ORDER BY u.id",
                )
                .map_err(map_db)?;
            s.query_map([], |r| {
                Ok(UserLimitEntry {
                    user_id: r.get(0)?,
                    email: r.get(1)?,
                    is_admin: r.get(2)?,
                    created_at_ms: r.get(3)?,
                    must_change_password: r.get(4)?,
                    can_reserve_directly: r.get(5)?,
                    overrides: overrides_at(r, 6)?,
                })
            })
            .map_err(map_db)?
            .collect::<Result<Vec<_>, _>>()
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
            must_change_password: false,
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
            Err(RepositoryError::NameTaken(NameConflict::AssignedToOther))
        ));
        r.create_session(NewSession {
            id: "s".into(),
            user_id: a.id,
            expires_at_ms: i64::MAX,
        })
        .await
        .expect("session");
        r.update_password(a.id, "new", true)
            .await
            .expect("password");
        assert!(r.session_by_id("s").await.expect("lookup").is_none());
        let reset = r.user_by_id(a.id).await.expect("lookup").expect("user");
        assert!(reset.must_change_password);
        r.update_password(a.id, "mine", false)
            .await
            .expect("password");
        let changed = r.user_by_id(a.id).await.expect("lookup").expect("user");
        assert!(!changed.must_change_password);
    }
    #[tokio::test]
    async fn sessions_are_stored_hashed_and_resolve_from_the_cookie_value() {
        let r = repo().await;
        let a = user(&r, "a@x.test").await;
        r.create_session(NewSession {
            id: "cookie-value".into(),
            user_id: a.id,
            expires_at_ms: i64::MAX,
        })
        .await
        .expect("session");
        let stored: String = r
            .run(|c| {
                c.query_row("SELECT id FROM sessions", [], |row| row.get(0))
                    .map_err(map_db)
            })
            .await
            .expect("stored id");
        assert_ne!(stored, "cookie-value");
        assert!(r.session_by_id(&stored).await.expect("lookup").is_none());
        let found = r.session_by_id("cookie-value").await.expect("lookup");
        assert_eq!(
            found.map(|s| (s.id, s.user_id)),
            Some(("cookie-value".into(), a.id))
        );
        r.delete_session("cookie-value").await.expect("logout");
        assert!(
            r.session_by_id("cookie-value")
                .await
                .expect("lookup")
                .is_none()
        );
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
    async fn requests_hold_names_until_approved_or_rejected() {
        let r = repo().await;
        let a = user(&r, "a@x.test").await;
        let b = user(&r, "b@x.test").await;
        r.request_subdomain("demo", a.id).await.expect("request");
        assert!(r.reservation_owner("demo").await.expect("owner").is_none());
        for (name, requester, expected) in [
            ("demo", a.id, NameConflict::AlreadyRequested),
            ("demo", b.id, NameConflict::RequestedByOther),
        ] {
            assert!(matches!(
                r.request_subdomain(name, requester).await,
                Err(RepositoryError::NameTaken(c)) if c == expected
            ));
        }
        assert!(matches!(
            r.reserve_subdomain("demo", b.id).await,
            Err(RepositoryError::NameTaken(NameConflict::RequestedByOther))
        ));
        assert_eq!(r.requests_for_user(a.id).await.expect("list"), ["demo"]);
        let listed = r.reservation_requests().await.expect("pending");
        assert_eq!(
            listed
                .iter()
                .map(|q| (q.name.as_str(), q.email.as_str()))
                .collect::<Vec<_>>(),
            [("demo", "a@x.test")]
        );
        let approved = r.approve_subdomain_request("demo").await.expect("approve");
        assert_eq!(approved.user_id, a.id);
        assert_eq!(
            r.reservation_owner("demo").await.expect("owner"),
            Some(a.id)
        );
        assert!(r.requests_for_user(a.id).await.expect("list").is_empty());
        assert!(matches!(
            r.approve_subdomain_request("demo").await,
            Err(RepositoryError::NotFound)
        ));

        r.request_subdomain("other", b.id).await.expect("request");
        assert!(r.reject_subdomain_request("other").await.expect("reject"));
        r.request_subdomain("other", a.id)
            .await
            .expect("free again");
        // Reserving directly replaces the requester's own pending request.
        r.reserve_subdomain("other", a.id).await.expect("reserve");
        assert!(r.requests_for_user(a.id).await.expect("list").is_empty());
        r.request_subdomain("mine", b.id).await.expect("request");
        assert!(
            r.cancel_subdomain_request("mine", b.id)
                .await
                .expect("cancel")
        );
        assert!(
            !r.cancel_subdomain_request("mine", b.id)
                .await
                .expect("gone")
        );
    }
    #[tokio::test]
    async fn access_changes_keep_one_admin() {
        let r = repo().await;
        let a = user(&r, "a@x.test").await;
        let admin = r.bootstrap_admin_for_test().await;
        r.set_user_access(a.id, true, true).await.expect("promote");
        let promoted = r.user_by_id(a.id).await.expect("lookup").expect("user");
        assert!(promoted.is_admin && promoted.can_reserve_directly);
        r.set_user_access(admin, false, false)
            .await
            .expect("demote");
        assert!(matches!(
            r.set_user_access(a.id, false, false).await,
            Err(RepositoryError::Invalid(_))
        ));
        assert!(
            r.user_by_id(a.id)
                .await
                .expect("lookup")
                .expect("user")
                .is_admin
        );
        assert!(matches!(
            r.set_user_access(9_999, false, false).await,
            Err(RepositoryError::NotFound)
        ));
    }
    #[tokio::test]
    async fn open_adds_new_user_columns_to_an_older_database() {
        let dir = std::env::temp_dir().join(format!("vorp-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("old.sqlite3");
        let _ = std::fs::remove_file(&path); // Absent on a clean run.
        {
            let c = Connection::open(&path).expect("old db");
            c.execute_batch(
                "CREATE TABLE users(id INTEGER PRIMARY KEY,email TEXT NOT NULL UNIQUE COLLATE NOCASE,password_hash TEXT NOT NULL,is_admin INTEGER NOT NULL CHECK(is_admin IN(0,1)),assigned_subdomain TEXT UNIQUE,created_at_ms INTEGER NOT NULL);
                 INSERT INTO users(email,password_hash,is_admin,created_at_ms) VALUES('old@x.test','hash',0,1);",
            )
            .expect("old schema");
        }
        let r = Repository::open(&path).await.expect("migrate");
        let old = r
            .user_by_email("old@x.test")
            .await
            .expect("lookup")
            .expect("user");
        assert!(!old.must_change_password && !old.can_reserve_directly);
        drop(r);
        // Opening again finds the columns present and leaves them alone.
        Repository::open(&path).await.expect("reopen");
        let _ = std::fs::remove_dir_all(&dir); // Leftover temp files are harmless.
    }
    #[tokio::test]
    async fn limits_resolve_defaults_overrides_and_admin_exemption() {
        let r = repo().await;
        let a = user(&r, "a@x.test").await;
        let admin = r.bootstrap_admin_for_test().await;
        assert_eq!(
            r.default_limits().await.expect("defaults"),
            UserLimits::DEFAULT
        );
        assert_eq!(
            r.effective_limits(a.id).await.expect("limits"),
            Some(UserLimits::DEFAULT)
        );
        assert_eq!(r.effective_limits(admin).await.expect("admin"), None);
        assert!(matches!(
            r.effective_limits(9_999).await,
            Err(RepositoryError::NotFound)
        ));

        let defaults = UserLimits {
            max_tunnels: 5,
            bandwidth_bytes_per_sec: 1_000,
            max_concurrent_requests: 8,
        };
        r.set_default_limits(defaults).await.expect("set defaults");
        let overrides = LimitOverrides {
            max_tunnels: Some(1),
            ..LimitOverrides::default()
        };
        r.set_limit_overrides(a.id, overrides)
            .await
            .expect("override");
        assert_eq!(
            r.effective_limits(a.id).await.expect("limits"),
            Some(UserLimits {
                max_tunnels: 1,
                ..defaults
            })
        );
        let listed = r.users_with_limits().await.expect("list");
        assert_eq!(listed.len(), 2);
        assert_eq!((listed[0].user_id, listed[0].overrides), (a.id, overrides));
        assert!(listed[1].is_admin);
        assert_eq!(listed[1].overrides, LimitOverrides::default());

        r.set_limit_overrides(a.id, LimitOverrides::default())
            .await
            .expect("clear");
        assert_eq!(
            r.effective_limits(a.id).await.expect("limits"),
            Some(defaults)
        );
        assert!(matches!(
            r.set_limit_overrides(9_999, overrides).await,
            Err(RepositoryError::NotFound)
        ));
        assert!(matches!(
            r.set_limit_overrides(
                a.id,
                LimitOverrides {
                    max_tunnels: Some(0),
                    ..LimitOverrides::default()
                }
            )
            .await,
            Err(RepositoryError::Invalid(_))
        ));
    }
    impl Repository {
        async fn bootstrap_admin_for_test(&self) -> i64 {
            self.create_user(NewUser {
                email: "admin@x.test".into(),
                password_hash: "hash".into(),
                is_admin: true,
                must_change_password: false,
            })
            .await
            .expect("admin")
            .id
        }
    }
}
