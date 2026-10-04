//! Admin changes to a user's role and reservation permission.

use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};

use crate::{
    ApiError, AppState, mutating_request,
    quota::{apply_live, require_admin},
};

/// Both fields are required: a `PUT` replaces the user's access.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub(crate) struct AccessBody {
    is_admin: bool,
    can_reserve_directly: bool,
}

pub(crate) async fn set_user_access(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<AccessBody>,
) -> Result<Json<AccessBody>, ApiError> {
    mutating_request(&headers)?;
    let admin = require_admin(&state, &headers).await?;
    // Another admin can demote this one; doing it yourself risks a lockout.
    if id == admin.id && !input.is_admin {
        return Err(ApiError::Invalid("you cannot remove your own admin role"));
    }
    state
        .repository
        .set_user_access(id, input.is_admin, input.can_reserve_directly)
        .await
        .map_err(ApiError::from)?;
    // Admins are exempt from quotas, so a role change moves the user's live
    // tunnels into or out of their limits.
    apply_live(&state).await?;
    Ok(Json(input))
}
