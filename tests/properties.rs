use zorin_explorer::core::PermissionBits;

#[test]
fn permission_bits_round_trip_unix_mode() {
    let bits = PermissionBits::from_mode(0o754);

    assert!(bits.owner_read);
    assert!(bits.owner_write);
    assert!(bits.owner_execute);
    assert_eq!(bits.to_mode(), 0o754);
}
