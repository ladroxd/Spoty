//! Search, Liked Songs, playlists and track details. These come from the same internal
//! endpoints the official app uses: the public Web API's quota for this client ID is shared
//! by every third-party player and permanently exhausted.

use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use serde_json::Value;
use tokio::sync::OnceCell;

use crate::auth::Auth;

const SPCLIENT: &str = "https://spclient.wg.spotify.com";

#[derive(Clone, Debug)]
pub struct Track {
    pub name: String,
    pub artists: String,
    pub album: String,
    pub duration_ms: u32,
    pub cover_url: Option<String>,
}

/// A display name for an album or artist, fetched lazily.
#[derive(Clone, Debug)]
pub struct Named {
    pub name: String,
    /// The album's artists; empty for artists.
    pub subtitle: String,
    /// The smallest cover Spotify has (64px), for list thumbnails.
    pub thumb_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Playlist {
    pub uri: String,
    pub name: String,
    pub songs: u32,
    /// A small cover, when the playlist has its own picture. Playlists showing Spotify's
    /// 4-album mosaic have none: building one would cost several requests each.
    pub thumb_url: Option<String>,
}

pub struct Library {
    http: reqwest::Client,
    auth: Arc<Auth>,
    username: OnceCell<String>,
}

impl Library {
    pub fn new(http: reqwest::Client, auth: Arc<Auth>) -> Library {
        Library { http, auth, username: OnceCell::new() }
    }

    async fn get(&self, path_or_url: &str) -> Result<Value, String> {
        let url = if path_or_url.starts_with("https://") { path_or_url.to_string() } else { format!("{SPCLIENT}{path_or_url}") };
        self.send(self.http.get(&url)).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        self.send(self.http.post(format!("{SPCLIENT}{path}")).json(&body)).await
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value, String> {
        let resp = req
            .bearer_auth(self.auth.access_token().await?)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("network error: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("Spotify returned {status}"));
        }
        resp.json().await.map_err(|e| e.to_string())
    }

    /// Track URIs for a context: a playlist, the user's collection, or a search.
    pub async fn context_tracks(&self, context_uri: &str) -> Result<Vec<String>, String> {
        Ok(self.context(context_uri, true).await?.1)
    }

    /// An artist's top tracks. (Their full discography would be dozens of extra requests.)
    pub async fn artist_tracks(&self, artist_uri: &str) -> Result<Vec<String>, String> {
        Ok(self.context(artist_uri, false).await?.1)
    }

    /// Returns the context's owner (if any) and its track URIs, following page links.
    /// `lazy_pages` also loads pages the context only links to (an artist's albums, for one).
    async fn context(&self, context_uri: &str, lazy_pages: bool) -> Result<(Option<String>, Vec<String>), String> {
        let ctx = self.get(&format!("/context-resolve/v1/{context_uri}")).await?;
        let owner = ctx["metadata"]["context_owner"].as_str().map(|o| o.trim_start_matches("spotify:user:").to_string());
        let mut uris = Vec::new();
        let mut pending = Vec::new();
        for page in ctx["pages"].as_array().into_iter().flatten() {
            let tracks = page["tracks"].as_array().map(Vec::as_slice).unwrap_or_default();
            collect_tracks(tracks, &mut uris);
            if lazy_pages && tracks.is_empty() && let Some(u) = page["page_url"].as_str() {
                pending.push(u.to_string());
            }
            if let Some(u) = page["next_page_url"].as_str() {
                pending.push(u.to_string());
            }
        }
        // Large contexts arrive in pages; follow them in order.
        while !pending.is_empty() {
            let url = pending.remove(0);
            let page = self.get(&format!("/{}", url.trim_start_matches("hm://"))).await?;
            collect_tracks(page["tracks"].as_array().map(Vec::as_slice).unwrap_or_default(), &mut uris);
            if let Some(u) = page["next_page_url"].as_str() {
                pending.insert(0, u.to_string());
            }
        }
        Ok((owner, uris))
    }

    pub async fn search(&self, query: &str) -> Result<Vec<String>, String> {
        let mut q = String::new();
        for b in query.trim().bytes() {
            match b {
                b' ' => q.push('+'),
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => q.push(b as char),
                _ => q.push_str(&format!("%{b:02X}")),
            }
        }
        self.context_tracks(&format!("spotify:search:{q}")).await
    }

    pub async fn liked(&self) -> Result<Vec<String>, String> {
        let (owner, uris) = self.context("spotify:collection", true).await?;
        if let Some(o) = owner {
            let _ = self.username.set(o);
        }
        Ok(uris)
    }

    async fn username(&self) -> Result<String, String> {
        if let Some(u) = self.username.get() {
            return Ok(u.clone());
        }
        // The collection context names its owner, which is the signed-in user.
        let (owner, _) = self.context("spotify:collection", true).await?;
        let owner = owner.ok_or("couldn't tell who is signed in")?;
        Ok(self.username.get_or_init(|| async { owner }).await.clone())
    }

    /// The user's saved and followed playlists, in sidebar order.
    pub async fn playlists(&self) -> Result<Vec<Playlist>, String> {
        const PAGE: usize = 500;
        let user = self.username().await?;
        let mut out = Vec::new();
        let mut from = 0;
        loop {
            let list = self
                .get(&format!("/playlist/v2/user/{user}/rootlist?decorate=attributes,length&from={from}&length={PAGE}"))
                .await?;
            let items = list["contents"]["items"].as_array().map(Vec::as_slice).unwrap_or_default();
            let meta = list["contents"]["metaItems"].as_array().map(Vec::as_slice).unwrap_or_default();
            for (i, item) in items.iter().enumerate() {
                let Some(uri) = item["uri"].as_str() else { continue };
                // Folders show up as start-group/end-group markers; skip them.
                if !uri.starts_with("spotify:playlist:") {
                    continue;
                }
                let m = meta.get(i).unwrap_or(&Value::Null);
                let name = m["attributes"]["name"].as_str().filter(|n| !n.is_empty()).unwrap_or("Untitled");
                out.push(Playlist {
                    uri: uri.to_string(),
                    name: name.to_string(),
                    songs: m["length"].as_u64().unwrap_or(0) as u32,
                    thumb_url: playlist_thumb(&m["attributes"]),
                });
            }
            from += items.len();
            if items.len() < PAGE || from >= list["length"].as_u64().unwrap_or(0) as usize {
                return Ok(out);
            }
        }
    }

    /// Details for several tracks, fetched a few at a time.
    pub async fn tracks(&self, uris: &[String]) -> Vec<(String, Track)> {
        stream::iter(uris.iter().cloned())
            .map(|uri| async move {
                let hex = base62_to_hex(uri.strip_prefix("spotify:track:")?)?;
                let t = self.get(&format!("/metadata/4/track/{hex}?market=from_token")).await.ok()?;
                Some((uri, parse_track(&t)))
            })
            .buffer_unordered(8)
            .filter_map(|x| async move { x })
            .collect()
            .await
    }

    /// Everything in one of the user's collection sets, newest first.
    async fn collection(&self, set: &str, kind: &str) -> Result<Vec<String>, String> {
        let user = self.username().await?;
        let mut items: Vec<(u64, String)> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut body = serde_json::json!({ "username": user, "set": set, "limit": 300 });
            if let Some(t) = &token {
                body["pagination_token"] = t.clone().into();
            }
            let page = self.post("/collection/v2/paging", body).await?;
            for it in page["items"].as_array().into_iter().flatten() {
                let Some(uri) = it["uri"].as_str().filter(|u| u.starts_with(kind)) else { continue };
                let added = it["added_at"].as_str().and_then(|a| a.parse().ok()).unwrap_or(0);
                items.push((added, uri.to_string()));
            }
            token = page["next_page_token"].as_str().filter(|t| !t.is_empty()).map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        items.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(items.into_iter().map(|(_, u)| u).collect())
    }

    pub async fn saved_albums(&self) -> Result<Vec<String>, String> {
        // The "collection" set holds liked songs too; keep only the albums.
        self.collection("collection", "spotify:album:").await
    }

    pub async fn followed_artists(&self) -> Result<Vec<String>, String> {
        self.collection("artist", "spotify:artist:").await
    }

    /// Names for albums and artists, fetched a few at a time.
    pub async fn names(&self, uris: &[String]) -> Vec<(String, Named)> {
        stream::iter(uris.iter().cloned())
            .map(|uri| async move {
                let (kind, id) = uri.strip_prefix("spotify:")?.split_once(':')?;
                let v = self.get(&format!("/metadata/4/{kind}/{}", base62_to_hex(id)?)).await.ok()?;
                let named = Named {
                    name: v["name"].as_str().unwrap_or("Unknown").to_string(),
                    subtitle: v["artist"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|a| a["name"].as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    thumb_url: v["cover_group"]["image"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .min_by_key(|i| i["width"].as_i64().unwrap_or(i64::MAX))
                        .and_then(|i| i["file_id"].as_str())
                        .map(|id| format!("https://i.scdn.co/image/{id}")),
                };
                Some((uri, named))
            })
            .buffer_unordered(8)
            .filter_map(|x| async move { x })
            .collect()
            .await
    }

    pub async fn cover(&self, url: &str) -> Result<Vec<u8>, String> {
        let resp = self.http.get(url).send().await.map_err(|e| e.to_string())?;
        resp.bytes().await.map(|b| b.to_vec()).map_err(|e| e.to_string())
    }
}

/// The smallest cover a playlist offers: its uploaded picture, or the image Spotify made for it.
fn playlist_thumb(attrs: &Value) -> Option<String> {
    use base64::Engine as _;
    if let Some(b64) = attrs["picture"].as_str()
        && let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(b64)
    {
        let mut hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        // Uploaded pictures come as the 300px image; swap in its small (~60px) sibling.
        if hex.len() == 40 && hex.starts_with("ab67706c0000") {
            hex.replace_range(12..16, "d72c");
        }
        return Some(format!("https://i.scdn.co/image/{hex}"));
    }
    let sizes = attrs["pictureSize"].as_array()?;
    let pick = |target: &str| sizes.iter().find(|s| s["targetName"] == target)?["url"].as_str().map(str::to_string);
    pick("small").or_else(|| pick("default"))
}

fn collect_tracks(tracks: &[Value], out: &mut Vec<String>) {
    for t in tracks {
        let uri = match t["uri"].as_str().filter(|u| !u.is_empty()) {
            Some(u) => u.to_string(),
            None => match t["gid"].as_str().and_then(hex_to_base62) {
                Some(id) => format!("spotify:track:{id}"),
                None => continue,
            },
        };
        // Local files and podcast episodes aren't playable here.
        if uri.starts_with("spotify:track:") {
            out.push(uri);
        }
    }
}

fn parse_track(t: &Value) -> Track {
    let names = |v: &Value| {
        v.as_array().into_iter().flatten().filter_map(|a| a["name"].as_str()).collect::<Vec<_>>().join(", ")
    };
    let album = &t["album"];
    // Pick the cover closest to 300px.
    let cover_url = album["cover_group"]["image"]
        .as_array()
        .into_iter()
        .flatten()
        .min_by_key(|i| (i["width"].as_i64().unwrap_or(0) - 300).abs())
        .and_then(|i| i["file_id"].as_str())
        .map(|id| format!("https://i.scdn.co/image/{id}"));
    Track {
        name: t["name"].as_str().unwrap_or("").to_string(),
        artists: names(&t["artist"]),
        album: album["name"].as_str().unwrap_or("").to_string(),
        duration_ms: t["duration"].as_u64().unwrap_or(0) as u32,
        cover_url,
    }
}

const BASE62: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

fn base62_to_hex(id: &str) -> Option<String> {
    let mut n: u128 = 0;
    for c in id.bytes() {
        let d = BASE62.iter().position(|&b| b == c)? as u128;
        n = n.checked_mul(62)?.checked_add(d)?;
    }
    Some(format!("{n:032x}"))
}

fn hex_to_base62(hex: &str) -> Option<String> {
    let mut n = u128::from_str_radix(hex, 16).ok()?;
    let mut out = [b'0'; 22];
    for slot in out.iter_mut().rev() {
        *slot = BASE62[(n % 62) as usize];
        n /= 62;
    }
    String::from_utf8(out.to_vec()).ok()
}
