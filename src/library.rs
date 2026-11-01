//! Search, Liked Songs, playlists and track details. These come from the same internal
//! endpoints the official app uses: the public Web API's quota for this client ID is shared
//! by every third-party player and permanently exhausted.

use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use serde_json::Value;
use tokio::sync::OnceCell;

use crate::auth::Auth;

const SPCLIENT: &str = "https://spclient.wg.spotify.com";
/// The context holding the user's Liked Songs.
pub const LIKED_SONGS: &str = "spotify:collection";

/// One of a track's artists. The ID is kept as a number: a track row needs far less memory
/// this way than with a full URI per artist, and the URI is only built when it's clicked.
#[derive(Clone, Debug)]
pub struct ArtistRef {
    pub id: u128,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Track {
    pub name: String,
    pub artists: Vec<ArtistRef>,
    pub album: String,
    pub album_id: Option<u128>,
    pub duration_ms: u32,
    pub cover_url: Option<String>,
}

/// A display name for an album or artist, fetched lazily.
#[derive(Clone, Debug)]
pub struct Named {
    pub name: String,
    /// The album's artists; empty for artists.
    pub subtitle: String,
    /// Release year for albums; 0 when unknown.
    pub year: u16,
    /// The smallest cover or portrait Spotify has, for list thumbnails.
    pub thumb_url: Option<String>,
}

/// Everything shown on an artist's page. It all comes from one request; albums are only IDs
/// here, and their names load as their rows scroll into view.
pub struct Artist {
    pub name: String,
    pub thumb_url: Option<String>,
    pub top_tracks: Vec<String>,
    pub albums: Vec<String>,
    pub singles: Vec<String>,
    pub compilations: Vec<String>,
    pub appears_on: Vec<String>,
}

pub fn artist_uri(id: u128) -> String {
    format!("spotify:artist:{}", id_to_base62(id))
}

pub fn album_uri(id: u128) -> String {
    format!("spotify:album:{}", id_to_base62(id))
}

/// A song on a list and when it was added to that list.
pub struct Song {
    pub uri: String,
    /// 0 when Spotify doesn't say. Only comparable with other songs from the same list.
    pub added_at: u64,
}

#[derive(Clone, Debug)]
pub struct Playlist {
    pub uri: String,
    pub name: String,
    pub songs: u32,
    /// Created by the signed-in user (as opposed to followed).
    pub mine: bool,
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
        self.fetch(req).await?.json().await.map_err(|e| e.to_string())
    }

    async fn fetch(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response, String> {
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
        Ok(resp)
    }

    /// Track URIs for a context: a playlist, an album, or the user's collection.
    pub async fn context_tracks(&self, context_uri: &str) -> Result<Vec<String>, String> {
        Ok(self.songs(context_uri).await?.into_iter().map(|s| s.uri).collect())
    }

    /// A context's songs with the dates they were added.
    pub async fn songs(&self, context_uri: &str) -> Result<Vec<Song>, String> {
        Ok(self.context(context_uri).await?.1)
    }

    /// An artist's top tracks and their releases, newest first.
    pub async fn artist(&self, uri: &str) -> Result<Artist, String> {
        let id = uri.strip_prefix("spotify:artist:").and_then(base62_to_hex).ok_or("not an artist link")?;
        let v = self.get(&format!("/metadata/4/artist/{id}")).await?;
        let top_tracks = v["top_track"][0]["track"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| Some(format!("spotify:track:{}", hex_to_base62(t["gid"].as_str()?)?)))
            .collect();
        // Each entry lists the same release once per regional edition; the first one will do.
        let group = |name: &str| -> Vec<String> {
            v[name]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|g| Some(format!("spotify:album:{}", hex_to_base62(g["album"][0]["gid"].as_str()?)?)))
                .collect()
        };
        Ok(Artist {
            name: v["name"].as_str().unwrap_or("Artist").to_string(),
            thumb_url: smallest_image(&v["portrait_group"]),
            top_tracks,
            albums: group("album_group"),
            singles: group("single_group"),
            compilations: group("compilation_group"),
            appears_on: group("appears_on_group"),
        })
    }

    /// Returns the context's owner (if any) and its songs, following page links.
    async fn context(&self, context_uri: &str) -> Result<(Option<String>, Vec<Song>), String> {
        let ctx = self.get(&format!("/context-resolve/v1/{context_uri}")).await?;
        let owner = context_owner(&ctx);
        let mut uris = Vec::new();
        let mut pending = Vec::new();
        for page in ctx["pages"].as_array().into_iter().flatten() {
            let tracks = page["tracks"].as_array().map(Vec::as_slice).unwrap_or_default();
            collect_tracks(tracks, &mut uris);
            if tracks.is_empty() && let Some(u) = page["page_url"].as_str() {
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

    /// Song search. Falls back to the older search context (top 20 only) if the search
    /// service is unavailable, so songs keep working either way.
    pub async fn search_tracks(&self, query: &str) -> Result<Vec<String>, String> {
        match self.search_hits("tracks", query, 50).await {
            Ok(body) => Ok(hits(&body).map(|h| h.uri.to_string()).collect()),
            Err(_) => self.context_tracks(&format!("spotify:search:{}", percent_encode(query, "+"))).await,
        }
    }

    pub async fn search_albums(&self, query: &str) -> Result<Vec<(String, Named)>, String> {
        Ok(hits(&self.search_hits("albums", query, 30).await?).map(Hit::into_named).collect())
    }

    pub async fn search_artists(&self, query: &str) -> Result<Vec<(String, Named)>, String> {
        Ok(hits(&self.search_hits("artists", query, 20).await?).map(Hit::into_named).collect())
    }

    /// Asks the search service for one kind of result. It always answers in protobuf.
    async fn search_hits(&self, kind: &str, query: &str, limit: u32) -> Result<Vec<u8>, String> {
        let url = format!(
            "{SPCLIENT}/searchview/v1/search/{kind}/{}?entityVersion=2&catalogue=&country=&platform=zelda&locale=en&limit={limit}&username={}",
            percent_encode(query, "%20"),
            percent_encode(&self.username().await?, "%20"),
        );
        let resp = self.fetch(self.http.get(&url)).await?;
        resp.bytes().await.map(|b| b.to_vec()).map_err(|e| e.to_string())
    }

    pub async fn liked(&self) -> Result<Vec<Song>, String> {
        let (owner, songs) = self.context(LIKED_SONGS).await?;
        if let Some(o) = owner {
            let _ = self.username.set(o);
        }
        Ok(songs)
    }

    async fn username(&self) -> Result<String, String> {
        if let Some(u) = self.username.get() {
            return Ok(u.clone());
        }
        // The collection context names its owner, which is the signed-in user.
        // The first page says so; there's no need to follow the rest of the collection.
        let ctx = self.get(&format!("/context-resolve/v1/{LIKED_SONGS}")).await?;
        let owner = context_owner(&ctx).ok_or("couldn't tell who is signed in")?;
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
                .get(&format!("/playlist/v2/user/{user}/rootlist?decorate=attributes,length,owner&from={from}&length={PAGE}"))
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
                    mine: m["ownerUsername"].as_str() == Some(user.as_str()),
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
                    year: v["date"]["year"].as_u64().unwrap_or(0) as u16,
                    thumb_url: smallest_image(&v["cover_group"]).or_else(|| smallest_image(&v["portrait_group"])),
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

fn context_owner(ctx: &Value) -> Option<String> {
    ctx["metadata"]["context_owner"].as_str().map(|o| o.trim_start_matches("spotify:user:").to_string())
}

fn smallest_image(group: &Value) -> Option<String> {
    group["image"]
        .as_array()?
        .iter()
        .min_by_key(|i| i["width"].as_i64().unwrap_or(i64::MAX))?["file_id"]
        .as_str()
        .map(|id| format!("https://i.scdn.co/image/{id}"))
}

fn percent_encode(text: &str, space: &str) -> String {
    let mut out = String::new();
    for b in text.trim().bytes() {
        match b {
            b' ' => out.push_str(space),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// One search result, borrowed from the response.
struct Hit<'a> {
    uri: &'a str,
    name: &'a str,
    image: &'a str,
    /// Artists and year; only albums have them.
    artists: &'a str,
    year: u16,
}

impl Hit<'_> {
    fn into_named(self) -> (String, Named) {
        let name = Named {
            name: self.name.to_string(),
            subtitle: self.artists.to_string(),
            year: self.year,
            thumb_url: search_thumb(self.image),
        };
        (self.uri.to_string(), name)
    }
}

/// Search gives images as `spotify:image:<id>`. Album covers come at 300px; ask for the 64px one.
fn search_thumb(image: &str) -> Option<String> {
    let id = image.strip_prefix("spotify:image:").filter(|id| !id.is_empty())?;
    Some(format!("https://i.scdn.co/image/{}", id.replacen("ab67616d00001e02", "ab67616d00004851", 1)))
}

/// The results in a search response, in the order Spotify ranked them.
fn hits(body: &[u8]) -> impl Iterator<Item = Hit<'_>> {
    Proto(body).filter_map(|(field, value)| {
        let Wire::Bytes(entity) = value else { return None };
        if field != 1 {
            return None;
        }
        let mut hit = Hit { uri: "", name: "", image: "", artists: "", year: 0 };
        for (f, v) in Proto(entity) {
            match (f, v) {
                (1, Wire::Bytes(b)) => hit.uri = std::str::from_utf8(b).ok()?,
                (2, Wire::Bytes(b)) => hit.name = std::str::from_utf8(b).ok()?,
                (3, Wire::Bytes(b)) => hit.image = std::str::from_utf8(b).unwrap_or(""),
                (6, Wire::Bytes(album)) => {
                    for (f, v) in Proto(album) {
                        match (f, v) {
                            (1, Wire::Bytes(b)) => hit.artists = std::str::from_utf8(b).unwrap_or(""),
                            (3, Wire::Int(y)) => hit.year = y as u16,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        (!hit.uri.is_empty()).then_some(hit)
    })
}

enum Wire<'a> {
    Int(u64),
    Bytes(&'a [u8]),
}

/// Walks the fields of a protobuf message. Search is the only thing that needs protobuf, and
/// this little reader is much lighter than a protobuf crate with generated types.
struct Proto<'a>(&'a [u8]);

impl Proto<'_> {
    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = self.0.split_first()?;
            self.0 = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Some(value);
            }
        }
        None
    }
}

impl<'a> Iterator for Proto<'a> {
    type Item = (u64, Wire<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let key = self.varint()?;
        let value = match key & 7 {
            0 => Wire::Int(self.varint()?),
            1 => {
                self.0 = self.0.get(8..)?;
                Wire::Int(0)
            }
            2 => {
                let len = usize::try_from(self.varint()?).ok()?;
                let (bytes, rest) = self.0.split_at_checked(len)?;
                self.0 = rest;
                Wire::Bytes(bytes)
            }
            5 => {
                self.0 = self.0.get(4..)?;
                Wire::Int(0)
            }
            _ => return None,
        };
        Some((key >> 3, value))
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

fn collect_tracks(tracks: &[Value], out: &mut Vec<Song>) {
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
            out.push(Song { uri, added_at: added_at(t) });
        }
    }
}

/// A context lists when each song was added, normally as a string of digits.
fn added_at(track: &Value) -> u64 {
    let value = &track["metadata"]["added_at"];
    value.as_u64().or_else(|| value.as_str()?.parse().ok()).unwrap_or(0)
}

fn parse_track(t: &Value) -> Track {
    let gid = |v: &Value| v["gid"].as_str().and_then(|g| u128::from_str_radix(g, 16).ok());
    let artists = t["artist"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Some(ArtistRef { id: gid(a)?, name: a["name"].as_str()?.to_string() }))
        .collect();
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
        artists,
        album: album["name"].as_str().unwrap_or("").to_string(),
        album_id: gid(album),
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
    Some(id_to_base62(u128::from_str_radix(hex, 16).ok()?))
}

fn id_to_base62(mut n: u128) -> String {
    let mut out = [b'0'; 22];
    for slot in out.iter_mut().rev() {
        *slot = BASE62[(n % 62) as usize];
        n /= 62;
    }
    out.iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(number: u8, bytes: &[u8]) -> Vec<u8> {
        let mut out = vec![number << 3 | 2, bytes.len() as u8];
        out.extend_from_slice(bytes);
        out
    }

    #[test]
    fn reads_album_hits_in_order() {
        // The shape of a real albums response: uri, name, image, then artists and year.
        let mut details = field(1, b"Daft Punk");
        details.extend([3 << 3, 0xDD, 0x0F]); // year 2013 as a varint
        let mut album = field(1, b"spotify:album:x");
        album.extend(field(2, b"Discovery"));
        album.extend(field(3, b"spotify:image:ab67616d00001e02abc"));
        album.extend(field(6, &details));
        let mut body = field(1, &album);
        body.extend(field(1, &field(1, b"spotify:album:y")));
        body.extend(field(3, &[1, 2, 4])); // other top-level fields are ignored

        let found: Vec<_> = hits(&body).collect();
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].uri, found[0].name, found[0].artists, found[0].year), ("spotify:album:x", "Discovery", "Daft Punk", 2013));
        assert_eq!(found[1].uri, "spotify:album:y");
        let (_, named) = hits(&body).next().unwrap().into_named();
        assert_eq!(named.thumb_url.as_deref(), Some("https://i.scdn.co/image/ab67616d00004851abc"));
    }

    #[test]
    fn stops_at_truncated_data_without_panicking() {
        let mut body = field(1, &field(1, b"spotify:album:x"));
        body.extend([1 << 3 | 2, 200, 1, 1, 2]); // claims far more bytes than remain
        assert_eq!(hits(&body).count(), 1);
    }

    #[test]
    fn ids_convert_both_ways() {
        let id = "4tZwfgrHOc3mvqYlEYSvVi";
        assert_eq!(base62_to_hex(id).as_deref(), Some("9352616e1b6a4a5fbe8764a1648b51ac"));
        assert_eq!(hex_to_base62("9352616e1b6a4a5fbe8764a1648b51ac").as_deref(), Some(id));
        assert_eq!(artist_uri(0x9352616e1b6a4a5fbe8764a1648b51ac), format!("spotify:artist:{id}"));
    }

    #[test]
    fn reads_when_a_song_was_added() {
        let track = |added: Value| serde_json::json!({ "uri": "spotify:track:x", "metadata": { "added_at": added } });
        assert_eq!(added_at(&track("1621959480000".into())), 1621959480000);
        assert_eq!(added_at(&track(1621959480.into())), 1621959480);
        assert_eq!(added_at(&track("not a date".into())), 0);
        assert_eq!(added_at(&serde_json::json!({ "uri": "spotify:track:x" })), 0);
    }

    #[test]
    fn encodes_queries_for_urls() {
        assert_eq!(percent_encode(" café del mar ", "%20"), "caf%C3%A9%20del%20mar");
        assert_eq!(percent_encode("a/b c", "+"), "a%2Fb+c");
    }
}
