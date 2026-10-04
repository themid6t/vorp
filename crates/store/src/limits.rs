use crate::RepositoryError;

/// Per-user quota values. Admins have none: an admin is exempt from every
/// quota, which callers see as `effective_limits` returning `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserLimits {
    /// Live tunnels the user may hold across all of their agents.
    pub max_tunnels: u32,
    /// Proxied body bytes per second, uploads and downloads combined.
    pub bandwidth_bytes_per_sec: u64,
    /// In-flight HTTP requests across all of the user's tunnels.
    pub max_concurrent_requests: u32,
}

impl UserLimits {
    /// Used until an admin stores different defaults.
    pub const DEFAULT: Self = Self {
        max_tunnels: 3,
        bandwidth_bytes_per_sec: 10 * 1024 * 1024,
        max_concurrent_requests: 64,
    };

    pub fn validate(&self) -> Result<(), RepositoryError> {
        LimitOverrides::from(*self).validate()
    }

    pub fn with_overrides(self, overrides: &LimitOverrides) -> Self {
        Self {
            max_tunnels: overrides.max_tunnels.unwrap_or(self.max_tunnels),
            bandwidth_bytes_per_sec: overrides
                .bandwidth_bytes_per_sec
                .unwrap_or(self.bandwidth_bytes_per_sec),
            max_concurrent_requests: overrides
                .max_concurrent_requests
                .unwrap_or(self.max_concurrent_requests),
        }
    }
}

/// An admin's per-user changes to the defaults; `None` inherits the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LimitOverrides {
    pub max_tunnels: Option<u32>,
    pub bandwidth_bytes_per_sec: Option<u64>,
    pub max_concurrent_requests: Option<u32>,
}

impl LimitOverrides {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Every value is at least 1 (a zero quota would wedge the user rather
    /// than express a policy) and fits SQLite's signed integer.
    pub fn validate(&self) -> Result<(), RepositoryError> {
        if self.max_tunnels == Some(0) {
            return Err(RepositoryError::Invalid("max_tunnels must be at least 1"));
        }
        if self.max_concurrent_requests == Some(0) {
            return Err(RepositoryError::Invalid(
                "max_concurrent_requests must be at least 1",
            ));
        }
        match self.bandwidth_bytes_per_sec {
            Some(0) => Err(RepositoryError::Invalid(
                "bandwidth_bytes_per_sec must be at least 1",
            )),
            Some(value) if i64::try_from(value).is_err() => Err(RepositoryError::Invalid(
                "bandwidth_bytes_per_sec too large",
            )),
            _ => Ok(()),
        }
    }
}

impl From<UserLimits> for LimitOverrides {
    fn from(limits: UserLimits) -> Self {
        Self {
            max_tunnels: Some(limits.max_tunnels),
            bandwidth_bytes_per_sec: Some(limits.bandwidth_bytes_per_sec),
            max_concurrent_requests: Some(limits.max_concurrent_requests),
        }
    }
}

/// One row of the admin user listing.
#[derive(Clone, Debug)]
pub struct UserLimitEntry {
    pub user_id: i64,
    pub email: String,
    pub is_admin: bool,
    pub created_at_ms: i64,
    pub overrides: LimitOverrides,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_replace_only_set_fields() {
        let merged = UserLimits::DEFAULT.with_overrides(&LimitOverrides {
            max_tunnels: Some(7),
            ..LimitOverrides::default()
        });
        assert_eq!(merged.max_tunnels, 7);
        assert_eq!(
            merged.bandwidth_bytes_per_sec,
            UserLimits::DEFAULT.bandwidth_bytes_per_sec
        );
        assert_eq!(
            merged.max_concurrent_requests,
            UserLimits::DEFAULT.max_concurrent_requests
        );
    }

    #[test]
    fn validation_table() {
        let none = LimitOverrides::default();
        let cases = [
            (none, true),
            (LimitOverrides::from(UserLimits::DEFAULT), true),
            (
                LimitOverrides {
                    max_tunnels: Some(0),
                    ..none
                },
                false,
            ),
            (
                LimitOverrides {
                    max_concurrent_requests: Some(0),
                    ..none
                },
                false,
            ),
            (
                LimitOverrides {
                    bandwidth_bytes_per_sec: Some(0),
                    ..none
                },
                false,
            ),
            (
                LimitOverrides {
                    bandwidth_bytes_per_sec: Some(u64::MAX),
                    ..none
                },
                false,
            ),
        ];
        for (overrides, valid) in cases {
            assert_eq!(overrides.validate().is_ok(), valid, "{overrides:?}");
        }
    }
}
