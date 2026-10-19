use serde::Serialize;
use serde_json::Value;

const MAX_MODEL_COUNT: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableModel {
    pub slug: String,
    pub display_name: String,
}

pub fn filter_listed_models(response: &Value) -> Result<Vec<AvailableModel>, String> {
    let models = response
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| "AUTH_MODEL_CATALOG_INVALID".to_owned())?;
    if models.len() > MAX_MODEL_COUNT {
        return Err("AUTH_MODEL_CATALOG_TOO_LARGE".to_owned());
    }
    Ok(models
        .iter()
        .filter(|model| model.get("visibility").and_then(Value::as_str) == Some("list"))
        .filter_map(|model| {
            let slug = model.get("slug")?.as_str()?;
            let display_name = model.get("display_name")?.as_str()?;
            (valid_slug(slug) && !display_name.trim().is_empty() && display_name.len() <= 200).then(
                || AvailableModel {
                    slug: slug.to_owned(),
                    display_name: display_name.to_owned(),
                },
            )
        })
        .collect())
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 160
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_model_serializes_display_name_for_the_typescript_client() {
        let model = AvailableModel {
            slug: "gpt-example".to_owned(),
            display_name: "GPT Example".to_owned(),
        };

        let serialized = serde_json::to_value(model).unwrap();

        assert_eq!(serialized["slug"], "gpt-example");
        assert_eq!(serialized["displayName"], "GPT Example");
        assert!(serialized.get("display_name").is_none());
    }
}
