//! Keep the controller's command receiver alive across recoverable native worker exits.
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::mpsc::Receiver,
};

pub(super) fn supervise<C>(
    commands: Receiver<C>,
    mut stopped: impl FnMut() -> bool,
    mut run: impl FnMut(&Receiver<C>) -> Result<(), String>,
    mut recover: impl FnMut(&str, &Receiver<C>),
) {
    while !stopped() {
        let outcome = catch_unwind(AssertUnwindSafe(|| run(&commands)));
        if stopped() {
            break;
        }
        let cause = match outcome {
            Ok(Err(error)) => error,
            Ok(Ok(())) => "native worker exited without shutdown".into(),
            Err(payload) => format!(
                "native worker panicked: {}",
                payload
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| payload.downcast_ref::<&str>().copied())
                    .unwrap_or("non-string panic payload")
            ),
        };
        recover(&cause, &commands);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        sync::mpsc,
    };

    #[test]
    fn error_and_panic_do_not_close_commands_or_hide_the_root_cause() {
        let (tx, rx) = mpsc::channel();
        let calls = Cell::new(0);
        let stopped = Cell::new(false);
        let causes = RefCell::new(Vec::new());
        supervise(
            rx,
            || stopped.get(),
            |commands| {
                let call = calls.get() + 1;
                calls.set(call);
                match call {
                    1 => Err("publishing texture: surface disappeared".into()),
                    2 => panic!("native callback failed"),
                    _ => {
                        assert_eq!(commands.recv().unwrap(), "shutdown");
                        stopped.set(true);
                        Ok(())
                    }
                }
            },
            |cause, commands| {
                causes.borrow_mut().push(cause.to_owned());
                tx.send("release input").unwrap();
                assert_eq!(commands.recv().unwrap(), "release input");
                if calls.get() == 2 {
                    tx.send("shutdown").unwrap();
                }
            },
        );
        assert_eq!(calls.get(), 3);
        assert_eq!(
            *causes.borrow(),
            [
                "publishing texture: surface disappeared",
                "native worker panicked: native callback failed"
            ]
        );
    }

    #[test]
    fn normal_shutdown_never_restarts() {
        let (_tx, rx) = mpsc::channel::<()>();
        let stopped = Cell::new(false);
        supervise(
            rx,
            || stopped.get(),
            |_| {
                stopped.set(true);
                Ok(())
            },
            |_, _| panic!("shutdown restarted"),
        );
    }
}
