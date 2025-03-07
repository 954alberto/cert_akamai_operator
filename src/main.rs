use base64::{engine::general_purpose, Engine as _};
use futures::StreamExt;
use k8s_openapi::api::core::v1::Secret;
use kube::{
    api::Api,
    client::Client,
    runtime::watcher::{watcher, Event},
};
use reqwest::Client as HttpClient;
use serde_json::json;
use std::{env, time::Duration};
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::try_default().await?;
    watch_secrets(client).await;
    Ok(())
}

async fn watch_secrets(client: Client) {
    let api: Api<Secret> = Api::default_namespaced(client);
    let mut stream = watcher(api, Default::default()).boxed();

    while let Some(event) = stream.next().await {
        match event {
            Ok(Event::Apply(secret)) => {
                println!(
                    "Secret Applied/Modified: {}",
                    secret.metadata.name.as_deref().unwrap_or("unknown")
                );
                if let Err(e) = handle_secret(&secret).await {
                    eprintln!("Error handling secret: {}", e);
                }
            }
            Ok(Event::Delete(secret)) => {
                println!(
                    "Secret Deleted: {}",
                    secret.metadata.name.as_deref().unwrap_or("unknown")
                );
            }
            _ => {}
        }
    }
}

async fn handle_secret(secret: &Secret) -> Result<(), Box<dyn std::error::Error>> {
    let data = secret.data.as_ref().ok_or("Secret data is missing")?;

    let cert_data = data.get("tls.crt").ok_or("Certificate data not found")?;
    let key_data = data.get("tls.key").ok_or("Private key data not found")?;
    let chain_data = data.get("tls.ca.crt").ok_or("Chain data not found")?;

    push_to_akamai(
        cert_data.0.as_ref(),
        key_data.0.as_ref(),
        chain_data.0.as_ref(),
    )
    .await
}

async fn push_to_akamai(
    cert_data: &[u8],
    key_data: &[u8],
    chain_data: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let akamai_url = env::var("AKAMAI_URL")
        .unwrap_or_else(|_| "https://akamai-api-endpoint/certificates".to_string());
    let api_key = env::var("AKAMAI_API_KEY")?;
    let client_token = env::var("AKAMAI_CLIENT_TOKEN")?;
    let client_secret = env::var("AKAMAI_CLIENT_SECRET")?;

    let client = HttpClient::new();
    let payload = json!({
        "certificate": general_purpose::STANDARD.encode(cert_data),
        "private_key": general_purpose::STANDARD.encode(key_data),
        "chain": general_purpose::STANDARD.encode(chain_data),
    });

    let mut retries = 3;
    let mut backoff_duration = Duration::from_secs(1); // Start with 1 second backoff

    while retries > 0 {
        let res = client
            .post(&akamai_url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Client-Token", client_token.clone()) // Clone here
            .header("Client-Secret", client_secret.clone()) // Clone here
            .json(&payload)
            .send()
            .await;

        match res {
            Ok(response) => {
                // Extract the status first (before moving the response)
                let status = response.status();

                // Now consume the body (using the response text)
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "No response body".to_string());

                if status.is_success() {
                    println!("Certificate successfully uploaded to Akamai.");
                    return Ok(()); // Success, exit the function
                } else {
                    eprintln!(
                        "Failed to upload certificate: {}. Response: {}",
                        status, body
                    );
                    return Err(Box::new(AkamaiError::from_response(status, body)));
                }
            }
            Err(err) => {
                eprintln!("Request failed: {}. Retrying...", err);
            }
        }

        retries -= 1;
        if retries > 0 {
            println!("Retrying in {} seconds...", backoff_duration.as_secs());
            sleep(backoff_duration).await;
            backoff_duration *= 2; // Exponential backoff
        }
    }

    Err("Exceeded maximum retries while uploading certificate.".into())
}

#[derive(Debug)]
struct AkamaiError {
    status: reqwest::StatusCode,
    body: String,
}

impl AkamaiError {
    fn from_response(status: reqwest::StatusCode, body: String) -> Self {
        AkamaiError { status, body }
    }
}

impl std::fmt::Display for AkamaiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Akamai error: {} - {}", self.status, self.body)
    }
}

impl std::error::Error for AkamaiError {}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito;
    use std::env;

    // Define constants for your Akamai paths and expected responses
    const PATH_CERTIFICATES: &str = "/certificates";

    // Helper function to set up mock Akamai API
    fn setup_mock_akamai() {
        // Create the mock for the "/certificates" endpoint
        let _mock = mockito::mock("POST", PATH_CERTIFICATES)
            .with_status(200)
            .with_body("mocked response")
            .create();

        // Set the AKAMAI_URL environment variable to the mock server URL
        env::set_var("AKAMAI_URL", mockito::server_url()); // Correctly use mockito::server_url()
    }

    // Test case for a successful push to Akamai
    #[tokio::test]
    async fn test_push_to_akamai_success() {
        setup_mock_akamai();

        let cert_data = b"test-cert";
        let key_data = b"test-key";
        let chain_data = b"test-chain";

        // Call the function to push to Akamai
        let result = push_to_akamai(cert_data, key_data, chain_data).await;

        // Debugging output to inspect result
        // Correct the borrowing of `e` using `ref` to avoid moving it
        match result {
            Ok(_) => println!("Push to Akamai succeeded"),
            Err(ref e) => eprintln!("Push to Akamai failed with error: {:?}", e), // Borrow `e` instead of moving it
        }

        // Now, you can safely use `result` on the next line
        assert!(result.is_ok());
    }

    // Test case for a failed push to Akamai (mocking failure response)
    #[tokio::test]
    async fn test_push_to_akamai_failure() {
        // Create a mock server with a 500 error for this test
        let _mock = mockito::mock("POST", PATH_CERTIFICATES)
            .with_status(500)
            .with_body("Internal Server Error")
            .create();

        env::set_var("AKAMAI_URL", mockito::server_url()); // Correctly set the mock server URL

        let cert_data = b"test-cert";
        let key_data = b"test-key";
        let chain_data = b"test-chain";

        // Call the function to push to Akamai and check for failure
        let result = push_to_akamai(cert_data, key_data, chain_data).await;

        // Assert the result is Err (i.e., failure)
        assert!(result.is_err());
    }

    // Test case for invalid input data (e.g., missing certificate data)
    #[tokio::test]
    async fn test_push_to_akamai_invalid_input() {
        setup_mock_akamai();

        let cert_data = b"";
        let key_data = b"test-key";
        let chain_data = b"test-chain";

        // Call the function with invalid certificate data (empty)
        let result = push_to_akamai(cert_data, key_data, chain_data).await;

        // Assert that the result is an error because of the invalid data
        assert!(result.is_err());
    }

    // Test case for no mock server URL (i.e., missing AKAMAI_URL environment variable)
    #[tokio::test]
    async fn test_push_to_akamai_missing_url() {
        // Unset the AKAMAI_URL to simulate missing URL
        env::remove_var("AKAMAI_URL");

        let cert_data = b"test-cert";
        let key_data = b"test-key";
        let chain_data = b"test-chain";

        // Call the function when the AKAMAI_URL is missing
        let result = push_to_akamai(cert_data, key_data, chain_data).await;

        // Assert that the result is an error because the URL is missing
        assert!(result.is_err());
    }
}
