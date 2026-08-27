use rust_decimal::Decimal;
use tukro_backend::utils::{
    compute_hash, validate_decimal_positive_or_zero, validate_trimmed_string, verify_hash,
};

#[test]
fn validate_decimal_positive_or_zero_works() {
    assert!(validate_decimal_positive_or_zero(&Decimal::new(10, 0)).is_ok());
    assert!(validate_decimal_positive_or_zero(&Decimal::new(0, 0)).is_ok());
    assert!(validate_decimal_positive_or_zero(&Decimal::new(-1, 0)).is_err());
    assert!(validate_decimal_positive_or_zero(&Decimal::new(1, 5)).is_err());
    assert!(validate_decimal_positive_or_zero(&Decimal::from(1_000_000_000)).is_err());
}

#[test]
fn validate_trimmed_string_works() {
    assert!(validate_trimmed_string("  hello  ").is_ok());
    assert!(validate_trimmed_string("hello").is_ok());
    let err = validate_trimmed_string("   ").unwrap_err();
    assert_eq!(err.code, "empty_or_whitespace");
}

#[test]
fn hash_and_verify_password_works() {
    let password = "my-secure-password".to_string();
    let hash = compute_hash(password.clone()).expect("Failed to compute hash");
    assert!(verify_hash(hash, password).is_ok());
}

#[test]
fn verify_wrong_password_fails() {
    let password = "my-secure-password".to_string();
    let wrong_password = "not-my-password".to_string();
    let hash = compute_hash(password).expect("Failed to compute hash");
    assert!(verify_hash(hash, wrong_password).is_err());
}
