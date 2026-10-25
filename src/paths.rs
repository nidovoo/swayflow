//! Locations of the state, lock and pid files, and of the overview config file.
//!
//! Runtime files live in `$XDG_RUNTIME_DIR`; if it is unset, `/tmp` is used and
//! the file names get a `-<uid>` suffix.

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::sys;

pub struct Paths {
    pub state: PathBuf,
    pub state_tmp: PathBuf,
    pub lock: PathBuf,
    pub daemon_lock: PathBuf,
    pub pid: PathBuf,
}

impl Paths {
    fn from_env() -> Self {
        let runtime = env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty());
        let suffix = if runtime.is_some() {
            String::new()
        } else {
            format!("-{}", sys::getuid())
        };
        let dir = runtime.map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
        let file = |name: &str, ext: &str| dir.join(format!("{name}{suffix}.{ext}"));

        let state = file("swayflow", "json");
        let mut state_tmp = OsString::from(state.as_os_str());
        state_tmp.push(".tmp");
        Paths {
            lock: file("swayflow", "lock"),
            daemon_lock: file("swayflow-daemon", "lock"),
            pid: file("swayflow-overview", "pid"),
            state_tmp: PathBuf::from(state_tmp),
            state,
        }
    }
}

pub fn paths() -> &'static Paths {
    static PATHS: OnceLock<Paths> = OnceLock::new();
    PATHS.get_or_init(Paths::from_env)
}

/// `$XDG_CONFIG_HOME/swayflow/overview.conf` (`~/.config/swayflow/overview.conf`
/// when `XDG_CONFIG_HOME` is unset), if a base directory is known.
pub fn overview_config() -> Option<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(base.join("swayflow").join("overview.conf"))
}
