//! The representable subset of Hotline access. A native capability may cover
//! several classic bits; partial groups are rejected rather than rounded up.

use super::{AccessMask, Privilege, Role};
use rabbithole_server_core::Caps;
use rabbithole_store_server::repo::{Account, AccountAccess};

pub(super) const GROUPS: &[(Caps, &[Privilege])] = &[
    (Caps::WHO, &[Privilege::GetClientInfo]),
    (Caps::CHAT_READ, &[Privilege::ReadChat]),
    (Caps::CHAT_SEND, &[Privilege::SendChat]),
    (Caps::CHAT_CREATE_ROOM, &[Privilege::OpenChat]),
    (Caps::CHAT_MODERATE, &[Privilege::CloseChat]),
    (Caps::DM_SEND, &[Privilege::SendPrivateMessages]),
    (Caps::BOARD_READ, &[Privilege::NewsReadArticle]),
    (Caps::BOARD_POST, &[Privilege::NewsPostArticle]),
    (
        Caps::BOARD_MODERATE,
        &[
            Privilege::NewsDeleteArticle,
            Privilege::NewsCreateCategory,
            Privilege::NewsDeleteCategory,
            Privilege::NewsCreateFolder,
            Privilege::NewsDeleteFolder,
        ],
    ),
    (Caps::FILE_DOWNLOAD, &[Privilege::DownloadFiles]),
    (Caps::FILE_UPLOAD, &[Privilege::UploadFiles]),
    (
        Caps::FILE_MANAGE,
        &[
            Privilege::DeleteFiles,
            Privilege::RenameFiles,
            Privilege::MoveFiles,
            Privilege::CreateFolders,
            Privilege::DeleteFolders,
            Privilege::RenameFolders,
            Privilege::MoveFolders,
            Privilege::UploadAnywhere,
            Privilege::SetFileComment,
            Privilege::SetFolderComment,
            Privilege::MakeAliases,
        ],
    ),
    (Caps::DROPBOX_VIEW, &[Privilege::ViewDropBoxes]),
    (Caps::USER_KICK, &[Privilege::DisconnectUsers]),
    (Caps::CANNOT_BE_KICKED, &[Privilege::CannotBeDisconnected]),
    (
        Caps::ACCOUNT_ADMIN,
        &[
            Privilege::CreateUsers,
            Privilege::DeleteUsers,
            Privilege::OpenUsers,
            Privilege::ModifyUsers,
        ],
    ),
    (Caps::BROADCAST, &[Privilege::Broadcast]),
];

pub(super) fn mapped_caps() -> u64 {
    GROUPS.iter().fold(0, |mask, (cap, _)| mask | cap.0)
}

pub(super) fn requested_caps(mask: &AccessMask) -> Result<u64, &'static str> {
    // These are identity policy indicators, not adjustable native rights.
    let mut supported: AccessMask = [
        Privilege::ShowInList,
        Privilege::ChangeOwnPassword,
        Privilege::AnyName,
    ]
    .into_iter()
    .collect();
    let mut caps = 0;
    for (cap, bits) in GROUPS {
        let count = bits.iter().filter(|bit| mask.has(**bit)).count();
        if count != 0 && count != bits.len() {
            return Err("access bits in a shared permission group must agree");
        }
        if count > 0 {
            caps |= cap.0;
        }
        for bit in *bits {
            supported.grant(*bit);
        }
    }
    if (0..64).any(|bit| mask.bit(bit) && !supported.bit(bit)) {
        return Err("unsupported access bit");
    }
    Ok(caps)
}

/// Compile the requested native rights into role/class overrides. When
/// changing roles, unrelated native rights retain their previous effective
/// values; promoting a Hotline bit must not also grant CONFIG_ADMIN, etc.
pub(super) fn overrides(
    role: Role,
    class_id: Option<i64>,
    class_mask: u64,
    desired: u64,
    previous: Option<&Account>,
) -> AccountAccess {
    let base = role.default_caps().0 | class_mask;
    let (grant, revoke) = previous
        .map(|a| {
            (
                a.grant_mask & !mapped_caps(),
                a.revoke_mask & !mapped_caps(),
            )
        })
        .unwrap_or_default();
    AccountAccess {
        role: role as u8,
        class_id,
        // Once explicitly edited, mapped choices remain exact even when a
        // shared class later adds/removes the same capability. Other native
        // class behavior keeps its normal inheritance rules.
        grant_mask: grant | (desired & mapped_caps()) | (desired & !base),
        revoke_mask: revoke | (mapped_caps() & !desired) | (base & !desired),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotline::access_mask_for;
    use rabbithole_server_core::Subject;

    #[test]
    fn every_supported_combination_roundtrips_without_extra_capabilities() {
        // Every subset of the 17 native groups, not just role-shaped masks.
        for subset in 0..(1 << GROUPS.len()) {
            let wanted = GROUPS.iter().enumerate().fold(0, |caps, (n, (cap, _))| {
                caps | if subset & (1 << n) != 0 { cap.0 } else { 0 }
            });
            let wire = access_mask_for(Role::User, wanted);
            assert_eq!(requested_caps(&wire), Ok(wanted));
        }
    }

    #[test]
    fn partial_groups_and_unsupported_bits_are_refused() {
        for (_, bits) in GROUPS.iter().filter(|(_, bits)| bits.len() > 1) {
            for bit in *bits {
                let mask: AccessMask = [*bit].into_iter().collect();
                assert!(requested_caps(&mask).is_err(), "{bit:?}");
                let mut mask: AccessMask = bits.iter().copied().collect();
                mask.revoke(*bit);
                assert!(requested_caps(&mask).is_err(), "missing {bit:?}");
            }
        }
        for bit in std::iter::once(Privilege::NoAgreement.bit()).chain(38..64) {
            let mut mask = AccessMask::NONE;
            mask.set_bit(bit, true);
            assert!(requested_caps(&mask).is_err());
        }
    }

    #[test]
    fn explicit_mapped_choices_survive_later_class_changes() {
        let wanted = Caps::FILE_DOWNLOAD.0 | Caps::BROADCAST.0;
        let edit = overrides(Role::User, Some(1), 0, wanted, None);
        for class_mask in [0, u64::MAX] {
            let subject = Subject {
                account_id: 1,
                role: Role::User,
                class_id: edit.class_id,
                class_mask,
                grant_mask: edit.grant_mask,
                revoke_mask: edit.revoke_mask,
            };
            assert_eq!(subject.base_caps() & mapped_caps(), wanted);
        }
    }

    #[test]
    fn role_changes_preserve_unmapped_native_rights_and_explicit_overrides() {
        let old = Account {
            id: 1,
            login: "alice".into(),
            phc: None,
            screen_name: "alice".into(),
            role: Role::User as u8,
            class_id: None,
            grant_mask: Caps::AUDIT_READ.0,
            revoke_mask: Caps::DOOR_RUN.0,
            disabled: false,
        };
        let old_caps = (Role::User.default_caps().0 | old.grant_mask) & !old.revoke_mask;
        let desired = (old_caps & !mapped_caps()) | Caps::ACCOUNT_ADMIN.0;
        let access = overrides(Role::Admin, Some(2), u64::MAX, desired, Some(&old));
        let subject = Subject {
            account_id: old.id,
            role: Role::Admin,
            class_id: access.class_id,
            class_mask: u64::MAX,
            grant_mask: access.grant_mask,
            revoke_mask: access.revoke_mask,
        };
        assert_eq!(subject.base_caps(), desired);
        assert_eq!(access.grant_mask & Caps::AUDIT_READ.0, Caps::AUDIT_READ.0);
        assert_eq!(access.revoke_mask & Caps::DOOR_RUN.0, Caps::DOOR_RUN.0);
        assert_eq!(subject.base_caps() & Caps::CONFIG_ADMIN.0, 0);
    }
}
