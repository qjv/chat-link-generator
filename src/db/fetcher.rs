use serde::de::DeserializeOwned;

const GW2_API_BASE: &str = "https://api.guildwars2.com/v2";
const BATCH_SIZE: usize = 200;
const BATCH_DELAY_MS: u64 = 100;

/// Fetch all IDs from an endpoint (e.g. `/v2/items` returns `[1,2,3,...]`).
pub async fn fetch_all_ids(
    client: &reqwest::Client,
    endpoint: &str,
) -> Result<Vec<u32>, String> {
    let url = format!("{}{}", GW2_API_BASE, endpoint);
    super::log_debug(&format!("[fetcher] Fetching IDs from {}", url));
    let resp = client.get(&url).send().await.map_err(|e| {
        let msg = format!("[fetcher] HTTP error fetching IDs from {}: {}", endpoint, e);
        super::log_error(&msg);
        e.to_string()
    })?;
    let status = resp.status();
    if !status.is_success() {
        let msg = format!("[fetcher] Non-200 status {} from {}", status, endpoint);
        super::log_error(&msg);
        return Err(msg);
    }
    let ids: Vec<u32> = resp.json().await.map_err(|e| {
        let msg = format!("[fetcher] Failed to parse IDs from {}: {}", endpoint, e);
        super::log_error(&msg);
        e.to_string()
    })?;
    super::log_debug(&format!("[fetcher] Got {} IDs from {}", ids.len(), endpoint));
    Ok(ids)
}

/// Batch-fetch entries from an endpoint using `?ids=1,2,3,...` in chunks of 200.
/// Calls `on_progress(fetched_so_far, total)` after each batch.
pub async fn batch_fetch<T: DeserializeOwned>(
    client: &reqwest::Client,
    endpoint: &str,
    ids: &[u32],
    on_progress: &dyn Fn(usize, usize),
) -> Result<Vec<T>, String> {
    let total = ids.len();
    let num_batches = (total + BATCH_SIZE - 1) / BATCH_SIZE;
    super::log_debug(&format!(
        "[fetcher] batch_fetch {} - {} IDs in {} batches",
        endpoint, total, num_batches
    ));
    let mut results: Vec<T> = Vec::with_capacity(total);

    for (i, chunk) in ids.chunks(BATCH_SIZE).enumerate() {
        let ids_param: String = chunk
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let url = format!("{}{}?ids={}", GW2_API_BASE, endpoint, ids_param);

        let resp = client.get(&url).send().await.map_err(|e| {
            let msg = format!(
                "[fetcher] HTTP error on batch {}/{} for {}: {}",
                i + 1, num_batches, endpoint, e
            );
            super::log_error(&msg);
            e.to_string()
        })?;

        let status = resp.status();
        if !status.is_success() {
            let msg = format!(
                "[fetcher] Non-200 status {} on batch {}/{} for {}",
                status, i + 1, num_batches, endpoint
            );
            super::log_error(&msg);
            return Err(msg);
        }

        let mut batch: Vec<T> = resp.json().await.map_err(|e| {
            let msg = format!(
                "[fetcher] Parse error on batch {}/{} for {}: {}",
                i + 1, num_batches, endpoint, e
            );
            super::log_error(&msg);
            e.to_string()
        })?;
        results.append(&mut batch);

        on_progress(results.len(), total);

        if i < num_batches - 1 {
            tokio::time::sleep(std::time::Duration::from_millis(BATCH_DELAY_MS)).await;
        }
    }

    super::log_debug(&format!(
        "[fetcher] batch_fetch {} complete - {} entries",
        endpoint, results.len()
    ));
    Ok(results)
}

/// Like `batch_fetch` but skips batches that fail (e.g. invalid IDs) instead of
/// aborting the entire operation. Useful for recipes where some output_item_ids
/// may reference items that no longer exist in the API.
pub async fn batch_fetch_lenient<T: DeserializeOwned>(
    client: &reqwest::Client,
    endpoint: &str,
    ids: &[u32],
    on_progress: &dyn Fn(usize, usize),
) -> Result<Vec<T>, String> {
    let total = ids.len();
    let num_batches = (total + BATCH_SIZE - 1) / BATCH_SIZE;
    super::log_debug(&format!(
        "[fetcher] batch_fetch_lenient {} - {} IDs in {} batches",
        endpoint, total, num_batches
    ));
    let mut results: Vec<T> = Vec::with_capacity(total);
    let mut skipped: usize = 0;

    for (i, chunk) in ids.chunks(BATCH_SIZE).enumerate() {
        let ids_param: String = chunk
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let url = format!("{}{}?ids={}", GW2_API_BASE, endpoint, ids_param);

        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                super::log_debug(&format!(
                    "[fetcher] Skipping batch {}/{} for {} (HTTP error: {})",
                    i + 1, num_batches, endpoint, e
                ));
                skipped += chunk.len();
                on_progress(results.len() + skipped, total);
                continue;
            }
        };

        if !resp.status().is_success() {
            super::log_debug(&format!(
                "[fetcher] Skipping batch {}/{} for {} (status {})",
                i + 1, num_batches, endpoint, resp.status()
            ));
            skipped += chunk.len();
            on_progress(results.len() + skipped, total);
            continue;
        }

        match resp.json::<Vec<T>>().await {
            Ok(mut batch) => {
                results.append(&mut batch);
            }
            Err(e) => {
                super::log_debug(&format!(
                    "[fetcher] Skipping batch {}/{} for {} (parse error: {})",
                    i + 1, num_batches, endpoint, e
                ));
                skipped += chunk.len();
            }
        }

        on_progress(results.len() + skipped, total);

        if i < num_batches - 1 {
            tokio::time::sleep(std::time::Duration::from_millis(BATCH_DELAY_MS)).await;
        }
    }

    if skipped > 0 {
        super::log_debug(&format!(
            "[fetcher] batch_fetch_lenient {} complete - {} entries, {} IDs skipped",
            endpoint, results.len(), skipped
        ));
    } else {
        super::log_debug(&format!(
            "[fetcher] batch_fetch_lenient {} complete - {} entries",
            endpoint, results.len()
        ));
    }
    Ok(results)
}

/// Helper to create a reqwest client with a reasonable timeout.
pub fn make_client() -> Result<reqwest::Client, String> {
    super::log_debug("[fetcher] Creating HTTP client");
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| {
            let msg = format!("[fetcher] Failed to create HTTP client: {}", e);
            super::log_error(&msg);
            e.to_string()
        })
}
