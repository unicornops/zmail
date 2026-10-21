// Ported from go-proton-api `paging.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust with a bounded
// `buffered` stream instead of juniper's parallel map; pages still run with
// up to one request per CPU and come back in page order.

//! Fetching paged API lists.

use std::{future::Future, thread};

use futures_util::{StreamExt, TryStreamExt, stream};

use crate::response::Error;

/// The largest page the API serves (`maxPageSize`).
pub const MAX_PAGE_SIZE: usize = 150;

/// Fetches `total` items in pages of `page_size`, with up to one request
/// per CPU in flight, and returns them in page order (`fetchPaged`).
///
/// Like upstream it asks for `total / page_size + 1` pages, so the last page
/// may be empty.
pub async fn fetch_paged<T, F, Fut>(
    total: usize,
    page_size: usize,
    fetch: F,
) -> Result<Vec<T>, Error>
where
    F: Fn(usize, usize) -> Fut,
    Fut: Future<Output = Result<Vec<T>, Error>>,
{
    let page_size = page_size.clamp(1, MAX_PAGE_SIZE);
    let pages = total / page_size + 1;
    let concurrency = thread::available_parallelism().map_or(1, usize::from);
    let pages: Vec<Vec<T>> = stream::iter(0..pages)
        .map(|page| fetch(page, page_size))
        .buffered(concurrency)
        .try_collect()
        .await?;
    Ok(pages.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    #[tokio::test]
    async fn fetches_every_page_in_order() {
        let items: Vec<usize> = (0..331).collect();
        let calls = Arc::new(AtomicUsize::new(0));
        let fetched = fetch_paged(items.len(), 150, |page, size| {
            let items = items.clone();
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                // Later pages finish first, to check ordering.
                tokio::time::sleep(std::time::Duration::from_millis(
                    u64::try_from(10 * (3 - page.min(3))).unwrap_or(0),
                ))
                .await;
                Ok(items.iter().skip(page * size).take(size).copied().collect())
            }
        })
        .await
        .expect("pages");
        assert_eq!(fetched, items);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn clamps_page_size_and_propagates_errors() {
        let sizes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = sizes.clone();
        let result: Result<Vec<u8>, Error> = fetch_paged(10, 500, move |page, size| {
            recorded.lock().expect("lock").push(size);
            async move {
                if page == 0 {
                    Err(Error::Config("boom".to_owned()))
                } else {
                    Ok(Vec::new())
                }
            }
        })
        .await;
        assert!(matches!(result, Err(Error::Config(_))));
        assert!(
            sizes
                .lock()
                .expect("lock")
                .iter()
                .all(|size| *size == MAX_PAGE_SIZE)
        );
    }
}
