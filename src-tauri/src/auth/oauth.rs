use jsonwebtoken::{
    Algorithm, DecodingKey, Validation, decode, decode_header,
    jwk::{AlgorithmParameters, JwkSet, KeyAlgorithm, KeyOperations, PublicKeyUse},
};
use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::Zeroizing;

const AUTH_ISSUER: &str = "https://auth.openai.com";
const MAX_CALLBACK_TARGET_BYTES: usize = 16 * 1024;
const MAX_CALLBACK_PARAMETERS: usize = 32;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkRoute {
    #[default]
    SystemProxy,
    Direct,
}

#[derive(Clone, PartialEq, Eq)]
pub enum AuthorizationResult {
    Code {
        code: Zeroizing<String>,
        client_id: String,
    },
    Denied,
}

impl fmt::Debug for AuthorizationResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code { client_id, .. } => formatter
                .debug_struct("Code")
                .field("code", &"[redacted]")
                .field("client_id", client_id)
                .finish(),
            Self::Denied => formatter.write_str("Denied"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedIdentity {
    pub subject: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Deserialize)]
struct IdTokenClaims {
    iss: String,
    aud: serde_json::Value,
    sub: String,
    exp: i64,
    iat: i64,
    nbf: Option<i64>,
    nonce: String,
    azp: Option<String>,
    email: Option<String>,
    email_verified: Option<bool>,
    name: Option<String>,
}

pub fn pkce_challenge(verifier: &str) -> Result<String, String> {
    if !(43..=128).contains(&verifier.len())
        || !verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
    {
        return Err("AUTH_PKCE_VERIFIER_INVALID".to_owned());
    }
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())))
}

pub fn parse_callback_target(
    request_target: &str,
    expected_state: &str,
    registered_client_id: Option<&str>,
) -> Result<AuthorizationResult, String> {
    if request_target.len() > MAX_CALLBACK_TARGET_BYTES
        || !request_target.starts_with("/auth/callback?")
        || request_target.contains('#')
    {
        return Err("AUTH_CALLBACK_TARGET_INVALID".to_owned());
    }
    let query = request_target
        .split_once('?')
        .map(|(_, query)| query)
        .filter(|query| !query.is_empty())
        .ok_or_else(|| "AUTH_CALLBACK_PARAMETERS_INVALID".to_owned())?;
    let mut parameters = std::collections::BTreeMap::<String, Zeroizing<String>>::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if parameters.len() >= MAX_CALLBACK_PARAMETERS && !parameters.contains_key(key.as_ref()) {
            return Err("AUTH_CALLBACK_TOO_MANY_PARAMETERS".to_owned());
        }
        if parameters
            .insert(key.into_owned(), Zeroizing::new(value.into_owned()))
            .is_some()
        {
            return Err("AUTH_CALLBACK_DUPLICATE_PARAMETER".to_owned());
        }
    }

    if parameters.get("state").map(|state| state.as_str()) != Some(expected_state) {
        return Err("AUTH_CALLBACK_STATE_MISMATCH".to_owned());
    }
    if parameters.contains_key("error") {
        return if parameters
            .get("error")
            .is_some_and(|error| error.as_str() == "access_denied")
        {
            Ok(AuthorizationResult::Denied)
        } else {
            Err("AUTH_CALLBACK_OAUTH_ERROR".to_owned())
        };
    }
    let code = parameters
        .get("code")
        .filter(|value| !value.is_empty() && value.len() <= 8192)
        .ok_or_else(|| "AUTH_CALLBACK_CODE_MISSING".to_owned())?
        .as_str()
        .to_owned();
    let client_id = match registered_client_id {
        Some(registered) => {
            if !valid_issued_client_id(registered) {
                return Err("AUTH_CLIENT_ID_INVALID".to_owned());
            }
            match parameters.get("client_id") {
                Some(returned) if returned.as_str() != registered => {
                    return Err("AUTH_CALLBACK_CLIENT_ID_MISMATCH".to_owned());
                }
                _ => registered.to_owned(),
            }
        }
        None => parameters
            .get("client_id")
            .filter(|client_id| valid_issued_client_id(client_id))
            .ok_or_else(|| "AUTH_CALLBACK_ISSUED_CLIENT_ID_MISSING".to_owned())?
            .as_str()
            .to_owned(),
    };
    Ok(AuthorizationResult::Code {
        code: Zeroizing::new(code),
        client_id,
    })
}

pub fn validate_id_token(
    token: &str,
    expected_client_id: &str,
    expected_nonce: &str,
    jwks: &JwkSet,
    now_unix: i64,
) -> Result<VerifiedIdentity, String> {
    if !valid_issued_client_id(expected_client_id) || token.is_empty() || token.len() > 64 * 1024 {
        return Err("AUTH_ID_TOKEN_INVALID".to_owned());
    }
    let header = decode_header(token).map_err(|_| "AUTH_ID_TOKEN_INVALID".to_owned())?;
    if header.alg != Algorithm::RS256
        || header.typ.as_deref().is_some_and(|value| value != "JWT")
        || header
            .crit
            .as_ref()
            .is_some_and(|critical| !critical.is_empty())
        || header.jku.is_some()
        || header.jwk.is_some()
        || header.x5u.is_some()
    {
        return Err("AUTH_ID_TOKEN_ALGORITHM_INVALID".to_owned());
    }
    let key_id = header
        .kid
        .as_deref()
        .ok_or_else(|| "AUTH_ID_TOKEN_KEY_ID_MISSING".to_owned())?;
    let jwk = jwks
        .keys
        .iter()
        .find(|key| key.common.key_id.as_deref() == Some(key_id))
        .ok_or_else(|| "AUTH_ID_TOKEN_KEY_UNKNOWN".to_owned())?;
    if !matches!(&jwk.algorithm, AlgorithmParameters::RSA(_))
        || jwk
            .common
            .public_key_use
            .as_ref()
            .is_some_and(|usage| usage != &PublicKeyUse::Signature)
        || jwk
            .common
            .key_operations
            .as_ref()
            .is_some_and(|operations| !operations.contains(&KeyOperations::Verify))
        || jwk
            .common
            .key_algorithm
            .is_some_and(|algorithm| algorithm != KeyAlgorithm::RS256)
    {
        return Err("AUTH_ID_TOKEN_KEY_INVALID".to_owned());
    }
    let decoding_key =
        DecodingKey::from_jwk(jwk).map_err(|_| "AUTH_ID_TOKEN_KEY_INVALID".to_owned())?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[AUTH_ISSUER]);
    validation.set_audience(&[expected_client_id]);
    validation.validate_exp = false;
    validation.validate_nbf = false;
    let claims = decode::<IdTokenClaims>(token, &decoding_key, &validation)
        .map_err(|_| "AUTH_ID_TOKEN_SIGNATURE_OR_CLAIMS_INVALID".to_owned())?
        .claims;

    let (audience_matches, audience_count) = if let Some(audience) = claims.aud.as_str() {
        (audience == expected_client_id, 1)
    } else if let Some(audiences) = claims.aud.as_array() {
        (
            audiences
                .iter()
                .any(|audience| audience.as_str() == Some(expected_client_id)),
            audiences.len(),
        )
    } else {
        (false, 0)
    };
    let authorized_party_matches = match claims.azp.as_deref() {
        Some(authorized_party) => authorized_party == expected_client_id,
        None => audience_count <= 1,
    };
    if claims.iss != AUTH_ISSUER
        || !audience_matches
        || !authorized_party_matches
        || claims.exp <= now_unix
        || claims.iat > now_unix.saturating_add(60)
        || claims.nbf.is_some_and(|not_before| not_before > now_unix)
        || claims.nonce != expected_nonce
        || claims.sub.trim().is_empty()
        || claims.sub.len() > 512
    {
        return Err("AUTH_ID_TOKEN_CLAIMS_INVALID".to_owned());
    }
    Ok(VerifiedIdentity {
        subject: claims.sub,
        email: claims
            .email_verified
            .unwrap_or(false)
            .then_some(claims.email)
            .flatten()
            .filter(|email| email.len() <= 320),
        display_name: claims
            .name
            .filter(|name| !name.trim().is_empty() && name.len() <= 160),
    })
}

fn valid_issued_client_id(client_id: &str) -> bool {
    client_id.starts_with("oaiapp_")
        && client_id.len() <= 512
        && client_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}
