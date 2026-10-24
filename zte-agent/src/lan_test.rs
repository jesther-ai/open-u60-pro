use std::convert::Infallible;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::OwnedSemaphorePermit;

/// Download chunk size.
const CHUNK: usize = 64 * 1024;

/// GET /api/lan/ping — empty 200 for RTT measurement.
pub fn ping() -> (u16, Value) {
    (200, json!({"ok": true}))
}

/// GET /api/lan/download?size=N — streams N zero bytes as slices of one
/// 64 KiB buffer per stream (a static buffer would sit in .rodata and cost
/// 64 KiB of binary). The buffer and the stream permit are released when the
/// body ends or the client goes away.
pub fn download_body(size: usize, permit: OwnedSemaphorePermit) -> Body {
    let zeros = Bytes::from(vec![0u8; size.min(CHUNK)]);
    Body::from_stream(futures_util::stream::unfold(
        (size, zeros, permit),
        |(left, zeros, permit)| async move {
            if left == 0 {
                return None;
            }
            let n = left.min(zeros.len());
            Some((Ok::<_, Infallible>(zeros.slice(..n)), (left - n, zeros, permit)))
        },
    ))
}

/// POST /api/lan/upload — reads and discards body, returns measured throughput.
/// A read error (client gone) or nothing new for `idle` (a phone that left
/// mid-upload) ends the count.
pub async fn upload(body: Body, permit: OwnedSemaphorePermit, idle: Duration) -> (u16, Value) {
    let _permit = permit;
    let start = Instant::now();
    let mut total: u64 = 0;
    let mut chunks = body.into_data_stream();
    while let Ok(Some(Ok(chunk))) = tokio::time::timeout(idle, chunks.next()).await {
        total += chunk.len() as u64;
    }
    let elapsed = start.elapsed().as_secs_f64();
    let mbps = if elapsed > 0.0 {
        (total as f64 * 8.0) / (elapsed * 1_000_000.0)
    } else {
        0.0
    };
    let round2 = |v: f64| (v * 100.0).round() / 100.0;
    (
        200,
        json!({
            "ok": true,
            "data": {
                "bytes": total,
                "duration_secs": round2(elapsed),
                "mbps": round2(mbps),
            }
        }),
    )
}
