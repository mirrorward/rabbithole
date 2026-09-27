//! Hand-pinned RFC 8949 v1 interchange fixtures and identity compatibility.
use rabbithole_swarm::{
    Manifest, ManifestCborError, ManifestFile, RabbitLink, CHUNK_SIZE, MAX_MANIFEST_CBOR_BYTES,
    MAX_MANIFEST_CBOR_FILES,
};

fn sample() -> Manifest {
    Manifest::new("m", vec![ManifestFile::new("a", 1, [0; 32], "")])
}

// {0: 1, 1: "m", 2: 1048576, 3: [["a", 1, h'00...00', ""]]}
// CBOR major types and arguments are hand-derived from RFC 8949 §3/§4.2.1,
// rather than generated from this codec. Integers/lengths use shortest forms.
#[rustfmt::skip]
const CBOR: &[u8] = &[
    0xa4,                   // map, four fields
    0x00, 0x01,             // key 0: version 1
    0x01, 0x61, 0x6d,       // key 1: name "m"
    0x02, 0x1a, 0x00, 0x10, 0x00, 0x00, // key 2: u32 chunk size 1048576
    0x03, 0x81,             // key 3: one file
    0x84,                   // four-element file record
    0x61, 0x61,             // path "a"
    0x01,                   // size 1
    0x58, 0x20,             // root: byte string, 32 bytes
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0x60,                   // MIME: empty text string
];

// The already-published 42-byte postcard fixture from wire_golden.rs. CBOR
// interchange must preserve this exact content-addressing input.
#[rustfmt::skip]
const POSTCARD: &[u8] = &[
    0x01, 0x6d, 0x80, 0x80, 0x40, 0x01, 0x01, 0x61, 0x01,
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0,
    0,
];

#[test]
fn pinned_cbor_and_postcard_preserve_the_same_manifest_and_link() {
    let manifest = sample();
    assert_eq!(manifest.encode_cbor().unwrap(), CBOR);
    assert_eq!(Manifest::decode_cbor(CBOR).unwrap(), manifest);
    let postcard = Manifest::decode(POSTCARD).unwrap();
    let back = Manifest::decode_cbor(&postcard.encode_cbor().unwrap()).unwrap();
    assert_eq!(back.encode_cbor().unwrap(), CBOR);
    assert_eq!(back.encode(), POSTCARD);
    assert_eq!(manifest.id(), *blake3::hash(POSTCARD).as_bytes());
    assert_eq!(back.id(), postcard.id());
    assert_ne!(
        back.id(),
        *blake3::hash(CBOR).as_bytes(),
        "CBOR bytes are not the link's hash input"
    );
    let link = RabbitLink::manifest("warren.example", None, postcard.id());
    assert_eq!(
        RabbitLink::manifest("warren.example", None, back.id()),
        link
    );
}

#[test]
fn integer_boundaries_use_the_pinned_shortest_forms() {
    let cases: &[(u64, &[u8])] = &[
        (0, &[0x00]),
        (23, &[0x17]),
        (24, &[0x18, 0x18]),
        (255, &[0x18, 0xff]),
        (256, &[0x19, 0x01, 0x00]),
        (65535, &[0x19, 0xff, 0xff]),
        (65536, &[0x1a, 0x00, 0x01, 0x00, 0x00]),
        (u32::MAX as u64, &[0x1a, 0xff, 0xff, 0xff, 0xff]),
        (u32::MAX as u64 + 1, &[0x1b, 0, 0, 0, 1, 0, 0, 0, 0]),
        (
            u64::MAX,
            &[0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        ),
    ];
    for &(size, encoded) in cases {
        let mut manifest = sample();
        manifest.files[0].size = size;
        let mut expected = CBOR.to_vec();
        expected.splice(17..18, encoded.iter().copied());
        assert_eq!(manifest.encode_cbor().unwrap(), expected, "{size}");
        assert_eq!(Manifest::decode_cbor(&expected).unwrap(), manifest);
    }
    for chunk_size in [0, 23, 24, 255, 256, 65535, 65536, CHUNK_SIZE, u32::MAX] {
        let mut manifest = sample();
        manifest.chunk_size = chunk_size;
        assert_eq!(
            Manifest::decode_cbor(&manifest.encode_cbor().unwrap()).unwrap(),
            manifest
        );
    }
}

#[test]
fn path_order_duplicates_and_unicode_are_lossless_not_normalized() {
    let paths = [
        "z/nested.txt",
        "a",
        "a",
        "é",
        "e\u{301}",
        "../x",
        "/absolute",
        "C:\\x",
        "",
        "nul\0name",
    ];
    let manifest = Manifest {
        name: "Release 🍄\0".into(),
        chunk_size: 123,
        files: paths
            .iter()
            .enumerate()
            .map(|(i, path)| ManifestFile::new(*path, i as u64, [i as u8; 32], "type/文"))
            .collect(),
    };
    let postcard = manifest.encode();
    let cbor = manifest.encode_cbor().unwrap();
    let back = Manifest::decode_cbor(&cbor).unwrap();
    assert_eq!(back, manifest);
    assert_eq!(back.encode(), postcard);
    assert_eq!(back.id(), manifest.id());
    assert_eq!(back.encode_cbor().unwrap(), cbor);
    assert_ne!(
        Manifest::new(&manifest.name, manifest.files.clone()).id(),
        manifest.id(),
        "sorting here would have silently changed identity"
    );
}

#[test]
fn empty_filesets_and_zero_length_files_roundtrip() {
    for manifest in [
        Manifest::new("", vec![]),
        Manifest::new("empty", vec![ManifestFile::new("empty", 0, [0xff; 32], "")]),
    ] {
        let back = Manifest::decode_cbor(&manifest.encode_cbor().unwrap()).unwrap();
        assert_eq!(back, manifest);
        assert_eq!(back.id(), manifest.id());
    }
}

fn changed(at: usize, value: u8) -> Vec<u8> {
    let mut bytes = CBOR.to_vec();
    bytes[at] = value;
    bytes
}

#[test]
fn rejects_invalid_schema_types_versions_and_trailing_data() {
    let malformed = [
        changed(0, 0xa3),  // missing field
        changed(0, 0xa5),  // extra field
        changed(1, 1),     // reordered/unknown first key
        changed(6, 1),     // duplicate key
        changed(12, 4),    // unknown key
        changed(4, 0x41),  // bytes instead of text name
        changed(5, 0xff),  // invalid UTF-8 name
        changed(7, 0x20),  // negative chunk size
        changed(13, 0xa1), // files map instead of array
        changed(14, 0x83), // incomplete file record
        changed(14, 0x85), // extra file field
        changed(15, 0x41), // byte-string path
        changed(16, 0xff), // invalid UTF-8 path
        changed(17, 0x20), // negative size
        changed(17, 0xf9), // floating size
        changed(18, 0x78), // text root
        changed(18, 0x98), // integer-array root
        changed(19, 31),   // short root
        changed(19, 33),   // long root
        changed(52, 0x40), // byte-string MIME
    ];
    for bytes in malformed {
        assert!(
            Manifest::decode_cbor(&bytes).is_err(),
            "accepted {}",
            hex::encode(bytes)
        );
    }
    assert_eq!(
        Manifest::decode_cbor(&changed(2, 2)),
        Err(ManifestCborError::Version(2))
    );
    let mut overflow = CBOR.to_vec();
    overflow.splice(7..12, [0x1b, 0, 0, 0, 1, 0, 0, 0, 0]);
    assert!(Manifest::decode_cbor(&overflow).is_err());
    for suffix in [vec![0], CBOR.to_vec()] {
        let mut bytes = CBOR.to_vec();
        bytes.extend(suffix);
        assert!(Manifest::decode_cbor(&bytes).is_err());
    }
}

#[test]
fn rejects_non_deterministic_encodings_and_unbounded_declared_lengths() {
    // Numerically identical arguments represented with too many bytes.
    for (range, replacement) in [
        (0..1, vec![0xb8, 4]),                          // map length
        (1..2, vec![0x18, 0]),                          // map key
        (2..3, vec![0x18, 1]),                          // version
        (4..5, vec![0x78, 1]),                          // text length
        (7..12, vec![0x1b, 0, 0, 0, 0, 0, 0x10, 0, 0]), // chunk size
        (13..14, vec![0x98, 1]),                        // files length
        (14..15, vec![0x98, 4]),                        // record length
        (17..18, vec![0x18, 1]),                        // file size
        (18..20, vec![0x59, 0, 32]),                    // root length
    ] {
        let mut bytes = CBOR.to_vec();
        bytes.splice(range, replacement);
        assert!(
            Manifest::decode_cbor(&bytes).is_err(),
            "accepted {}",
            hex::encode(bytes)
        );
    }
    for (at, value) in [(0, 0xbf), (4, 0x7f), (13, 0x9f), (14, 0x9f), (18, 0x5f)] {
        assert!(
            Manifest::decode_cbor(&changed(at, value)).is_err(),
            "indefinite item at {at}"
        );
    }
    let mut tagged = vec![0xd9, 0xd9, 0xf7];
    tagged.extend(CBOR);
    assert!(
        Manifest::decode_cbor(&tagged).is_err(),
        "even the self-describe tag is outside v1"
    );
    for at in [4, 13, 15, 18, 52] {
        let mut bytes = CBOR.to_vec();
        let major = bytes[at] & 0xe0;
        bytes.splice(
            at..at + 1,
            [major | 27, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        );
        assert!(
            Manifest::decode_cbor(&bytes).is_err(),
            "huge declared length at {at}"
        );
    }
}

#[test]
fn byte_and_file_limits_are_checked_on_both_sides() {
    let mut manifest = Manifest {
        name: "n".repeat(MAX_MANIFEST_CBOR_BYTES - 13),
        chunk_size: 0,
        files: vec![],
    };
    let bytes = manifest.encode_cbor().unwrap();
    assert_eq!(bytes.len(), MAX_MANIFEST_CBOR_BYTES);
    assert_eq!(Manifest::decode_cbor(&bytes).unwrap(), manifest);
    manifest.name.push('n');
    assert_eq!(
        manifest.encode_cbor(),
        Err(ManifestCborError::Limit("byte length"))
    );
    let mut too_big = bytes;
    too_big.push(0);
    assert_eq!(
        Manifest::decode_cbor(&too_big),
        Err(ManifestCborError::Limit("byte length"))
    );

    let file = ManifestFile::new("", 0, [0; 32], "");
    let mut manifest = Manifest::new("", vec![file.clone(); MAX_MANIFEST_CBOR_FILES]);
    let bytes = manifest.encode_cbor().unwrap();
    assert_eq!(Manifest::decode_cbor(&bytes).unwrap(), manifest);
    manifest.files.push(file);
    assert_eq!(
        manifest.encode_cbor(),
        Err(ManifestCborError::Limit("file count"))
    );
    // Replace one file with a declared count of 65,537, with no big allocation.
    let mut declared = CBOR.to_vec();
    declared.splice(13..14, [0x9a, 0x00, 0x01, 0x00, 0x01]);
    assert_eq!(
        Manifest::decode_cbor(&declared),
        Err(ManifestCborError::Limit("file count"))
    );
}

#[test]
fn truncations_bitflips_and_random_inputs_are_total_and_accepted_forms_are_fixed_points() {
    for cut in 0..CBOR.len() {
        assert!(
            Manifest::decode_cbor(&CBOR[..cut]).is_err(),
            "truncation at {cut}"
        );
    }
    for at in 0..CBOR.len() {
        for bit in 0..8 {
            let mut bytes = CBOR.to_vec();
            bytes[at] ^= 1 << bit;
            if let Ok(manifest) = Manifest::decode_cbor(&bytes) {
                assert_eq!(manifest.encode_cbor().unwrap(), bytes);
            }
        }
    }
    let mut state = 0x71_cb07_u64;
    // The fixed LCG makes every malformed-input run reproducible.
    for length in 0..512 {
        for _ in 0..16 {
            let mut bytes = Vec::with_capacity(length);
            for _ in 0..length {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                bytes.push((state >> 32) as u8);
            }
            if let Ok(manifest) = Manifest::decode_cbor(&bytes) {
                assert_eq!(manifest.encode_cbor().unwrap(), bytes);
            }
        }
    }
}
