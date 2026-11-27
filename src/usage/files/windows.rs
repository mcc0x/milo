//! Windows form of the session walk: every name is opened relative to the folder above it with
//! NtCreateFile and FILE_OPEN_REPARSE_POINT, and a link, junction or mount point is refused,
//! never followed.

use std::ffi::{OsStr, OsString, c_void};
use std::fs::File;
use std::io;
use std::ops::ControlFlow;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::fs::MetadataExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
use std::path::{Component, Path, PathBuf};
use std::ptr;

use super::{FileVersion, Scan, Tree};
use crate::fs_ops::ERROR_REPARSE_POINT_ENCOUNTERED;

type Handle = *mut c_void;
type NtStatus = i32;

const FILE_GENERIC_READ: u32 = 0x0012_0089;
const SHARE_ALL: u32 = 0x7;
const FILE_OPEN: u32 = 1;
const FILE_DIRECTORY_FILE: u32 = 0x1;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_OPEN_NO_RECALL: u32 = 0x0040_0000;
const OBJ_CASE_INSENSITIVE: u32 = 0x40;
const FILE_DIRECTORY_INFORMATION: u32 = 1;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const STATUS_NO_MORE_FILES: NtStatus = 0x8000_0006_u32.cast_signed();
const NAMES_AT: usize = 64;

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
#[derive(Default)]
struct IoStatusBlock {
    status: usize,
    information: usize,
}

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
    fn NtQueryDirectoryFile(
        handle: Handle,
        event: Handle,
        apc_routine: *const c_void,
        apc_context: *const c_void,
        status_block: *mut IoStatusBlock,
        information: *mut c_void,
        length: u32,
        class: u32,
        return_single_entry: u8,
        file_name: *const UnicodeString,
        restart_scan: u8,
    ) -> NtStatus;
    safe fn RtlNtStatusToDosError(status: NtStatus) -> u32;
}

fn check(status: NtStatus) -> io::Result<()> {
    if status >= 0 {
        return Ok(());
    }
    Err(io::Error::from_raw_os_error(i32::try_from(RtlNtStatusToDosError(status)).unwrap_or(i32::MAX)))
}

/// Opens one name inside an open folder, refusing it if it is a reparse point.
fn open_at(dir: &File, name: &OsStr, directory: bool) -> io::Result<File> {
    let wide: Vec<u16> = name.encode_wide().collect();
    if wide.is_empty() || wide.iter().any(|&unit| matches!(unit, 0 | 0x2F | 0x3A | 0x5C)) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let length = u16::try_from(wide.len() * 2).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let name = UnicodeString { length, maximum_length: length, buffer: wide.as_ptr() };
    let attributes = ObjectAttributes {
        length: u32::try_from(size_of::<ObjectAttributes>()).unwrap_or(0),
        root_directory: dir.as_raw_handle(),
        object_name: &raw const name,
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor: ptr::null(),
        security_quality_of_service: ptr::null(),
    };
    let kind = if directory { FILE_DIRECTORY_FILE } else { FILE_NON_DIRECTORY_FILE };
    let no_recall = if directory { 0 } else { FILE_OPEN_NO_RECALL };
    let options = kind | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT | no_recall;
    let mut handle = ptr::null_mut();
    let mut block = IoStatusBlock::default();
    // SAFETY: The folder handle, the name and the attributes all outlive the call, and the
    // handle and status block are writable.
    let status = unsafe {
        NtCreateFile(
            &raw mut handle,
            FILE_GENERIC_READ,
            &raw const attributes,
            &raw mut block,
            ptr::null(),
            0,
            SHARE_ALL,
            FILE_OPEN,
            options,
            ptr::null(),
            0,
        )
    };
    check(status)?;
    // SAFETY: A successful create returns a handle this call now owns.
    let file = unsafe { File::from_raw_handle(handle) };
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::from_raw_os_error(ERROR_REPARSE_POINT_ENCOUNTERED));
    }
    Ok(file)
}

/// Opens a relative path one component at a time, so no link on the way can redirect it.
fn walk(
    root: &File,
    path: &Path,
    open: impl FnOnce(&File, &OsStr) -> Option<(File, FileVersion)>,
) -> Option<(File, FileVersion)> {
    let mut parts = path.components().peekable();
    let mut folder: Option<File> = None;
    while let Some(part) = parts.next() {
        let Component::Normal(name) = part else { return None };
        let dir = folder.as_ref().unwrap_or(root);
        if parts.peek().is_none() {
            return open(dir, name);
        }
        folder = Some(open_at(dir, name, true).ok()?);
    }
    None
}

pub(super) fn version(root: &File, path: &Path) -> Option<FileVersion> {
    walk(root, path, |dir, name| {
        let file = open_at(dir, name, false).ok()?;
        let version = FileVersion::opened(&file)?;
        Some((file, version))
    })
    .map(|(_, version)| version)
}

pub(super) fn open(root: &File, path: &Path) -> Option<(File, FileVersion)> {
    walk(root, path, |dir, name| {
        let file = open_at(dir, name, false).ok()?;
        let version = FileVersion::opened(&file)?;
        Some((file, version))
    })
}

struct Listed {
    name: OsString,
    attributes: u32,
}

impl Listed {
    fn is_dir(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 && self.attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
    }

    fn is_file(&self) -> bool {
        self.attributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) == 0
    }
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Every name in a folder except `.` and `..`, handed over as each batch is read so that stopping
/// early also stops the reading.
fn list(dir: &File, mut each: impl FnMut(Listed) -> ControlFlow<()>) -> io::Result<()> {
    let mut buffer = vec![0_u64; 8192];
    let size = u32::try_from(buffer.len() * size_of::<u64>()).unwrap_or(0);
    let mut restart = 1;
    loop {
        let mut block = IoStatusBlock::default();
        // SAFETY: The folder handle is live, and the buffer is writable for the length given.
        let status = unsafe {
            NtQueryDirectoryFile(
                dir.as_raw_handle(),
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                &raw mut block,
                buffer.as_mut_ptr().cast(),
                size,
                FILE_DIRECTORY_INFORMATION,
                0,
                ptr::null(),
                restart,
            )
        };
        restart = 0;
        if status == STATUS_NO_MORE_FILES {
            return Ok(());
        }
        check(status)?;
        // SAFETY: The buffer is initialized, and its bytes are only read while it is borrowed.
        let bytes =
            unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), buffer.len() * size_of::<u64>()) };
        let mut at = 0;
        loop {
            let (Some(next), Some(attributes), Some(length)) =
                (le_u32(bytes, at), le_u32(bytes, at + 56), le_u32(bytes, at + 60))
            else {
                return Err(io::ErrorKind::InvalidData.into());
            };
            let end = at + NAMES_AT + length as usize;
            let units = bytes.get(at + NAMES_AT..end).ok_or(io::ErrorKind::InvalidData)?;
            let name: Vec<u16> = units.as_chunks::<2>().0.iter().map(|&pair| u16::from_le_bytes(pair)).collect();
            let name = OsString::from_wide(&name);
            if name != "." && name != ".." && each(Listed { name, attributes }).is_break() {
                return Ok(());
            }
            if next == 0 {
                break;
            }
            at += next as usize;
        }
    }
}

impl Scan {
    pub(super) fn directory(&mut self, dir: &File, tree: &Tree, path: &mut PathBuf, depth: usize) {
        if depth < 3 && matches!(tree, Tree::Codex) {
            let mut names = Vec::new();
            let listed = list(dir, |entry| {
                if entry.is_dir() && tree.directory(entry.name.as_encoded_bytes(), depth) {
                    names.push(entry.name);
                }
                ControlFlow::Continue(())
            });
            self.unreadable(listed);
            names.sort_unstable_by(|a, b| b.as_encoded_bytes().cmp(a.as_encoded_bytes()));
            for name in names {
                if !self.descend(dir, &name, tree, path, depth) {
                    break;
                }
            }
            return;
        }
        let listed = list(dir, |entry| {
            let bytes = entry.name.as_encoded_bytes();
            if entry.is_file()
                && tree.file(bytes, depth)
                && let Ok(file) = open_at(dir, &entry.name, false)
                && let Some(version) = FileVersion::opened(&file)
                && tree.includes(version)
            {
                if !self.admit() {
                    return ControlFlow::Break(());
                }
                self.keep(tree.limit(), path, &entry.name, version);
            }
            if entry.is_dir() && tree.directory(bytes, depth) && !self.descend(dir, &entry.name, tree, path, depth) {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        });
        self.unreadable(listed);
    }

    /// A folder that stops answering mid-read leaves the result short, as a refused folder does.
    fn unreadable(&mut self, listed: io::Result<()>) {
        let Err(error) = listed else { return };
        self.partial = true;
        self.inaccessible |= error.kind() == io::ErrorKind::PermissionDenied;
    }

    fn descend(&mut self, dir: &File, name: &OsStr, tree: &Tree, path: &mut PathBuf, depth: usize) -> bool {
        match open_at(dir, name, true) {
            Ok(child) => {
                if !self.admit() {
                    return false;
                }
                path.push(name);
                self.directory(&child, tree, path, depth + 1);
                path.pop();
            }
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                self.inaccessible = true;
                self.partial = true;
            }
            Err(_) => {}
        }
        true
    }
}
