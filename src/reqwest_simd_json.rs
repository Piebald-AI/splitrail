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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn simd_json_sets_body_and_content_type() {
        let client = reqwest::Client::new();
        let builder = client.post("http://example.com");

        #[derive(Serialize)]
        struct Payload {
            value: u32,
        }

        let request = builder
            .simd_json(&Payload { value: 42 })
            .build()
            .expect("Failed to build request");

        // Verify Content-Type header is set correctly
        let content_type = request
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .expect("Content-Type header should be set");
        assert_eq!(content_type, "application/json");

        // Verify body is set with correct JSON content
        let body = request.body().expect("Body should be set");
        let body_bytes = body.as_bytes().expect("Body should be bytes");
        assert_eq!(body_bytes, b"{\"value\":42}");
    }
}
