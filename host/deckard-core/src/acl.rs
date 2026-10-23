//! POSIX ACL xattrs (linux/posix_acl_xattr.h) for Android's shared storage.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rustix::fs::XattrFlags;

use crate::Error;

pub const VERSION: u32 = 2;
pub const NO_ID: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u16)]
pub enum Tag {
    Owner = 0x01,
    User = 0x02,
    OwningGroup = 0x04,
    Group = 0x08,
    Mask = 0x10,
    Other = 0x20,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub tag: Tag,
    pub permissions: u16,
    pub id: u32,
}

pub fn encode(entries: &[Entry]) -> Result<Vec<u8>, Error> {
    let mut entries = entries.to_vec();
    entries.sort_by_key(|entry| (entry.tag, entry.id));
    for (index, entry) in entries.iter().enumerate() {
        let named = matches!(entry.tag, Tag::User | Tag::Group);
        if entry.permissions > 7
            || (named == (entry.id == NO_ID))
            || entries[..index]
                .iter()
                .any(|previous| previous.tag == entry.tag && previous.id == entry.id)
        {
            return Err(Error::InvalidAcl);
        }
    }
    for tag in [Tag::Owner, Tag::OwningGroup, Tag::Other] {
        if entries.iter().filter(|entry| entry.tag == tag).count() != 1 {
            return Err(Error::InvalidAcl);
        }
    }
    if entries
        .iter()
        .any(|entry| matches!(entry.tag, Tag::User | Tag::Group))
        && !entries.iter().any(|entry| entry.tag == Tag::Mask)
    {
        return Err(Error::InvalidAcl);
    }
    let mut output = Vec::with_capacity(4 + entries.len() * 8);
    output.extend_from_slice(&VERSION.to_le_bytes());
    for entry in entries {
        output.extend_from_slice(&(entry.tag as u16).to_le_bytes());
        output.extend_from_slice(&entry.permissions.to_le_bytes());
        output.extend_from_slice(&entry.id.to_le_bytes());
    }
    Ok(output)
}

pub fn access(mode: u32, directory: bool, media_group: u32) -> Result<Vec<u8>, Error> {
    let owner = ((mode >> 6) & 7) as u16;
    let group = ((mode >> 3) & 7) as u16;
    let other = (mode & 7) as u16;
    let media = if directory { 7 } else { 6 };
    encode(&[
        Entry {
            tag: Tag::Owner,
            permissions: owner,
            id: NO_ID,
        },
        Entry {
            tag: Tag::OwningGroup,
            permissions: group,
            id: NO_ID,
        },
        Entry {
            tag: Tag::Group,
            permissions: media,
            id: media_group,
        },
        Entry {
            tag: Tag::Mask,
            permissions: group | media,
            id: NO_ID,
        },
        Entry {
            tag: Tag::Other,
            permissions: other,
            id: NO_ID,
        },
    ])
}

pub fn default_acl(mode: u32, owner_uid: u32, media_group: u32) -> Result<Vec<u8>, Error> {
    encode(&[
        Entry {
            tag: Tag::Owner,
            permissions: 7,
            id: NO_ID,
        },
        Entry {
            tag: Tag::User,
            permissions: 7,
            id: owner_uid,
        },
        Entry {
            tag: Tag::OwningGroup,
            permissions: ((mode >> 3) & 7) as u16,
            id: NO_ID,
        },
        Entry {
            tag: Tag::Group,
            permissions: 7,
            id: media_group,
        },
        Entry {
            tag: Tag::Mask,
            permissions: 7,
            id: NO_ID,
        },
        Entry {
            tag: Tag::Other,
            permissions: (mode & 7) as u16,
            id: NO_ID,
        },
    ])
}

const ACCESS: &str = "system.posix_acl_access";
const DEFAULT: &str = "system.posix_acl_default";

/// Whether an encoded ACL has a named-group entry for `gid`.
#[must_use]
pub fn names_group(acl: &[u8], gid: u32) -> bool {
    let (entries, _) = acl.get(4..).unwrap_or_default().as_chunks::<8>();
    entries.iter().any(|entry| {
        entry[..2] == (Tag::Group as u16).to_le_bytes() && entry[4..] == gid.to_le_bytes()
    })
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
    let mode = metadata.mode();
    let invalid = |_| io::Error::from(io::ErrorKind::InvalidInput);
    let access = access(mode, metadata.is_dir(), media_group).map_err(invalid)?;
    rustix::fs::lsetxattr(path, ACCESS, &access, XattrFlags::empty())?;
    if metadata.is_dir() {
        let default = default_acl(mode, uid, media_group).map_err(invalid)?;
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
        let bytes = access(0o754, true, 1023).unwrap();
        assert_eq!(bytes.len(), 44);
        assert_eq!(&bytes[..12], &[2, 0, 0, 0, 1, 0, 7, 0, 255, 255, 255, 255]);
        assert_eq!(&bytes[20..28], &[8, 0, 7, 0, 255, 3, 0, 0]);
        assert_eq!(&bytes[36..44], &[32, 0, 4, 0, 255, 255, 255, 255]);
        assert_eq!(access(0o600, false, 1023).unwrap()[22], 6);
        assert_eq!(default_acl(0o755, 1000, 1023).unwrap().len(), 52);
    }

    #[test]
    fn rejects_missing_base_entries_duplicate_entries_and_bad_permissions() {
        assert_eq!(encode(&[]), Err(Error::InvalidAcl));
        let entry = Entry {
            tag: Tag::Owner,
            permissions: 7,
            id: NO_ID,
        };
        assert_eq!(encode(&[entry, entry]), Err(Error::InvalidAcl));
        assert_eq!(
            encode(&[Entry {
                permissions: 8,
                ..entry
            }]),
            Err(Error::InvalidAcl)
        );
        assert!(access(0o755, true, NO_ID).is_err());
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
