use anyhow::Result;
use reqwest::{RequestBuilder, Response};
use serde::Serialize;

/// Extension trait to add simd-json support to reqwest
pub trait ReqwestSimdJsonExt {
    /// Set the request body as JSON using simd-json for serialization
    fn simd_json<T>(self, json: &T) -> RequestBuilder
    where
        T: Serialize + ?Sized;
}

/// Extension trait to add simd-json parsing support to reqwest responses
pub trait ResponseSimdJsonExt {
    /// Parse response body as JSON using simd-json
    async fn simd_json<T>(self) -> Result<T>
    where
        T: serde::de::DeserializeOwned;
}

impl ReqwestSimdJsonExt for RequestBuilder {
    fn simd_json<T>(self, json: &T) -> RequestBuilder
    where
        T: Serialize + ?Sized,
    {
        // Serialize using simd-json
        let body = simd_json::to_vec(json).expect("Failed to serialize JSON");

        self.header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
    }
}

impl ResponseSimdJsonExt for Response {
    async fn simd_json<T>(self) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let bytes = self.bytes().await?;
        let mut bytes = bytes.to_vec();
        let result = simd_json::from_slice(&mut bytes)?;
        Ok(result)
    }
}
