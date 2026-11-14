//! Show posters for the Movies & shows results. Movie of the Night returns signed links to its
//! image CDN with every search result; workers fetch them off the SDL thread, keep the JPEG on
//! disk beside the preferences, and decode it at card size. The renderer uploads each decoded
//! poster once, like a camera frame. Only links on that CDN over HTTPS are fetched: links come
//! from the network, so anything else is refused.
use crate::cameras::Frame;
use crate::motn_api::Show;
use crate::search_cache::DiskCache;
use image::RgbaImage;
use std::{
    collections::HashSet,
    io::Read,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

/// Card size in logical pixels; posters are 2:3.
pub const SIZE: (u32, u32) = (270, 405);
const HOST: &str = "https://cdn.movieofthenight.com/";
/// A 360px poster is about 30 KB; nothing legitimate comes close to this.
const LIMIT: u64 = 1024 * 1024;
/// Posters load a few at a time, top results first.
const WORKERS: usize = 3;

/// The poster link for a show: its 360px-wide vertical poster, if it is on the CDN.
pub fn url(show: &Show) -> Option<&str> {
    let url = show.image_set.vertical_poster.w360.as_str();
    (url.starts_with(HOST) && !url[HOST.len()..].contains(['@', '\\'])).then_some(url)
}

/// The cache key: the link without its signature, which changes while the image does not.
fn cache_key(url: &str) -> String {
    format!("poster:{}", url.split(['?', '#']).next().unwrap_or(url))
}

/// The texture key the renderer keeps one texture for.
pub fn texture_key(show_id: &str) -> String {
    format!("poster:{show_id}")
}

pub struct Request {
    pub show_id: String,
    pub url: String,
}

/// A poster, or None when it could not be fetched or decoded.
pub type Reply = (String, Option<Arc<Frame>>);

pub struct Service {
    sender: mpsc::Sender<Request>,
    pub receiver: mpsc::Receiver<Reply>,
    /// Shows still on screen; queued requests for any other show are skipped.
    wanted: Arc<Mutex<HashSet<String>>>,
}

impl Service {
    pub fn new(cache: DiskCache) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .max_redirects(0)
            .proxy(None)
            .http_status_as_error(false)
            .build()
            .new_agent();
        Self::with_fetch(cache, move |url| fetch(&agent, url))
    }

    fn with_fetch(
        cache: DiskCache,
        fetch: impl Fn(&str) -> Result<Vec<u8>, String> + Send + Sync + 'static,
    ) -> Self {
        let (sender, requests) = mpsc::channel::<Request>();
        let (replies, receiver) = mpsc::channel();
        let requests = Arc::new(Mutex::new(requests));
        let wanted = Arc::new(Mutex::new(HashSet::new()));
        let fetch = Arc::new(fetch);
        for _ in 0..WORKERS {
            let (requests, replies, wanted, cache, fetch) = (
                requests.clone(),
                replies.clone(),
                wanted.clone(),
                cache.clone(),
                fetch.clone(),
            );
            std::thread::spawn(move || {
                loop {
                    let request = match requests.lock() {
                        Ok(requests) => requests.recv(),
                        Err(_) => return,
                    };
                    let Ok(request) = request else { return };
                    if !wanted
                        .lock()
                        .is_ok_and(|wanted| wanted.contains(&request.show_id))
                    {
                        continue;
                    }
                    let frame = load(&cache, &*fetch, &request.url).map(|pixels| {
                        Arc::new(Frame {
                            key: texture_key(&request.show_id),
                            serial: 1,
                            pixels,
                            live: false,
                        })
                    });
                    if replies.send((request.show_id, frame)).is_err() {
                        return;
                    }
                }
            });
        }
        Self {
            sender,
            receiver,
            wanted,
        }
    }

    /// The shows whose posters are still wanted: those in the current results.
    pub fn want<'a>(&self, show_ids: impl IntoIterator<Item = &'a str>) {
        if let Ok(mut wanted) = self.wanted.lock() {
            *wanted = show_ids.into_iter().map(str::to_owned).collect();
        }
    }

    pub fn send(&self, request: Request) {
        let _ = self.sender.send(request);
    }
}

/// From the disk cache, or fetched and then cached. Undecodable bytes are not cached.
fn load(
    cache: &DiskCache,
    fetch: &dyn Fn(&str) -> Result<Vec<u8>, String>,
    url: &str,
) -> Option<RgbaImage> {
    let key = cache_key(url);
    if let Some(bytes) = cache.get_bytes(&key)
        && let Some(image) = crate::cameras::decode(&bytes, SIZE)
    {
        return Some(image);
    }
    let bytes = fetch(url)
        .map_err(|error| eprintln!("Luma poster: {error}"))
        .ok()?;
    let image = crate::cameras::decode(&bytes, SIZE)?;
    cache.put_bytes(&key, &bytes);
    Some(image)
}

fn fetch(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let mut response = agent
        .get(url)
        .header("User-Agent", "LumaNative")
        .call()
        .map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}"));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("The poster is too large".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        RgbaImage::from_pixel(width, height, image::Rgba([200, 40, 40, 255]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    fn cache(name: &str) -> DiskCache {
        let dir = std::env::temp_dir().join(format!("luma-posters-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        DiskCache::new(dir, crate::search_cache::POSTER_LIMIT)
    }

    const LINK: &str = "https://cdn.movieofthenight.com/show/426/poster/vertical/en/360.jpg";

    #[test]
    fn only_cdn_links_are_fetched() {
        let mut show: Show = serde_json::from_value(serde_json::json!({
            "itemType": "show", "showType": "movie", "id": "426", "imdbId": "tt1",
            "tmdbId": "movie/1", "title": "Dune", "originalTitle": "Dune", "overview": "",
            "genres": [], "cast": [], "rating": 80, "streamingOptions": {},
            "imageSet": {
                "verticalPoster": {"w240": "", "w360": format!("{LINK}?Expires=1&Signature=a"),
                    "w480": "", "w600": "", "w720": ""},
                "horizontalPoster": {"w360": "", "w480": "", "w720": "", "w1080": "", "w1440": ""}
            }
        }))
        .unwrap();
        assert!(url(&show).is_some());
        for bad in [
            "http://cdn.movieofthenight.com/show/1.jpg",
            "https://cdn.movieofthenight.com.evil.example/1.jpg",
            "https://cdn.movieofthenight.com/@evil.example/1.jpg",
            "https://example.com/1.jpg",
            "",
        ] {
            show.image_set.vertical_poster.w360 = bad.into();
            assert_eq!(url(&show), None, "{bad}");
        }
    }

    #[test]
    fn posters_are_cached_without_their_signature_and_decoded_to_card_size() {
        let cache = cache("load");
        let calls = AtomicUsize::new(0);
        let fetch = |_: &str| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(png(360, 540))
        };
        let first = load(&cache, &fetch, &format!("{LINK}?Signature=a")).unwrap();
        assert_eq!(first.dimensions(), SIZE);
        // A new signature for the same image is a cache hit.
        load(&cache, &fetch, &format!("{LINK}?Signature=b")).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        let broken = |_: &str| Ok(b"not an image".to_vec());
        let other = "https://cdn.movieofthenight.com/show/1/poster.jpg";
        assert!(load(&cache, &broken, other).is_none());
        assert_eq!(cache.get_bytes(&cache_key(other)), None, "not cached");
        let failed = |_: &str| Err("offline".to_owned());
        assert!(load(&cache, &failed, other).is_none());
    }

    #[test]
    fn the_service_skips_shows_no_longer_wanted() {
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let log = fetched.clone();
        let service = Service::with_fetch(cache("service"), move |url| {
            log.lock().unwrap().push(url.to_owned());
            Ok(png(36, 54))
        });
        service.want(["kept"]);
        for id in ["dropped", "kept"] {
            service.send(Request {
                show_id: id.into(),
                url: format!("https://cdn.movieofthenight.com/{id}.jpg"),
            });
        }
        let (id, frame) = service
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(id, "kept");
        let frame = frame.unwrap();
        assert_eq!(frame.key, "poster:kept");
        assert_eq!(frame.pixels.dimensions(), (36, 54));
        assert!(
            service
                .receiver
                .recv_timeout(Duration::from_millis(200))
                .is_err()
        );
        assert_eq!(
            *fetched.lock().unwrap(),
            ["https://cdn.movieofthenight.com/kept.jpg"]
        );
    }
}
