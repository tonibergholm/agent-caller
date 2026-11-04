//! Windows owner-only file checks. Type-checked from Linux
//! (`cargo check --target x86_64-pc-windows-msvc`); never run there. See
//! `docs/windows.md` for the manual checks.
//!
//! Open: `FILE_FLAG_OPEN_REPARSE_POINT` so a symlink or junction in the last
//! component is opened itself and refused, not followed. Then, on the open
//! handle (so nothing can be swapped between check and use), the owner SID and
//! the DACL are read and judged by `winacl::judge`. Anything that cannot be
//! read or understood is an error.
use std::{
    fs::File,
    io::{Read, Take},
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle},
    },
    path::Path,
    ptr::null_mut,
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, LocalFree, HANDLE, INVALID_HANDLE_VALUE},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
        },
        GetAce, GetAclInformation, GetTokenInformation, AclSizeInformation, IsValidSid, ACL,
        ACL_SIZE_INFORMATION, ACCESS_ALLOWED_ACE, ACE_HEADER,
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        SECURITY_ATTRIBUTES, TokenUser, TOKEN_QUERY, TOKEN_USER,
    },
    Storage::FileSystem::{
        CreateDirectoryW, CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_READ_DATA,
        FILE_SHARE_READ, READ_CONTROL,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

use crate::{
    winacl::{self, Ace, AceKind, Verdict},
    SecureFsError,
};

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PATH_NOT_FOUND: i32 = 3;
const ERROR_DIRECTORY: i32 = 267;
/// ACCESS_ALLOWED_ACE_TYPE (winnt.h); windows-sys has no constant for it.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

fn io(path: &Path, e: std::io::Error) -> SecureFsError {
    SecureFsError::Io(path.into(), e)
}

fn denied(path: &Path, why: &'static str) -> SecureFsError {
    SecureFsError::Io(
        path.into(),
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, why),
    )
}

fn win32(code: u32) -> std::io::Error {
    std::io::Error::from_raw_os_error(code as i32)
}

fn wide(s: &std::ffi::OsStr) -> Option<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let mut v: Vec<u16> = s.encode_wide().collect();
    if v.contains(&0) {
        return None;
    }
    v.push(0);
    Some(v)
}

/// Frees a `LocalAlloc`ed block on drop.
struct LocalBlock(*mut core::ffi::c_void);
impl Drop for LocalBlock {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from an API documented to allocate with LocalAlloc.
            unsafe { LocalFree(self.0) };
        }
    }
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle we opened and own.
        unsafe { CloseHandle(self.0) };
    }
}

/// `S-1-...` string for a SID the caller has checked with `IsValidSid`.
fn sid_to_string(sid: PSID) -> Option<String> {
    let mut out: *mut u16 = null_mut();
    // SAFETY: sid is a valid SID; out receives a LocalAlloc'd NUL-terminated string.
    if unsafe { ConvertSidToStringSidW(sid, &mut out) } == 0 || out.is_null() {
        return None;
    }
    let _guard = LocalBlock(out.cast());
    // SAFETY: out is NUL terminated.
    let len = (0..).take_while(|&i| unsafe { *out.add(i) } != 0).count();
    let s = String::from_utf16(unsafe { std::slice::from_raw_parts(out, len) }).ok()?;
    winacl::valid_sid_string(&s).then_some(s)
}

/// The SID of the user this process runs as, from the process token.
pub fn current_user_sid() -> Result<String, std::io::Error> {
    let mut token: HANDLE = null_mut();
    // SAFETY: GetCurrentProcess is a pseudo handle; token receives a new handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(win32(unsafe { GetLastError() }));
    }
    let token = Handle(token);
    let mut need = 0u32;
    // SAFETY: a size query with a null buffer.
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut need) };
    if need == 0 || need > 4096 {
        return Err(win32(unsafe { GetLastError() }));
    }
    // u64 elements: the buffer is 8-byte aligned, as TOKEN_USER needs.
    let mut buf = vec![0u64; (need as usize).div_ceil(8)];
    // SAFETY: buf holds at least `need` bytes.
    if unsafe { GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), need, &mut need) } == 0 {
        return Err(win32(unsafe { GetLastError() }));
    }
    // SAFETY: the call filled a TOKEN_USER at the start of buf.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(std::io::Error::other("the process token has no valid user SID"));
    }
    sid_to_string(sid).ok_or_else(|| std::io::Error::other("the user SID could not be read"))
}

fn open_path(path: &Path, directory: bool) -> Result<File, SecureFsError> {
    let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
    if directory {
        flags |= FILE_FLAG_BACKUP_SEMANTICS; // needed to open a directory at all
    }
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL)
        .custom_flags(flags)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => SecureFsError::Missing(path.into()),
            Some(ERROR_DIRECTORY) => SecureFsError::NotRegular(path.into()),
            _ => io(path, e),
        })?;
    Ok(file)
}

/// Reads the DACL entries of a security descriptor held in `acl`.
///
/// # Safety
/// `acl` is null or points at a valid ACL that outlives the call.
unsafe fn read_aces(acl: *const ACL) -> Option<Vec<Ace>> {
    if acl.is_null() {
        return None;
    }
    let mut info = ACL_SIZE_INFORMATION { AceCount: 0, AclBytesInUse: 0, AclBytesFree: 0 };
    // SAFETY: info is the documented output size for AclSizeInformation.
    let ok = unsafe {
        GetAclInformation(
            acl,
            (&raw mut info).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    let unreadable = || Some(vec![Ace { kind: AceKind::Other, sid: String::new() }]);
    if ok == 0 {
        return unreadable();
    }
    let mut out = Vec::new();
    for i in 0..info.AceCount {
        let mut p: *mut core::ffi::c_void = null_mut();
        // SAFETY: i < AceCount.
        if unsafe { GetAce(acl, i, &mut p) } == 0 || p.is_null() {
            return unreadable();
        }
        // SAFETY: GetAce returned a pointer to an ACE inside the ACL.
        let kind = unsafe { (*p.cast::<ACE_HEADER>()).AceType };
        if kind != ACCESS_ALLOWED_ACE_TYPE {
            out.push(Ace { kind: AceKind::Other, sid: String::new() });
            continue;
        }
        // SAFETY: an access-allowed ACE; its SID starts at SidStart.
        let sid: PSID = unsafe { (&raw mut (*p.cast::<ACCESS_ALLOWED_ACE>()).SidStart).cast() };
        let s = if unsafe { IsValidSid(sid) } != 0 { sid_to_string(sid) } else { None };
        out.push(Ace { kind: AceKind::Allow, sid: s.unwrap_or_default() });
    }
    Some(out)
}

/// Owner SID and DACL entries of an open handle.
fn security_of(file: &File, path: &Path) -> Result<(String, Option<Vec<Ace>>), SecureFsError> {
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: the handle is open; out pointers are valid; sd is freed below.
    let rc = unsafe {
        GetSecurityInfo(
            file.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    let _guard = LocalBlock(sd);
    if rc != 0 {
        return Err(io(path, win32(rc)));
    }
    if owner.is_null() || unsafe { IsValidSid(owner) } == 0 {
        return Err(SecureFsError::Owner(path.into()));
    }
    let owner = sid_to_string(owner).ok_or_else(|| SecureFsError::Owner(path.into()))?;
    // SAFETY: dacl is null or points into sd, which lives until _guard drops.
    let aces = unsafe { read_aces(dacl) };
    Ok((owner, aces))
}

fn verify(file: &File, path: &Path, directory: bool, require_private_dacl: bool) -> Result<(), SecureFsError> {
    let m = file.metadata().map_err(|e| io(path, e))?;
    let attrs = m.file_attributes();
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(SecureFsError::Symlink(path.into()));
    }
    let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
    if is_dir != directory || (!directory && !m.is_file()) {
        return Err(SecureFsError::NotRegular(path.into()));
    }
    let user = current_user_sid().map_err(|e| io(path, e))?;
    let (owner, aces) = security_of(file, path)?;
    let verdict = if require_private_dacl {
        winacl::judge(&owner, &user, aces.as_deref())
    } else if owner.eq_ignore_ascii_case(&user) {
        Verdict::Ok
    } else {
        Verdict::Owner
    };
    match verdict {
        Verdict::Ok => Ok(()),
        Verdict::Owner => Err(SecureFsError::Owner(path.into())),
        Verdict::Open(why) => Err(denied(path, why)),
    }
}

/// A directory owned by the current user whose DACL grants only the current
/// user. `_max_mode` has no meaning on Windows; the rule is the strictest one.
pub fn check_private_dir(dir: &Path, _max_mode: u32) -> Result<(), SecureFsError> {
    let f = open_path(dir, true)?;
    verify(&f, dir, true, true)
}

/// As `check_private_dir`, for a regular file (not a reparse point).
pub fn check_private_file(path: &Path, _max_mode: u32) -> Result<(), SecureFsError> {
    let f = open_path(path, false)?;
    verify(&f, path, false, true)
}

/// Opens a regular file owned by the current user, with no DACL requirement,
/// so a caller can inspect and repair it through the returned handle.
pub fn open_owned_file(path: &Path) -> Result<File, SecureFsError> {
    let f = open_path(path, false)?;
    verify(&f, path, false, false)?;
    Ok(f)
}

pub fn read_private(path: &Path, _max_mode: u32, max_len: u64) -> Result<Vec<u8>, SecureFsError> {
    let mut file = open_path(path, false)?;
    verify(&file, path, false, true)?;
    let mut reader: Take<&mut File> = (&mut file).take(max_len.saturating_add(1));
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|e| io(path, e))?;
    if bytes.len() as u64 > max_len {
        return Err(SecureFsError::TooLarge(path.into()));
    }
    Ok(bytes)
}

/// A SECURITY_ATTRIBUTES for `sddl`, and the block that backs it.
fn attributes(sddl: &str) -> Result<(SECURITY_ATTRIBUTES, LocalBlock), std::io::Error> {
    let w = wide(sddl.as_ref()).ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: w is NUL terminated; sd receives a LocalAlloc'd descriptor.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, null_mut())
    } == 0
    {
        return Err(win32(unsafe { GetLastError() }));
    }
    let guard = LocalBlock(sd);
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    Ok((sa, guard))
}

/// Creates one directory (the parent must exist) owned by the user, with a
/// protected DACL granting only the user, inherited by its children. Fails if
/// the path exists. The result is verified before it is returned.
pub fn create_private_dir(path: &Path) -> Result<(), SecureFsError> {
    let user = current_user_sid().map_err(|e| io(path, e))?;
    let sddl = winacl::private_dir_sddl(&user).ok_or_else(|| denied(path, "the user SID is not usable"))?;
    let (sa, _sd) = attributes(&sddl).map_err(|e| io(path, e))?;
    let w = wide(path.as_os_str()).ok_or_else(|| denied(path, "the path contains a NUL"))?;
    // SAFETY: w is NUL terminated; sa and its descriptor live through the call.
    if unsafe { CreateDirectoryW(w.as_ptr(), &sa) } == 0 {
        return Err(io(path, win32(unsafe { GetLastError() })));
    }
    check_private_dir(path, 0o700)
}

/// `mkdir -p`: creates each missing directory on the way with
/// `create_private_dir`, and requires the final one to pass
/// `check_private_dir`, whether it was created now or already existed.
/// Existing ancestors are not judged: they are the user's profile.
pub fn create_private_dir_all(path: &Path) -> Result<(), SecureFsError> {
    let mut missing: Vec<&Path> = Vec::new();
    for a in path.ancestors() {
        if a.as_os_str().is_empty() || std::fs::symlink_metadata(a).is_ok() {
            break;
        }
        missing.push(a);
    }
    for a in missing.into_iter().rev() {
        match create_private_dir(a) {
            Ok(()) => {}
            // Lost a race with another creator: judged below like any existing dir.
            Err(SecureFsError::Io(_, e)) if e.raw_os_error() == Some(183) => {}
            Err(e) => return Err(e),
        }
    }
    check_private_dir(path, 0o700)
}

/// Creates a new file (fails if it exists) owned by the user with a protected
/// DACL granting only the user, opened for writing and verified.
pub fn create_private_file(path: &Path) -> Result<File, SecureFsError> {
    let user = current_user_sid().map_err(|e| io(path, e))?;
    let sddl = winacl::private_file_sddl(&user).ok_or_else(|| denied(path, "the user SID is not usable"))?;
    let (sa, _sd) = attributes(&sddl).map_err(|e| io(path, e))?;
    let w = wide(path.as_os_str()).ok_or_else(|| denied(path, "the path contains a NUL"))?;
    // SAFETY: w is NUL terminated; sa and its descriptor live through the call.
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            FILE_SHARE_READ,
            &sa,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io(path, win32(unsafe { GetLastError() })));
    }
    // SAFETY: h is a newly created handle that File now owns.
    let file = unsafe { File::from_raw_handle(h as _) };
    verify(&file, path, false, true)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_user_sid_is_a_valid_sid_string() {
        let sid = current_user_sid().unwrap();
        assert!(winacl::valid_sid_string(&sid), "{sid}");
    }

    #[test]
    fn a_created_dir_and_file_pass_and_a_loosened_one_fails() {
        let base = std::env::temp_dir().join(format!("acd-securefs-win-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("a").join("b");
        create_private_dir_all(&dir).unwrap();
        check_private_dir(&dir, 0o700).unwrap();
        let f = dir.join("client.token");
        {
            use std::io::Write;
            let mut h = create_private_file(&f).unwrap();
            h.write_all(b"secret").unwrap();
        }
        check_private_file(&f, 0o600).unwrap();
        assert_eq!(read_private(&f, 0o600, 16).unwrap(), b"secret");
        assert!(matches!(read_private(&f, 0o600, 3), Err(SecureFsError::TooLarge(_))));
        assert!(create_private_file(&f).is_err(), "CREATE_NEW must not overwrite");
        // A file created the ordinary way inherits the profile's ACL, which
        // names SYSTEM and Administrators: refused.
        let plain = std::env::temp_dir().join(format!("acd-securefs-win-plain-{}", std::process::id()));
        std::fs::write(&plain, b"x").unwrap();
        let r = check_private_file(&plain, 0o600);
        let _ = std::fs::remove_file(&plain);
        assert!(r.is_err(), "{r:?}");
        assert!(matches!(check_private_file(&base.join("nope"), 0o600), Err(SecureFsError::Missing(_))));
        let _ = std::fs::remove_dir_all(&base);
    }

    // Junctions are covered by the manual checklist (docs/windows.md, item 10).
    #[test]
    fn a_file_symlink_is_refused() {
        let base = std::env::temp_dir().join(format!("acd-securefs-win-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        create_private_dir_all(&base).unwrap();
        let real = base.join("real");
        drop(create_private_file(&real).unwrap());
        let link = base.join("link");
        // Needs Developer Mode or elevation; skip rather than pass falsely.
        if std::os::windows::fs::symlink_file(&real, &link).is_err() {
            return;
        }
        assert!(matches!(check_private_file(&link, 0o600), Err(SecureFsError::Symlink(_))));
        let _ = std::fs::remove_dir_all(&base);
    }
}
