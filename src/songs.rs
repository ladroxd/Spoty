//! The order of the songs inside a playlist: which sort each list uses and custom orders.
//! Only the choices are kept on disk; the list itself is fetched and sorted when it's opened.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::auth::data_dir;
use crate::library::{Song, Track};

/// Sorting by name means one request per song, so very long lists don't offer it.
pub const MAX_SONGS_FOR_NAME_SORT: usize = 1000;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SongSort {
    /// The order the playlist has on Spotify.
    Playlist,
    Newest,
    Oldest,
    Title,
    Artist,
    Album,
    /// An order the user arranged by hand, saved on this PC.
    Custom,
}

impl SongSort {
    /// Every sort with its menu label and the name it's saved under.
    pub const ALL: [(SongSort, &str, &str); 7] = [
        (SongSort::Playlist, "Playlist order", "playlist"),
        (SongSort::Newest, "Recently added", "newest"),
        (SongSort::Oldest, "Oldest added", "oldest"),
        (SongSort::Title, "Title, A to Z", "title"),
        (SongSort::Artist, "Artist, A to Z", "artist"),
        (SongSort::Album, "Album, A to Z", "album"),
        (SongSort::Custom, "Custom order", "custom"),
    ];

    pub fn label(self) -> &'static str {
        Self::ALL.iter().find(|(s, ..)| *s == self).map_or("", |(_, label, _)| label)
    }

    fn saved_name(self) -> &'static str {
        Self::ALL.iter().find(|(s, ..)| *s == self).map_or("", |(.., name)| name)
    }

    fn from_saved_name(name: &str) -> Option<SongSort> {
        Self::ALL.iter().find(|(.., n)| *n == name).map(|(s, ..)| *s)
    }

    /// Whether this sort has to load every song's details first.
    pub fn needs_details(self) -> bool {
        matches!(self, SongSort::Title | SongSort::Artist | SongSort::Album)
    }
}

/// Newest or oldest first. Songs added together keep their playlist order, and songs without a
/// date go last either way.
pub fn by_date(mut songs: Vec<Song>, newest: bool) -> Vec<String> {
    if newest {
        songs.sort_by_key(|s| std::cmp::Reverse(s.added_at));
    } else {
        songs.sort_by_key(|s| (s.added_at == 0, s.added_at));
    }
    songs.into_iter().map(|s| s.uri).collect()
}

/// Sorts by title, artist or album. Songs whose details didn't load go last.
pub fn by_name(list: &mut [String], details: &HashMap<String, Track>, sort: SongSort) {
    list.sort_by_cached_key(|uri| {
        let Some(t) = details.get(uri) else { return (true, String::new()) };
        let artist = t.artists.first().map_or("", |a| a.name.as_str());
        let key = match sort {
            SongSort::Artist => format!("{artist}\0{}\0{}", t.album, t.name),
            SongSort::Album => format!("{}\0{}", t.album, t.name),
            _ => format!("{}\0{artist}", t.name),
        };
        (false, key.to_lowercase())
    });
}

/// Arranges the playlist's songs in a saved order. Songs since removed from the playlist are
/// dropped, and songs added since go at the end. A song listed twice can stay listed twice.
pub fn in_saved_order(saved: &[String], songs: Vec<String>) -> Vec<String> {
    let mut left: HashMap<&str, u32> = HashMap::with_capacity(songs.len());
    for uri in &songs {
        *left.entry(uri).or_default() += 1;
    }
    let mut out = Vec::with_capacity(songs.len());
    let mut take = |uri: &str, out: &mut Vec<String>| {
        if let Some(n) = left.get_mut(uri).filter(|n| **n > 0) {
            *n -= 1;
            out.push(uri.to_string());
        }
    };
    for uri in saved {
        take(uri, &mut out);
    }
    for uri in &songs {
        take(uri, &mut out);
    }
    out
}

/// Moves the song at `from` into the gap before position `gap` (`gap` may equal the length, for
/// the end of the list). Returns false if that wouldn't change anything.
pub fn move_song(list: &mut Vec<String>, from: usize, gap: usize) -> bool {
    if from >= list.len() || gap > list.len() || gap == from || gap == from + 1 {
        return false;
    }
    let song = list.remove(from);
    list.insert(if gap > from { gap - 1 } else { gap }, song);
    true
}

fn sorts_file() -> PathBuf {
    data_dir().join("song-sorts.json")
}

/// The sort of every list that doesn't use its playlist order, by list URI.
pub fn load_sorts() -> HashMap<String, SongSort> {
    let text = std::fs::read_to_string(sorts_file()).unwrap_or_default();
    let saved: HashMap<String, String> = serde_json::from_str(&text).unwrap_or_default();
    saved.into_iter().filter_map(|(uri, name)| Some((uri, SongSort::from_saved_name(&name)?))).collect()
}

pub fn save_sorts(sorts: &HashMap<String, SongSort>) {
    let saved: HashMap<&str, &str> = sorts.iter().map(|(uri, sort)| (uri.as_str(), sort.saved_name())).collect();
    let _ = std::fs::create_dir_all(data_dir());
    let _ = std::fs::write(sorts_file(), serde_json::to_string(&saved).unwrap_or_default());
}

fn order_file(list_uri: &str) -> PathBuf {
    let name: String = list_uri.rsplit(':').next().unwrap_or("").chars().filter(char::is_ascii_alphanumeric).collect();
    data_dir().join("orders").join(format!("{name}.txt"))
}

/// A list's custom order, if one was ever saved.
pub fn load_order(list_uri: &str) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(order_file(list_uri)).ok()?;
    Some(text.lines().map(|id| format!("spotify:track:{id}")).collect())
}

pub fn save_order(list_uri: &str, list: &[String]) {
    let path = order_file(list_uri);
    let text: Vec<&str> = list.iter().map(|uri| uri.strip_prefix("spotify:track:").unwrap_or(uri)).collect();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, text.join("\n"));
}

pub fn forget_order(list_uri: &str) {
    let _ = std::fs::remove_file(order_file(list_uri));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::ArtistRef;

    fn song(uri: &str, added_at: u64) -> Song {
        Song { uri: uri.into(), added_at }
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn dates_sort_both_ways_and_unknown_dates_go_last() {
        let songs = || vec![song("old", 100), song("none", 0), song("new", 300), song("tie-1", 200), song("tie-2", 200)];
        assert_eq!(by_date(songs(), true), ["new", "tie-1", "tie-2", "old", "none"]);
        assert_eq!(by_date(songs(), false), ["old", "tie-1", "tie-2", "new", "none"]);
    }

    #[test]
    fn names_sort_ignoring_case_with_missing_details_last() {
        let track = |name: &str, artist: &str, album: &str| Track {
            name: name.into(),
            artists: vec![ArtistRef { id: 1, name: artist.into() }],
            album: album.into(),
            album_id: None,
            duration_ms: 0,
            cover_url: None,
        };
        let details: HashMap<String, Track> = [
            ("a".to_string(), track("banana", "Zed", "Middle")),
            ("b".to_string(), track("Apple", "Yan", "Zoo")),
            ("c".to_string(), track("cherry", "yan", "Able")),
        ]
        .into();
        let sorted = |sort| {
            let mut list = strings(&["unknown", "a", "b", "c"]);
            by_name(&mut list, &details, sort);
            list
        };
        assert_eq!(sorted(SongSort::Title), ["b", "a", "c", "unknown"]);
        assert_eq!(sorted(SongSort::Artist), ["c", "b", "a", "unknown"]);
        assert_eq!(sorted(SongSort::Album), ["c", "a", "b", "unknown"]);
    }

    #[test]
    fn a_saved_order_follows_changes_to_the_playlist() {
        let saved = strings(&["c", "removed", "a", "a"]);
        // "a" is now only in the playlist once; "new" was added since.
        assert_eq!(in_saved_order(&saved, strings(&["a", "b", "c", "new"])), ["c", "a", "b", "new"]);
        // A song on the playlist twice is kept twice even if the saved order lost one.
        assert_eq!(in_saved_order(&strings(&["b", "a"]), strings(&["a", "b", "a"])), ["b", "a", "a"]);
        assert_eq!(in_saved_order(&[], strings(&["a", "b"])), ["a", "b"]);
    }

    #[test]
    fn moving_a_song_lands_in_the_gap() {
        let moved = |from, gap| {
            let mut list = strings(&["a", "b", "c", "d"]);
            let changed = move_song(&mut list, from, gap);
            (changed, list.concat())
        };
        assert_eq!(moved(0, 2), (true, "bacd".to_string()));
        assert_eq!(moved(3, 0), (true, "dabc".to_string()));
        assert_eq!(moved(1, 4), (true, "acdb".to_string()));
        // Dropping a song right where it already is, or somewhere impossible, does nothing.
        assert_eq!(moved(1, 1), (false, "abcd".to_string()));
        assert_eq!(moved(1, 2), (false, "abcd".to_string()));
        assert_eq!(moved(9, 0), (false, "abcd".to_string()));
        assert_eq!(moved(0, 9), (false, "abcd".to_string()));
    }

    #[test]
    fn every_sort_has_a_label_and_a_saved_name() {
        for (sort, label, name) in SongSort::ALL {
            assert_eq!(sort.label(), label);
            assert_eq!(SongSort::from_saved_name(name), Some(sort));
        }
    }

    #[test]
    fn order_files_stay_inside_their_folder() {
        assert!(order_file("spotify:playlist:abc123").ends_with("orders/abc123.txt"));
        assert!(order_file("spotify:collection").ends_with("orders/collection.txt"));
        assert!(order_file("spotify:playlist:..\\..\\evil").ends_with("orders/evil.txt"));
    }
}
