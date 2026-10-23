//! POSIX ACL xattrs (linux/posix_acl_xattr.h) for Android's shared storage.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rustix::fs::XattrFlags;

const ACCESS: &str = "system.posix_acl_access";
const DEFAULT: &str = "system.posix_acl_default";
const VERSION: u32 = 2;
const NO_ID: u32 = u32::MAX;
const USER_OBJ: u16 = 0x01;
const USER: u16 = 0x02;
const GROUP_OBJ: u16 = 0x04;
const GROUP: u16 = 0x08;
const MASK: u16 = 0x10;
const OTHER: u16 = 0x20;

/// An ACL of `(tag, permissions, id)` entries, given in kernel order.
fn encode(entries: &[(u16, u16, u32)]) -> Vec<u8> {
    let mut output = VERSION.to_le_bytes().to_vec();
    for (tag, permissions, id) in entries {
        output.extend_from_slice(&tag.to_le_bytes());
        output.extend_from_slice(&permissions.to_le_bytes());
        output.extend_from_slice(&id.to_le_bytes());
    }
    output
}

fn bits(mode: u32, shift: u32) -> u16 {
    ((mode >> shift) & 7) as u16
}

fn access_acl(mode: u32, directory: bool, media_group: u32) -> Vec<u8> {
    let (group, media) = (bits(mode, 3), if directory { 7 } else { 6 });
    encode(&[
        (USER_OBJ, bits(mode, 6), NO_ID),
        (GROUP_OBJ, group, NO_ID),
        (GROUP, media, media_group),
        (MASK, group | media, NO_ID),
        (OTHER, bits(mode, 0), NO_ID),
    ])
}

fn default_acl(mode: u32, owner_uid: u32, media_group: u32) -> Vec<u8> {
    encode(&[
        (USER_OBJ, 7, NO_ID),
        (USER, 7, owner_uid),
        (GROUP_OBJ, bits(mode, 3), NO_ID),
        (GROUP, 7, media_group),
        (MASK, 7, NO_ID),
        (OTHER, bits(mode, 0), NO_ID),
    ])
}

/// Whether an encoded ACL has a named-group entry for `gid`.
#[must_use]
pub fn names_group(acl: &[u8], gid: u32) -> bool {
    let (entries, _) = acl.get(4..).unwrap_or_default().as_chunks::<8>();
    entries
        .iter()
        .any(|entry| entry[..2] == GROUP.to_le_bytes() && entry[4..] == gid.to_le_bytes())
}

/// Lets `media_group` read and write `path`, a file or directory the
/// caller owns; a directory also passes that access, and the owner's, to
/// what is created in it. An ACL that already names the group is left
/// alone, so watchers of the folder are not woken.
///
/// # Errors
///
/// `path` is a symlink, belongs to someone else, or its filesystem has no
/// POSIX ACLs.
pub fn grant(path: &Path, media_group: u32) -> io::Result<()> {
    let metadata = path.symlink_metadata()?;
    let uid = rustix::process::getuid().as_raw();
    if metadata.file_type().is_symlink() || metadata.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "not the user's own file",
        ));
    }
    let mut current = [0u8; 512];
    if let Ok(length) = rustix::fs::lgetxattr(path, ACCESS, &mut current)
        && names_group(&current[..length], media_group)
    {
        return Ok(());
    }
    let access = access_acl(metadata.mode(), metadata.is_dir(), media_group);
    rustix::fs::lsetxattr(path, ACCESS, &access, XattrFlags::empty())?;
    if metadata.is_dir() {
        let default = default_acl(metadata.mode(), uid, media_group);
        rustix::fs::lsetxattr(path, DEFAULT, &default, XattrFlags::empty())?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests inspect encoded ACL records")]
mod tests {
    use super::*;

    #[test]
    fn writes_kernel_little_endian_layout_and_preserves_modes() {
        let bytes = access_acl(0o754, true, 1023);
        assert_eq!(bytes.len(), 44);
        assert_eq!(&bytes[..12], &[2, 0, 0, 0, 1, 0, 7, 0, 255, 255, 255, 255]);
        assert_eq!(&bytes[20..28], &[8, 0, 7, 0, 255, 3, 0, 0]);
        assert_eq!(&bytes[36..44], &[32, 0, 4, 0, 255, 255, 255, 255]);
        assert_eq!(access_acl(0o600, false, 1023)[22], 6);
        assert_eq!(default_acl(0o755, 1000, 1023).len(), 52);
    }

    fn entries(path: &Path, name: &str) -> Vec<(u16, u16, u32)> {
        let mut buffer = [0u8; 512];
        let length = rustix::fs::lgetxattr(path, name, &mut buffer).unwrap_or(0);
        let (records, _) = buffer[..length]
            .get(4..)
            .unwrap_or_default()
            .as_chunks::<8>();
        records
            .iter()
            .map(|r| {
                let word = |at: usize| u16::from_le_bytes([r[at], r[at + 1]]);
                (
                    word(0),
                    word(2),
                    u32::from_le_bytes([r[4], r[5], r[6], r[7]]),
                )
            })
            .collect()
    }

    #[test]
    fn grants_media_rw_and_passes_it_on() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("photo.jpg");
        std::fs::write(&file, "x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        match grant(&file, 1023) {
            Err(error)
                if error.raw_os_error() == Some(rustix::io::Errno::OPNOTSUPP.raw_os_error()) =>
            {
                return;
            }
            result => result.unwrap(),
        }
        let access = entries(&file, ACCESS);
        assert!(access.contains(&(0x08, 6, 1023)));
        assert!(access.contains(&(0x20, 4, NO_ID)));
        assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o002, 0);

        grant(dir.path(), 1023).unwrap();
        let defaults = entries(dir.path(), DEFAULT);
        assert!(defaults.contains(&(0x08, 7, 1023)));
        assert!(defaults.contains(&(0x02, 7, rustix::process::getuid().as_raw())));
        let child = dir.path().join("new.txt");
        std::fs::write(&child, "x").unwrap();
        assert!(entries(&child, ACCESS).contains(&(0x08, 7, 1023)));

        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(grant(&link, 1023).is_err());
    }
}
