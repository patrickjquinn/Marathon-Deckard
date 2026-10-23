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
}
