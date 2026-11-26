//! Windows form of [`super`]: resolves reader paths from a held project folder, one name at a time.
//!
//! Each name is opened with NtCreateFile relative to the folder handle above it, so renaming a
//! folder elsewhere cannot redirect a lookup, and with FILE_OPEN_REPARSE_POINT, so a symbolic
//! link, junction or mount point is opened as itself and refused rather than followed. Names that
//! Win32 would read as something else (a stream, a device, a name ending in a dot or space) are
//! refused before they reach the file system. UNC paths are refused.

use std::ffi::{OsStr, OsString, c_void};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use std::ptr;

use super::LookupError;
use crate::fs_ops::ERROR_REPARSE_POINT_ENCOUNTERED;
use crate::read::{self, Content};

type Handle = *mut c_void;
type NtStatus = i32;

const FILE_READ_DATA: u32 = 0x0001;
const FILE_WRITE_DATA: u32 = 0x0002;
const FILE_READ_EA: u32 = 0x0008;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const READ_CONTROL: u32 = 0x0002_0000;
const SYNCHRONIZE: u32 = 0x0010_0000;
const FILE_GENERIC_READ: u32 = READ_CONTROL | FILE_READ_DATA | FILE_READ_ATTRIBUTES | FILE_READ_EA | SYNCHRONIZE;

const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const FILE_SHARE_DELETE: u32 = 0x4;
const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

const FILE_OPEN: u32 = 1;
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_OPEN_NO_RECALL: u32 = 0x0040_0000;
const OBJ_CASE_INSENSITIVE: u32 = 0x40;

/// The CreateFileW flags for opening a name itself, never what it points to or a recalled copy.
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_FLAG_OPEN_NO_RECALL: u32 = 0x0010_0000;

const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
const FILE_ATTRIBUTE_DEVICE: u32 = 0x0000_0040;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
/// A placeholder whose contents another program, such as a cloud client, fetches when it is opened.
const DATALESS: u32 = FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;
/// Set in the tag of a reparse point that names another file: a link, junction or mount point.
const NAME_SURROGATE: u32 = 0x2000_0000;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
const IO_REPARSE_TAG_LX_SYMLINK: u32 = 0xA000_001D;
const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00A8;
const MAXIMUM_REPARSE_DATA_BUFFER_SIZE: usize = 16 * 1024;

const FILE_BASIC_INFORMATION: u32 = 4;
const FILE_STANDARD_INFORMATION: u32 = 5;
const FILE_NAME_INFORMATION: u32 = 9;
const FILE_ATTRIBUTE_TAG_INFORMATION: u32 = 35;
const FILE_FS_DEVICE_INFORMATION: u32 = 4;
const FILE_FS_ATTRIBUTE_INFORMATION: u32 = 5;
const FILE_REMOTE_DEVICE: u32 = 0x10;
const FILE_READ_ONLY_VOLUME: u32 = 0x0008_0000;

const STATUS_BUFFER_OVERFLOW: NtStatus = 0x8000_0005_u32.cast_signed();
const STATUS_FILE_IS_A_DIRECTORY: NtStatus = 0xC000_00BA_u32.cast_signed();

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: Handle,
    object_name: *const UnicodeString,
    attributes: u32,
    security_descriptor: *const c_void,
    security_quality_of_service: *const c_void,
}

#[repr(C)]
struct IoStatusBlock {
    status: usize,
    information: usize,
}

/// A record that NtQueryInformationFile or NtQueryVolumeInformationFile fills in.
///
/// # Safety
///
/// The type is `#[repr(C)]` and holds only integers, so any bytes the kernel writes are a valid value.
unsafe trait Record {}

#[repr(C)]
struct AttributeTagInformation {
    attributes: u32,
    reparse_tag: u32,
}

#[repr(C)]
struct BasicInformation {
    creation_time: i64,
    last_access_time: i64,
    last_write_time: i64,
    change_time: i64,
    attributes: u32,
}

#[repr(C)]
struct StandardInformation {
    allocation_size: i64,
    end_of_file: i64,
    number_of_links: u32,
    delete_pending: u8,
    directory: u8,
}

#[repr(C)]
struct NameInformation {
    length: u32,
    name: [u16; 2],
}

#[repr(C)]
struct DeviceInformation {
    device_type: u32,
    characteristics: u32,
}

#[repr(C)]
struct VolumeAttributeInformation {
    attributes: u32,
    maximum_component_length: i32,
    name_length: u32,
    name: [u16; 64],
}

// SAFETY: Each of these is #[repr(C)] and holds only integers and arrays of integers.
unsafe impl Record for AttributeTagInformation {}
// SAFETY: As above.
unsafe impl Record for BasicInformation {}
// SAFETY: As above.
unsafe impl Record for StandardInformation {}
// SAFETY: As above.
unsafe impl Record for NameInformation {}
// SAFETY: As above.
unsafe impl Record for DeviceInformation {}
// SAFETY: As above.
unsafe impl Record for VolumeAttributeInformation {}

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn NtCreateFile(
        handle: *mut Handle,
        access: u32,
        attributes: *const ObjectAttributes,
        status_block: *mut IoStatusBlock,
        allocation_size: *const i64,
        file_attributes: u32,
        share: u32,
        disposition: u32,
        options: u32,
        ea_buffer: *const c_void,
        ea_length: u32,
    ) -> NtStatus;
    fn NtQueryInformationFile(
        handle: Handle,
        status_block: *mut IoStatusBlock,
        information: *mut c_void,
        length: u32,
        class: u32,
    ) -> NtStatus;
    fn NtQueryVolumeInformationFile(
        handle: Handle,
        status_block: *mut IoStatusBlock,
        information: *mut c_void,
        length: u32,
        class: u32,
    ) -> NtStatus;
    fn NtFsControlFile(
        handle: Handle,
        event: Handle,
        apc_routine: *const c_void,
        apc_context: *const c_void,
        status_block: *mut IoStatusBlock,
        code: u32,
        input: *const c_void,
        input_length: u32,
        output: *mut c_void,
        output_length: u32,
    ) -> NtStatus;
    safe fn RtlNtStatusToDosError(status: NtStatus) -> u32;
}

pub(in crate::reader) struct Root {
    dir: File,
    identity: (u64, u64),
    read_only_volume: bool,
}

impl Root {
    /// Opens an absolute path on a local drive, such as `C:\Users\me\project` or its `\\?\` form,
    /// walking down from the root of the drive. Refuses the path if any folder on the way, the
    /// last included, is a reparse point, or if the drive letter stands for a network share or
    /// for a folder.
    pub(in crate::reader) fn open(path: &Path) -> io::Result<Self> {
        let refused = |why: &'static str| io::Error::new(io::ErrorKind::InvalidInput, why);
        let mut parts = path.components();
        let letter = match parts.next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter.to_ascii_uppercase(),
                _ => return Err(refused("not a path on a local drive")),
            },
            _ => return Err(refused("not an absolute path")),
        };
        if parts.next() != Some(Component::RootDir) {
            return Err(refused("not an absolute path"));
        }
        let names = parts
            .map(|part| match part {
                Component::Normal(name) => plain(name).map_err(|_| refused("the path names a stream or device")),
                _ => Err(refused("the path is not in its simplest form")),
            })
            .collect::<io::Result<Vec<_>>>()?;
        let drive: Vec<u16> = format!(r"\??\{}:\", char::from(letter)).encode_utf16().collect();
        let mut dir = directory(None, &drive, SHARE_ALL)?;
        if !local_volume_root(&dir)? {
            return Err(refused("the drive is not a local disk"));
        }
        let read_only_volume = read_only_volume(&dir)?;
        for name in &names {
            dir = directory(Some(&dir), name, SHARE_ALL)?;
        }
        Ok(Self { identity: crate::sys::identity(&dir)?, dir, read_only_volume })
    }

    /// The folder as originally opened, even if its pathname is later reused.
    pub(in crate::reader) fn identity(&self) -> (u64, u64) {
        self.identity
    }

    pub(in crate::reader) fn same_directory(&self, other: &Self) -> bool {
        self.identity == other.identity
    }

    pub(in crate::reader) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self { dir: self.dir.try_clone()?, identity: self.identity, read_only_volume: self.read_only_volume })
    }

    /// Git still needs a pathname. Check that it names the folder we are holding.
    pub(in crate::reader) fn at_path(&self, path: &Path) -> Result<bool, LookupError> {
        let flags = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL;
        let mut options = OpenOptions::new();
        options.access_mode(FILE_READ_ATTRIBUTES).share_mode(SHARE_ALL).custom_flags(flags);
        let Ok(stat) = options.open(path).and_then(|dir| stat(&dir)) else { return Ok(false) };
        if stat.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(LookupError::Outside);
        }
        Ok(stat.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 && stat.identity == self.identity)
    }

    /// Looks up a path below the project, opening each folder on the way relative to the one
    /// above it. The folders stay open, without delete sharing, for as long as the entry lives,
    /// so none of them can be renamed out of the project before the file is opened. A folder
    /// that is a reparse point is `Outside`, so `base` and `links` are not needed here.
    pub(in crate::reader) fn entry(
        &self,
        _base: &Path,
        relative: &Path,
        _links: &mut u8,
    ) -> Result<Entry<'_>, LookupError> {
        let mut names = Vec::new();
        for part in relative.components() {
            match part {
                Component::Normal(name) => names.push(plain(name)?),
                Component::CurDir => {}
                _ => return Err(LookupError::Outside),
            }
        }
        let name = names.pop().ok_or(LookupError::Gone)?;
        let mut dirs: Vec<File> = Vec::with_capacity(names.len());
        for part in &names {
            let dir = directory(Some(dirs.last().unwrap_or(&self.dir)), part, FILE_SHARE_READ | FILE_SHARE_WRITE)
                .map_err(LookupError::of)?;
            dirs.push(dir);
        }
        let stat = lookup(dirs.last().unwrap_or(&self.dir), &name).map_err(LookupError::of)?;
        Ok(Entry { root: self, dirs, name, stat })
    }
}

/// What a lookup learns about a name without opening its data.
#[derive(Clone, Copy)]
struct Stat {
    identity: (u64, u64),
    changed: i64,
    attributes: u32,
    reparse_tag: u32,
    size: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Regular,
    Link,
    Directory,
    Other,
}

impl Stat {
    fn kind(&self) -> Kind {
        kind(self.attributes, self.reparse_tag)
    }

    fn dataless(&self) -> bool {
        self.attributes & DATALESS != 0
    }
}

/// A placeholder counts as a regular file, as a data-less file does on macOS. Any other reparse
/// point that is not a link, such as a compressed or deduplicated file, is read through a filter
/// that opening the reparse point itself bypasses, so it is not.
fn kind(attributes: u32, reparse_tag: u32) -> Kind {
    let reparse = attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    if reparse && reparse_tag & NAME_SURROGATE != 0 {
        Kind::Link
    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        Kind::Directory
    } else if attributes & FILE_ATTRIBUTE_DEVICE != 0 || (reparse && attributes & DATALESS == 0) {
        Kind::Other
    } else {
        Kind::Regular
    }
}

/// Includes file identity and change time: saving by replacement can preserve length and write time.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::reader) struct Stamp {
    identity: (u64, u64),
    changed: i64,
    kind: Kind,
}

impl Stamp {
    fn of_stat(stat: &Stat) -> Self {
        Self { identity: stat.identity, changed: stat.changed, kind: stat.kind() }
    }

    /// Looks at the name itself, not what a link there points to.
    pub(in crate::reader) fn at(path: &Path) -> Option<Self> {
        let mut options = OpenOptions::new();
        options
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL);
        Self::of_file(&options.open(path).ok()?)
    }

    pub(in crate::reader) fn of_file(file: &File) -> Option<Self> {
        let stat = stat(file).ok()?;
        (stat.kind() == Kind::Regular).then(|| Self::of_stat(&stat))
    }
}

pub(in crate::reader) struct Entry<'a> {
    root: &'a Root,
    /// The folders between the project and the file, outermost first.
    dirs: Vec<File>,
    name: Vec<u16>,
    stat: Stat,
}

impl Entry<'_> {
    fn parent(&self) -> &File {
        self.dirs.last().unwrap_or(&self.root.dir)
    }

    pub(in crate::reader) fn stamp(&self) -> Stamp {
        Stamp::of_stat(&self.stat)
    }

    pub(in crate::reader) fn is_special(&self) -> bool {
        !matches!(self.stat.kind(), Kind::Regular | Kind::Link)
    }

    pub(in crate::reader) fn is_regular(&self) -> bool {
        self.stat.kind() == Kind::Regular
    }

    pub(in crate::reader) fn is_dataless(&self) -> bool {
        self.stat.dataless()
    }

    pub(in crate::reader) fn is_link(&self) -> bool {
        self.stat.kind() == Kind::Link
    }

    /// Opens the file to read and write. Until it is closed, other programs may read the file
    /// but not write, rename or delete it. A link that has taken the file's place is refused
    /// with `ERROR_REPARSE_POINT_ENCOUNTERED`, not followed.
    pub(in crate::reader) fn open_edit(&self) -> io::Result<File> {
        open_file(self.parent(), &self.name, FILE_GENERIC_READ | FILE_WRITE_DATA, FILE_SHARE_READ)
    }

    /// Opens the file to read only, sharing it with every other program, and refusing a link the same way.
    pub(in crate::reader) fn open_read(&self) -> io::Result<File> {
        open_file(self.parent(), &self.name, FILE_GENERIC_READ, SHARE_ALL)
    }

    /// Transfers a guarded pathname to the source lane without opening its data.
    pub(in crate::reader) fn source(mut self) -> io::Result<ReadTarget> {
        let stamp = self.stamp();
        let dir = match self.dirs.pop() {
            Some(parent) => parent,
            None => self.root.dir.try_clone()?,
        };
        Ok(ReadTarget { dir, name: self.name, stamp, size: self.stat.size })
    }

    /// Whether the file is marked read-only or its volume is. A folder's permissions are
    /// checked when a save opens the file, since opening it to write now and closing it looks
    /// like a write to whoever watches it.
    pub(in crate::reader) fn writable(&self) -> bool {
        !self.root.read_only_volume && self.stat.attributes & FILE_ATTRIBUTE_READONLY == 0
    }

    /// The file's length when it was looked up.
    pub(in crate::reader) fn size(&self) -> u64 {
        self.stat.size
    }

    pub(in crate::reader) fn read(&self, limit: u64) -> Content {
        let dir = self.parent();
        let link = || link_at(dir, &self.name).map_or_else(|e| LookupError::of(e).content(), Content::Link);
        match self.stat.kind() {
            Kind::Link => link(),
            Kind::Regular => match open_file(dir, &self.name, FILE_GENERIC_READ, SHARE_ALL) {
                Ok(file) => read::opened(file, limit),
                Err(e) if e.raw_os_error() == Some(ERROR_REPARSE_POINT_ENCOUNTERED) => link(),
                Err(e) => LookupError::of(e).content(),
            },
            Kind::Directory | Kind::Other => Content::NotFile,
        }
    }
}

pub(in crate::reader) struct ReadTarget {
    dir: File,
    name: Vec<u16>,
    stamp: Stamp,
    pub(in crate::reader) size: u64,
}

impl ReadTarget {
    /// Refuse data-less placeholders before an open can request their contents.
    pub(in crate::reader) fn open(&self) -> Result<File, &'static str> {
        const UNREADABLE: &str = "This file cannot be read.";
        let stat = lookup(&self.dir, &self.name).map_err(|_| UNREADABLE)?;
        if stat.kind() != Kind::Regular || Stamp::of_stat(&stat) != self.stamp {
            return Err(UNREADABLE);
        }
        if stat.dataless() {
            return Err("Download this file before opening its preview.");
        }
        let file = open_file(&self.dir, &self.name, FILE_GENERIC_READ, SHARE_ALL).map_err(|_| UNREADABLE)?;
        if !self.matches(&file) {
            return Err(UNREADABLE);
        }
        Ok(file)
    }

    pub(in crate::reader) fn matches(&self, file: &File) -> bool {
        Stamp::of_file(file) == Some(self.stamp)
    }
}

/// A name as one path component, or why it cannot be one. Win32 would read a name with a colon
/// as a stream, a reserved name such as `nul.txt` as a device, and a trailing dot or space as the
/// same name without it; those are refused as leaving the file.
fn plain(name: &OsStr) -> Result<Vec<u16>, LookupError> {
    let wide: Vec<u16> = name.encode_wide().collect();
    let trailing = wide.last().is_some_and(|&c| c == u16::from(b'.') || c == u16::from(b' '));
    if wide.contains(&u16::from(b':')) || trailing || reserved(name) {
        return Err(LookupError::Outside);
    }
    let forbidden = |c: &u16| *c < 32 || br#"<>"/\|?*"#.iter().any(|b| u16::from(*b) == *c);
    if wide.iter().any(forbidden) {
        return Err(LookupError::Unreadable);
    }
    Ok(wide)
}

/// Whether Win32 would open a device for this name, whatever its extension or case.
fn reserved(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    let stem = name.split('.').next().unwrap_or_default().trim_end_matches(' ').to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        return true;
    }
    let Some(number) = stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT")) else { return false };
    let mut chars = number.chars();
    matches!((chars.next(), chars.next()), (Some('0'..='9' | '¹' | '²' | '³'), None))
}

/// Opens a folder by name below `parent`, or by its full NT name without one, refusing a reparse point.
fn directory(parent: Option<&File>, name: &[u16], share: u32) -> io::Result<File> {
    let options = FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT;
    let dir = open_at(parent, name, FILE_READ_ATTRIBUTES | SYNCHRONIZE, share, options)?;
    if attribute_tag(&dir)?.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::from_raw_os_error(ERROR_REPARSE_POINT_ENCOUNTERED));
    }
    Ok(dir)
}

/// Learns about a name below `dir` through a handle that can read only its attributes.
fn lookup(dir: &File, name: &[u16]) -> io::Result<Stat> {
    stat(&open_at(Some(dir), name, FILE_READ_ATTRIBUTES, SHARE_ALL, 0)?)
}

/// Opens a regular file's data below `dir`, refusing anything else found under its name.
fn open_file(dir: &File, name: &[u16], access: u32, share: u32) -> io::Result<File> {
    let options = FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT;
    let file = open_at(Some(dir), name, access, share, options)?;
    let info = attribute_tag(&file)?;
    match kind(info.attributes, info.reparse_tag) {
        Kind::Link => Err(io::Error::from_raw_os_error(ERROR_REPARSE_POINT_ENCOUNTERED)),
        Kind::Regular if info.attributes & DATALESS == 0 => Ok(file),
        _ => Err(io::ErrorKind::InvalidInput.into()),
    }
}

/// Opens an existing name exactly as given, never creating it and never following a reparse point.
fn open_at(parent: Option<&File>, name: &[u16], access: u32, share: u32, options: u32) -> io::Result<File> {
    let length = u16::try_from(size_of_val(name)).map_err(|_| io::Error::from(io::ErrorKind::InvalidFilename))?;
    let name = UnicodeString { length, maximum_length: length, buffer: name.as_ptr() };
    let attributes = ObjectAttributes {
        length: length_of::<ObjectAttributes>(),
        root_directory: parent.map_or(ptr::null_mut(), |dir| dir.as_raw_handle()),
        object_name: &raw const name,
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor: ptr::null(),
        security_quality_of_service: ptr::null(),
    };
    let mut handle = ptr::null_mut();
    let mut status_block = IoStatusBlock { status: 0, information: 0 };
    let options = options | FILE_OPEN_REPARSE_POINT | FILE_OPEN_NO_RECALL;
    // SAFETY: The name and attributes outlive the call, the parent handle is open while borrowed,
    // and the handle and status block are writable. FILE_OPEN never creates or overwrites a file.
    let status = unsafe {
        NtCreateFile(
            &raw mut handle,
            access,
            &raw const attributes,
            &raw mut status_block,
            ptr::null(),
            0,
            share,
            FILE_OPEN,
            options,
            ptr::null(),
            0,
        )
    };
    check(status)?;
    // SAFETY: A successful NtCreateFile returns a new handle that nothing else owns.
    Ok(File::from(unsafe { OwnedHandle::from_raw_handle(handle) }))
}

fn stat(file: &File) -> io::Result<Stat> {
    let tag = attribute_tag(file)?;
    let mut basic =
        BasicInformation { creation_time: 0, last_access_time: 0, last_write_time: 0, change_time: 0, attributes: 0 };
    check(query_file(file, FILE_BASIC_INFORMATION, &mut basic))?;
    let mut standard =
        StandardInformation { allocation_size: 0, end_of_file: 0, number_of_links: 0, delete_pending: 0, directory: 0 };
    check(query_file(file, FILE_STANDARD_INFORMATION, &mut standard))?;
    Ok(Stat {
        identity: crate::sys::identity(file)?,
        changed: basic.change_time,
        attributes: tag.attributes,
        reparse_tag: tag.reparse_tag,
        size: u64::try_from(standard.end_of_file).unwrap_or(0),
    })
}

fn attribute_tag(file: &File) -> io::Result<AttributeTagInformation> {
    let mut info = AttributeTagInformation { attributes: 0, reparse_tag: 0 };
    check(query_file(file, FILE_ATTRIBUTE_TAG_INFORMATION, &mut info))?;
    Ok(info)
}

/// The destination a link records, read from the link itself below `dir`.
fn link_at(dir: &File, name: &[u16]) -> io::Result<PathBuf> {
    let options = FILE_SYNCHRONOUS_IO_NONALERT;
    let link = open_at(Some(dir), name, FILE_READ_ATTRIBUTES | SYNCHRONIZE, SHARE_ALL, options)?;
    let mut data = vec![0_u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE];
    let mut status_block = IoStatusBlock { status: 0, information: 0 };
    // SAFETY: The handle is open and synchronous, so the call completes before returning, and the
    // output buffer is writable for the length given. The request takes no input.
    let status = unsafe {
        NtFsControlFile(
            link.as_raw_handle(),
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            &raw mut status_block,
            FSCTL_GET_REPARSE_POINT,
            ptr::null(),
            0,
            data.as_mut_ptr().cast(),
            length_of::<[u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE]>(),
        )
    };
    check(status)?;
    data.truncate(status_block.information);
    reparse_target(&data).ok_or_else(|| io::ErrorKind::InvalidData.into())
}

/// Reads the destination out of a symbolic link, junction or WSL link's reparse data,
/// preferring the name the link was made with over the form the system resolves.
fn reparse_target(data: &[u8]) -> Option<PathBuf> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?));
    let tag = u32::from_le_bytes(data.get(..4)?.try_into().ok()?);
    let end = 8 + usize::from(u16_at(4)?);
    let names_start = match tag {
        IO_REPARSE_TAG_SYMLINK => 20,
        IO_REPARSE_TAG_MOUNT_POINT => 16,
        IO_REPARSE_TAG_LX_SYMLINK => return Some(PathBuf::from(std::str::from_utf8(data.get(12..end)?).ok()?)),
        _ => return None,
    };
    let (offset, length, substitute) = match (u16_at(12)?, u16_at(14)?) {
        (_, 0) => (u16_at(8)?, u16_at(10)?, true),
        (offset, length) => (offset, length, false),
    };
    let start = names_start + usize::from(offset);
    let stop = start + usize::from(length);
    let bytes = data.get(start..stop).filter(|_| stop <= end)?;
    let wide: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    let nt_prefix: Vec<u16> = r"\??\".encode_utf16().collect();
    let wide = if substitute { wide.strip_prefix(nt_prefix.as_slice()).unwrap_or(&wide) } else { &wide };
    Some(PathBuf::from(OsString::from_wide(wide)))
}

/// Whether the folder is the root of a volume on this computer, rather than a network share
/// or the folder a substituted drive letter stands for.
fn local_volume_root(dir: &File) -> io::Result<bool> {
    let mut device = DeviceInformation { device_type: 0, characteristics: 0 };
    check(query_volume(dir, FILE_FS_DEVICE_INFORMATION, &mut device))?;
    if device.characteristics & FILE_REMOTE_DEVICE != 0 {
        return Ok(false);
    }
    let mut name = NameInformation { length: 0, name: [0; 2] };
    match query_file(dir, FILE_NAME_INFORMATION, &mut name) {
        STATUS_BUFFER_OVERFLOW => Ok(false),
        status => check(status).map(|()| name.length == 2 && name.name[0] == u16::from(b'\\')),
    }
}

fn read_only_volume(dir: &File) -> io::Result<bool> {
    let mut info =
        VolumeAttributeInformation { attributes: 0, maximum_component_length: 0, name_length: 0, name: [0; 64] };
    // A file system name longer than the buffer still leaves the attributes filled in.
    match query_volume(dir, FILE_FS_ATTRIBUTE_INFORMATION, &mut info) {
        STATUS_BUFFER_OVERFLOW => {}
        status => check(status)?,
    }
    Ok(info.attributes & FILE_READ_ONLY_VOLUME != 0)
}

fn query_file<T: Record>(file: &File, class: u32, info: &mut T) -> NtStatus {
    let mut status_block = IoStatusBlock { status: 0, information: 0 };
    let length = length_of::<T>();
    // SAFETY: The handle is open while borrowed, and `info` is a writable record of the length
    // given, valid for whatever bytes the call writes into it.
    unsafe {
        NtQueryInformationFile(file.as_raw_handle(), &raw mut status_block, (&raw mut *info).cast(), length, class)
    }
}

fn query_volume<T: Record>(file: &File, class: u32, info: &mut T) -> NtStatus {
    let mut status_block = IoStatusBlock { status: 0, information: 0 };
    let length = length_of::<T>();
    // SAFETY: As in `query_file`.
    unsafe {
        NtQueryVolumeInformationFile(
            file.as_raw_handle(),
            &raw mut status_block,
            (&raw mut *info).cast(),
            length,
            class,
        )
    }
}

fn length_of<T>() -> u32 {
    u32::try_from(size_of::<T>()).expect("system records are small")
}

fn check(status: NtStatus) -> io::Result<()> {
    if status >= 0 {
        return Ok(());
    }
    if status == STATUS_FILE_IS_A_DIRECTORY {
        return Err(io::ErrorKind::IsADirectory.into());
    }
    Err(io::Error::from_raw_os_error(RtlNtStatusToDosError(status).cast_signed()))
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::{Read as _, Write as _};
    use std::os::windows::fs::{symlink_dir, symlink_file};
    use std::process::{Command, Stdio};
    use std::thread::sleep;
    use std::time::Duration;

    use super::*;
    use crate::fixture::Fixture;

    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

    fn root(fixture: &Fixture) -> Root {
        Root::open(&fixture.root).unwrap()
    }

    fn find<'a>(root: &'a Root, fixture: &Fixture, path: &str) -> Result<Entry<'a>, LookupError> {
        root.entry(&fixture.root, Path::new(path), &mut 0)
    }

    /// The path without its `\\?\` prefix, as a person or `cmd` would write it.
    fn plain_form(path: &Path) -> PathBuf {
        let text = path.to_str().unwrap();
        PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text))
    }

    /// Whether a link was made; false only where this account may not create symbolic links.
    fn linked(made: io::Result<()>) -> bool {
        match made {
            Ok(()) => true,
            Err(e) if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => false,
            Err(e) => panic!("cannot create a link: {e}"),
        }
    }

    fn junction(link: &Path, target: &Path) {
        let status = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(plain_form(link))
            .arg(plain_form(target))
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "mklink /J failed");
    }

    #[test]
    fn a_plain_file_is_found_read_and_opened_to_edit() {
        let fixture = Fixture::folder();
        fixture.write("docs/notes.md", "# Notes");
        let root = root(&fixture);
        for path in ["docs/notes.md", r"docs\notes.md", r"docs\.\NOTES.md"] {
            let entry = find(&root, &fixture, path).unwrap();
            assert!(entry.is_regular() && !entry.is_link() && !entry.is_special() && !entry.is_dataless(), "{path}");
            assert!(entry.writable() && entry.size() == 7);
            let mut text = String::new();
            entry.open_read().unwrap().read_to_string(&mut text).unwrap();
            assert_eq!(text, "# Notes");
            assert!(matches!(entry.read(1024), Content::Bytes(bytes) if bytes == b"# Notes"));
            assert!(matches!(entry.read(3), Content::TooLarge));
        }
        let entry = find(&root, &fixture, "docs/notes.md").unwrap();
        assert!(entry.open_edit().unwrap().metadata().unwrap().is_file());
    }

    #[test]
    fn a_folder_is_special_and_does_not_open_as_a_file() {
        let fixture = Fixture::folder();
        fixture.write("docs/notes.md", "# Notes");
        let root = root(&fixture);
        let entry = find(&root, &fixture, "docs").unwrap();
        assert!(entry.is_special() && !entry.is_regular() && !entry.is_link());
        assert!(matches!(entry.read(1024), Content::NotFile));
        assert_eq!(entry.open_read().unwrap_err().kind(), io::ErrorKind::IsADirectory);
        assert!(Stamp::at(&fixture.root.join("docs")).is_none());
    }

    #[test]
    fn missing_names_are_gone() {
        let fixture = Fixture::folder();
        fixture.write("notes.md", "# Notes");
        let root = root(&fixture);
        for path in ["missing.md", "missing/notes.md", "notes.md/inner.md", ""] {
            assert_eq!(find(&root, &fixture, path).err(), Some(LookupError::Gone), "{path}");
        }
    }

    #[test]
    fn names_that_leave_the_file_or_the_project_are_outside() {
        let fixture = Fixture::folder();
        fixture.write("notes.md", "# Notes");
        let root = root(&fixture);
        let outside = [
            "../notes.md", r"C:\notes.md", r"\notes.md", "notes.md:hidden", "notes.md::$DATA", "notes.md.",
            "notes.md ", "nul", "CON.md", "com1.txt", "lpt\u{b9}", "CONOUT$",
        ];
        for path in outside {
            assert_eq!(find(&root, &fixture, path).err(), Some(LookupError::Outside), "{path}");
        }
        assert_eq!(find(&root, &fixture, "no*tes.md").err(), Some(LookupError::Unreadable));
        assert_eq!(find(&root, &fixture, "console.md").err(), Some(LookupError::Gone));
    }

    #[test]
    fn a_read_only_file_is_not_writable() {
        let fixture = Fixture::folder();
        fixture.write("notes.md", "# Notes");
        let path = fixture.root.join("notes.md");
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();
        assert!(!find(&root(&fixture), &fixture, "notes.md").unwrap().writable());
    }

    #[test]
    fn an_open_edit_lets_others_read_but_not_write_or_delete() {
        let fixture = Fixture::folder();
        fixture.write("notes.md", "# Notes");
        let path = fixture.root.join("notes.md");
        let root = root(&fixture);
        let held = find(&root, &fixture, "notes.md").unwrap().open_edit().unwrap();
        assert!(File::open(&path).is_ok());
        let write = OpenOptions::new().write(true).open(&path).unwrap_err();
        assert_eq!(write.raw_os_error(), Some(ERROR_SHARING_VIOLATION));
        assert_eq!(fs::remove_file(&path).unwrap_err().raw_os_error(), Some(ERROR_SHARING_VIOLATION));
        drop(held);
        assert!(OpenOptions::new().write(true).open(&path).is_ok());
    }

    #[test]
    fn a_folder_on_the_way_cannot_be_renamed_while_an_entry_holds_it() {
        let fixture = Fixture::folder();
        fixture.write("docs/notes.md", "# Notes");
        let root = root(&fixture);
        let entry = find(&root, &fixture, "docs/notes.md").unwrap();
        assert!(fs::rename(fixture.root.join("docs"), fixture.root.join("moved")).is_err());
        drop(entry);
        fs::rename(fixture.root.join("docs"), fixture.root.join("moved")).unwrap();
    }

    #[test]
    fn only_absolute_paths_on_a_drive_open() {
        let fixture = Fixture::folder();
        let refused = [
            r"relative\project",
            r"\project",
            r"C:project",
            r"\\server\share\project",
            r"\\.\C:\project",
            r"C:\Windows\..\Windows",
            r"\\?\C:\Windows\..\Windows",
            r"\\?\C:\Windows\.",
            r"C:\Windows:stream",
        ];
        for path in refused {
            let kind = Root::open(Path::new(path)).err().map(|e| e.kind());
            assert_eq!(kind, Some(io::ErrorKind::InvalidInput), "{path}");
        }
        let verbatim = Root::open(&fixture.root).unwrap();
        let plain = Root::open(&plain_form(&fixture.root)).unwrap();
        assert!(verbatim.same_directory(&plain) && verbatim.try_clone().unwrap().identity() == plain.identity());
    }

    #[test]
    fn the_stamp_changes_when_the_file_is_written_or_replaced() {
        let fixture = Fixture::folder();
        fixture.write("notes.md", "# Notes");
        let path = fixture.root.join("notes.md");
        let root = root(&fixture);
        let entry = find(&root, &fixture, "notes.md").unwrap();
        let stamp = entry.stamp();
        assert!(Stamp::at(&path) == Some(stamp));
        assert!(Stamp::of_file(&entry.open_read().unwrap()) == Some(stamp));
        let target = entry.source().unwrap();
        assert!(target.size == 7 && target.open().is_ok());

        sleep(Duration::from_millis(20));
        OpenOptions::new().write(true).open(&path).unwrap().write_all(b"# Notez").unwrap();
        let written = Stamp::at(&path).unwrap();
        assert!(written != stamp);
        assert_eq!(target.open().err(), Some("This file cannot be read."));

        fixture.write("draft.md", "# Notez");
        fs::rename(fixture.root.join("draft.md"), &path).unwrap();
        let replaced = Stamp::at(&path).unwrap();
        assert!(replaced != written && replaced != stamp);
    }

    #[test]
    fn the_held_folder_is_recognised_by_its_path() {
        let fixture = Fixture::folder();
        fixture.write("docs/notes.md", "# Notes");
        let root = root(&fixture);
        assert_eq!(root.at_path(&fixture.root), Ok(true));
        assert_eq!(root.at_path(&plain_form(&fixture.root)), Ok(true));
        assert_eq!(root.at_path(&fixture.root.join("docs")), Ok(false));
        assert_eq!(root.at_path(&fixture.root.join("missing")), Ok(false));
        let moved = fixture.root.with_file_name("moved");
        fs::rename(&fixture.root, &moved).unwrap();
        assert_eq!(root.at_path(&moved), Ok(true));
        fs::create_dir(&fixture.root).unwrap();
        assert_eq!(root.at_path(&fixture.root), Ok(false));
    }

    #[test]
    fn a_junction_is_a_link_and_is_never_followed() {
        let fixture = Fixture::folder();
        fixture.write("docs/notes.md", "# Notes");
        let target = plain_form(&fixture.root.join("docs"));
        let joined = fixture.root.join("joined");
        junction(&joined, &target);
        let root = root(&fixture);
        let entry = find(&root, &fixture, "joined").unwrap();
        assert!(entry.is_link() && !entry.is_special() && !entry.is_regular());
        assert!(matches!(entry.read(1024), Content::Link(path) if path == target));
        assert_eq!(find(&root, &fixture, "joined/notes.md").err(), Some(LookupError::Outside));
        assert_eq!(root.at_path(&joined), Err(LookupError::Outside));
        let through = Root::open(&joined).err().and_then(|e| e.raw_os_error());
        assert_eq!(through, Some(ERROR_REPARSE_POINT_ENCOUNTERED));
    }

    #[test]
    fn a_symbolic_link_to_a_file_is_read_as_its_destination_and_never_opened() {
        let fixture = Fixture::folder();
        fixture.write("notes.md", "# Notes");
        if !linked(symlink_file("notes.md", fixture.root.join("link.md"))) {
            return;
        }
        let root = root(&fixture);
        let entry = find(&root, &fixture, "link.md").unwrap();
        assert!(entry.is_link() && !entry.is_special() && !entry.is_regular());
        assert!(matches!(entry.read(1024), Content::Link(path) if path == Path::new("notes.md")));
        assert_eq!(entry.open_read().unwrap_err().raw_os_error(), Some(ERROR_REPARSE_POINT_ENCOUNTERED));
        assert_eq!(entry.open_edit().unwrap_err().raw_os_error(), Some(ERROR_REPARSE_POINT_ENCOUNTERED));
        assert!(Stamp::at(&fixture.root.join("link.md")).is_none());
    }

    #[test]
    fn a_symbolic_link_to_a_folder_is_outside_when_walked_through() {
        let fixture = Fixture::folder();
        fixture.write("docs/notes.md", "# Notes");
        if !linked(symlink_dir("docs", fixture.root.join("linked"))) {
            return;
        }
        let root = root(&fixture);
        assert!(find(&root, &fixture, "linked").unwrap().is_link());
        assert_eq!(find(&root, &fixture, "linked/notes.md").err(), Some(LookupError::Outside));
    }

    #[test]
    fn a_link_put_in_place_after_the_lookup_is_not_followed() {
        let fixture = Fixture::folder();
        fixture.write("notes.md", "# Notes");
        fixture.write("other.md", "# Other");
        let root = root(&fixture);
        let entry = find(&root, &fixture, "notes.md").unwrap();
        fs::remove_file(fixture.root.join("notes.md")).unwrap();
        if !linked(symlink_file("other.md", fixture.root.join("notes.md"))) {
            return;
        }
        assert_eq!(entry.open_edit().unwrap_err().raw_os_error(), Some(ERROR_REPARSE_POINT_ENCOUNTERED));
        assert!(matches!(entry.read(1024), Content::Link(path) if path == Path::new("other.md")));
    }
}
