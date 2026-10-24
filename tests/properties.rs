use lorin_explorer::core::{PermissionBits, PropertyDetails};

#[test]
fn permission_bits_round_trip_unix_mode() {
    let bits = PermissionBits::from_mode(0o754);

    assert!(bits.owner_read);
    assert!(bits.owner_write);
    assert!(bits.owner_execute);
    assert_eq!(bits.to_mode(), 0o754);
}

#[test]
fn property_details_include_owner_and_group() {
    let details = PropertyDetails::load(std::path::Path::new("/tmp")).unwrap();

    assert!(!details.owner.is_empty());
    assert!(!details.group.is_empty());
}
