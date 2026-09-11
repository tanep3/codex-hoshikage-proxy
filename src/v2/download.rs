use super::{Error, Result, id, now, service::Service, store};
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};
use tokio::io::AsyncReadExt;
struct Pin {
    service: Arc<Service>,
    key: String,
}
impl Drop for Pin {
    fn drop(&mut self) {
        let _ = self.service.store.transaction(|tx| {
            tx.execute(
                "DELETE FROM records WHERE kind='read_pin' AND id=?1",
                [&self.key],
            )?;
            Ok(())
        });
    }
}
pub async fn content(
    s: Arc<Service>,
    kind: &'static str,
    rid: String,
    headers: HeaderMap,
) -> Result<Response> {
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(s.limits.download_timeout_seconds);
    let permit = s
        .downloads
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::code(429, "download_capacity_busy"))?;
    let svc = s.clone();
    let (mut file, size, hash, name, media, pin) = tokio::task::spawn_blocking(move || {
        let (mut file, meta, pin_key) = svc.store.transaction(|tx| {
            let r = store::get(tx, kind, &rid)?;
            super::retention::check_access(tx, &r)?;
            let m = if kind == "response" {
                r["output"].clone()
            } else {
                r
            };
            if m["state"] == "expired" {
                return Err(Error::code(410, "content_expired"));
            }
            if m["state"] == "corrupt" {
                return Err(Error::code(503, "content_corrupt"));
            }
            if kind == "response" && matches!(m["state"].as_str(), Some("failed" | "unavailable")) {
                return Err(Error::code(409, "output_unavailable"));
            }
            if m["state"] != "ready" {
                return Err(Error::code(409, "output_not_ready"));
            }
            if super::retention::expiry(tx, kind, &rid, m["expires_at_ms"].as_u64().unwrap_or(0))?
                <= now()
            {
                return Err(Error::code(410, "content_expired"));
            }
            let key = id("read");
            store::put(
                tx,
                "read_pin",
                &key,
                &serde_json::json!({
                    "resource_id":rid,
                    "hold_until_ms":now()+svc.limits.download_timeout_seconds*1000
                }),
            )?;
            let file = std::fs::File::open(svc.store.root.join("blobs").join(&rid))
                .map_err(|_| Error::code(503, "content_corrupt"))?;
            Ok((file, m, key))
        })?;
        let pin = Pin {
            service: svc.clone(),
            key: pin_key,
        };
        let size = meta["size_bytes"]
            .as_u64()
            .ok_or_else(|| Error::code(503, "content_corrupt"))?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        let mut total = 0;
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::code(408, "download_timeout"));
            }
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > size {
                return Err(Error::code(503, "content_corrupt"));
            }
            hash.update(&buffer[..n]);
        }
        let hash = hash.finalize();
        if total != size || meta["sha256"] != format!("{hash:x}") {
            return Err(Error::code(503, "content_corrupt"));
        }
        file.seek(SeekFrom::Start(0))?;
        let name = meta["display_name"]
            .as_str()
            .unwrap_or("response.json")
            .to_owned();
        let media = if kind == "response" {
            "application/json"
        } else {
            meta["media_type"]
                .as_str()
                .unwrap_or("application/octet-stream")
        }
        .to_owned();
        Ok((file, size, hash.to_vec(), name, media, pin))
    })
    .await
    .map_err(|_| Error::code(503, "store_unavailable"))??;
    let etag = format!(
        "\"{}\"",
        hash.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let range = headers
        .get("range")
        .and_then(|v| v.to_str().ok())
        .filter(|_| {
            headers
                .get("if-range")
                .is_none_or(|v| v.to_str().ok() == Some(&etag))
        });
    let (start, end, partial) = if let Some(range) = range {
        let (a, b) = match parse_range(range, size) {
            Ok(v) => v,
            Err(e) => {
                let mut response = e.into_response();
                response
                    .headers_mut()
                    .insert("content-range", format!("bytes */{size}").parse().unwrap());
                return Ok(response);
            }
        };
        (a, b, true)
    } else {
        (0, size.saturating_sub(1), false)
    };
    let count = if size == 0 { 0 } else { end - start + 1 };
    let content_hash = if partial {
        let (returned, digest) = tokio::task::spawn_blocking(move || -> Result<_> {
            file.seek(SeekFrom::Start(start))?;
            let mut remaining = count;
            let mut h = Sha256::new();
            let mut bytes = [0u8; 65536];
            while remaining > 0 {
                if tokio::time::Instant::now() >= deadline {
                    return Err(Error::code(408, "download_timeout"));
                }
                let n = file.read(&mut bytes[..remaining.min(65536) as usize])?;
                if n == 0 {
                    return Err(Error::code(503, "content_corrupt"));
                }
                h.update(&bytes[..n]);
                remaining -= n as u64;
            }
            file.seek(SeekFrom::Start(start))?;
            Ok((file, h.finalize().to_vec()))
        })
        .await
        .map_err(|_| Error::code(503, "store_unavailable"))??;
        file = returned;
        digest
    } else {
        hash.clone()
    };
    file.seek(SeekFrom::Start(start))?;
    let (tx, rx) =
        tokio::sync::mpsc::channel::<std::result::Result<axum::body::Bytes, std::io::Error>>(4);
    tokio::spawn(async move {
        let _pin = pin;
        let _permit = permit;
        let mut f = tokio::fs::File::from_std(file).take(count);
        let result = tokio::time::timeout_at(deadline, async {
            let mut remaining = count;
            while remaining > 0 {
                let mut b = vec![0u8; 65536.min(remaining as usize)];
                match f.read(&mut b).await {
                    Ok(0) => {
                        let _ = tx
                            .send(Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)))
                            .await;
                        return;
                    }
                    Ok(n) => {
                        b.truncate(n);
                        remaining -= n as u64;
                        if tx.send(Ok(b.into())).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                }
            }
        })
        .await;
        if result.is_err() {
            let _ = tx.try_send(Err(std::io::Error::from(std::io::ErrorKind::TimedOut)));
        }
    });
    let mut response =
        Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)).into_response();
    *response.status_mut() = if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let h = response.headers_mut();
    h.insert("content-length", count.to_string().parse().unwrap());
    h.insert("etag", etag.parse().unwrap());
    h.insert("accept-ranges", "bytes".parse().unwrap());
    {
        h.insert(
            "content-digest",
            format!(
                "sha-256=:{}:",
                base64::engine::general_purpose::STANDARD.encode(&content_hash)
            )
            .parse()
            .unwrap(),
        );
    }
    h.insert(
        "content-type",
        media
            .parse()
            .map_err(|_| Error::code(503, "store_corrupt"))?,
    );
    h.insert("cache-control", "private, no-store".parse().unwrap());
    h.insert("x-content-type-options", "nosniff".parse().unwrap());
    let encoded = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect::<String>();
    h.insert(
        "content-disposition",
        format!("attachment; filename=\"download\"; filename*=UTF-8''{encoded}")
            .parse()
            .unwrap(),
    );
    if partial {
        h.insert(
            "content-range",
            format!("bytes {start}-{end}/{size}").parse().unwrap(),
        );
    }
    Ok(response)
}
pub fn parse_range(value: &str, size: u64) -> Result<(u64, u64)> {
    let error = || Error::code(416, "range_not_satisfiable");
    let value = value.strip_prefix("bytes=").ok_or_else(error)?;
    let (a, b) = value.split_once('-').ok_or_else(error)?;
    if size == 0 || b.contains(',') {
        return Err(error());
    }
    if a.is_empty() {
        let suffix = b.parse::<u64>().map_err(|_| error())?;
        if suffix == 0 {
            return Err(error());
        }
        return Ok((size.saturating_sub(suffix), size - 1));
    }
    let start = a.parse::<u64>().map_err(|_| error())?;
    let end = if b.is_empty() {
        size - 1
    } else {
        b.parse::<u64>().map_err(|_| error())?.min(size - 1)
    };
    if start > end || start >= size {
        return Err(error());
    }
    Ok((start, end))
}
