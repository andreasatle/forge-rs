use super::*;

#[test]
fn search_request_converts_gb_to_mb_and_pins_query_shape() {
    // The Vast.ai bundles endpoint expects gpu_ram in MB and dph_total in
    // USD/hour, wrapped in `{gte: ...}` / `{lte: ...}` range filters.
    let request = build_search_request(40.0, 0.6);
    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains(r#""gpu_ram":{"gte":40960.0}"#), "got: {json}");
    assert!(json.contains(r#""dph_total":{"lte":0.6}"#), "got: {json}");
    assert!(json.contains(r#""type":"ondemand"#), "got: {json}");
    assert!(json.contains(r#""rentable":{"eq":true}"#), "got: {json}");
}

#[test]
fn raw_offer_converts_ram_from_mb_to_gb() {
    let raw = RawOffer {
        id: 42,
        gpu_name: "RTX_4090".to_string(),
        gpu_ram: 24576.0,
        dph_total: 0.35,
        num_gpus: 1,
    };
    let offer = VastOffer::from(raw);
    assert_eq!(offer.id, 42);
    assert_eq!(offer.gpu_ram_gb, 24.0);
    assert_eq!(offer.price_per_hour, 0.35);
}

#[test]
fn raw_instance_defaults_missing_status_to_unknown() {
    let raw = RawInstance {
        id: 7,
        actual_status: None,
        ssh_host: None,
        ssh_port: None,
        gpu_name: "RTX_4090".to_string(),
    };
    let instance = VastInstance::from(raw);
    assert_eq!(instance.status, "unknown");
}

#[test]
fn raw_instance_carries_status_through_when_present() {
    let raw = RawInstance {
        id: 7,
        actual_status: Some("running".to_string()),
        ssh_host: Some("1.2.3.4".to_string()),
        ssh_port: Some(2222),
        gpu_name: "RTX_4090".to_string(),
    };
    let instance = VastInstance::from(raw);
    assert_eq!(instance.status, "running");
}

#[test]
fn resolve_ssh_url_builds_url_when_host_and_port_present() {
    let instance = VastInstance {
        id: 7,
        status: "running".to_string(),
        ssh_host: Some("1.2.3.4".to_string()),
        ssh_port: Some(2222),
        gpu_name: "RTX_4090".to_string(),
    };
    let url = resolve_ssh_url(7, &instance).unwrap();
    assert_eq!(url, "ssh://root@1.2.3.4:2222");
}

#[test]
fn resolve_ssh_url_errors_when_not_ready() {
    let instance = VastInstance {
        id: 7,
        status: "loading".to_string(),
        ssh_host: None,
        ssh_port: None,
        gpu_name: "RTX_4090".to_string(),
    };
    let err = resolve_ssh_url(7, &instance).unwrap_err();
    assert!(matches!(err, VastError::Http(_)));
}

#[test]
fn vast_error_display_is_human_readable() {
    let cases = [
        (VastError::ApiKeyMissing, "VAST_API_KEY is not set"),
        (
            VastError::Http("HTTP 404: not found".to_string()),
            "Vast.ai API request failed: HTTP 404: not found",
        ),
        (
            VastError::Parse("unexpected EOF".to_string()),
            "Vast.ai API response parse error: unexpected EOF",
        ),
    ];
    for (err, expected) in cases {
        assert_eq!(err.to_string(), expected);
    }
}
