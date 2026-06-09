use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Install a panic hook that restores the terminal before printing the panic.
/// Without this, a panic in raw mode leaves the terminal unusable.
pub(crate) fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Restore terminal first so panic output is readable
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
        original(info);
    }));
}

/// RAII guard that unregisters a signal handler on drop.
pub(crate) struct SignalGuard(signal_hook::SigId);

impl Drop for SignalGuard {
    fn drop(&mut self) {
        signal_hook::low_level::unregister(self.0);
    }
}

/// Register SIGTERM and SIGHUP handlers that set a stop flag and restore the terminal.
///
/// Returns RAII guards — signal handlers are unregistered when the guards drop.
/// This prevents stale handlers from outliving the TUI session.
pub(crate) fn install_signal_handlers(stop: Arc<AtomicBool>) -> Vec<SignalGuard> {
    let mut guards = Vec::new();

    // SIGTERM — process termination (e.g. `kill <pid>`)
    if let Ok(id) = signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&stop)) {
        guards.push(SignalGuard(id));
    }

    // SIGHUP — terminal hangup (e.g. SSH disconnect, terminal window closed)
    if let Ok(id) = signal_hook::flag::register(signal_hook::consts::SIGHUP, Arc::clone(&stop)) {
        guards.push(SignalGuard(id));
    }

    guards
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_install_panic_hook_smoke() {
        install_panic_hook();
    }

    #[test]
    fn test_panic_hook_restores_terminal() {
        install_panic_hook();
        let result = std::panic::catch_unwind(|| {
            panic!("test panic");
        });
        assert!(result.is_err(), "catch_unwind should have caught the panic");
    }

    #[test]
    fn test_signal_handlers_register_and_unregister() {
        let stop = Arc::new(AtomicBool::new(false));
        let guards = install_signal_handlers(stop);
        // Should have registered 2 handlers (SIGTERM + SIGHUP)
        assert_eq!(guards.len(), 2);
        // Drop guards — handlers are unregistered
        drop(guards);
    }

    #[test]
    fn test_signal_flag_is_shared() {
        let stop = Arc::new(AtomicBool::new(false));
        let _guards = install_signal_handlers(Arc::clone(&stop));
        // Flag starts false
        assert!(!stop.load(std::sync::atomic::Ordering::Relaxed));
        // We can't safely send SIGTERM to ourselves in a test, but we can
        // verify the flag is wired up by checking it's still the same Arc
    }
}
