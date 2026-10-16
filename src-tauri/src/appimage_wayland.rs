//! Works around the outdated `libwayland-client.so.0` bundled into our AppImage.
//!
//! linuxdeploy copies `libwayland-client.so.0` into the AppDir because the
//! excludelist compiled into the version Tauri ships predates the entry for it.
//! The copy comes from the `ubuntu-22.04` build runner (1.20.0, Sep 2022) and is
//! missing symbols that current Mesa needs (`wl_fixes_interface`,
//! `wl_display_create_queue_with_name`, `wl_display_dispatch_queue_timeout`), so
//! `libEGL_mesa.so.0` fails to load. With the vendor library gone, *every* EGL
//! platform fails — not just Wayland — which is why `GDK_BACKEND=x11` and
//! `LIBGL_ALWAYS_SOFTWARE` don't help: they are read after EGL is already dead.
//! WebKit then logs `Could not create default EGL display: EGL_BAD_PARAMETER`
//! and either renders nothing or aborts.
//!
//! `libwayland-client.so.0` has to be resolved before `main()` runs, so setting
//! an environment variable in-process is too late. Instead we re-exec once with
//! the system copy in `LD_PRELOAD`: preloaded objects are loaded first, and the
//! later `DT_NEEDED` reference to that SONAME resolves to the already-loaded
//! object instead of searching the AppDir.
//!
//! Set `ILOADER_NO_WAYLAND_PRELOAD=1` to skip this entirely.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// Name we need to shadow, and the one we look for on the system.
pub const SONAME: &str = "libwayland-client.so.0";

/// Set on the re-executed process so we only ever do this once.
pub const GUARD_VAR: &str = "ILOADER_WAYLAND_PRELOAD";

/// Opt out.
pub const DISABLE_VAR: &str = "ILOADER_NO_WAYLAND_PRELOAD";

/// `ldconfig -p` tags the architecture of each entry. Map Rust's arch name onto
/// the tag so a 64-bit build doesn't pick up a 32-bit library on a multiarch
/// system. Returns `None` for architectures we don't have a mapping for, in
/// which case every candidate is treated equally.
pub fn arch_tag(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("x86-64"),
        "x86" => Some("i386"),
        "aarch64" => Some("AArch64"),
        _ => None,
    }
}

/// Pick the system copy of [`SONAME`] out of `ldconfig -p` output.
///
/// Anything inside `appdir` is skipped — that's the bundled copy we're trying to
/// get away from. Entries matching `arch_tag` are preferred so a 64-bit process
/// doesn't select a 32-bit library, but a non-matching entry is still returned
/// rather than giving up, since the tag format varies between distributions.
pub fn pick_system_lib(
    ldconfig_output: &str,
    appdir: Option<&Path>,
    arch_tag: Option<&str>,
) -> Option<PathBuf> {
    let mut preferred: Option<PathBuf> = None;
    let mut fallback: Option<PathBuf> = None;

    for line in ldconfig_output.lines() {
        let Some((head, path)) = line.split_once("=>") else {
            continue;
        };
        let path = Path::new(path.trim());

        if path.file_name().and_then(|n| n.to_str()) != Some(SONAME) {
            continue;
        }
        if appdir.is_some_and(|dir| path.starts_with(dir)) {
            continue;
        }

        let matches_arch = arch_tag.is_some_and(|tag| head.contains(tag));
        if matches_arch {
            if preferred.is_none() {
                preferred = Some(path.to_path_buf());
            }
        } else if fallback.is_none() {
            fallback = Some(path.to_path_buf());
        }
    }

    preferred.or(fallback)
}

/// Build the new `LD_PRELOAD`, keeping anything the user already set.
pub fn prepend_preload(existing: Option<&str>, lib: &Path) -> String {
    let lib = lib.to_string_lossy();
    match existing.map(str::trim).filter(|v| !v.is_empty()) {
        Some(current) => format!("{lib}:{current}"),
        None => lib.into_owned(),
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::env;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    /// Directories to check when `ldconfig` isn't available (musl systems, or a
    /// trimmed container).
    const FALLBACK_DIRS: &[&str] = &[
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
        "/lib/x86_64-linux-gnu",
        "/lib/aarch64-linux-gnu",
        "/lib64",
        "/lib",
    ];

    fn system_lib(appdir: &Path) -> Option<PathBuf> {
        let from_ldconfig = Command::new("ldconfig")
            .arg("-p")
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| {
                let text = String::from_utf8_lossy(&out.stdout).into_owned();
                pick_system_lib(&text, Some(appdir), arch_tag(env::consts::ARCH))
            })
            .filter(|path| path.exists());

        from_ldconfig.or_else(|| {
            FALLBACK_DIRS
                .iter()
                .map(|dir| Path::new(dir).join(SONAME))
                .find(|path| !path.starts_with(appdir) && path.exists())
        })
    }

    /// Re-exec with the system `libwayland-client.so.0` preloaded, if we're an
    /// AppImage that bundles its own. Returns normally when there's nothing to
    /// do; on success it does not return at all.
    pub fn preload_system_wayland_client() {
        if env::var_os(GUARD_VAR).is_some() || env::var_os(DISABLE_VAR).is_some() {
            return;
        }

        // APPDIR is set by AppRun; outside an AppImage there's nothing to shadow.
        let Some(appdir) = env::var_os("APPDIR").map(PathBuf::from) else {
            return;
        };
        if !appdir.join("usr/lib").join(SONAME).exists() {
            return;
        }

        let Some(system_lib) = system_lib(&appdir) else {
            // No system copy to fall back on. Carry on and let WebKit try, which
            // is no worse than today.
            return;
        };

        let preload = prepend_preload(env::var("LD_PRELOAD").ok().as_deref(), &system_lib);

        let Ok(exe) = env::current_exe() else {
            return;
        };

        let error = Command::new(exe)
            .args(env::args_os().skip(1))
            .env(GUARD_VAR, "1")
            .env("LD_PRELOAD", preload)
            .exec();

        // exec() only returns on failure. Keep going unpreloaded rather than
        // taking the app down over a workaround.
        eprintln!("iloader: could not re-exec with {SONAME} preloaded: {error}");
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    pub fn preload_system_wayland_client() {}
}

pub use imp::preload_system_wayland_client;

#[cfg(test)]
mod tests {
    use super::*;

    const LDCONFIG: &str = "\t\
libwayland-cursor.so.0 (libc6,x86-64) => /lib/x86_64-linux-gnu/libwayland-cursor.so.0
\tlibwayland-client.so.0 (libc6) => /lib/i386-linux-gnu/libwayland-client.so.0
\tlibwayland-client.so.0 (libc6,x86-64) => /lib/x86_64-linux-gnu/libwayland-client.so.0
\tlibwayland-egl.so.1 (libc6,x86-64) => /lib/x86_64-linux-gnu/libwayland-egl.so.1";

    #[test]
    fn prefers_the_entry_matching_the_process_architecture() {
        let found = pick_system_lib(LDCONFIG, None, Some("x86-64"));
        assert_eq!(
            found,
            Some(PathBuf::from(
                "/lib/x86_64-linux-gnu/libwayland-client.so.0"
            ))
        );
    }

    #[test]
    fn falls_back_when_no_entry_matches_the_architecture() {
        let found = pick_system_lib(LDCONFIG, None, Some("riscv64"));
        assert_eq!(
            found,
            Some(PathBuf::from("/lib/i386-linux-gnu/libwayland-client.so.0"))
        );
    }

    #[test]
    fn never_selects_the_bundled_copy() {
        let bundled = "\tlibwayland-client.so.0 (libc6,x86-64) => /tmp/.mount_iloader/usr/lib/libwayland-client.so.0";
        let appdir = PathBuf::from("/tmp/.mount_iloader");

        assert_eq!(
            pick_system_lib(bundled, Some(&appdir), Some("x86-64")),
            None
        );

        let both = format!("{bundled}\n{LDCONFIG}");
        assert_eq!(
            pick_system_lib(&both, Some(&appdir), Some("x86-64")),
            Some(PathBuf::from(
                "/lib/x86_64-linux-gnu/libwayland-client.so.0"
            ))
        );
    }

    #[test]
    fn ignores_other_wayland_libraries() {
        let others =
            "\tlibwayland-egl.so.1 (libc6,x86-64) => /lib/x86_64-linux-gnu/libwayland-egl.so.1";
        assert_eq!(pick_system_lib(others, None, Some("x86-64")), None);
    }

    #[test]
    fn keeps_an_existing_ld_preload() {
        let lib = Path::new("/lib/x86_64-linux-gnu/libwayland-client.so.0");
        assert_eq!(
            prepend_preload(Some("/opt/thing.so"), lib),
            "/lib/x86_64-linux-gnu/libwayland-client.so.0:/opt/thing.so"
        );
        assert_eq!(
            prepend_preload(None, lib),
            "/lib/x86_64-linux-gnu/libwayland-client.so.0"
        );
        assert_eq!(
            prepend_preload(Some("   "), lib),
            "/lib/x86_64-linux-gnu/libwayland-client.so.0"
        );
    }

    #[test]
    fn maps_known_architectures_only() {
        assert_eq!(arch_tag("x86_64"), Some("x86-64"));
        assert_eq!(arch_tag("aarch64"), Some("AArch64"));
        assert_eq!(arch_tag("powerpc64"), None);
    }
}
