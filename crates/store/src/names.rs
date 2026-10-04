//! Who may take a subdomain name. A name is held by a reservation, a pending
//! reservation request, or a user's assigned subdomain; the decision is pure so
//! the table below can be tested without a database.

/// Why a name cannot be reserved or requested. Callers show `message` to the
/// requesting user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameConflict {
    AlreadyReserved,
    AlreadyRequested,
    ReservedByOther,
    RequestedByOther,
    AssignedToOther,
}

impl NameConflict {
    pub fn message(self) -> &'static str {
        match self {
            Self::AlreadyReserved => "you have already reserved this name",
            Self::AlreadyRequested => {
                "you have already requested this name; it is waiting for an administrator"
            }
            Self::ReservedByOther => "this name is already reserved by another user",
            Self::RequestedByOther => "another user has already requested this name",
            Self::AssignedToOther => "this name is another user's assigned subdomain",
        }
    }
}

/// The users currently holding a name, each `None` when nobody does.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NameHolders {
    pub reserved_by: Option<i64>,
    pub requested_by: Option<i64>,
    pub assigned_to: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameAction {
    /// Take the name now. The requester's own pending request is replaced.
    Reserve,
    /// Ask an administrator for the name.
    Request,
}

pub(crate) fn name_conflict(
    holders: NameHolders,
    requester: i64,
    action: NameAction,
) -> Option<NameConflict> {
    if let Some(owner) = holders.reserved_by {
        return Some(if owner == requester {
            NameConflict::AlreadyReserved
        } else {
            NameConflict::ReservedByOther
        });
    }
    if holders.assigned_to.is_some_and(|owner| owner != requester) {
        return Some(NameConflict::AssignedToOther);
    }
    match holders.requested_by {
        Some(owner) if owner != requester => Some(NameConflict::RequestedByOther),
        Some(_) if action == NameAction::Request => Some(NameConflict::AlreadyRequested),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: i64 = 1;
    const OTHER: i64 = 2;

    #[test]
    fn decision_table() {
        use NameAction::{Request, Reserve};
        use NameConflict::*;
        let h = |reserved_by, requested_by, assigned_to| NameHolders {
            reserved_by,
            requested_by,
            assigned_to,
        };
        let cases = [
            (h(None, None, None), Reserve, None),
            (h(None, None, None), Request, None),
            (h(Some(ME), None, None), Reserve, Some(AlreadyReserved)),
            (h(Some(ME), None, None), Request, Some(AlreadyReserved)),
            (h(Some(OTHER), None, None), Reserve, Some(ReservedByOther)),
            (h(Some(OTHER), None, None), Request, Some(ReservedByOther)),
            (h(None, Some(ME), None), Reserve, None),
            (h(None, Some(ME), None), Request, Some(AlreadyRequested)),
            (h(None, Some(OTHER), None), Reserve, Some(RequestedByOther)),
            (h(None, Some(OTHER), None), Request, Some(RequestedByOther)),
            (h(None, None, Some(ME)), Reserve, None),
            (h(None, None, Some(ME)), Request, None),
            (h(None, None, Some(OTHER)), Reserve, Some(AssignedToOther)),
            (h(None, None, Some(OTHER)), Request, Some(AssignedToOther)),
            // A reservation outranks an assignment or request on the same name.
            (
                h(Some(OTHER), Some(ME), Some(OTHER)),
                Reserve,
                Some(ReservedByOther),
            ),
            (
                h(None, Some(OTHER), Some(OTHER)),
                Request,
                Some(AssignedToOther),
            ),
        ];
        for (holders, action, expected) in cases {
            assert_eq!(
                name_conflict(holders, ME, action),
                expected,
                "{holders:?} {action:?}"
            );
        }
    }
}
