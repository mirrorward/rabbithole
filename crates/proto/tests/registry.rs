//! Wire-registry guard tests: the mechanical guard against *accidental*
//! wire-format changes in the `proto` crate.
//!
//! Three assertions, three failure modes they catch:
//!
//! 1. `no_collisions` — two messages sharing one `(family, message_type)` is
//!    a silent routing collision. This makes it a hard error.
//! 2. `count_matches_expected` — the "did you mean to change the wire?"
//!    tripwire: add/remove an `impl Message` and the count drifts from the
//!    checked-in [`EXPECTED`], forcing a conscious update.
//! 3. `golden_matches` — the byte-for-byte snapshot of the whole table.
//!    Any renumber/rename/add/remove shifts it; the author must re-bless.
//!
//! See `crates/proto/src/registry.rs` and `docs/protocol/versioning.md`.

use std::collections::HashMap;
use std::path::PathBuf;

use rabbithole_proto::registry::{golden_text, EXPECTED, REGISTRY};

fn golden_path() -> PathBuf {
    // Relative to this crate, independent of the test's working directory.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("wire-registry.golden")
}

/// No two registered messages may share a `(family, message_type)` routing
/// key — that would be a silent wire collision.
#[test]
fn no_collisions() {
    let mut seen: HashMap<(u8, u16), &str> = HashMap::new();
    let mut collisions = Vec::new();
    for entry in REGISTRY {
        let key = (entry.family.0, entry.message_type);
        if let Some(prev) = seen.insert(key, entry.name) {
            collisions.push(format!(
                "family {} type {} is claimed by BOTH `{}` and `{}`",
                entry.family.0, entry.message_type, prev, entry.name
            ));
        }
    }
    assert!(
        collisions.is_empty(),
        "wire (family, message_type) collision(s) detected — a reused number \
         silently routes one message's bytes into another:\n  {}",
        collisions.join("\n  ")
    );
}

/// The registry length must equal the checked-in [`EXPECTED`] total. This is
/// the completeness tripwire: it fails the moment an `impl Message` is added
/// or removed without a matching, deliberate update here.
#[test]
fn count_matches_expected() {
    assert_eq!(
        REGISTRY.len(),
        EXPECTED,
        "registry holds {} entries but EXPECTED = {}.\n\
         Did you add or remove an `impl Message`? If so:\n  \
         1. add/remove its entry in crates/proto/src/registry.rs,\n  \
         2. update `pub const EXPECTED` to {},\n  \
         3. re-bless the golden (see the golden_matches test).\n\
         This mismatch is the intentional \"did you mean to change the wire?\" checkpoint.",
        REGISTRY.len(),
        EXPECTED,
        REGISTRY.len(),
    );
}

/// The canonical, sorted text form of the whole registry must match the
/// checked-in golden file. Any intentional wire change is re-blessed by
/// regenerating it; an *un*intentional one is caught here.
#[test]
fn golden_matches() {
    let actual = golden_text();
    let path = golden_path();

    // Re-bless path: `BLESS=1 cargo test -p rabbithole-proto --test registry`.
    if std::env::var_os("BLESS").is_some() {
        std::fs::write(&path, &actual).expect("failed to write golden file");
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {} ({e}). Create it with:\n  \
             BLESS=1 cargo test -p rabbithole-proto --test registry golden_matches",
            path.display()
        )
    });

    assert_eq!(
        actual,
        expected,
        "\nthe wire registry no longer matches {}.\n\
         This means a message type was added, removed, renamed, or renumbered.\n\
         If that change is INTENTIONAL, re-bless the golden:\n  \
         BLESS=1 cargo test -p rabbithole-proto --test registry golden_matches\n\
         and commit the updated golden alongside your change. If it is NOT\n\
         intentional, you just caught an accidental wire-format change.\n",
        path.display(),
    );
}
