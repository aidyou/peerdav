use peerdav_lib::sync::{crypto, repository_types};

use crypto::RepositoryKeys;
use repository_types::{ChunkerProfile, RepoDescriptor, RepositoryMode};

fn descriptor(id: &str) -> RepoDescriptor {
    RepoDescriptor {
        version: 1,
        repo_id: id.into(),
        mode: RepositoryMode::Encrypted,
        chunker: ChunkerProfile {
            name: "fastcdc".into(),
            version: "5.0.0".into(),
            normalization: "v2020".into(),
            level: 1,
            min: 1_048_576,
            average: 4_194_304,
            max: 8_388_608,
            seed: 0,
        },
    }
}

#[test]
fn wrap_round_trip_and_aad_rejection() {
    let d = descriptor("00112233445566778899aabbccddeeff");
    let (keys, wrapped) = RepositoryKeys::create(&d, b"password").expect("create");
    let unlocked = RepositoryKeys::unlock(&d, &wrapped, b"password").expect("unlock");
    assert!(RepositoryKeys::unlock(&d, &wrapped, b"wrong").is_err());
    let frame = keys.seal_block(b"hello").expect("seal");
    assert_eq!(
        unlocked.open_block(&frame).expect("open").as_slice(),
        b"hello"
    );
    assert_ne!(keys.seal_block(b"hello").expect("seal").nonce, frame.nonce);
}

#[test]
fn metadata_blocks_and_head_reject_tampering_and_cross_purpose() {
    let d = descriptor("00112233445566778899aabbccddeeff");
    let (keys, _) = RepositoryKeys::create(&d, b"password").expect("create");
    let block = keys.seal_block(b"sensitive-content-marker").expect("block");
    assert_eq!(block.repo_id, d.repo_id);
    assert_eq!(
        block.block_id,
        Some(hex::encode(
            keys.block_id(b"sensitive-content-marker").expect("ID")
        ))
    );
    let mut broken = block.clone();
    for length in [0, 1, 16, 30] {
        broken.ciphertext = vec![0; length];
        assert!(keys.open_block(&broken).is_err());
    }
    for field in [
        "nonce",
        "ciphertext",
        "repo_id",
        "block_id",
        "kind",
        "version",
    ] {
        let mut broken = block.clone();
        match field {
            "nonce" => broken.nonce = "00".repeat(24),
            "ciphertext" => broken.ciphertext[0] ^= 1,
            "repo_id" => broken.repo_id = "ff".repeat(16),
            "block_id" => broken.block_id = Some("ff".repeat(32)),
            "kind" => broken.kind = "tree".into(),
            "version" => broken.version = 2,
            _ => unreachable!(),
        }
        assert!(keys.open_block(&broken).is_err(), "field {field}");
    }
    assert_ne!(
        keys.block_id(b"abc").expect("ID"),
        keys.block_id(b"abc\0").expect("ID")
    );
    for kind in ["tree", "commit"] {
        let envelope = keys
            .seal_metadata(kind, b"private-file-name-marker")
            .expect("metadata");
        assert_eq!(
            keys.open_metadata(&envelope, kind)
                .expect("open")
                .as_slice(),
            b"private-file-name-marker"
        );
        let other = if kind == "tree" { "commit" } else { "tree" };
        assert!(keys.open_metadata(&envelope, other).is_err());
        let mut changed = envelope.clone();
        changed.kind = other.into();
        assert!(keys.open_metadata(&changed, other).is_err());
        let encoded = envelope.to_wire().expect("encode");
        assert!(!encoded.windows(b"private-file-name-marker".len()).any(|window| window == b"private-file-name-marker"));
        let decoded = repository_types::Envelope::from_wire(&encoded).expect("wire roundtrip");
        assert_eq!(keys.open_metadata(&decoded, kind).expect("open wire").as_slice(), b"private-file-name-marker");
    }
    let (other, _) =
        RepositoryKeys::create(&descriptor("ffeeddccbbaa99887766554433221100"), b"password")
            .expect("other repo");
    assert!(other.open_block(&block).is_err());
    let tag = keys.sign_head(b"protected HEAD fields").expect("tag");
    keys.verify_head(b"protected HEAD fields", &tag)
        .expect("verify");
    assert!(keys.verify_head(b"modified HEAD fields", &tag).is_err());
    assert!(
        keys.verify_head(b"protected HEAD fields", &tag[..31])
            .is_err()
    );
}

#[test]
fn kdf_and_descriptor_limits_reject_before_expensive_derivation() {
    let d = descriptor("00112233445566778899aabbccddeeff");
    let (_, wrapped) = RepositoryKeys::create(&d, b"password").expect("create");
    let mut changed = wrapped.clone();
    changed.kdf.memory_kib = u32::MAX;
    assert!(matches!(
        RepositoryKeys::unlock(&d, &changed, b"password"),
        Err(crypto::CryptoError::Input(_))
    ));
    changed = wrapped.clone();
    changed.nonce = "a".repeat(4096);
    assert!(RepositoryKeys::unlock(&d, &changed, b"password").is_err());
    changed = wrapped.clone();
    changed.ciphertext.clear();
    assert!(RepositoryKeys::unlock(&d, &changed, b"password").is_err());
    let mut wrong = d.clone();
    wrong.chunker.seed = 1;
    assert!(RepositoryKeys::unlock(&wrong, &wrapped, b"password").is_err());
    wrong = d.clone();
    wrong.mode = RepositoryMode::Plain;
    assert!(RepositoryKeys::unlock(&wrong, &wrapped, b"password").is_err());
    assert!(repository_types::hex_exact::<16>(&"aa".repeat(65536)).is_err());
    assert!(repository_types::hex_exact::<16>(&"AA".repeat(16)).is_err());
}

#[test]
fn canonical_wire_rejects_unknown_and_duplicate_fields() {
    let d = descriptor("00112233445566778899aabbccddeeff");
    let (_, wrapped) = RepositoryKeys::create(&d, b"password").expect("create");
    let bytes = repository_types::encode(&wrapped, 64 * 1024).expect("encode");
    assert!(repository_types::decode::<repository_types::WrappedMaster>(&bytes, 64 * 1024).is_ok());
    assert!(
        repository_types::decode::<repository_types::WrappedMaster>(
            br#"{"kdf":{},"salt":"","nonce":"","ciphertext":"","x":1}"#,
            64 * 1024
        )
        .is_err()
    );
    assert!(
        repository_types::decode::<repository_types::WrappedMaster>(
            br#"{"kdf":{},"kdf":{},"salt":"","nonce":"","ciphertext":""}"#,
            64 * 1024
        )
        .is_err()
    );
}
