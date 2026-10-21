// Ported from go-proton-api `hv.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api), Copyright (c) 2020 James
// Houlahan, Copyright (c) 2022 Proton AG, MIT License
// (crates/zmail-proton-api/LICENSE-MIT); and from proton-bridge
// `internal/hv/hv_default.go` and `internal/hv/hv.go` at v3.27.1
// (43b9237786204a0215abfd4dcc51ee7bb92f52d4), Copyright (c) 2026 Proton AG,
// GPL-3.0-or-later, used under GPL-3.0-only.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust. The verification
// URL percent-encodes its parameters; Bridge inserts them as they are.

//! Human verification (API code 9001).
//!
//! When Proton wants proof that a person is signing in, the login fails
//! with [`Error::HumanVerificationRequired`]. The caller shows
//! [`HumanVerification::verify_url`] in the user's browser and, once the
//! user has completed it there, retries the login with the same
//! [`HumanVerification`]: the token then travels in the
//! `x-pm-human-verification-token` headers. Zmail never solves or bypasses
//! the check itself.

use serde::Deserialize;

use crate::{manager::Request, response::Error};

/// Header with the verification token.
pub(crate) const HV_TOKEN_HEADER: &str = "x-pm-human-verification-token";
/// Header with the verification methods, comma separated.
pub(crate) const HV_TOKEN_TYPE_HEADER: &str = "x-pm-human-verification-token-type";

/// The page where the user completes human verification.
const VERIFY_URL: &str = "https://verify.proton.me/";

/// A human verification request (go-proton-api's `APIHVDetails`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HumanVerification {
    /// Verification methods Proton offers, such as `captcha` or `email`.
    #[serde(rename = "HumanVerificationMethods", default)]
    pub methods: Vec<String>,
    /// The token that identifies this verification.
    #[serde(rename = "HumanVerificationToken")]
    pub token: String,
}

impl HumanVerification {
    /// The URL to open in the user's browser (Bridge's `FormatHvURL`).
    pub fn verify_url(&self) -> String {
        let methods = self
            .methods
            .iter()
            .map(|method| encode(method))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{VERIFY_URL}?methods={methods}&token={}",
            encode(&self.token)
        )
    }
}

impl Request {
    /// Adds the human verification headers (`addHVToRequest`).
    pub(crate) fn human_verification(self, hv: Option<&HumanVerification>) -> Self {
        match hv {
            Some(hv) => self
                .header(HV_TOKEN_HEADER, hv.token.clone())
                .header(HV_TOKEN_TYPE_HEADER, hv.methods.join(",")),
            None => self,
        }
    }
}

impl Error {
    /// Returns the verification request carried by a 9001 error
    /// (`GetHVDetails`, Bridge's `VerifyAndExtractHvRequest`): `None` for
    /// any other error, and an error when the details cannot be read.
    pub fn human_verification(&self) -> Option<Result<HumanVerification, Error>> {
        let Self::HumanVerificationRequired(api) = self else {
            return None;
        };
        let details = api.details.clone().unwrap_or_default();
        Some(serde_json::from_value(details).map_err(Error::from))
    }
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::{ApiError, Code};

    #[test]
    fn builds_bridge_verify_url() {
        let hv = HumanVerification {
            methods: vec!["captcha".to_owned(), "email".to_owned()],
            token: "abc-123_XYZ".to_owned(),
        };
        assert_eq!(
            hv.verify_url(),
            "https://verify.proton.me/?methods=captcha,email&token=abc-123_XYZ"
        );
        let odd = HumanVerification {
            methods: vec!["captcha".to_owned()],
            token: "a&b=c d".to_owned(),
        };
        assert_eq!(
            odd.verify_url(),
            "https://verify.proton.me/?methods=captcha&token=a%26b%3Dc+d"
        );
    }

    #[test]
    fn extracts_details_from_9001_only() {
        let error = Error::from_api(ApiError {
            status: 422,
            code: Code::HUMAN_VERIFICATION_REQUIRED,
            message: "Human verification required".to_owned(),
            details: Some(serde_json::json!({
                "HumanVerificationMethods": ["captcha"],
                "HumanVerificationToken": "token-1",
            })),
        });
        let hv = error
            .human_verification()
            .expect("hv error")
            .expect("details");
        assert_eq!(hv.methods, ["captcha"]);
        assert_eq!(hv.token, "token-1");

        let broken = Error::from_api(ApiError {
            status: 422,
            code: Code::HUMAN_VERIFICATION_REQUIRED,
            message: String::new(),
            details: None,
        });
        assert!(matches!(broken.human_verification(), Some(Err(_))));

        let other = Error::Config("x".to_owned());
        assert!(other.human_verification().is_none());
    }
}
