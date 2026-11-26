//! Uninstall the running, explicitly confirmed copy of Milo, not the first Milo on PATH.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, Metadata};
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::json::Value;
use crate::read::{self, Content};

const TAP: &str = "mcc0x/milo";
const FORMULA: &str = "mcc0x/milo/milo";

pub(crate) fn run() -> Result<String, String> {
    let launched = env::current_exe().map_err(|e| format!("Could not locate the running Milo: {e}"))?;
    let path = launched.canonicalize().map_err(|_| unsupported(&launched, "could not locate the installed file"))?;
    let id = running_id().ok_or_else(|| unsupported(&path, "could not verify the running executable"))?;
    let home = crate::paths::home();
    let custom = env::var_os("MILO_BIN_DIR").filter(|dir| !dir.is_empty());
    let install = Installation::find(&path, id, home.as_deref(), custom.as_deref(), Path::new("/opt/homebrew/bin"))?;

    let settings = crate::setup::doctor::remove().map_err(|e| format!("{e}\nThe Milo executable was not removed."))?;
    let removed = install.remove(&path, id).map_err(|e| format!("{settings}\n{e}"))?;
    // The helper would go on showing the activity from the removed program, and a later install would start with presence on.
    crate::presence::turn_off();
    Ok(format!("{removed}\n{settings}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileId(u64, u64);

impl FileId {
    fn of(meta: &Metadata) -> Self {
        let device = meta.dev();
        #[cfg(target_os = "macos")]
        let device = device & u64::from(u32::MAX); // Match the kernel's unsigned vnode device number.
        Self(device, meta.ino())
    }

    fn matches(self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file() && self == Self::of(&meta))
    }
}

#[cfg(target_os = "macos")]
fn running_id() -> Option<FileId> {
    // Darwin's current_exe() gives a path, which may now name a different file.
    // Ask the kernel for the vnode backing the running executable's Mach-O header.
    const PROC_PIDREGIONPATHINFO: libc::c_int = 8;
    #[repr(C)]
    struct RegionPath {
        region: [u64; 12], // The unused 96-byte proc_regioninfo from sys/proc_info.h.
        vnode: libc::vnode_info_path,
    }

    let mut region = std::mem::MaybeUninit::<RegionPath>::uninit();
    let size = libc::c_int::try_from(std::mem::size_of::<RegionPath>()).ok()?;
    // The Mach-O linker defines this symbol at the main executable's load address.
    unsafe extern "C" {
        static _mh_execute_header: u8;
    }
    let address = (&raw const _mh_execute_header) as u64;
    let pid = libc::pid_t::try_from(std::process::id()).ok()?;
    // SAFETY: The buffer has the documented size and alignment for this flavor of proc_pidinfo.
    let read = unsafe { libc::proc_pidinfo(pid, PROC_PIDREGIONPATHINFO, address, region.as_mut_ptr().cast(), size) };
    if read != size {
        return None;
    }
    // SAFETY: proc_pidinfo copied the entire C struct; its fields are all integers or integer arrays.
    let stat = unsafe { region.assume_init() }.vnode.vip_vi.vi_stat;
    (stat.vst_ino != 0).then_some(FileId(u64::from(stat.vst_dev), stat.vst_ino))
}

#[cfg(target_os = "linux")]
fn running_id() -> Option<FileId> {
    fs::metadata("/proc/self/exe").ok().map(|meta| FileId::of(&meta))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn running_id() -> Option<FileId> {
    None
}

fn unsupported(path: &Path, reason: &str) -> String {
    format!(
        "Milo will not remove {}: {reason}. Relaunch the installed copy (set MILO_BIN_DIR to the install directory for a custom install), or use `milo setup --remove` and the original installer or package manager.",
        path.display()
    )
}

fn checkout_product(path: &Path) -> bool {
    path.ancestors().skip(1).any(|dir| {
        dir.file_name().is_some_and(|name| name == "target")
            || dir.join("CACHEDIR.TAG").exists()
            || (dir.join("src/main.rs").is_file()
                && matches!(read::file(&dir.join("Cargo.toml"), 64 * 1024), Content::Bytes(bytes)
                    if bytes.split(|&b| b == b'\n').any(|line| line.trim_ascii() == b"name = \"milo\"")))
    })
}

// The script installers support an absolute or working-directory-relative MILO_BIN_DIR.
fn in_directory(path: &Path, dir: &Path) -> bool {
    dir.canonicalize().is_ok_and(|dir| path.parent() == Some(dir.as_path()))
}

enum Installation {
    Standalone,
    Homebrew(Brew),
}

impl Installation {
    fn find(
        path: &Path,
        id: FileId,
        home: Option<&Path>,
        custom: Option<&OsStr>,
        homebrew_bin: &Path,
    ) -> Result<Self, String> {
        if path.file_name() != Some(OsStr::new("milo")) || !id.matches(path) {
            return Err(unsupported(path, "the file is not the running Milo binary"));
        }
        if checkout_product(path) {
            return Err(unsupported(path, "this is a checkout or build product, not an installed copy"));
        }
        if let Some(keg) = Brew::keg_for(path) {
            return Brew::find(path, keg, homebrew_bin).map(Self::Homebrew);
        }
        // Never interpret an unexpected package-manager layout as a custom standalone install.
        if path.ancestors().filter_map(Path::file_name).any(|name| name == "Cellar" || name == "Caskroom")
            || path.parent().and_then(Path::parent).is_some_and(|dir| dir.join("INSTALL_RECEIPT.json").exists())
        {
            return Err(unsupported(path, "this package-manager installation is not supported"));
        }
        if in_directory(path, homebrew_bin)
            || home.is_some_and(|home| in_directory(path, &home.join(".local/bin")))
            || custom.is_some_and(|dir| in_directory(path, Path::new(dir)))
        {
            Ok(Self::Standalone)
        } else {
            Err(unsupported(path, "this is not a recognized installation path"))
        }
    }

    fn remove(self, path: &Path, id: FileId) -> Result<String, String> {
        if !path.canonicalize().is_ok_and(|current| current == path) || !id.matches(path) {
            return Err(unsupported(path, "the installed file changed while removing integrations"));
        }
        match self {
            Self::Standalone => {
                fs::remove_file(path).map_err(|e| format!("Could not remove {}: {e}", path.display()))?;
                Ok(format!("Removed {}.", path.display()))
            }
            Self::Homebrew(brew) => brew.remove(path),
        }
    }
}

struct Brew {
    executable: PathBuf,
    opt: PathBuf,
}

impl Brew {
    fn keg_for(path: &Path) -> Option<&Path> {
        let bin = path.parent()?;
        let keg = bin.parent()?;
        let rack = keg.parent()?;
        let cellar = rack.parent()?;
        (bin.file_name()? == "bin" && rack.file_name()? == "milo" && cellar.file_name()? == "Cellar").then_some(keg)
    }

    fn official(keg: &Path) -> bool {
        let Content::Bytes(bytes) = read::file(&keg.join("INSTALL_RECEIPT.json"), 64 * 1024) else { return false };
        Value::parse(&bytes)
            .and_then(|tab| tab.fields(["source"])[0])
            .and_then(|source| source.fields(["tap"])[0])
            .is_some_and(|tap| tap.is(TAP))
    }

    fn find(path: &Path, keg: &Path, homebrew_bin: &Path) -> Result<Self, String> {
        // A receipt describes a keg, not a trustworthy program to execute.
        let cellar = homebrew_bin.parent().and_then(|prefix| prefix.join("Cellar").canonicalize().ok());
        if cellar.is_none() || cellar.as_deref() != keg.ancestors().nth(2) {
            return Err(unsupported(path, "this is not the configured Homebrew Cellar; use your package manager"));
        }
        if !Self::official(keg) {
            return Err(unsupported(path, "the Homebrew receipt does not identify the official Milo formula"));
        }
        let executable = homebrew_bin.join("brew");
        let output = brew_command(&executable).args(["--prefix", "--installed", FORMULA]).output().map_err(|e| {
            unsupported(
                path,
                &format!("could not verify the Homebrew installation with {} ({e})", executable.display()),
            )
        })?;
        if !output.status.success() {
            return Err(unsupported(path, &format!("Homebrew could not verify {FORMULA} ({})", brew_error(&output))));
        }
        let mut prefix = output.stdout;
        if prefix.last() == Some(&b'\n') {
            prefix.pop();
        }
        let opt = PathBuf::from(OsString::from_vec(prefix));
        let brew = Self { executable, opt };
        if !brew.is_current(keg) {
            return Err(unsupported(path, "this is not the active Homebrew keg; relaunch the current Homebrew Milo"));
        }
        Ok(brew)
    }

    fn is_current(&self, keg: &Path) -> bool {
        self.opt.is_absolute()
            && self.opt.ends_with("opt/milo")
            && fs::symlink_metadata(&self.opt).is_ok_and(|m| m.is_symlink())
            && self.opt.canonicalize().is_ok_and(|current| current == keg)
    }

    fn remove(self, path: &Path) -> Result<String, String> {
        let Some(keg) = Self::keg_for(path) else {
            return Err(unsupported(path, "the Homebrew keg path changed"));
        };
        if !Self::official(keg) || !self.is_current(keg) {
            return Err(unsupported(path, "the Homebrew keg changed while removing integrations"));
        }
        let output = brew_command(&self.executable).args(["uninstall", "--formula", FORMULA]).output();
        let reason = match output {
            Ok(output) if !output.status.success() => brew_error(&output),
            Ok(_) if matches!(fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound) => {
                return Ok("Removed Milo with Homebrew.".into());
            }
            Ok(_) => "Homebrew finished but removal of the running binary could not be verified".into(),
            Err(e) => e.to_string(),
        };
        Err(format!(
            "Could not uninstall with Homebrew: {reason}. After checking the active keg, retry `brew uninstall --formula {FORMULA}`."
        ))
    }
}

fn brew_command(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command.env("HOMEBREW_NO_AUTO_UPDATE", "1").env("HOMEBREW_NO_AUTOREMOVE", "1");
    command
}

fn brew_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.lines().find(|line| !line.trim().is_empty()).map_or_else(|| output.status.to_string(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;
    use crate::fixture::Fixture;

    fn id(path: &Path) -> FileId {
        FileId::of(&fs::metadata(path).unwrap())
    }

    fn standalone(path: &Path, id: FileId, home: &Path, custom: Option<&Path>) -> Result<Installation, String> {
        Installation::find(path, id, Some(home), custom.map(Path::as_os_str), &home.join("opt/homebrew/bin"))
    }

    #[test]
    fn only_the_running_standalone_copy_is_removed() {
        let f = Fixture::folder();
        for copy in [".local/bin/milo", "custom bin/milo", "opt/homebrew/bin/milo"] {
            f.write(copy, copy);
        }
        f.write(".local/bin/other", "untouched");
        let path = f.root.join(".local/bin/milo");
        let custom = f.root.join("custom bin");
        let running = id(&path);
        assert!(standalone(&custom.join("milo"), running, &f.root, Some(&custom)).is_err());
        let install = standalone(&path, running, &f.root, Some(&custom)).unwrap();
        install.remove(&path, running).unwrap();
        assert!(!path.exists());
        assert!(custom.join("milo").exists());
        assert!(f.root.join(".local/bin/other").exists());
        let path = f.root.join("opt/homebrew/bin/milo");
        let running = id(&path);
        let install = standalone(&path, running, &f.root, None).unwrap();
        assert!(matches!(install, Installation::Standalone));
        install.remove(&path, running).unwrap();
        assert!(!path.exists());
        let path = custom.join("milo");
        symlink("custom bin", f.root.join("linked bin")).unwrap();
        assert!(standalone(&path, id(&path), &f.root, None).is_err());
        assert!(matches!(
            standalone(&path, id(&path), &f.root, Some(&f.root.join("linked bin"))).unwrap(),
            Installation::Standalone
        ));
    }

    #[test]
    fn checkout_outputs_and_linked_or_replaced_files_are_not_removed() {
        let f = Fixture::folder();
        f.write("target/release/milo", "standard build");
        f.write("cache/CACHEDIR.TAG", "cargo artifact root");
        f.write("cache/release/milo", "custom build");
        f.write("checkout/Cargo.toml", "[package]\nname = \"milo\"\n");
        f.write("checkout/src/main.rs", "fn main() {}\n");
        f.write("checkout/out/milo", "checkout product");
        for file in ["target/release/milo", "cache/release/milo", "checkout/out/milo"] {
            let path = f.root.join(file);
            let result = standalone(&path, id(&path), &f.root, path.parent());
            assert!(matches!(result, Err(e) if e.contains("checkout or build product")));
        }
        let path = f.root.join(".local/bin/milo");
        f.write(".local/bin/milo", "original");
        let running = id(&path);
        let install = standalone(&path, running, &f.root, None).unwrap();
        fs::rename(&path, path.with_extension("old")).unwrap();
        f.write(".local/bin/milo", "replacement");
        assert!(install.remove(&path, running).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "replacement");
        fs::remove_file(&path).unwrap();
        symlink("milo.old", &path).unwrap();
        assert!(standalone(&path, running, &f.root, None).is_err());
        assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
    }

    fn brew_fixture() -> (Fixture, PathBuf) {
        let f = Fixture::folder();
        f.write("brew prefix/Cellar/milo/1/bin/milo", "running");
        f.write("brew prefix/Cellar/milo/1/INSTALL_RECEIPT.json", r#"{"source":{"tap":"mcc0x/milo"}}"#);
        f.write("brew prefix/Cellar/milo/2/bin/milo", "other version");
        f.write(
            "brew prefix/bin/brew",
            r#"#!/bin/sh
set -eu
prefix=${0%/bin/brew}
[ "$HOMEBREW_NO_AUTO_UPDATE" = 1 ]
[ "$HOMEBREW_NO_AUTOREMOVE" = 1 ]
[ "$#" = 3 ]
[ "$3" = mcc0x/milo/milo ]
case "$1:$2" in
    --prefix:--installed) printf '%s/opt/milo\n' "$prefix" ;;
    uninstall:--formula) /bin/rm -- "$prefix/Cellar/milo/1/bin/milo" ;;
    *) exit 1 ;;
esac
"#,
        );
        let prefix = f.root.join("brew prefix");
        fs::set_permissions(prefix.join("bin/brew"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(prefix.join("opt")).unwrap();
        symlink("../Cellar/milo/1", prefix.join("opt/milo")).unwrap();
        (f, prefix)
    }

    #[test]
    fn only_the_current_official_formula_is_uninstalled_via_its_brew() {
        let (f, prefix) = brew_fixture();
        let path = prefix.join("Cellar/milo/1/bin/milo");
        let running = id(&path);
        assert!(
            matches!(standalone(&path, running, &f.root, None), Err(e) if e.contains("configured Homebrew Cellar"))
        );
        let install = Installation::find(&path, running, Some(&f.root), None, &prefix.join("bin")).unwrap();
        assert!(matches!(install, Installation::Homebrew(_)));
        install.remove(&path, running).unwrap();
        assert!(!path.exists());
        assert!(prefix.join("Cellar/milo/2/bin/milo").exists());
    }

    #[test]
    fn foreign_and_inactive_kegs_do_not_fall_back_to_standalone() {
        let (f, prefix) = brew_fixture();
        let path = prefix.join("Cellar/milo/1/bin/milo");
        let running = id(&path);
        let classify = || {
            Installation::find(&path, running, Some(&f.root), path.parent().map(Path::as_os_str), &prefix.join("bin"))
        };
        let install = classify().unwrap();
        fs::remove_file(prefix.join("opt/milo")).unwrap();
        symlink("../Cellar/milo/2", prefix.join("opt/milo")).unwrap();
        assert!(install.remove(&path, running).is_err());
        assert!(matches!(classify(), Err(e) if e.contains("not the active")));
        symlink("../Cellar/milo/1", prefix.join("opt/temp")).unwrap();
        fs::rename(prefix.join("opt/temp"), prefix.join("opt/milo")).unwrap();
        f.write("brew prefix/Cellar/milo/1/INSTALL_RECEIPT.json", r#"{"source":{"tap":"external/milo"}}"#);
        assert!(matches!(classify(), Err(e) if e.contains("receipt")));
        assert!(path.exists());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn running_identity_matches_the_actual_executable() {
        let exe = env::current_exe().unwrap().canonicalize().unwrap();
        assert!(running_id().is_some_and(|running| running.matches(&exe)));
    }
}
