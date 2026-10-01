use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("database operation failed: {0}")]
    Database(String),
    #[error("record not found")]
    NotFound,
    #[error("record already exists")]
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindPolicy {
    Any,
    Temporary,
    Reserved,
}

#[derive(Clone)]
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

/// Sole SQL boundary. All methods are stubs until the persistence workstream.
#[derive(Clone)]
pub struct Repository;

impl Repository {
    pub async fn open(_path: &Path) -> Result<Self, RepositoryError> {
        todo!("store workstream opens SQLite and applies migrations")
    }

    pub async fn create_user(&self, _user: NewUser) -> Result<User, RepositoryError> {
        todo!("store workstream implements user creation")
    }

    pub async fn user_by_email(&self, _email: &str) -> Result<Option<User>, RepositoryError> {
        todo!("store workstream implements user lookup")
    }

    pub async fn get_or_create_assigned_subdomain(
        &self,
        _user_id: i64,
    ) -> Result<String, RepositoryError> {
        todo!("store workstream assigns a unique user subdomain from a CSPRNG")
    }

    pub async fn assigned_subdomain_owner(
        &self,
        _name: &str,
    ) -> Result<Option<i64>, RepositoryError> {
        todo!("store workstream implements assigned-name ownership lookup")
    }

    pub async fn create_session(&self, _session: NewSession) -> Result<(), RepositoryError> {
        todo!("store workstream implements session creation")
    }

    pub async fn session_by_id(&self, _id: &str) -> Result<Option<Session>, RepositoryError> {
        todo!("store workstream implements session lookup")
    }

    pub async fn delete_session(&self, _id: &str) -> Result<(), RepositoryError> {
        todo!("store workstream implements logout")
    }

    pub async fn delete_user_sessions(&self, _user_id: i64) -> Result<(), RepositoryError> {
        todo!("store workstream implements password-change invalidation")
    }

    pub async fn create_token(
        &self,
        _token: NewAgentToken,
    ) -> Result<TokenRecord, RepositoryError> {
        todo!("store workstream implements token creation")
    }

    pub async fn token_by_id(
        &self,
        _token_id: i64,
    ) -> Result<Option<TokenRecord>, RepositoryError> {
        todo!("store workstream implements token authentication")
    }

    pub async fn tokens_for_user(
        &self,
        _user_id: i64,
    ) -> Result<Vec<TokenRecord>, RepositoryError> {
        todo!("store workstream implements scoped token listing")
    }

    pub async fn revoke_token(
        &self,
        _token_id: i64,
        _owner_user_id: i64,
    ) -> Result<bool, RepositoryError> {
        todo!("store workstream implements scoped token revocation")
    }

    pub async fn reservation_owner(&self, _name: &str) -> Result<Option<i64>, RepositoryError> {
        todo!("store workstream implements reservation lookup")
    }

    pub async fn reserve_subdomain(
        &self,
        _name: &str,
        _owner_user_id: i64,
    ) -> Result<ReservedSubdomain, RepositoryError> {
        todo!("store workstream implements reservation creation")
    }

    pub async fn release_subdomain(
        &self,
        _name: &str,
        _owner_user_id: i64,
    ) -> Result<bool, RepositoryError> {
        todo!("store workstream implements reservation deletion")
    }
}
