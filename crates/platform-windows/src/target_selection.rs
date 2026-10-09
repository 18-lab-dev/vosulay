//! Foreground-retention policy, independent of Win32 for deterministic regressions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) pid: u32,
    pub(crate) created: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ticket {
    pub(crate) identity: Identity,
    generation: u64,
}

/// Pure policy: an unrelated foreground never chooses a target; an exit forgets
/// the old instance. Generations also reject A -> B -> A attachment completions.
#[derive(Default)]
pub(crate) struct Selection {
    pub(crate) ticket: Option<Ticket>,
    generation: u64,
}
impl Selection {
    pub(crate) fn observe(&mut self, retained_alive: bool, supported_foreground: Option<Identity>) {
        if self.ticket.is_some() && !retained_alive {
            self.generation += 1;
            self.ticket = None;
        }
        if let Some(identity) = supported_foreground
            && self.ticket.is_none_or(|ticket| ticket.identity != identity)
        {
            self.generation += 1;
            self.ticket = Some(Ticket {
                identity,
                generation: self.generation,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Identity, Selection};
    const A: Identity = Identity {
        pid: 17,
        created: 100,
    };
    const B: Identity = Identity {
        pid: 19,
        created: 200,
    };

    #[test]
    fn unrelated_foregrounds_retain_only_a_supported_selection() {
        let mut selection = Selection::default();
        selection.observe(false, None);
        assert_eq!(selection.ticket, None);
        selection.observe(false, Some(A));
        let attached = selection.ticket;
        selection.observe(true, None);
        selection.observe(true, None);
        assert_eq!(selection.ticket, attached);
        selection.observe(true, Some(A));
        assert_eq!(selection.ticket, attached);
    }
    #[test]
    fn rapid_supported_switch_then_unrelated_rejects_pending_attach() {
        let mut selection = Selection::default();
        selection.observe(false, Some(A));
        let pending = selection.ticket.unwrap();
        selection.observe(true, Some(B));
        selection.observe(true, None);
        assert_eq!(selection.ticket.unwrap().identity, B);
        assert_ne!(selection.ticket, Some(pending));
        selection.observe(true, Some(A));
        assert_ne!(
            selection.ticket,
            Some(pending),
            "A -> B -> A must advance the generation"
        );
    }
    #[test]
    fn selected_exit_forgets_the_instance_without_a_fallback() {
        let mut selection = Selection::default();
        selection.observe(false, Some(A));
        selection.observe(false, None);
        assert_eq!(selection.ticket, None);
        selection.observe(false, None);
        assert_eq!(selection.ticket, None);
    }
    #[test]
    fn restart_and_pid_reuse_require_a_new_supported_foreground() {
        let mut selection = Selection::default();
        selection.observe(false, Some(A));
        let pending = selection.ticket;
        selection.observe(false, None);
        // A replacement with the same PID is not discovered by enumeration.
        assert_eq!(selection.ticket, None);
        let restarted = Identity { created: 300, ..A };
        selection.observe(false, Some(restarted));
        assert_eq!(selection.ticket.unwrap().identity, restarted);
        assert_ne!(selection.ticket, pending);
        // Reuse can also be observed before the old-handle exit callback.
        let reused = Identity { created: 400, ..A };
        selection.observe(false, Some(reused));
        assert_eq!(selection.ticket.unwrap().identity, reused);
    }
}
