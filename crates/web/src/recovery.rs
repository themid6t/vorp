//! Host-side account recovery for `vorp admin reset-password`. Only someone
//! with access to the relay's database file can run it, which is the trust
//! boundary: there is deliberately no web-facing reset.

use vorp_store::{Repository, RepositoryError};

use crate::{ApiError, hash_password};

#[derive(Debug, thiserror::Error)]
pub enum ResetError {
    #[error("no account with that email")]
    UnknownEmail,
    #[error("password generation failed")]
    Random,
    #[error("password hashing failed")]
    Hashing,
    #[error("database: {0}")]
    Repository(#[from] RepositoryError),
}

/// Replaces the account's password with a fresh random one, ends all of its
/// sessions, and returns the new password. The caller shows it once; it is
/// never stored, and the dashboard makes the owner replace it at next login.
pub async fn reset_password(repository: &Repository, email: &str) -> Result<String, ResetError> {
    let user = repository
        .user_by_email(email.trim())
        .await?
        .ok_or(ResetError::UnknownEmail)?;
    let password = random_password()?;
    let hash = hash_password(&password)
        .await
        .map_err(|_: ApiError| ResetError::Hashing)?;
    // The printed password travels over a terminal; the owner replaces it at
    // the next login.
    repository.update_password(user.id, &hash, true).await?;
    Ok(password)
}

/// 128 bits from the CSPRNG as 32 hex characters, inside the 12–1024
/// character password policy.
fn random_password() -> Result<String, ResetError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| ResetError::Random)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify_password;
    use vorp_store::{NewSession, NewUser};

    #[tokio::test]
    async fn reset_replaces_password_and_ends_sessions() {
        let repository = Repository::open(std::path::Path::new(":memory:"))
            .await
            .expect("db");
        let user = repository
            .create_user(NewUser {
                email: "admin@example.test".into(),
                password_hash: hash_password("forgotten password").await.expect("hash"),
                is_admin: true,
                must_change_password: false,
            })
            .await
            .expect("user");
        repository
            .create_session(NewSession {
                id: "live-session".into(),
                user_id: user.id,
                expires_at_ms: i64::MAX,
            })
            .await
            .expect("session");

        let password = reset_password(&repository, " Admin@Example.test ")
            .await
            .expect("reset");

        assert_eq!(password.len(), 32);
        let stored = repository
            .user_by_id(user.id)
            .await
            .expect("lookup")
            .expect("user");
        assert!(verify_password(&password, &stored.password_hash).await);
        assert!(!verify_password("forgotten password", &stored.password_hash).await);
        assert!(stored.is_admin, "reset must not change the role");
        assert!(
            stored.must_change_password,
            "the printed password is temporary"
        );
        assert!(
            repository
                .session_by_id("live-session")
                .await
                .expect("lookup")
                .is_none()
        );
        assert!(matches!(
            reset_password(&repository, "nobody@example.test").await,
            Err(ResetError::UnknownEmail)
        ));
    }
}
