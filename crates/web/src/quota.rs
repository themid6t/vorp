//! Admin APIs for per-user quotas: the deployment-wide defaults and each
//! user's overrides. Admins are exempt from quotas, so their rows report no
//! effective limits.

use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use vorp_store::{LimitOverrides, User, UserLimits};

use crate::{ApiError, AppState, current_user, mutating_request};

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(crate) struct LimitsBody {
    max_tunnels: u32,
    bandwidth_bytes_per_sec: u64,
    max_concurrent_requests: u32,
}

impl From<UserLimits> for LimitsBody {
    fn from(l: UserLimits) -> Self {
        Self {
            max_tunnels: l.max_tunnels,
            bandwidth_bytes_per_sec: l.bandwidth_bytes_per_sec,
            max_concurrent_requests: l.max_concurrent_requests,
        }
    }
}

impl From<LimitsBody> for UserLimits {
    fn from(l: LimitsBody) -> Self {
        Self {
            max_tunnels: l.max_tunnels,
            bandwidth_bytes_per_sec: l.bandwidth_bytes_per_sec,
            max_concurrent_requests: l.max_concurrent_requests,
        }
    }
}

/// A missing or `null` field inherits the default; a `PUT` replaces every
/// field, so omitting one clears that override.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub(crate) struct OverridesBody {
    #[serde(default)]
    max_tunnels: Option<u32>,
    #[serde(default)]
    bandwidth_bytes_per_sec: Option<u64>,
    #[serde(default)]
    max_concurrent_requests: Option<u32>,
}

impl From<LimitOverrides> for OverridesBody {
    fn from(o: LimitOverrides) -> Self {
        Self {
            max_tunnels: o.max_tunnels,
            bandwidth_bytes_per_sec: o.bandwidth_bytes_per_sec,
            max_concurrent_requests: o.max_concurrent_requests,
        }
    }
}

impl From<OverridesBody> for LimitOverrides {
    fn from(o: OverridesBody) -> Self {
        Self {
            max_tunnels: o.max_tunnels,
            bandwidth_bytes_per_sec: o.bandwidth_bytes_per_sec,
            max_concurrent_requests: o.max_concurrent_requests,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct UserLimitsBody {
    id: i64,
    email: String,
    is_admin: bool,
    created_at_ms: i64,
    overrides: OverridesBody,
    /// `null` for an admin.
    effective: Option<LimitsBody>,
}

async fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<User, ApiError> {
    let user = current_user(state, headers).await?;
    if user.is_admin {
        Ok(user)
    } else {
        Err(ApiError::Forbidden)
    }
}

/// The write is already stored when this runs, so a failure only delays
/// live sessions until their next registration; it is reported so the admin
/// can retry, which is idempotent.
async fn apply_live(state: &AppState) -> Result<(), ApiError> {
    match &state.runtime {
        Some(runtime) => runtime
            .limits_changed()
            .await
            .map_err(|_| ApiError::Unavailable),
        None => Ok(()),
    }
}

pub(crate) async fn default_limits(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<LimitsBody>, ApiError> {
    require_admin(&state, &headers).await?;
    Ok(Json(
        state
            .repository
            .default_limits()
            .await
            .map_err(ApiError::from)?
            .into(),
    ))
}

pub(crate) async fn set_default_limits(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<LimitsBody>,
) -> Result<Json<LimitsBody>, ApiError> {
    mutating_request(&headers)?;
    require_admin(&state, &headers).await?;
    state
        .repository
        .set_default_limits(input.into())
        .await
        .map_err(ApiError::from)?;
    apply_live(&state).await?;
    Ok(Json(input))
}

pub(crate) async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<UserLimitsBody>>, ApiError> {
    require_admin(&state, &headers).await?;
    let defaults = state
        .repository
        .default_limits()
        .await
        .map_err(ApiError::from)?;
    let users = state
        .repository
        .users_with_limits()
        .await
        .map_err(ApiError::from)?;
    Ok(Json(
        users
            .into_iter()
            .map(|u| UserLimitsBody {
                id: u.user_id,
                email: u.email,
                is_admin: u.is_admin,
                created_at_ms: u.created_at_ms,
                overrides: u.overrides.into(),
                effective: (!u.is_admin).then(|| defaults.with_overrides(&u.overrides).into()),
            })
            .collect(),
    ))
}

pub(crate) async fn set_user_limits(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<OverridesBody>,
) -> Result<Json<OverridesBody>, ApiError> {
    mutating_request(&headers)?;
    require_admin(&state, &headers).await?;
    state
        .repository
        .set_limit_overrides(id, input.into())
        .await
        .map_err(ApiError::from)?;
    apply_live(&state).await?;
    Ok(Json(input))
}
