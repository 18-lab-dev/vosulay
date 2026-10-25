//! Context-switch policy shared by the injected hook and its platform-neutral tests.

/// Lookup and publication own their guards only for their call. Setup may dispatch
/// a synchronous native window callback that needs the same registry.
pub fn get_or_create<T, E>(
    lookup: impl FnOnce() -> Option<T>,
    setup: impl FnOnce() -> Result<T, E>,
    publish: impl FnOnce(T) -> T,
) -> Result<T, E> {
    if let Some(value) = lookup() {
        return Ok(value);
    }
    let value = setup()?;
    Ok(publish(value))
}

/// A callback must be able to find its forwarding target even when the native
/// installation call itself synchronously invokes that callback.
pub fn install_callback(publish: impl FnOnce() -> bool, install: impl FnOnce()) {
    if publish() {
        install();
    }
}

/// An absent, disconnected, busy or failed overlay never suppresses the game swap.
pub fn present<R>(overlay: impl FnOnce(), original: impl FnOnce() -> R) -> R {
    overlay();
    original()
}
#[derive(Debug, PartialEq, Eq)]
pub enum BindFailure {
    Owner,
    Restore,
}

/// Never run GL destruction after a failed bind; restore even when it was unnecessary
/// to change the owner. Callers retain resources until this closure actually runs.
pub fn with_owner_context(
    owner: (usize, usize),
    current: impl Fn() -> (usize, usize),
    mut bind: impl FnMut((usize, usize)) -> bool,
    destroy: impl FnOnce(),
) -> Result<(), BindFailure> {
    let previous = current();
    if previous != owner && (!bind(owner) || current() != owner) {
        // A failed driver bind must not leave the caller on an unexpected context.
        if current() != previous {
            let _ = bind(previous);
        }
        return Err(BindFailure::Owner);
    }
    destroy();
    if current() != previous && (!bind(previous) || current() != previous) {
        return Err(BindFailure::Restore);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn deletion_runs_only_in_owner_and_restores_the_callers_context() {
        let current = Cell::new((1, 11));
        let calls = RefCell::new(Vec::new());
        assert_eq!(
            with_owner_context(
                (2, 22),
                || current.get(),
                |next| {
                    calls.borrow_mut().push(next);
                    current.set(next);
                    true
                },
                || assert_eq!(current.get(), (2, 22))
            ),
            Ok(())
        );
        assert_eq!(current.get(), (1, 11));
        assert_eq!(*calls.borrow(), [(2, 22), (1, 11)]);
    }

    #[test]
    fn failed_or_ineffective_bind_never_destroys_foreign_objects() {
        for reported_success in [false, true] {
            let destroyed = Cell::new(false);
            assert_eq!(
                with_owner_context(
                    (2, 22),
                    || (1, 11),
                    |_| reported_success,
                    || destroyed.set(true)
                ),
                Err(BindFailure::Owner)
            );
            assert!(!destroyed.get());
        }
    }

    #[test]
    fn current_owner_needs_no_switch_and_restore_failure_is_reported() {
        assert_eq!(
            with_owner_context((2, 22), || (2, 22), |_| panic!("unnecessary switch"), || {}),
            Ok(())
        );
        let current = Cell::new((1, 11));
        assert_eq!(
            with_owner_context(
                (2, 22),
                || current.get(),
                |next| {
                    if next == (1, 11) {
                        return false;
                    }
                    current.set(next);
                    true
                },
                || {}
            ),
            Err(BindFailure::Restore)
        );
    }
}

#[cfg(test)]
mod reentrancy_tests {
    use super::*;
    use std::{cell::Cell, collections::HashMap, sync::Mutex};

    #[test]
    fn native_install_callback_sees_published_forwarding_target_without_a_lock() {
        let forwarding = Mutex::new(None);
        let swaps = Cell::new(0);
        present(
            || {
                install_callback(
                    || {
                        *forwarding.lock().unwrap() = Some(123);
                        true
                    },
                    || assert_eq!(*forwarding.try_lock().unwrap(), Some(123)),
                )
            },
            || swaps.set(swaps.get() + 1),
        );
        install_callback(|| false, || panic!("duplicate installation"));
        assert_eq!(swaps.get(), 1);
    }

    #[test]
    fn synchronous_setup_callback_reenters_registry_and_original_present_runs() {
        let registry = Mutex::new(HashMap::<usize, usize>::new());
        let swaps = Cell::new(0);
        for key in [11, 22] {
            // two contexts on the same game DC
            let result = present(
                || {
                    let value = get_or_create(
                        || registry.lock().unwrap().get(&key).copied(),
                        || {
                            // Synthetic WM_WINDOWPOSCHANGED lookup during native setup:
                            // try_lock turns a reentrant-lock regression into a failure,
                            // not a test process that hangs forever.
                            assert!(registry.try_lock().unwrap().get(&key).is_none());
                            Ok::<_, ()>(key)
                        },
                        |value| *registry.lock().unwrap().entry(key).or_insert(value),
                    )
                    .unwrap();
                    assert_eq!(value, key);
                },
                || {
                    swaps.set(swaps.get() + 1);
                    true
                },
            );
            assert!(result);
        }
        assert_eq!(swaps.get(), 2);
        assert_eq!(registry.lock().unwrap().len(), 2);
        present(
            || {
                assert!(
                    get_or_create(|| None::<usize>, || Err::<usize, _>(()), |_| unreachable!())
                        .is_err()
                );
            },
            || swaps.set(swaps.get() + 1),
        );
        assert_eq!(swaps.get(), 3);
    }
}
