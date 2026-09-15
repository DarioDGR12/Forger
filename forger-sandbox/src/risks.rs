//! Documented accepted risks. These are not silent.
//!
//! 1. **Rename via `write`/`rename` is closed.** Destination paths are
//!    canonicalized (`canonicalize(parent)` + file name) and
//!    [`crate::denylist::is_sensitive`] runs on that result, not only the
//!    originally requested name. `write("config")` then `rename(".env")`
//!    is refused. A `run_command` can still `mv` onto `.env` inside the
//!    shell; that remains an accepted gap (Landlock is a tree allowlist,
//!    not a filename denylist).
//!
//! 2. **Landlock SCOPE_SIGNAL (Linux < 6.12 / ABI < 6):** a process inside
//!    the sandbox can send `kill -9` to other processes of the same user,
//!    including Forger itself. On 6.12+ with Landlock V6 this is closed.
//!    [`signal_scope_warning`] must be logged whenever the protection is
//!    missing — never fail closed-but-quiet.

use tracing::warn;

const SIGNAL_SCOPE_MIN_ABI: u32 = 6;

const SIGNAL_SCOPE_WARNING: &str = "\
Landlock SCOPE_SIGNAL is NOT available on this kernel (need Landlock ABI 6+, Linux 6.12+). \
A process inside the sandbox can send kill -9 to other processes of the same user, \
including Forger itself. This is an accepted, documented risk — Forger will not fail \
silently, but it will not abort. Upgrade the kernel to close this hole.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandlockAbi {
    Unavailable,
    Version(u32),
}

pub fn probe_landlock_abi() -> LandlockAbi {
    #[cfg(not(target_os = "linux"))]
    {
        return LandlockAbi::Unavailable;
    }
    #[cfg(target_os = "linux")]
    {
        crate::landlock_linux::probe_abi()
    }
}

pub fn signal_scope_warning(abi: LandlockAbi) -> Option<String> {
    match abi {
        LandlockAbi::Version(v) if v >= SIGNAL_SCOPE_MIN_ABI => None,
        LandlockAbi::Version(v) => {
            let msg = format!("Landlock ABI {v} < {SIGNAL_SCOPE_MIN_ABI}. {SIGNAL_SCOPE_WARNING}");
            Some(msg)
        }
        LandlockAbi::Unavailable => {
            Some(format!("Landlock is unavailable. {SIGNAL_SCOPE_WARNING}"))
        }
    }
}

/// Call once at sandbox construction. Logs; never returns an error for the
/// missing-scope case (accepted risk).
pub fn warn_if_no_signal_scope() -> Option<String> {
    let abi = probe_landlock_abi();
    let warning = signal_scope_warning(abi);
    if let Some(ref w) = warning {
        warn!("{w}");
    }
    warning
}

#[cfg(test)]
mod tests {
    use super::{signal_scope_warning, LandlockAbi};

    #[test]
    fn abi6_is_silent() {
        assert!(signal_scope_warning(LandlockAbi::Version(6)).is_none());
        assert!(signal_scope_warning(LandlockAbi::Version(7)).is_none());
    }

    #[test]
    fn abi5_warns_and_mentions_kill() {
        let w = signal_scope_warning(LandlockAbi::Version(5)).unwrap();
        assert!(w.contains("kill -9"));
        assert!(w.contains("6.12"));
    }

    #[test]
    fn unavailable_warns() {
        let w = signal_scope_warning(LandlockAbi::Unavailable).unwrap();
        assert!(
            w.contains("unavailable")
                || w.contains("UNAVAILABLE")
                || w.contains("unavailable")
                || w.to_lowercase().contains("unavailable")
        );
        assert!(w.contains("kill -9"));
    }
}
