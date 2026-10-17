//! The only path by which the engine can load a resource.
//!
//! Blitz has no I/O of its own: every `<img>`, stylesheet, `@import`,
//! `@font-face`, CSS `background-image` and `<iframe src>` becomes a
//! [`NetProvider::fetch`] call. This provider answers those calls from data
//! zmail already holds (inline `cid:` parts and `data:` URLs) and refuses
//! everything else unless the remote-content policy allows it.

use std::{
    collections::HashMap,
    io::Cursor,
    sync::{Arc, Mutex},
};

use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};

/// Largest single resource handed to the engine.
const MAX_RESOURCE_BYTES: usize = 8 * 1024 * 1024;
/// Most resources one message may load.
const MAX_RESOURCES: usize = 256;
/// Largest decoded image the engine may allocate (pixels).
const MAX_IMAGE_PIXELS: u64 = 24_000_000;

/// An inline MIME part addressable by `cid:`.
#[derive(Debug, Clone)]
pub struct InlinePart {
    /// MIME content type of the part, for example `image/png`.
    pub content_type: String,
    /// Decoded part bytes.
    pub bytes: Bytes,
}

/// Fetches an allowed remote URL. Only consulted when the user allowed
/// remote content for the message; the spike ships no implementation.
pub trait RemoteFetcher: Send + Sync + 'static {
    /// Returns the response body, or `None` to treat the load as failed.
    fn fetch(&self, url: &url::Url) -> Option<Bytes>;
}

/// Remote-content decision for one render.
#[derive(Clone, Default)]
pub enum RemotePolicy {
    /// Never load `http(s)` resources (zmail's default).
    #[default]
    Blocked,
    /// Load `http(s)` resources through the given fetcher.
    Allowed(Arc<dyn RemoteFetcher>),
}

/// Why a resource request was refused, for the "remote content blocked"
/// banner and for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A remote URL under [`RemotePolicy::Blocked`].
    Remote(String),
    /// A `cid:` reference with no matching inline part.
    MissingCid(String),
    /// A scheme the provider never serves (`file:`, `ftp:`, ...).
    Scheme(String),
    /// A resource kind the policy does not allow (for example fonts).
    Kind(String),
    /// Over a size, count or pixel limit.
    Limit(String),
}

/// Resources and policy for rendering one message.
#[derive(Clone, Default)]
pub struct MailResources {
    inline: HashMap<String, InlinePart>,
    /// Remote-content decision.
    pub remote: RemotePolicy,
    /// Whether embedded web fonts (`@font-face` from `data:`/`cid:`) load.
    /// Off by default: it keeps font parsing on bundled/system fonts only.
    pub allow_embedded_fonts: bool,
}

impl MailResources {
    /// Registers an inline part under its `Content-ID` (angle brackets
    /// optional, compared case-insensitively as most clients do).
    pub fn insert_inline(&mut self, content_id: &str, part: InlinePart) {
        self.inline.insert(normalize_cid(content_id), part);
    }
}

fn normalize_cid(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_ascii_lowercase()
}

/// The [`NetProvider`] handed to Blitz. Answers synchronously, so every
/// resource has resolved by the time `fetch` returns.
pub(crate) struct MailNetProvider {
    resources: MailResources,
    log: Mutex<ProviderLog>,
}

#[derive(Default)]
pub(crate) struct ProviderLog {
    pub(crate) served: usize,
    pub(crate) refused: Vec<Refusal>,
}

impl MailNetProvider {
    pub(crate) fn new(resources: MailResources) -> Self {
        Self {
            resources,
            log: Mutex::new(ProviderLog::default()),
        }
    }

    pub(crate) fn take_log(&self) -> ProviderLog {
        std::mem::take(&mut *self.log.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn resolve(&self, url: &url::Url) -> Result<Bytes, Refusal> {
        let (content_type, bytes) = match url.scheme() {
            "cid" => {
                let id = percent_decode(url.path());
                let part = self
                    .resources
                    .inline
                    .get(&normalize_cid(&id))
                    .ok_or_else(|| Refusal::MissingCid(id.clone()))?;
                (part.content_type.to_ascii_lowercase(), part.bytes.clone())
            }
            "data" => decode_data_url(url.as_str())?,
            "http" | "https" => match &self.resources.remote {
                RemotePolicy::Blocked => return Err(Refusal::Remote(url.to_string())),
                RemotePolicy::Allowed(fetcher) => {
                    let bytes = fetcher
                        .fetch(url)
                        .ok_or_else(|| Refusal::Remote(url.to_string()))?;
                    // Remote responses are typed by sniffing below.
                    (String::new(), bytes)
                }
            },
            other => return Err(Refusal::Scheme(other.to_owned())),
        };

        if bytes.len() > MAX_RESOURCE_BYTES {
            return Err(Refusal::Limit(format!("{} bytes", bytes.len())));
        }
        if looks_like_font(&content_type, &bytes) && !self.resources.allow_embedded_fonts {
            return Err(Refusal::Kind("font".to_owned()));
        }
        check_image_dimensions(&bytes)?;
        Ok(bytes)
    }
}

impl NetProvider for MailNetProvider {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let mut log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        if log.served + log.refused.len() >= MAX_RESOURCES {
            log.refused
                .push(Refusal::Limit(format!("over {MAX_RESOURCES} resources")));
            return;
        }
        match self.resolve(&request.url) {
            Ok(bytes) => {
                log.served += 1;
                // Release the lock first: the handler may re-enter `fetch`
                // (a stylesheet's `@import`).
                drop(log);
                handler.bytes(request.url.to_string(), bytes);
            }
            // A refused request still has to complete: Blitz holds layout
            // while a `<link rel=stylesheet>` in `<head>` is pending, and
            // the trait has no error callback. Empty bytes parse as an
            // empty stylesheet or a failed image, so the element renders
            // without the resource (alt text, no background).
            Err(refusal) => {
                log.refused.push(refusal);
                drop(log);
                handler.bytes(request.url.to_string(), Bytes::new());
            }
        }
    }
}

/// Rejects images in formats mail does not need (the `image` crate's
/// feature set is unified across the app, so GPUI's extra decoders would
/// otherwise be reachable) and images whose header declares more pixels
/// than we will decode. Non-images (CSS, SVG, unknown) pass through.
fn check_image_dimensions(bytes: &[u8]) -> Result<(), Refusal> {
    use image::ImageFormat;
    let Ok(format) = image::guess_format(bytes) else {
        return Ok(());
    };
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
    ) {
        return Err(Refusal::Kind(format!("{format:?} image")));
    }
    let reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    match reader.into_dimensions() {
        Ok((w, h)) if u64::from(w) * u64::from(h) > MAX_IMAGE_PIXELS => {
            Err(Refusal::Limit(format!("image {w}x{h}")))
        }
        Ok(_) => Ok(()),
        // A header the decoder cannot read will not decode either.
        Err(_) => Err(Refusal::Kind(format!("corrupt {format:?} image"))),
    }
}

fn looks_like_font(content_type: &str, bytes: &[u8]) -> bool {
    content_type.starts_with("font/")
        || content_type.contains("font-")
        || content_type.ends_with("opentype")
        || matches!(
            bytes.get(..4),
            Some(b"wOFF" | b"wOF2" | b"OTTO" | b"true" | b"ttcf" | [0, 1, 0, 0])
        )
}

/// Decodes an RFC 2397 `data:` URL into (media type, bytes).
fn decode_data_url(url: &str) -> Result<(String, Bytes), Refusal> {
    let rest = url
        .strip_prefix("data:")
        .ok_or_else(|| Refusal::Scheme("data".to_owned()))?;
    let (meta, payload) = rest
        .split_once(',')
        .ok_or_else(|| Refusal::Scheme("malformed data: URL".to_owned()))?;
    if payload.len() > MAX_RESOURCE_BYTES * 2 {
        return Err(Refusal::Limit(format!("data: URL {} bytes", payload.len())));
    }
    let is_base64 = meta.to_ascii_lowercase().ends_with(";base64");
    let media_type = meta.split(';').next().unwrap_or("").to_ascii_lowercase();
    let bytes = if is_base64 {
        decode_base64(&percent_decode(payload))
            .ok_or_else(|| Refusal::Scheme("bad base64 in data: URL".to_owned()))?
    } else {
        percent_decode(payload).into_bytes()
    };
    Ok((media_type, Bytes::from(bytes)))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = input.get(i + 1..i + 3)
            && let Ok(value) = u8::from_str_radix(hex, 16)
        {
            out.push(value);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in input.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(resources: MailResources) -> MailNetProvider {
        MailNetProvider::new(resources)
    }

    fn url(s: &str) -> url::Url {
        url::Url::parse(s).expect("test URL parses")
    }

    #[test]
    fn remote_urls_are_refused_by_default() {
        let p = provider(MailResources::default());
        assert_eq!(
            p.resolve(&url("https://tracker.example/pixel.gif")),
            Err(Refusal::Remote("https://tracker.example/pixel.gif".into()))
        );
    }

    #[test]
    fn file_and_other_schemes_are_never_served() {
        let p = provider(MailResources::default());
        for target in ["file:///etc/passwd", "ftp://example.org/a", "about:blank"] {
            assert!(matches!(p.resolve(&url(target)), Err(Refusal::Scheme(_))));
        }
    }

    #[test]
    fn cid_lookup_ignores_brackets_and_case() {
        let mut resources = MailResources::default();
        resources.insert_inline(
            "<Logo@Example>",
            InlinePart {
                content_type: "text/plain".into(),
                bytes: Bytes::from_static(b"hello"),
            },
        );
        let p = provider(resources);
        assert_eq!(
            p.resolve(&url("cid:logo@example")).as_deref(),
            Ok(&b"hello"[..])
        );
        assert!(matches!(
            p.resolve(&url("cid:other@example")),
            Err(Refusal::MissingCid(_))
        ));
    }

    #[test]
    fn data_urls_decode_and_fonts_are_refused() {
        let p = provider(MailResources::default());
        assert_eq!(
            p.resolve(&url("data:text/plain;base64,aGk=")).as_deref(),
            Ok(&b"hi"[..])
        );
        assert_eq!(
            p.resolve(&url("data:font/woff2;base64,d09GMg==")),
            Err(Refusal::Kind("font".into()))
        );
    }

    #[test]
    fn oversized_image_headers_are_refused_before_decode() {
        fn crc32(bytes: &[u8]) -> u32 {
            let mut crc = !0u32;
            for &byte in bytes {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (0xedb8_8320 & (!(crc & 1)).wrapping_add(1));
                }
            }
            !crc
        }
        // A PNG whose header claims 20000x20000 RGBA pixels (1.6 GB decoded).
        let mut ihdr = b"IHDR".to_vec();
        ihdr.extend_from_slice(&20_000u32.to_be_bytes());
        ihdr.extend_from_slice(&20_000u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(&ihdr);
        png.extend_from_slice(&crc32(&ihdr).to_be_bytes());
        // The decoder reads up to the first image data chunk.
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IDAT");
        png.extend_from_slice(&crc32(b"IDAT").to_be_bytes());
        let result = check_image_dimensions(&png);
        assert!(matches!(result, Err(Refusal::Limit(_))), "{result:?}");
    }
}
