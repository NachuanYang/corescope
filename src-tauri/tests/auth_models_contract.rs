use corescope_lib::auth::models::filter_listed_models;
use serde_json::json;

#[test]
fn model_catalog_preserves_server_order_and_exposes_only_listed_slugs() {
    let response = json!({"models":[
        {"slug":"model-z", "display_name":"Model Z", "visibility":"list"},
        {"slug":"hidden", "display_name":"Hidden", "visibility":"internal"},
        {"slug":"model-a", "display_name":"Model A", "visibility":"list"},
        {"slug":"bad name", "display_name":"Bad", "visibility":"list"},
        {"display_name":"No slug", "visibility":"list"}
    ]});
    let models = filter_listed_models(&response).unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model.slug.as_str())
            .collect::<Vec<_>>(),
        ["model-z", "model-a"]
    );
    assert_eq!(models[0].display_name, "Model Z");
}

#[test]
fn malformed_model_catalog_fails_closed() {
    assert!(filter_listed_models(&json!({"items":[]})).is_err());
    assert!(filter_listed_models(&json!({"models":"unexpected"})).is_err());
}
