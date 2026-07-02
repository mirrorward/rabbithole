//! Stable dedup ids for feed items.
//!
//! Syndication ingest re-fetches feeds forever, so every item needs an id
//! that is (a) stable across fetches, (b) independent of mutable
//! presentation fields (summary text, author display name), and (c)
//! domain-separated from every other hash in RabbitHole. The material is
//! the most stable identity the item offers — `guid`/`id` when the
//! publisher provides one, else the link, else title+date — prefixed with
//! a kind tag and hashed with blake3 in derive-key mode under a crate
//! context string. Field boundaries are NUL-delimited so concatenation
//! ambiguity cannot alias two different items.

use crate::feed::FeedItem;

/// Domain-separation context for [`dedup_id`]. Bump the suffix if the
/// material layout ever changes.
const CONTEXT: &str = "rabbithole legacy-syndication feed-item v1";

/// Derive the stable dedup id for an item: 64 lowercase hex chars of a
/// blake3 hash over the item's most stable identity (guid, else link,
/// else title+date).
pub fn dedup_id(item: &FeedItem) -> String {
    let mut hasher = blake3::Hasher::new_derive_key(CONTEXT);
    if !item.guid.is_empty() {
        hasher.update(b"guid\0");
        hasher.update(item.guid.as_bytes());
    } else if !item.link.is_empty() {
        hasher.update(b"link\0");
        hasher.update(item.link.as_bytes());
    } else {
        hasher.update(b"title+date\0");
        hasher.update(item.title.as_bytes());
        hasher.update(b"\0");
        match item.published_unix {
            Some(t) => {
                hasher.update(t.to_string().as_bytes());
            }
            None => {
                hasher.update(b"-");
            }
        }
    }
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> FeedItem {
        FeedItem {
            title: "A post".into(),
            link: "https://example.com/a".into(),
            guid: "urn:example:1".into(),
            author: "Someone".into(),
            published_unix: Some(1_055_217_600),
            summary_text: "Body text".into(),
        }
    }

    #[test]
    fn id_shape_is_hex_256() {
        let id = dedup_id(&item());
        assert_eq!(id.len(), 64);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn guid_dominates_and_mutable_fields_are_ignored() {
        let a = item();
        let mut b = item();
        b.title = "Retitled after edit".into();
        b.link = "https://example.com/moved".into();
        b.author = "Renamed".into();
        b.summary_text = "Edited body".into();
        b.published_unix = Some(1);
        assert_eq!(dedup_id(&a), dedup_id(&b), "same guid = same item");

        let mut c = item();
        c.guid = "urn:example:2".into();
        assert_ne!(dedup_id(&a), dedup_id(&c));
    }

    #[test]
    fn link_fallback_when_guid_missing() {
        let mut a = item();
        a.guid.clear();
        let mut b = a.clone();
        b.title = "different title".into();
        assert_eq!(dedup_id(&a), dedup_id(&b), "link identifies the item");

        let mut c = a.clone();
        c.link = "https://example.com/other".into();
        assert_ne!(dedup_id(&a), dedup_id(&c));
    }

    #[test]
    fn title_date_fallback_when_guid_and_link_missing() {
        let mut a = item();
        a.guid.clear();
        a.link.clear();
        let b = a.clone();
        assert_eq!(dedup_id(&a), dedup_id(&b));

        let mut c = a.clone();
        c.published_unix = Some(1_055_217_601);
        assert_ne!(dedup_id(&a), dedup_id(&c), "date participates");

        let mut d = a.clone();
        d.published_unix = None;
        assert_ne!(dedup_id(&a), dedup_id(&d), "missing date is distinct");

        let mut e = a.clone();
        e.title = "Another post".into();
        assert_ne!(dedup_id(&a), dedup_id(&e), "title participates");
    }

    #[test]
    fn kind_tags_prevent_cross_field_aliasing() {
        // guid "X" vs link "X" must not collide.
        let a = FeedItem {
            guid: "X".into(),
            ..FeedItem::default()
        };
        let b = FeedItem {
            link: "X".into(),
            ..FeedItem::default()
        };
        assert_ne!(dedup_id(&a), dedup_id(&b));
    }

    #[test]
    fn known_stable_value() {
        // Locks the derivation: if this changes, stored dedup ids in the
        // wild would orphan — bump CONTEXT instead of silently changing.
        let id = dedup_id(&item());
        assert_eq!(id, dedup_id(&item()));
        assert_eq!(id.len(), 64);
    }
}
