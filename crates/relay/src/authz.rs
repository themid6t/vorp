use vorp_protocol::ErrorCode;
use vorp_store::BindPolicy;

use crate::subdomain;

pub(crate) struct BindContext<'a> {
    pub requested: &'a str,
    pub user_id: i64,
    pub policy: BindPolicy,
    pub allowlist: &'a [String],
    pub assigned_name: Option<&'a str>,
    pub reservation_owner: Option<i64>,
}

pub(crate) fn decide_bind(context: &BindContext<'_>) -> Result<(), ErrorCode> {
    if context.requested.is_empty() {
        return if context.policy == BindPolicy::Reserved {
            Err(ErrorCode::SubdomainNotAllowed)
        } else {
            Ok(())
        };
    }
    if context.policy == BindPolicy::Temporary {
        return Err(ErrorCode::SubdomainNotAllowed);
    }
    if !subdomain::valid(context.requested) {
        return Err(ErrorCode::SubdomainInvalid);
    }
    if let Some(owner) = context.reservation_owner {
        if owner != context.user_id {
            return Err(ErrorCode::SubdomainTaken);
        }
        if context.policy == BindPolicy::Reserved
            && !context
                .allowlist
                .iter()
                .any(|name| name == context.requested)
        {
            return Err(ErrorCode::SubdomainNotAllowed);
        }
        return Ok(());
    }
    if context.policy == BindPolicy::Reserved {
        return Err(ErrorCode::SubdomainNotAllowed);
    }
    if context.assigned_name == Some(context.requested) {
        Ok(())
    } else {
        Err(ErrorCode::SubdomainNotAllowed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_table() {
        use BindPolicy::{Any, Reserved, Temporary};
        use ErrorCode::{SubdomainInvalid, SubdomainNotAllowed, SubdomainTaken};
        let cases = [
            ("temporary any", "", Any, None, false, None),
            ("temporary temporary", "", Temporary, None, false, None),
            (
                "temporary reserved",
                "",
                Reserved,
                None,
                false,
                Some(SubdomainNotAllowed),
            ),
            (
                "named temporary",
                "app",
                Temporary,
                None,
                false,
                Some(SubdomainNotAllowed),
            ),
            (
                "invalid any",
                "-bad",
                Any,
                None,
                false,
                Some(SubdomainInvalid),
            ),
            (
                "system reserved",
                "admin",
                Any,
                None,
                false,
                Some(SubdomainInvalid),
            ),
            ("owned any", "app", Any, Some(1), false, None),
            (
                "other any",
                "app",
                Any,
                Some(2),
                false,
                Some(SubdomainTaken),
            ),
            (
                "owned reserved listed",
                "app",
                Reserved,
                Some(1),
                true,
                None,
            ),
            (
                "owned reserved unlisted",
                "app",
                Reserved,
                Some(1),
                false,
                Some(SubdomainNotAllowed),
            ),
            (
                "other reserved",
                "app",
                Reserved,
                Some(2),
                true,
                Some(SubdomainTaken),
            ),
            (
                "unreserved reserved",
                "app",
                Reserved,
                None,
                true,
                Some(SubdomainNotAllowed),
            ),
            ("assigned any", "assigned", Any, None, false, None),
            (
                "unassigned any",
                "random",
                Any,
                None,
                false,
                Some(SubdomainNotAllowed),
            ),
        ];
        for (label, requested, policy, owner, listed, expected) in cases {
            let allowlist = if listed {
                vec!["app".into()]
            } else {
                Vec::new()
            };
            let context = BindContext {
                requested,
                user_id: 1,
                policy,
                allowlist: &allowlist,
                assigned_name: Some("assigned"),
                reservation_owner: owner,
            };
            assert_eq!(decide_bind(&context).err(), expected, "{label}");
        }
    }

    #[test]
    fn full_policy_name_owner_allowlist_cross_product() {
        use BindPolicy::{Any, Reserved, Temporary};
        use ErrorCode::{SubdomainInvalid, SubdomainNotAllowed, SubdomainTaken};
        let policies = [Any, Temporary, Reserved];
        let requested = ["", "app", "assigned", "-bad", "admin"];
        let owners = [None, Some(1), Some(2)];
        for policy in policies {
            for name in requested {
                for owner in owners {
                    for listed in [false, true] {
                        let allowlist = if listed {
                            vec![name.to_owned()]
                        } else {
                            Vec::new()
                        };
                        let context = BindContext {
                            requested: name,
                            user_id: 1,
                            policy,
                            allowlist: &allowlist,
                            assigned_name: Some("assigned"),
                            reservation_owner: owner,
                        };
                        // Rows are ordered as owner: none, self, other.
                        let expected = match (policy, name) {
                            (Any | Temporary, "") => None,
                            (Reserved, "") => Some(SubdomainNotAllowed),
                            (Temporary, _) => Some(SubdomainNotAllowed),
                            (_, "-bad" | "admin") => Some(SubdomainInvalid),
                            (Any, "assigned") => match owner {
                                None | Some(1) => None,
                                Some(2) => Some(SubdomainTaken),
                                _ => unreachable!(),
                            },
                            (Any, _) => match owner {
                                None => Some(SubdomainNotAllowed),
                                Some(1) => None,
                                Some(2) => Some(SubdomainTaken),
                                _ => unreachable!(),
                            },
                            (Reserved, _) => match owner {
                                Some(2) => Some(SubdomainTaken),
                                Some(1) if listed => None,
                                None | Some(1) => Some(SubdomainNotAllowed),
                                _ => unreachable!(),
                            },
                        };
                        assert_eq!(
                            decide_bind(&context).err(),
                            expected,
                            "policy={policy:?} name={name:?} owner={owner:?} listed={listed}"
                        );
                    }
                }
            }
        }
    }
}
