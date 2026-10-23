use std::path::PathBuf;

use lorin_explorer::core::{
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
        Err(lorin_explorer::core::LocationError::UnsafeLocation)
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

#[test]
fn parallel_non_local_locations_survive_repeated_construction_and_teardown() {
    // gvfs can resurrect a freed `GMountSpec` from its global `unique_hash`, so
    // building a non-local `Location` while another thread drops one for the
    // same mount corrupted the heap. Without the retained mount this aborts or
    // segfaults the whole binary within a few thousand cycles.
    let mut threads = Vec::new();
    for index in 0..8 {
        threads.push(std::thread::spawn(move || {
            for cycle in 0..8_000 {
                let location =
                    Location::from_uri(&format!("smb://parallel/share/{index}/{cycle}")).unwrap();
                assert!(location.local_path().is_none());
            }
        }));
    }
    for thread in threads {
        thread.join().expect("no worker may corrupt native memory");
    }
}

#[test]
fn an_archive_is_addressed_by_its_own_uri_escaped_twice() {
    // gvfs mounts an archive under a host that is the archive's URI, escaped
    // once to fit in an authority and again because parsing unescapes it. The
    // canonical form has to come back byte for byte, or it names a mount that
    // does not exist -- and the archive's own path is case-sensitive, so the
    // host is the one authority that must not be lowercased.
    let archive = Location::from_uri("file:///home/user/Downloads/Sample.zip").unwrap();
    let inside = archive.archive_contents().unwrap();
    assert_eq!(
        inside.uri(),
        "archive://file%253A%252F%252F%252Fhome%252Fuser%252FDownloads%252FSample.zip/"
    );
    assert!(inside.is_inside_archive());
    assert_eq!(Location::from_uri(inside.uri()).unwrap(), inside);
    assert_eq!(inside.local_path(), None, "an archive is not a local path");
}

#[test]
fn a_hostname_is_still_folded_while_an_opaque_authority_is_not() {
    assert_eq!(
        Location::from_uri("SMB://HOST/Share").unwrap().uri(),
        "smb://host/Share"
    );
    assert_eq!(
        Location::from_uri("archive://file%253A%252F%252F%252FA%252FB.zip/")
            .unwrap()
            .uri(),
        "archive://file%253A%252F%252F%252FA%252FB.zip/"
    );
}

#[test]
fn a_location_in_an_archive_is_named_rather_than_redacted() {
    let archive = Location::from_uri("file:///home/user/Backups/photos 2025.tar.gz").unwrap();
    let root = archive.archive_contents().unwrap();
    assert_eq!(root.display_name(), "photos 2025.tar.gz");
    let inner = Location::from_uri(&format!("{}Holiday/Day%201", root.uri())).unwrap();
    assert_eq!(inner.display_name(), "Day 1");
}

#[test]
fn leaving_an_archive_returns_to_the_folder_it_is_in() {
    // GIO stops at a mount root, so Up inside an archive had nowhere to go.
    let archive = Location::from_uri("file:///home/user/Backups/photos.zip").unwrap();
    let root = archive.archive_contents().unwrap();
    assert_eq!(
        root.parent().map(|parent| parent.uri().to_owned()),
        Some("file:///home/user/Backups".to_owned())
    );
    let inner = Location::from_uri(&format!("{}Holiday", root.uri())).unwrap();
    assert_eq!(inner.parent().as_ref(), Some(&root));
}
