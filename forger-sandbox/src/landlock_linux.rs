//! Landlock probe + child confinement. Applied only in the spawned command,
//! never on the Forger process itself.

use crate::risks::LandlockAbi;
use tracing::warn;

pub fn probe_abi() -> LandlockAbi {
    let sys = landlock_create_ruleset_sysno();
    const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
    let ret = unsafe {
        libc::syscall(
            sys,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if ret < 0 {
        return LandlockAbi::Unavailable;
    }
    LandlockAbi::Version(ret as u32)
}

fn landlock_create_ruleset_sysno() -> libc::c_long {
    #[cfg(target_arch = "x86_64")]
    {
        444
    }
    #[cfg(target_arch = "aarch64")]
    {
        445
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        444
    }
}

/// Build and enforce a ruleset in the *current* process. Intended for
/// `CommandExt::pre_exec` in the child only.
pub fn restrict_self(workspace: &std::path::Path) -> Result<(), String> {
    use landlock::{
        path_beneath_rules, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, Scope, ABI,
    };

    let abi = match probe_abi() {
        LandlockAbi::Version(v) if v >= 6 => ABI::V6,
        LandlockAbi::Version(v) if v >= 1 => ABI::V1,
        _ => {
            warn!("Landlock restrict_self skipped: ABI unavailable");
            return Ok(());
        }
    };

    let ws = workspace
        .to_str()
        .ok_or_else(|| "workspace path is not UTF-8".to_string())?;

    let mut ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .map_err(|e| e.to_string())?;

    if matches!(probe_abi(), LandlockAbi::Version(v) if v >= 6) {
        ruleset = ruleset.scope(Scope::Signal).map_err(|e| e.to_string())?;
    }

    let ro: Vec<&str> = [
        "/usr", "/bin", "/sbin", "/lib", "/lib64", "/proc", "/dev", "/etc", "/run",
    ]
    .into_iter()
    .filter(|p| std::path::Path::new(p).exists())
    .collect();
    let status = ruleset
        .create()
        .map_err(|e| e.to_string())?
        .add_rules(path_beneath_rules(&ro, AccessFs::from_read(abi)))
        .map_err(|e| e.to_string())?
        .add_rules(path_beneath_rules(&[ws], AccessFs::from_all(abi)))
        .map_err(|e| e.to_string())?
        .restrict_self()
        .map_err(|e| e.to_string())?;

    if matches!(status.ruleset, landlock::RulesetStatus::NotEnforced) {
        warn!("Landlock ruleset was not enforced");
    }
    Ok(())
}
