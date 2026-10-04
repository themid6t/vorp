//! Reserved subdomain names. A user with direct reservation (and every admin)
//! takes a free name at once; anyone else files a request that an admin
//! approves or rejects. A pending request already holds the name, so a
//! conflict is reported to the second requester immediately.

use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};

use crate::{ApiError, AppState, OkBody, current_user, mutating_request, quota::require_admin};

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ReservationStatus {
    Reserved,
    Pending,
}

#[derive(Serialize)]
pub(crate) struct ReservationBody {
    name: String,
    status: ReservationStatus,
}

pub(crate) async fn list_reservations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ReservationBody>>, ApiError> {
    let u = current_user(&state, &headers).await?;
    let reserved = state
        .repository
        .reservations_for_user(u.id)
        .await
        .map_err(ApiError::from)?;
    let pending = state
        .repository
        .requests_for_user(u.id)
        .await
        .map_err(ApiError::from)?;
    let reserved = reserved.into_iter().map(|r| ReservationBody {
        name: r.name,
        status: ReservationStatus::Reserved,
    });
    let pending = pending.into_iter().map(|name| ReservationBody {
        name,
        status: ReservationStatus::Pending,
    });
    Ok(Json(reserved.chain(pending).collect()))
}

#[derive(Deserialize)]
pub(crate) struct ReserveInput {
    name: String,
}

pub(crate) async fn reserve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<ReserveInput>,
) -> Result<Json<ReservationBody>, ApiError> {
    mutating_request(&headers)?;
    let u = current_user(&state, &headers).await?;
    let name = input.name.trim().to_ascii_lowercase();
    let status = if u.is_admin || u.can_reserve_directly {
        state
            .repository
            .reserve_subdomain(&name, u.id)
            .await
            .map_err(ApiError::from)?;
        ReservationStatus::Reserved
    } else {
        state
            .repository
            .request_subdomain(&name, u.id)
            .await
            .map_err(ApiError::from)?;
        ReservationStatus::Pending
    };
    Ok(Json(ReservationBody { name, status }))
}

/// Releases a reservation, or withdraws a pending request for the name.
pub(crate) async fn release(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<OkBody>, ApiError> {
    mutating_request(&headers)?;
    let u = current_user(&state, &headers).await?;
    let released = state
        .repository
        .release_subdomain(&name, u.id)
        .await
        .map_err(ApiError::from)?;
    let cancelled = !released
        && state
            .repository
            .cancel_subdomain_request(&name, u.id)
            .await
            .map_err(ApiError::from)?;
    Ok(Json(OkBody {
        ok: released || cancelled,
    }))
}

#[derive(Serialize)]
pub(crate) struct RequestBody {
    name: String,
    user_id: i64,
    email: String,
    created_at_ms: i64,
}

pub(crate) async fn list_requests(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<RequestBody>>, ApiError> {
    require_admin(&state, &headers).await?;
    Ok(Json(
        state
            .repository
            .reservation_requests()
            .await
            .map_err(ApiError::from)?
            .into_iter()
            .map(|r| RequestBody {
                name: r.name,
                user_id: r.user_id,
                email: r.email,
                created_at_ms: r.created_at_ms,
            })
            .collect(),
    ))
}

#[derive(Serialize)]
pub(crate) struct ApprovedBody {
    name: String,
    user_id: i64,
}

pub(crate) async fn approve_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<ApprovedBody>, ApiError> {
    mutating_request(&headers)?;
    require_admin(&state, &headers).await?;
    let r = state
        .repository
        .approve_subdomain_request(&name)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(ApprovedBody {
        name: r.name,
        user_id: r.user_id,
    }))
}

pub(crate) async fn reject_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<OkBody>, ApiError> {
    mutating_request(&headers)?;
    require_admin(&state, &headers).await?;
    Ok(Json(OkBody {
        ok: state
            .repository
            .reject_subdomain_request(&name)
            .await
            .map_err(ApiError::from)?,
    }))
}
