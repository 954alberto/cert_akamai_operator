use kube::{
    api::Api,
    client::Client,
    runtime::watcher::{watcher, Event},
};
use k8s_openapi::api::core::v1::Secret;
use serde_json::json;
use reqwest::Client as HttpClient;
use futures::StreamExt;
use base64::{engine::general_purpose, Engine as _};
use std::env;

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

    push_to_akamai(cert_data.0.as_ref(), key_data.0.as_ref(), chain_data.0.as_ref()).await
}

async fn push_to_akamai(cert_data: &[u8], key_data: &[u8], chain_data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let akamai_url = env::var("AKAMAI_URL").unwrap_or_else(|_| "https://akamai-api-endpoint/certificates".to_string());
    let api_key = env::var("AKAMAI_API_KEY")?;
    let client_token = env::var("AKAMAI_CLIENT_TOKEN")?;
    let client_secret = env::var("AKAMAI_CLIENT_SECRET")?;

    let client = HttpClient::new();
    let payload = json!({
        "certificate": general_purpose::STANDARD.encode(cert_data),
        "private_key": general_purpose::STANDARD.encode(key_data),
        "chain": general_purpose::STANDARD.encode(chain_data),
    });

    let res = client.post(&akamai_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Client-Token", client_token)
        .header("Client-Secret", client_secret)
        .json(&payload)
        .send()
        .await?;

    if res.status().is_success() {
        println!("Certificate successfully uploaded to Akamai.");
    } else {
        println!("Failed to upload certificate: {}", res.status());
    }

    Ok(())
}
