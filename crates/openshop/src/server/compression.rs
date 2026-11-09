//! Response compression (gzip or Brotli) for bodies large enough to benefit from it.
//!
//! A response is compressed while it is sent, by [`CompressLarge`]. The assets
//! of the admin UI are not: each is compressed once per ETag and encoding, by
//! [`encode_asset`], and the result is kept.

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::ready;
use std::io::Write;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

use axum::body::Bytes;
use axum::extract::Request;
use axum::http::header::{ACCEPT_ENCODING, CONTENT_ENCODING, ETAG};
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use futures::future::{BoxFuture, FutureExt, Shared};
use tower::{Service, ServiceExt, service_fn};
use tower_http::compression::predicate::{Predicate, SizeAbove};
use tower_http::compression::{Compression, CompressionLevel};

/// Smaller responses are sent as they are: compressing a body that fits in one
/// packet costs far more CPU than it saves, and often makes it larger.
const MIN_COMPRESSED_BODY_BYTES: u16 = 1024;

/// The level of a body compressed while it is sent: tower-http's default,
/// Brotli quality 4 and gzip level 6. These are answers of the API and pages;
/// the large bodies are assets, compressed at [`ASSET_BROTLI_QUALITY`].
const STREAMED_LEVEL: CompressionLevel = CompressionLevel::Default;

/// The Brotli quality of an asset. `openshop dev` runs a debug build, where the
/// compressor is not optimized: a 43 MB debug `.wasm` takes 5.3 s at quality 4
/// and 1.2 s at quality 1, for the same 3 MB. Quality 0 is no faster, and larger.
const ASSET_BROTLI_QUALITY: u32 = 1;

/// The gzip level of an asset: in a debug build, level 3 takes as long as level
/// 1 and its output is a third smaller; level 6 takes twice as long.
const ASSET_GZIP_LEVEL: u32 = 3;

/// The Brotli window of an asset, 4 MB: the default of the encoder.
const ASSET_BROTLI_WINDOW_BITS: u32 = 22;

/// The bytes of compressed assets kept. Beyond it, the least recently asked
/// for are dropped: a debug build of the UI leaves a new `.wasm` of a few
/// megabytes, compressed, at each rebuild.
const ENCODED_ASSETS_BUDGET: usize = 64 * 1024 * 1024;

/// Whether a body of this length is large enough to be compressed.
pub(crate) fn is_compressible(body_len: usize) -> bool {
    body_len > usize::from(MIN_COMPRESSED_BODY_BYTES)
}

/// A coding this server compresses with. Brotli ranks above gzip: it is chosen
/// when a client accepts both as much.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Encoding {
    Gzip,
    Brotli,
}

impl Encoding {
    /// The `Content-Encoding` of a body compressed with it.
    pub(crate) fn header_value(self) -> HeaderValue {
        HeaderValue::from_static(match self {
            Self::Gzip => "gzip",
            Self::Brotli => "br",
        })
    }

    /// The bytes of an asset, compressed.
    fn encode(self, data: &[u8]) -> Vec<u8> {
        const IN_MEMORY: &str = "writing to memory does not fail";
        match self {
            Self::Brotli => {
                let (quality, window) = (ASSET_BROTLI_QUALITY, ASSET_BROTLI_WINDOW_BITS);
                let mut encoder = brotli::CompressorWriter::new(Vec::new(), 64 * 1024, quality, window);
                encoder.write_all(data).expect(IN_MEMORY);
                encoder.into_inner()
            }
            Self::Gzip => {
                let level = flate2::Compression::new(ASSET_GZIP_LEVEL);
                let mut encoder = flate2::write::GzEncoder::new(Vec::new(), level);
                encoder.write_all(data).expect(IN_MEMORY);
                encoder.finish().expect(IN_MEMORY)
            }
        }
    }
}

/// The coding a body is compressed with for this request, chosen as tower-http
/// chooses it: the accepted coding with the highest q-value, Brotli on a tie,
/// and none when `identity` ranks higher. `*` stands for the codings it does
/// not name.
pub(crate) fn preferred_encoding(headers: &HeaderMap) -> Option<Encoding> {
    let (mut identity, mut gzip, mut brotli, mut any) = (None, None, None, None);
    let entries = headers
        .get_all(ACCEPT_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','));
    for entry in entries {
        let (coding, parameters) = match entry.split_once(';') {
            Some((coding, parameters)) => (coding.trim(), Some(parameters)),
            None => (entry.trim(), None),
        };
        let Some(q) = q_value(parameters) else {
            continue;
        };
        let slot = if coding.eq_ignore_ascii_case("br") {
            &mut brotli
        } else if coding.eq_ignore_ascii_case("gzip") || coding.eq_ignore_ascii_case("x-gzip") {
            &mut gzip
        } else if coding.eq_ignore_ascii_case("identity") {
            &mut identity
        } else if coding == "*" {
            &mut any
        } else {
            continue;
        };
        slot.get_or_insert(q);
    }
    let q = |named: Option<u16>| named.or(any).unwrap_or(0);
    [
        (q(identity), None),
        (q(gzip), Some(Encoding::Gzip)),
        (q(brotli), Some(Encoding::Brotli)),
    ]
    .into_iter()
    .filter(|(q, _)| *q > 0)
    .max()
    .and_then(|(_, encoding)| encoding)
}

/// The q-value of an `Accept-Encoding` entry, in thousandths, read from what
/// follows its `;`: 1000 without it, `None` when it is not a q-value.
fn q_value(parameters: Option<&str>) -> Option<u16> {
    let Some(parameters) = parameters else {
        return Some(1000);
    };
    let value = parameters.trim().strip_prefix(['q', 'Q'])?.strip_prefix('=')?;
    let (units, decimals) = value.split_once('.').unwrap_or((value, ""));
    if decimals.len() > 3 || !decimals.bytes().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    let thousandths: u16 = format!("{decimals:0<3}").parse().ok()?;
    match units {
        "0" => Some(thousandths),
        "1" if thousandths == 0 => Some(1000),
        _ => None,
    }
}

/// The weak form of an ETag.
pub(crate) fn weak_etag(etag: &HeaderValue) -> HeaderValue {
    if etag.as_bytes().starts_with(b"W/") {
        return etag.clone();
    }
    HeaderValue::from_bytes(&[b"W/", etag.as_bytes()].concat()).unwrap_or_else(|_| etag.clone())
}

/// A compressed body is another representation than the one the ETag names, so
/// the ETag of a response that was compressed is weak.
fn weaken_etag(response: &mut Response) {
    if !response.headers().contains_key(CONTENT_ENCODING) {
        return;
    }
    if let Some(etag) = response.headers().get(ETAG).map(weak_etag) {
        response.headers_mut().insert(ETAG, etag);
    }
}

/// Marks a response whose encoding is chosen already: [`CompressLarge`] sends it
/// as it is.
#[derive(Clone, Copy)]
pub(crate) struct Negotiated;

/// An asset compressed, or being compressed: `None` if compressing it failed.
type Pending = Shared<BoxFuture<'static, Option<Bytes>>>;

/// Which asset (by strong ETag) and which encoding.
type AssetKey = (HeaderValue, Encoding);

struct EncodedAsset {
    body: Pending,
    /// Once compressed.
    len: Option<usize>,
    /// When it was last asked for, on the clock of [`EncodedAssets`].
    used: u64,
}

/// The assets compressed so far, up to a budget of bytes.
struct EncodedAssets {
    by_key: HashMap<AssetKey, EncodedAsset>,
    /// The bytes of those compressed.
    len: usize,
    budget: usize,
    clock: u64,
}

impl EncodedAssets {
    fn new(budget: usize) -> Self {
        Self {
            by_key: HashMap::new(),
            len: 0,
            budget,
            clock: 0,
        }
    }

    /// The asset under `key`, compressed by `compress` if it is not yet.
    fn get_or_compress(&mut self, key: AssetKey, compress: impl FnOnce() -> Pending) -> Pending {
        self.clock += 1;
        let asset = self.by_key.entry(key).or_insert_with(|| EncodedAsset {
            body: compress(),
            len: None,
            used: 0,
        });
        asset.used = self.clock;
        asset.body.clone()
    }

    /// Counts an asset that is compressed now, then drops the least recently
    /// used ones beyond the budget. One larger than the budget is not kept.
    fn compressed(&mut self, key: &AssetKey, len: usize) {
        if len > self.budget {
            self.by_key.remove(key);
            return;
        }
        let Some(asset) = self.by_key.get_mut(key) else {
            return;
        };
        asset.len = Some(len);
        self.len += len;
        while self.len > self.budget {
            let oldest = self
                .by_key
                .iter()
                .filter(|(_, asset)| asset.len.is_some())
                .min_by_key(|(_, asset)| asset.used)
                .map(|(key, _)| key.clone());
            let Some(evicted) = oldest.and_then(|key| self.by_key.remove(&key)) else {
                break;
            };
            self.len -= evicted.len.unwrap_or_default();
        }
    }
}

static ENCODED_ASSETS: LazyLock<Mutex<EncodedAssets>> =
    LazyLock::new(|| Mutex::new(EncodedAssets::new(ENCODED_ASSETS_BUDGET)));

fn encoded_assets() -> MutexGuard<'static, EncodedAssets> {
    ENCODED_ASSETS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `body`, the bytes of an asset whose strong ETag is `etag`, compressed with
/// `encoding`; `None` if compressing it failed.
///
/// An asset is compressed once per ETag and encoding, on a blocking thread,
/// and kept. The requests that ask for it meanwhile wait for that compression,
/// which goes on when they leave.
pub(crate) async fn encode_asset(etag: &HeaderValue, encoding: Encoding, body: Bytes) -> Option<Bytes> {
    let key = (etag.clone(), encoding);
    let pending = encoded_assets().get_or_compress(key.clone(), || {
        let compressing = tokio::spawn(async move {
            let encoded = tokio::task::spawn_blocking(move || {
                let mut encoded = encoding.encode(&body);
                // Kept for long: without the spare room of its growth.
                encoded.shrink_to_fit();
                Bytes::from(encoded)
            })
            .await;
            let mut assets = encoded_assets();
            match encoded {
                Ok(encoded) => {
                    assets.compressed(&key, encoded.len());
                    Some(encoded)
                }
                Err(_) => {
                    // Asked for again, it is compressed again.
                    assets.by_key.remove(&key);
                    None
                }
            }
        });
        async move { compressing.await.ok().flatten() }.boxed().shared()
    });
    pending.await
}

/// Compresses the responses of the inner service when the client accepts it.
///
/// tower-http wraps every response it is given in a body of several kilobytes,
/// compressed or not, so the small ones never reach it, nor those that are
/// [`Negotiated`].
#[derive(Clone)]
pub(crate) struct CompressLarge<S>(pub(crate) S);

impl<S> Service<Request> for CompressLarge<S>
where
    S: Service<Request, Response = Response, Error = Infallible>,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Response, Infallible>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        // All that tower-http reads of the request, kept until the response is known.
        let mut accepted = Request::new(());
        for encoding in request.headers().get_all(ACCEPT_ENCODING) {
            accepted.headers_mut().append(ACCEPT_ENCODING, encoding.clone());
        }
        let response = self.0.call(request);
        Box::pin(async move {
            let mut response = response.await?;
            if response.extensions().get::<Negotiated>().is_some()
                || !SizeAbove::new(u64::from(MIN_COMPRESSED_BODY_BYTES)).should_compress(&response)
            {
                return Ok(response);
            }
            let respond = service_fn(move |_: Request<()>| ready(Ok::<_, Infallible>(std::mem::take(&mut response))));
            let mut compressed = Compression::new(respond)
                .quality(STREAMED_LEVEL)
                .oneshot(accepted)
                .await?
                .into_response();
            weaken_etag(&mut compressed);
            Ok(compressed)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn accept(value: &str) -> HeaderMap {
        HeaderMap::from_iter([(ACCEPT_ENCODING, HeaderValue::from_str(value).unwrap())])
    }

    fn decode(encoding: Encoding, body: &[u8]) -> Vec<u8> {
        let mut decoded = Vec::new();
        match encoding {
            Encoding::Brotli => brotli::Decompressor::new(body, 4096).read_to_end(&mut decoded),
            Encoding::Gzip => flate2::read::GzDecoder::new(body).read_to_end(&mut decoded),
        }
        .unwrap();
        decoded
    }

    #[test]
    fn chooses_the_coding_a_client_prefers() {
        use Encoding::{Brotli, Gzip};
        for (header, expected) in [
            ("gzip, deflate, br, zstd", Some(Brotli)),
            ("deflate, gzip", Some(Gzip)),
            ("x-gzip", Some(Gzip)),
            ("GZIP;Q=0.5, BR;q=0.4", Some(Gzip)),
            ("gzip;q=0.8, br;q=0.8", Some(Brotli)),
            ("br;q=0, gzip", Some(Gzip)),
            ("br;q=0.000, gzip;q=0", None),
            ("identity, gzip;q=0.5", None),
            ("identity;q=0.5, gzip;q=0.5", Some(Gzip)),
            ("*", Some(Brotli)),
            ("*;q=0.5, br;q=0.1", Some(Gzip)),
            ("*;q=0", None),
            ("br;q=1.5, gzip;q=bad", None),
            ("deflate, zstd", None),
            ("", None),
        ] {
            assert_eq!(preferred_encoding(&accept(header)), expected, "{header}");
        }
        assert_eq!(preferred_encoding(&HeaderMap::new()), None);
    }

    #[tokio::test]
    async fn an_asset_is_compressed_once_per_etag_and_encoding() {
        let data = Bytes::from("openshop compresses each asset once. ".repeat(20_000));
        let etag = HeaderValue::from_static("\"compressed-once\"");
        let (first, meanwhile) = tokio::join!(
            encode_asset(&etag, Encoding::Brotli, data.clone()),
            encode_asset(&etag, Encoding::Brotli, data.clone()),
        );
        let (first, meanwhile) = (first.unwrap(), meanwhile.unwrap());
        let later = encode_asset(&etag, Encoding::Brotli, data.clone()).await.unwrap();
        // The same bytes, not equal ones: they were compressed once.
        assert_eq!(meanwhile.as_ptr(), first.as_ptr());
        assert_eq!(later.as_ptr(), first.as_ptr());
        assert!(first.len() < data.len() / 10);
        assert_eq!(decode(Encoding::Brotli, &first), data);

        let gzip = encode_asset(&etag, Encoding::Gzip, data.clone()).await.unwrap();
        assert_ne!(gzip, first);
        assert_eq!(decode(Encoding::Gzip, &gzip), data);
        let gzip_again = encode_asset(&etag, Encoding::Gzip, data.clone()).await.unwrap();
        assert_eq!(gzip_again.as_ptr(), gzip.as_ptr());

        let other = HeaderValue::from_static("\"another-asset\"");
        let changed = Bytes::from("openshop compressed this one as well. ".repeat(20_000));
        let another = encode_asset(&other, Encoding::Brotli, changed.clone()).await.unwrap();
        assert_eq!(decode(Encoding::Brotli, &another), changed);
    }

    fn key(name: &'static str) -> AssetKey {
        (HeaderValue::from_static(name), Encoding::Brotli)
    }

    /// Asks for an asset that nothing compresses: [`EncodedAssets::compressed`] says when it is.
    fn ask(assets: &mut EncodedAssets, name: &'static str) {
        let _pending = assets.get_or_compress(key(name), || async { None }.boxed().shared());
    }

    #[test]
    fn beyond_the_budget_the_least_recently_used_assets_go() {
        let mut assets = EncodedAssets::new(100);
        for name in ["a", "b"] {
            ask(&mut assets, name);
            assets.compressed(&key(name), 40);
        }
        let _kept = assets.get_or_compress(key("a"), || unreachable!("`a` is kept"));
        ask(&mut assets, "c");
        assets.compressed(&key("c"), 40);
        let mut kept: Vec<_> = assets.by_key.keys().map(|(etag, _)| etag.clone()).collect();
        kept.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        assert_eq!(kept, ["a", "c"]);
        assert_eq!(assets.len, 80);

        // An asset larger than the budget is not kept, and pushes out none.
        ask(&mut assets, "d");
        assets.compressed(&key("d"), 500);
        assert!(!assets.by_key.contains_key(&key("d")));
        assert_eq!(assets.len, 80);

        // One being compressed is never pushed out: its length is not known.
        ask(&mut assets, "e");
        ask(&mut assets, "f");
        assets.compressed(&key("f"), 90);
        assert!(assets.by_key.contains_key(&key("e")));
        assert!(assets.by_key.contains_key(&key("f")));
        assert_eq!(assets.len, 90);
    }
}
