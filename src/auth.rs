use argon2::{Algorithm, Argon2, Params, Version};
use base64::{
    Engine, alphabet,
    engine::{
        DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig,
        general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD},
    },
};
use chrono::{DateTime, TimeDelta, Utc};
use hmac::{Hmac, Mac};
use rand::{RngCore, rngs::OsRng};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{MapAccess, Visitor},
};
use sha2::{Digest, Sha256};

fn random_bytes(count: usize) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0; count];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| "could not generate random bytes".to_string())?;
    Ok(bytes)
}

pub fn random_token(count: usize) -> Result<String, String> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes(count)?))
}

pub fn identifier() -> Result<String, String> {
    Ok(hex(&random_bytes(16)?))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn token_hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

// Go's Raw{Std,URL}Encoding ignores CR/LF and permits nonzero unused bits.
// Keep signatures canonical: this decoder is only for password data/payloads.
fn decode_go_base64(value: &str, url_safe: bool) -> Result<Vec<u8>, base64::DecodeError> {
    let config = GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::RequireNone)
        .with_decode_allow_trailing_bits(true);
    let engine = GeneralPurpose::new(
        if url_safe {
            &alphabet::URL_SAFE
        } else {
            &alphabet::STANDARD
        },
        config,
    );
    let cleaned: Vec<u8> = value
        .bytes()
        .filter(|byte| !matches!(byte, b'\r' | b'\n'))
        .collect();
    engine.decode(cleaned)
}

pub fn hash_password(password: &str) -> Result<String, String> {
    if password.len() < 8 {
        return Err("password must contain at least 8 characters".into());
    }
    if password.len() > 1024 {
        return Err("password is too long".into());
    }
    let salt = random_bytes(16)?;
    let mut hash = [0; 32];
    let params = Params::new(64 * 1024, 3, 2, Some(32)).map_err(|error| error.to_string())?;
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), &salt, &mut hash)
        .map_err(|error| error.to_string())?;
    Ok(format!(
        "$argon2id$v=19$m=65536,t=3,p=2${}${}",
        STANDARD_NO_PAD.encode(salt),
        STANDARD_NO_PAD.encode(hash)
    ))
}

pub fn verify_password(encoded: &str, password: &str) -> bool {
    let parts: Vec<_> = encoded.split('$').collect();
    if parts.len() != 6 || parts[1] != "argon2id" || parts[2] != "v=19" {
        return false;
    }
    let Some((memory, iterations, threads)) = password_parameters(parts[3]) else {
        return false;
    };
    if memory > 256 * 1024 || iterations > 10 || threads > 16 {
        return false;
    }
    let (Ok(salt), Ok(expected)) = (
        decode_go_base64(parts[4], false),
        decode_go_base64(parts[5], false),
    ) else {
        return false;
    };
    if expected.is_empty() {
        return false;
    }
    let Ok(params) = Params::new(memory, iterations, threads, Some(expected.len())) else {
        return false;
    };
    let mut actual = vec![0; expected.len()];
    if Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), &salt, &mut actual)
        .is_err()
    {
        return false;
    }
    // Use the MAC crate's constant-time verification primitive for equality.
    let mut expected_mac = Hmac::<Sha256>::new_from_slice(b"balance password comparison")
        .expect("HMAC accepts any key size");
    expected_mac.update(&expected);
    let expected_tag = expected_mac.finalize().into_bytes();
    let mut actual_mac = Hmac::<Sha256>::new_from_slice(b"balance password comparison")
        .expect("HMAC accepts any key size");
    actual_mac.update(&actual);
    actual_mac.verify_slice(&expected_tag).is_ok()
}

fn password_parameters(mut text: &str) -> Option<(u32, u32, u32)> {
    fn scan(text: &mut &str, prefix: &str) -> Option<u32> {
        *text = text.strip_prefix(prefix)?;
        *text = text.trim_start_matches(|c: char| c.is_whitespace() && c != '\n');
        let digits = *text;
        let end = digits
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(digits.len());
        let value = digits[..end].parse().ok()?;
        *text = &digits[end..];
        Some(value)
    }
    let memory = scan(&mut text, "m=")?;
    let iterations = scan(&mut text, ",t=")?;
    let threads = scan(&mut text, ",p=")?;
    // fmt.Sscanf stops after its final conversion and accepts a trailing suffix.
    if threads > u8::MAX as u32 {
        return None;
    }
    Some((memory, iterations, threads))
}

#[derive(Clone, Default, Debug, Serialize)]
pub struct Claims {
    #[serde(rename = "sub")]
    pub subject: String,
    pub email: String,
    #[serde(rename = "iss")]
    pub issuer: String,
    #[serde(rename = "iat")]
    pub issued: i64,
    #[serde(rename = "exp")]
    pub expires: i64,
    #[serde(rename = "jti")]
    pub id: String,
}

impl<'de> Deserialize<'de> for Claims {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ClaimsVisitor;
        impl<'de> Visitor<'de> for ClaimsVisitor {
            type Value = Claims;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JWT claims object")
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<Claims, E> {
                Ok(Claims::default())
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Claims, M::Error> {
                let mut claims = Claims::default();
                while let Some(key) = map.next_key::<String>()? {
                    // encoding/json matches tagged struct fields without case,
                    // and applies duplicate fields in order. Null leaves scalar
                    // fields unchanged, including a previously decoded value.
                    match key.to_ascii_lowercase().replace('ſ', "s").as_str() {
                        "sub" => {
                            if let Some(value) = map.next_value::<Option<String>>()? {
                                claims.subject = value;
                            }
                        }
                        "email" => {
                            if let Some(value) = map.next_value::<Option<String>>()? {
                                claims.email = value;
                            }
                        }
                        "iss" => {
                            if let Some(value) = map.next_value::<Option<String>>()? {
                                claims.issuer = value;
                            }
                        }
                        "iat" => {
                            if let Some(value) = map.next_value::<Option<i64>>()? {
                                claims.issued = value;
                            }
                        }
                        "exp" => {
                            if let Some(value) = map.next_value::<Option<i64>>()? {
                                claims.expires = value;
                            }
                        }
                        "jti" => {
                            if let Some(value) = map.next_value::<Option<String>>()? {
                                claims.id = value;
                            }
                        }
                        _ => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(claims)
            }
        }
        deserializer.deserialize_any(ClaimsVisitor)
    }
}

#[derive(Clone)]
pub struct TokenManager {
    secret: Vec<u8>,
    access_ttl: TimeDelta,
}

impl TokenManager {
    pub fn new(secret: &str, access_ttl: TimeDelta) -> Self {
        Self {
            secret: secret.as_bytes().to_vec(),
            access_ttl,
        }
    }

    pub fn access_token(
        &self,
        user_id: &str,
        email: &str,
    ) -> Result<(String, DateTime<Utc>), String> {
        let now = Utc::now();
        let expires = now + self.access_ttl;
        let claims = Claims {
            subject: user_id.into(),
            email: email.into(),
            issuer: "balance-server".into(),
            issued: now.timestamp(),
            expires: expires.timestamp(),
            id: random_token(16)?,
        };
        let payload = serde_json::to_vec(&claims).map_err(|error| error.to_string())?;
        let unsigned = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#),
            URL_SAFE_NO_PAD.encode(payload)
        );
        Ok((format!("{}.{}", unsigned, self.sign(&unsigned)), expires))
    }

    pub fn verify(&self, token: &str) -> Result<Claims, String> {
        let parts: Vec<_> = token.split('.').collect();
        if parts.len() != 3 {
            return Err("invalid token".into());
        }
        let unsigned = format!("{}.{}", parts[0], parts[1]);
        // Go compares the encoded signature, so reject noncanonical encodings too.
        let expected = self.sign(&unsigned);
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.secret).expect("HMAC accepts any key size");
        mac.update(parts[2].as_bytes());
        let signature_tag = mac.finalize().into_bytes();
        let mut expected_mac =
            Hmac::<Sha256>::new_from_slice(&self.secret).expect("HMAC accepts any key size");
        expected_mac.update(expected.as_bytes());
        if expected_mac.verify_slice(&signature_tag).is_err() {
            return Err("invalid token signature".into());
        }
        let payload =
            decode_go_base64(parts[1], true).map_err(|_| "invalid token payload".to_string())?;
        let claims: Claims = serde_json::from_slice(&crate::go_json::normalize(&payload))
            .map_err(|_| "invalid token claims".to_string())?;
        if claims.issuer != "balance-server"
            || claims.subject.is_empty()
            || Utc::now().timestamp() >= claims.expires
        {
            return Err("expired or invalid token".into());
        }
        Ok(claims)
    }

    fn sign(&self, value: &str) -> String {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.secret).expect("HMAC accepts any key size");
        mac.update(value.as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_claims_follow_go_scalar_and_duplicate_field_rules() {
        let manager = TokenManager::new("secret", TimeDelta::minutes(1));
        let claims = r#"{"SUB":"old","sub":"user","sub":null,"ISS":"balance-server","EXP":4102444800,"exp":null,"EMAIL":null,"iat":null,"jti":"id","JTI":null}"#;
        // The Go verifier does not inspect or decode the JWT header.
        let unsigned = format!("ignored-header.{}", URL_SAFE_NO_PAD.encode(claims));
        let token = format!("{}.{}", unsigned, manager.sign(&unsigned));
        let actual = manager.verify(&token).unwrap();
        assert_eq!(actual.subject, "user");
        assert_eq!(actual.email, "");
        assert_eq!(actual.expires, 4_102_444_800);
        assert_eq!(actual.id, "id");
        for invalid in [r#"{"sub":1}"#, r#"{"iat":1.0}"#, r#"[]"#] {
            assert!(serde_json::from_str::<Claims>(invalid).is_err());
        }
    }

    #[test]
    fn jwt_claims_replace_malformed_unicode_as_go_does() {
        let manager = TokenManager::new("secret", TimeDelta::minutes(1));
        for payload in [
            br#"{"sub":"\ud800","iss":"balance-server","exp":4102444800}"#.as_slice(),
            b"{\"sub\":\"\xff\",\"iss\":\"balance-server\",\"exp\":4102444800}".as_slice(),
        ] {
            let unsigned = format!("ignored.{}", URL_SAFE_NO_PAD.encode(payload));
            let token = format!("{}.{}", unsigned, manager.sign(&unsigned));
            assert_eq!(manager.verify(&token).unwrap().subject, "�");
        }
    }

    #[test]
    fn go_base64_ignores_line_breaks_and_unused_bits_only() {
        assert_eq!(decode_go_base64("Zh\r\n", true).unwrap(), b"f");
        assert_eq!(decode_go_base64("Zh\r\n", false).unwrap(), b"f");
        for invalid in ["Zg=", "Z g", "Zg\t", "Z"] {
            assert!(decode_go_base64(invalid, true).is_err());
        }
        assert_eq!(
            password_parameters("m=65536,t=3,p=2suffix"),
            Some((65536, 3, 2))
        );
        assert_eq!(password_parameters("m=65536,t= 3,p=2"), Some((65536, 3, 2)));
        assert_eq!(password_parameters("m=65536,t=3,p=256"), None);
        assert_eq!(password_parameters("m=+65536,t=3,p=2"), None);
    }

    #[test]
    fn accepts_existing_password_hash_with_go_parameters() {
        // Fixed salt and an independently computed libargon2 reference result.
        // Go's argon2.IDKey uses this same Argon2id v19 representation.
        let encoded = "$argon2id$v=19$m=65536,t=3,p=2$MDEyMzQ1Njc4OWFiY2RlZg$0R79n3aEb762TWx/8plp8QrcDJd8M65F5ccBf1ugp4Y";
        assert!(verify_password(encoded, "Go-compatible-✓"));
        assert!(!verify_password(encoded, "Go-compatible-x"));
        assert_eq!(
            hash_password("short").unwrap_err(),
            "password must contain at least 8 characters"
        );
        assert_eq!(
            hash_password(&"x".repeat(1025)).unwrap_err(),
            "password is too long"
        );
        // The original Go implementation measures UTF-8 bytes, not characters.
        assert!(hash_password("éééé").is_ok());
        for malformed in [
            "",
            "$argon2i$v=19$m=65536,t=3,p=2$c2FsdA$aGFzaA",
            "$argon2id$v=19$m=999999,t=3,p=2$c2FsdA$aGFzaA",
            "$argon2id$v=19$m=65536,t=3,p=2$!$!",
        ] {
            assert!(!verify_password(malformed, "password"));
        }
    }

    #[test]
    fn accepts_go_jwt_wire_format_and_preserves_refresh_hashes() {
        // Signed independently using Go's exact header, claims and HS256 encoding.
        let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJnby11c2VyIiwiZW1haWwiOiJnb0BleGFtcGxlLmNvbSIsImlzcyI6ImJhbGFuY2Utc2VydmVyIiwiaWF0IjoxNzAwMDAwMDAwLCJleHAiOjQxMDI0NDQ4MDAsImp0aSI6ImdvLXNlc3Npb24ifQ.BJrD7QmRGbRhYqS3EI1stCV5AEkocqOcFAJ3yC4MqeI";
        let manager = TokenManager::new("go-fixture-secret", TimeDelta::minutes(15));
        let claims = manager.verify(token).unwrap();
        assert_eq!(claims.subject, "go-user");
        assert_eq!(claims.email, "go@example.com");
        assert_eq!(claims.expires, 4_102_444_800);
        assert_eq!(claims.id, "go-session");
        assert!(manager.verify(&format!("{token}=")).is_err());
        assert!(
            manager
                .verify(&token.replace("eyJzdWI", "eyJzdWJ"))
                .is_err()
        );
        assert_eq!(
            token_hash("existing-go-refresh-token"),
            "66e9debb6786892dfecaec6bff6761635629efae55baaeb78645820772911605"
        );
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(random_token(48).unwrap())
                .unwrap()
                .len(),
            48
        );
        let id = identifier().unwrap();
        assert_eq!(id.len(), 32);
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
    }

    #[test]
    fn passwords_and_tokens_round_trip() {
        let encoded = hash_password("password123").unwrap();
        assert!(encoded.starts_with("$argon2id$v=19$m=65536,t=3,p=2$"));
        assert!(verify_password(&encoded, "password123"));
        assert!(!verify_password(&encoded, "wrong"));
        let manager = TokenManager::new("test secret", TimeDelta::minutes(15));
        let (token, _) = manager.access_token("user", "user@example.com").unwrap();
        assert_eq!(manager.verify(&token).unwrap().subject, "user");
        assert!(
            TokenManager::new("other", TimeDelta::minutes(15))
                .verify(&token)
                .is_err()
        );
        let (expired, _) = TokenManager::new("test secret", TimeDelta::seconds(-1))
            .access_token("user", "email")
            .unwrap();
        assert!(manager.verify(&expired).is_err());
    }
}
