use std::path::PathBuf;

use zorin_explorer::core::{
    application_preferences::{
        decode_session, decode_session_state, encode_session, import_legacy_local_paths,
        RecoverableStateNotice, UriStateV2,
    },
    Location,
};

#[test]
fn rejects_unsafe_uri_components_without_exposing_them() {
    for uri in [
        "smb://user@host/share",
        "smb://host/share?token=secret",
        "smb://host/share#secret",
        "smb://host/%",
        "smb://host/%zz",
        "smb://host/line\nbreak",
    ] {
        assert!(Location::from_uri(uri).is_err());
    }
}

#[test]
fn canonicalizes_uri_structure_without_decoding_reserved_bytes() {
    assert_eq!(
        Location::from_uri("SMB://HOST/%7e").unwrap().uri(),
        "smb://host/~"
    );
    assert_eq!(
        Location::from_uri("http://HOST:80/a").unwrap().uri(),
        "http://host/a"
    );
    assert_eq!(
        Location::from_uri("smb://host/a%2fb").unwrap().uri(),
        "smb://host/a%2Fb"
    );
    assert_ne!(
        Location::from_uri("smb://host/a%2Fb").unwrap(),
        Location::from_uri("smb://host/a/b").unwrap(),
    );
}

#[test]
fn redacts_remote_path_details() {
    assert_eq!(
        Location::from_uri("sftp://host/private/name")
            .unwrap()
            .redacted(),
        "sftp://host/…"
    );
}

#[test]
fn untrusted_gio_file_input_is_fallible_without_panicking() {
    let untrusted = gio::File::for_uri("smb://user@host/share");
    assert_eq!(
        Location::try_from_file(untrusted),
        Err(zorin_explorer::core::LocationError::UnsafeLocation)
    );
}

#[test]
fn session_codec_round_trips_remote_locations_and_skips_bad_records() {
    let remote = Location::from_uri("smb://host/share").unwrap();
    let bytes = encode_session(&[remote.clone()]).unwrap();
    assert_eq!(decode_session(&bytes).unwrap(), vec![remote]);

    let bytes = serde_json::to_vec(&UriStateV2 {
        schema_version: 2,
        locations: vec!["sftp://host/ok".into(), "smb://user@host/no".into()],
    })
    .unwrap();
    let decoded = decode_session_state(&bytes).unwrap();
    assert_eq!(decoded.locations.len(), 1);
    assert_eq!(decoded.locations[0].uri(), "sftp://host/ok");
    assert_eq!(
        decoded.notices,
        vec![RecoverableStateNotice::MalformedRecord]
    );
}

#[test]
fn v1_persisted_json_migrates_and_a_subsequent_write_is_v2() {
    let v1 = br#"{"locations":["file:///tmp/legacy","/tmp/bare"]}"#;
    let migrated = decode_session(v1).unwrap();
    assert_eq!(migrated[0].uri(), "file:///tmp/legacy");
    assert_eq!(migrated[1].uri(), "file:///tmp/bare");
    let v2: UriStateV2 = serde_json::from_slice(&encode_session(&migrated).unwrap()).unwrap();
    assert_eq!(v2.schema_version, 2);
    assert_eq!(v2.locations, vec!["file:///tmp/legacy", "file:///tmp/bare"]);
}

#[test]
fn legacy_local_path_importer_is_idempotent() {
    let paths = vec![PathBuf::from("/tmp/one"), PathBuf::from("/tmp/two")];
    let first = import_legacy_local_paths(&paths);
    let second = import_legacy_local_paths(&paths);
    assert_eq!(first, second);
    assert_eq!(first[0].uri(), "file:///tmp/one");
}
