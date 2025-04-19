use nostr_sdk::{
    nips::nip96::{Error, ServerConfig, UploadResponse, UploadResponseStatus},
    nips::nip98::{HttpData, HttpMethod},
    NostrSigner, TagKind, TagStandard, Url,
};
use nostr_sdk::hashes::{sha256::Hash as Sha256Hash, Hash};
use reqwest::{
    multipart::{self, Part},
    Body, Client
};
use std::net::SocketAddr;
use tokio::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Makes a reqwest client
fn make_client(proxy: Option<SocketAddr>) -> Result<Client, Error> {
    let client: Client = {
        let mut builder = Client::builder();
        if let Some(proxy) = proxy {
            let proxy = format!("socks5h://{proxy}");
            use reqwest::Proxy;
            builder = builder.proxy(Proxy::all(proxy)?);
        }
        builder.build()?
    };

    Ok(client)
}

/// Custom upload stream that allows tracking progress
struct ProgressTrackingStream {
    total_size: u64,
    bytes_sent: Arc<Mutex<u64>>,
    inner: mpsc::Receiver<Result<Vec<u8>, std::io::Error>>,
}

impl ProgressTrackingStream {
    fn new(data: Vec<u8>, bytes_sent: Arc<Mutex<u64>>) -> Self {
        let total_size = data.len() as u64;
        let (tx, rx) = mpsc::channel(8); // Buffer size of 8 chunks
        
        // Spawn a background task to feed the stream
        tokio::spawn(async move {
            let chunk_size = 64 * 1024; // 64 KB chunks
            let mut position = 0;
            
            while position < data.len() {
                let end = std::cmp::min(position + chunk_size, data.len());
                let chunk = data[position..end].to_vec();
                let chunk_size = chunk.len();
                
                // Send chunk through channel
                if tx.send(Ok(chunk)).await.is_err() {
                    break; // Receiver was dropped
                }
                
                position += chunk_size;
            }
        });
        
        Self {
            total_size,
            bytes_sent,
            inner: rx,
        }
    }
}

impl futures_util::Stream for ProgressTrackingStream {
    type Item = Result<Vec<u8>, std::io::Error>;
    
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        
        match self.inner.poll_recv(cx) {
            Poll::Ready(Some(result)) => {
                // Update the bytes sent counter
                if let Ok(chunk) = &result {
                    let mut bytes_sent = self.bytes_sent.lock().unwrap();
                    *bytes_sent += chunk.len() as u64;
                }
                Poll::Ready(Some(result))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Progress callback function type
pub type ProgressCallback = Box<dyn Fn(Option<u8>, Option<u64>) -> Result<(), String> + Send + Sync>;

/// Uploads data to a NIP-96 server with progress callback
///
/// This function extends the standard NIP-96 upload_data function by adding progress reporting
/// via a callback function that is called periodically during the upload process.
pub async fn upload_data_with_progress<T>(
    signer: &T,
    desc: &ServerConfig,
    file_data: Vec<u8>,
    mime_type: Option<&str>,
    proxy: Option<SocketAddr>,
    progress_callback: ProgressCallback,
) -> Result<Url, Error>
where
    T: NostrSigner,
{
    // Build NIP98 Authorization header
    let payload: Sha256Hash = Sha256Hash::hash(&file_data);
    let data = HttpData::new(desc.api_url.clone(), HttpMethod::POST).payload(payload);
    let nip98_auth: String = data.to_authorization(signer).await?;

    // Create shared counter for tracking upload progress
    let bytes_sent = Arc::new(Mutex::new(0u64));
    let total_size = file_data.len() as u64;
    
    // Report initial progress (0%)
    progress_callback(Some(0), Some(0)).map_err(|e| Error::UploadError(e))?;

    // Make client
    let client: Client = make_client(proxy)?;

    // Create form with tracking stream
    let file_part = {
        let tracking_stream = ProgressTrackingStream::new(file_data.clone(), bytes_sent.clone());
        let body = Body::wrap_stream(tracking_stream);
        
        let mut part = Part::stream(body)
            .file_name("filename");
            
        // Set MIME type if provided
        if let Some(mime_str) = mime_type {
            part = part.mime_str(mime_str).map_err(|_| Error::MultipartMimeError)?;
        }
        
        part
    };
    
    let form = multipart::Form::new().part("file", file_part);

    // Launch upload as a future, but don't await it yet
    let mut response_future = client
        .post(desc.api_url.clone())
        .header("Authorization", nip98_auth)
        .multipart(form)
        .send();
    
    // Create a future that polls the bytes_sent counter periodically
    let mut last_percentage = 0;
    let mut poll_interval = tokio::time::interval(tokio::time::Duration::from_millis(100));
    
    // Use tokio::select to concurrently wait for the response and report progress
    let response = loop {
        tokio::select! {
            // Check if the response is ready
            response = &mut response_future => {
                break response?;
            },
            // Report progress periodically
            _ = poll_interval.tick() => {
                let current_bytes = *bytes_sent.lock().unwrap();
                let percentage = if total_size > 0 {
                    ((current_bytes as f64 / total_size as f64) * 100.0) as u8
                } else {
                    0
                };
                
                // Only report when percentage changes to reduce events
                if percentage > last_percentage {
                    if let Err(e) = progress_callback(Some(percentage), Some(current_bytes)) {
                        return Err(Error::UploadError(e));
                    }
                    last_percentage = percentage;
                }
            }
        }
    };
    
    // Report 100% completion
    progress_callback(Some(100), Some(total_size)).map_err(|e| Error::UploadError(e))?;
    
    // Decode response
    let res: UploadResponse = response.json().await?;

    // Check status
    if res.status == UploadResponseStatus::Error {
        return Err(Error::UploadError(res.message));
    }

    // Extract url
    let nip94_event = res.nip94_event.ok_or(Error::ResponseDecodeError)?;
    match nip94_event.tags.find_standardized(TagKind::Url) {
        Some(TagStandard::Url(url)) => Ok(url.clone()),
        _ => Err(Error::ResponseDecodeError),
    }
}
