mod support;
#[test]
fn static_with_ftp_disabled() {
    assert!(
        curl::Version::get()
            .protocols()
            .filter(|&p| p == "ftp")
            .next()
            .is_none()
    );
}
