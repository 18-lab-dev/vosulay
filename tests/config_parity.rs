use std::fs;
use std::path::Path;

use vosulay::config::AppConfig;

#[test]
fn rust_config_normalization_matches_shared_golden_result() {
    let fixture_directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("windows/test/fixtures");
    let input = fs::read(fixture_directory.join("config-parity-input.json"))
        .expect("shared config parity input should be readable");
    let expected = fs::read(fixture_directory.join("config-parity-expected.json"))
        .expect("shared config parity result should be readable");

    let mut config: AppConfig =
        serde_json::from_slice(&input).expect("shared config parity input should deserialize");
    config.normalize();
    let actual = serde_json::to_value(config).expect("normalized config should serialize");
    let expected: serde_json::Value =
        serde_json::from_slice(&expected).expect("shared config parity result should deserialize");

    assert_eq!(actual, expected);
}
