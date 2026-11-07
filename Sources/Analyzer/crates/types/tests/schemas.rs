use mixlib_types::*;
#[test]
fn schema_and_semantics_positive_negative_fixtures() {
    let good: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/beats.valid.json")).unwrap();
    validate_value::<Beats>(&good).unwrap();
    let bad: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/beats.duplicate.invalid.json"
    ))
    .unwrap();
    assert_eq!(
        validate_value::<Beats>(&bad).unwrap_err().code,
        "BEAT_ORDER"
    );
    let mut missing = good.clone();
    missing["downbeat_ids"] = serde_json::json!(["missing"]);
    assert_eq!(
        validate_value::<Beats>(&missing).unwrap_err().code,
        "DOWNBEAT"
    );
    let mut malformed = good;
    malformed["events"] = serde_json::json!([null]);
    assert!(validate_value::<Beats>(&malformed).is_err());
    assert_eq!(
        schema::<Beats>()["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
}
