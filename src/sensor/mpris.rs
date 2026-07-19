// MPRIS media player integration: session-bus watcher + sensor provider.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;

use anyhow::{Context, Result};
use zbus::zvariant::OwnedValue;
use zbus::{Connection, proxy};

use crate::config::MediaConfig;
use crate::render::background::BackgroundImage;
use crate::sensor::{SensorDescriptor, SensorProvider, SensorReading};

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";

/// Latest metadata from the selected MPRIS player.
#[derive(Clone)]
pub struct MediaSnapshot {
    pub player_id: String,
    pub status: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub position_us: i64,
    pub length_us: i64,
    pub art_url: String,
    pub art: Option<Arc<BackgroundImage>>,
    pub updated_at: Instant,
}

impl Default for MediaSnapshot {
    fn default() -> Self {
        Self {
            player_id: String::new(),
            status: String::new(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            position_us: 0,
            length_us: 0,
            art_url: String::new(),
            art: None,
            updated_at: Instant::now(),
        }
    }
}

/// Candidate player used for selection ranking (unit-testable).
#[derive(Debug, Clone)]
pub struct PlayerCandidate {
    pub bus_name: String,
    pub status: String,
    pub title: String,
    /// Higher = more recently seen.
    pub recency: u64,
}

#[proxy(
    interface = "org.freedesktop.DBus",
    default_service = "org.freedesktop.DBus",
    default_path = "/org/freedesktop/DBus"
)]
trait DBusPeer {
    fn list_names(&self) -> zbus::Result<Vec<String>>;
}

#[proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    default_path = "/org/mpris/MediaPlayer2"
)]
trait MediaPlayer {
    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    #[zbus(property)]
    fn position(&self) -> zbus::Result<i64>;
}

pub struct MprisProvider {
    snapshot: Arc<RwLock<MediaSnapshot>>,
}

impl MprisProvider {
    pub fn new(snapshot: Arc<RwLock<MediaSnapshot>>) -> Self {
        Self { snapshot }
    }
}

impl SensorProvider for MprisProvider {
    fn name(&self) -> &str {
        "mpris"
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
        let snap = self.snapshot.read().unwrap_or_else(|e| e.into_inner());

        let position_us = extrapolate_position(&snap);
        let progress = track_progress(position_us, snap.length_us);
        let has_art = if snap.art.is_some() { "1" } else { "0" };

        Ok(vec![
            reading("track_title", &snap.title, ""),
            reading("track_artist", &snap.artist, ""),
            reading("track_album", &snap.album, ""),
            reading("track_status", &snap.status, ""),
            reading("track_player", &snap.player_id, ""),
            reading("track_position", &format_mpris_time(position_us), ""),
            reading("track_duration", &format_mpris_time(snap.length_us), ""),
            reading(
                "track_position_s",
                &format_position_seconds(position_us),
                "s",
            ),
            reading(
                "track_duration_s",
                &format_position_seconds(snap.length_us),
                "s",
            ),
            reading("track_progress", &progress.to_string(), "%"),
            reading("track_has_art", has_art, ""),
        ])
    }

    fn available_sensors(&self) -> Vec<SensorDescriptor> {
        vec![
            desc("track_title", "Track title"),
            desc("track_artist", "Track artist"),
            desc("track_album", "Track album"),
            desc("track_status", "Playback status"),
            desc("track_player", "Player id"),
            desc("track_position", "Track position"),
            desc("track_duration", "Track duration"),
            desc_unit("track_position_s", "Track position (seconds)", "s"),
            desc_unit("track_duration_s", "Track duration (seconds)", "s"),
            desc_unit("track_progress", "Track progress", "%"),
            desc("track_has_art", "Album art available"),
        ]
    }
}

fn reading(key: &str, value: &str, unit: &str) -> SensorReading {
    SensorReading {
        key: key.to_string(),
        value: value.to_string(),
        unit: unit.to_string(),
    }
}

fn desc(key: &str, name: &str) -> SensorDescriptor {
    SensorDescriptor {
        key: key.to_string(),
        name: name.to_string(),
        unit: String::new(),
    }
}

fn desc_unit(key: &str, name: &str, unit: &str) -> SensorDescriptor {
    SensorDescriptor {
        key: key.to_string(),
        name: name.to_string(),
        unit: unit.to_string(),
    }
}

fn extrapolate_position(snap: &MediaSnapshot) -> i64 {
    let mut position_us = snap.position_us.max(0);
    if snap.status == "Playing" && snap.length_us > 0 {
        let extra = snap.updated_at.elapsed().as_micros() as i64;
        position_us = (position_us + extra).min(snap.length_us);
    }
    position_us
}

/// Format microseconds as `m:ss` or `h:mm:ss` when ≥ 1 hour.
pub fn format_mpris_time(us: i64) -> String {
    let total_secs = (us.max(0) / 1_000_000) as u64;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn format_position_seconds(us: i64) -> String {
    (us.max(0) / 1_000_000).to_string()
}

/// Progress percentage 0–100.
pub fn track_progress(position_us: i64, length_us: i64) -> i64 {
    if length_us <= 0 {
        return 0;
    }
    let pos = position_us.max(0) as i128;
    let len = length_us as i128;
    ((pos * 100) / len).clamp(0, 100) as i64
}

#[derive(Debug, Clone, Default)]
pub struct ParsedMetadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub length_us: i64,
    pub art_url: String,
}

/// Parse MPRIS `Metadata` property map into display fields.
pub fn parse_mpris_metadata(map: &HashMap<String, OwnedValue>) -> ParsedMetadata {
    let title = map
        .get("xesam:title")
        .and_then(value_as_str)
        .unwrap_or_default();
    let artist = map
        .get("xesam:artist")
        .map(parse_artist_value)
        .unwrap_or_default();
    let album = map
        .get("xesam:album")
        .and_then(value_as_str)
        .unwrap_or_default();
    let length_us = map.get("mpris:length").and_then(value_as_i64).unwrap_or(0);
    let art_url = map
        .get("mpris:artUrl")
        .and_then(value_as_str)
        .unwrap_or_default();

    ParsedMetadata {
        title,
        artist,
        album,
        length_us,
        art_url,
    }
}

fn value_as_str(value: &OwnedValue) -> Option<String> {
    if let Ok(s) = <&str>::try_from(value) {
        Some(s.to_string())
    } else if let Ok(s) = String::try_from(value.clone()) {
        Some(s)
    } else {
        None
    }
}

fn value_as_i64(value: &OwnedValue) -> Option<i64> {
    i64::try_from(value).ok().or_else(|| {
        u64::try_from(value)
            .ok()
            .and_then(|v| i64::try_from(v).ok())
    })
}

fn parse_artist_value(value: &OwnedValue) -> String {
    if let Ok(s) = <&str>::try_from(value) {
        return s.to_string();
    }
    if let Ok(s) = String::try_from(value.clone()) {
        return s;
    }
    if let Ok(values) = Vec::<String>::try_from(value.clone()) {
        return values.join(", ");
    }
    String::new()
}

pub fn player_id_from_bus_name(bus_name: &str) -> String {
    bus_name
        .strip_prefix(MPRIS_PREFIX)
        .unwrap_or(bus_name)
        .to_string()
}

fn is_mpris_player(name: &str) -> bool {
    name.starts_with(MPRIS_PREFIX)
}

/// Select the active MPRIS player from known candidates.
pub fn select_player<'a>(
    players: &'a [PlayerCandidate],
    preferred_player: &str,
) -> Option<&'a PlayerCandidate> {
    if players.is_empty() {
        return None;
    }

    let preferred = preferred_player.trim();
    if !preferred.is_empty() {
        let preferred_lower = preferred.to_ascii_lowercase();
        let mut matches: Vec<&PlayerCandidate> = players
            .iter()
            .filter(|p| p.bus_name.to_ascii_lowercase().contains(&preferred_lower))
            .collect();
        if matches.is_empty() {
            return None;
        }
        matches.sort_by_key(|p| std::cmp::Reverse(p.recency));
        if let Some(playing) = matches.iter().find(|p| p.status == "Playing") {
            return Some(playing);
        }
        return matches.into_iter().next();
    }

    if let Some(playing) = players.iter().find(|p| p.status == "Playing") {
        return Some(playing);
    }

    let mut idle: Vec<&PlayerCandidate> = players
        .iter()
        .filter(|p| p.status != "Stopped" && !p.title.is_empty())
        .collect();
    idle.sort_by_key(|p| std::cmp::Reverse(p.recency));
    idle.into_iter().next()
}

fn art_url_to_path(url: &str) -> Option<PathBuf> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("file://") {
        let path = if let Some(path) = rest.strip_prefix('/') {
            format!("/{path}")
        } else {
            rest.to_string()
        };
        let decoded = percent_encoding::percent_decode_str(&path).decode_utf8_lossy();
        return Some(PathBuf::from(decoded.as_ref()));
    }
    if trimmed.starts_with('/') {
        return Some(PathBuf::from(trimmed));
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        log::debug!("Skipping remote album art URL (v1 supports file/local only): {trimmed}");
        return None;
    }
    None
}

async fn load_art_from_url(url: &str) -> Option<Arc<BackgroundImage>> {
    let path = art_url_to_path(url)?;
    let path_display = path.display().to_string();
    match tokio::task::spawn_blocking(move || BackgroundImage::from_file(Path::new(&path))).await {
        Ok(Ok(img)) => Some(Arc::new(img)),
        Ok(Err(e)) => {
            log::debug!("Failed to load album art from {}: {e:#}", path_display);
            None
        }
        Err(e) => {
            log::warn!("Album art load task panicked: {e:#}");
            None
        }
    }
}

struct PlayerState {
    bus_name: String,
    status: String,
    title: String,
    artist: String,
    album: String,
    position_us: i64,
    length_us: i64,
    art_url: String,
    recency: u64,
}

enum PollExit {
    ConfigChanged,
    Closed,
}

fn clear_snapshot(snapshot: &Arc<RwLock<MediaSnapshot>>) {
    if let Ok(mut guard) = snapshot.write() {
        *guard = MediaSnapshot::default();
        guard.updated_at = Instant::now();
    }
}

pub struct MediaWatcher;

impl MediaWatcher {
    pub fn spawn(
        snapshot: Arc<RwLock<MediaSnapshot>>,
        mut config_rx: watch::Receiver<MediaConfig>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let config = config_rx.borrow_and_update().clone().normalized();
                if !config.enabled {
                    clear_snapshot(&snapshot);
                    if config_rx.changed().await.is_err() {
                        break;
                    }
                    continue;
                }

                match Self::poll_session(&snapshot, &config.player, &mut config_rx).await {
                    Ok(PollExit::Closed) => break,
                    Ok(PollExit::ConfigChanged) => continue,
                    Err(error) => {
                        log::warn!("MPRIS watcher error: {error:#}");
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                            res = config_rx.changed() => {
                                if res.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        })
    }

    async fn poll_session(
        snapshot: &Arc<RwLock<MediaSnapshot>>,
        preferred_player: &str,
        config_rx: &mut watch::Receiver<MediaConfig>,
    ) -> Result<PollExit> {
        let connection = Connection::session()
            .await
            .context("Failed to connect to session D-Bus")?;
        let dbus = DBusPeerProxy::new(&connection).await?;

        let mut players: HashMap<String, PlayerState> = HashMap::new();
        let mut recency: u64 = 0;
        let mut last_loaded_art_url = String::new();
        let mut last_loaded_art: Option<Arc<BackgroundImage>> = None;

        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                changed = config_rx.changed() => {
                    if let Some(exit) = Self::handle_config_change(config_rx, preferred_player, changed) {
                        return Ok(exit);
                    }
                }
                _ = interval.tick() => {
                    let cycle = Self::poll_cycle(
                        &connection,
                        &dbus,
                        &mut players,
                        preferred_player,
                        &mut recency,
                        &mut last_loaded_art_url,
                        &mut last_loaded_art,
                        snapshot,
                    );
                    tokio::select! {
                        biased;
                        changed = config_rx.changed() => {
                            if let Some(exit) = Self::handle_config_change(config_rx, preferred_player, changed) {
                                return Ok(exit);
                            }
                        }
                        result = cycle => {
                            result?;
                        }
                    }
                }
            }
        }
    }

    fn handle_config_change(
        config_rx: &mut watch::Receiver<MediaConfig>,
        preferred_player: &str,
        changed: Result<(), tokio::sync::watch::error::RecvError>,
    ) -> Option<PollExit> {
        match changed {
            Ok(()) => {
                let cfg = config_rx.borrow_and_update().clone().normalized();
                if !cfg.enabled || cfg.player != preferred_player {
                    Some(PollExit::ConfigChanged)
                } else {
                    None
                }
            }
            Err(_) => Some(PollExit::Closed),
        }
    }

    async fn poll_cycle(
        connection: &Connection,
        dbus: &DBusPeerProxy<'_>,
        players: &mut HashMap<String, PlayerState>,
        preferred_player: &str,
        recency: &mut u64,
        last_loaded_art_url: &mut String,
        last_loaded_art: &mut Option<Arc<BackgroundImage>>,
        snapshot: &Arc<RwLock<MediaSnapshot>>,
    ) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for name in dbus.list_names().await? {
            if !is_mpris_player(&name) {
                continue;
            }
            seen.insert(name.clone());
            *recency += 1;
            if let Some(state) = refresh_player(connection, &name, *recency).await {
                players.insert(name, state);
            } else {
                players.remove(&name);
            }
        }
        players.retain(|name, _| seen.contains(name));

        let candidates: Vec<PlayerCandidate> = players
            .values()
            .map(|p| PlayerCandidate {
                bus_name: p.bus_name.clone(),
                status: p.status.clone(),
                title: p.title.clone(),
                recency: p.recency,
            })
            .collect();

        let selected = select_player(&candidates, preferred_player);
        let mut next = MediaSnapshot::default();
        next.updated_at = Instant::now();

        if let Some(candidate) = selected {
            let state = players
                .get(&candidate.bus_name)
                .expect("selected player must exist");
            next.player_id = player_id_from_bus_name(&state.bus_name);
            next.status = state.status.clone();
            next.title = state.title.clone();
            next.artist = state.artist.clone();
            next.album = state.album.clone();
            next.position_us = state.position_us;
            next.length_us = state.length_us;
            next.art_url = state.art_url.clone();

            if next.art_url.is_empty() {
                last_loaded_art_url.clear();
                last_loaded_art.take();
                next.art = None;
            } else if next.art_url == *last_loaded_art_url {
                next.art = last_loaded_art.clone();
            } else if let Some(art) = load_art_from_url(&next.art_url).await {
                last_loaded_art_url.clone_from(&next.art_url);
                *last_loaded_art = Some(art.clone());
                next.art = Some(art);
            } else {
                next.art = None;
            }
        } else {
            last_loaded_art_url.clear();
            last_loaded_art.take();
        }

        if let Ok(mut guard) = snapshot.write() {
            *guard = next;
        }

        Ok(())
    }
}

async fn refresh_player(
    connection: &Connection,
    bus_name: &str,
    recency: u64,
) -> Option<PlayerState> {
    let proxy = match MediaPlayerProxy::builder(connection)
        .destination(bus_name)
        .ok()?
        .path(MPRIS_PATH)
        .ok()?
        .build()
        .await
    {
        Ok(proxy) => proxy,
        Err(error) => {
            log::debug!("Failed to build MPRIS proxy for {bus_name}: {error}");
            return None;
        }
    };

    let status = proxy.playback_status().await.unwrap_or_default();
    let metadata_map = proxy.metadata().await.unwrap_or_default();
    let parsed = parse_mpris_metadata(&metadata_map);
    let position_us = proxy.position().await.unwrap_or(0);

    Some(PlayerState {
        bus_name: bus_name.to_string(),
        status,
        title: parsed.title,
        artist: parsed.artist,
        album: parsed.album,
        position_us,
        length_us: parsed.length_us,
        art_url: parsed.art_url,
        recency,
    })
}

/// Effective SVG background when album-art override is enabled.
pub fn effective_background(
    media_config: &MediaConfig,
    media_snapshot: &Arc<RwLock<MediaSnapshot>>,
    user_background: &Option<Arc<BackgroundImage>>,
) -> Option<Arc<BackgroundImage>> {
    if !media_config.enabled || !media_config.album_art_background {
        return user_background.clone();
    }
    let snap = media_snapshot.read().unwrap_or_else(|e| e.into_inner());
    if snap.status == "Playing" || snap.status == "Paused" {
        if let Some(art) = &snap.art {
            return Some(art.clone());
        }
    }
    user_background.clone()
}

pub fn backgrounds_equal(
    current: &Option<Arc<BackgroundImage>>,
    next: &Option<Arc<BackgroundImage>>,
) -> bool {
    match (current, next) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;
    fn metadata_map(entries: Vec<(&str, Value<'_>)>) -> HashMap<String, OwnedValue> {
        entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), OwnedValue::try_from(v).unwrap()))
            .collect()
    }

    #[test]
    fn parse_mpris_metadata_joins_artist_array() {
        let map = metadata_map(vec![
            ("xesam:title", Value::from("Hard Times")),
            (
                "xesam:artist",
                Value::from(vec!["Hayley Williams", "Taylor York"]),
            ),
            ("xesam:album", Value::from("After Laughter")),
            ("mpris:length", Value::from(192_000_000_i64)),
            ("mpris:artUrl", Value::from("file:///tmp/cover.jpg")),
        ]);
        let parsed = parse_mpris_metadata(&map);
        assert_eq!(parsed.title, "Hard Times");
        assert_eq!(parsed.artist, "Hayley Williams, Taylor York");
        assert_eq!(parsed.album, "After Laughter");
        assert_eq!(parsed.length_us, 192_000_000);
        assert_eq!(parsed.art_url, "file:///tmp/cover.jpg");
    }

    #[test]
    fn format_mpris_time_clamps_negative_and_formats_hour() {
        assert_eq!(format_mpris_time(-5), "0:00");
        assert_eq!(format_mpris_time(83_000_000), "1:23");
        assert_eq!(format_mpris_time(3_665_000_000), "1:01:05");
    }

    #[test]
    fn track_progress_clamps() {
        assert_eq!(track_progress(83_000_000, 0), 0);
        assert_eq!(track_progress(83_000_000, 192_000_000), 43);
        assert_eq!(track_progress(300_000_000, 200_000_000), 100);
    }

    #[test]
    fn select_player_prefers_preferred_substring_playing() {
        let players = vec![
            PlayerCandidate {
                bus_name: "org.mpris.MediaPlayer2.firefox".into(),
                status: "Playing".into(),
                title: "A".into(),
                recency: 1,
            },
            PlayerCandidate {
                bus_name: "org.mpris.MediaPlayer2.spotify".into(),
                status: "Paused".into(),
                title: "B".into(),
                recency: 2,
            },
        ];
        let selected = select_player(&players, "spotify").unwrap();
        assert!(selected.bus_name.contains("spotify"));
    }

    #[test]
    fn select_player_prefers_playing_when_no_preference() {
        let players = vec![
            PlayerCandidate {
                bus_name: "org.mpris.MediaPlayer2.firefox".into(),
                status: "Paused".into(),
                title: "A".into(),
                recency: 2,
            },
            PlayerCandidate {
                bus_name: "org.mpris.MediaPlayer2.spotify".into(),
                status: "Playing".into(),
                title: "B".into(),
                recency: 1,
            },
        ];
        let selected = select_player(&players, "").unwrap();
        assert_eq!(selected.status, "Playing");
    }

    #[test]
    fn idle_provider_emits_empty_track_keys() {
        let snapshot = Arc::new(RwLock::new(MediaSnapshot::default()));
        let mut provider = MprisProvider::new(snapshot);
        let readings = provider.poll().unwrap();
        let map: HashMap<_, _> = readings.into_iter().map(|r| (r.key, r.value)).collect();
        assert_eq!(map["track_title"], "");
        assert_eq!(map["track_position"], "0:00");
        assert_eq!(map["track_progress"], "0");
        assert_eq!(map["track_has_art"], "0");
    }

    #[test]
    fn effective_background_uses_art_when_playing() {
        let art_bytes = include_bytes!("../../assets/backgrounds/dark-solid.png");
        let art = Arc::new(BackgroundImage::decode(art_bytes).unwrap());
        let user = Arc::new(BackgroundImage::decode(art_bytes).unwrap());

        let snapshot = Arc::new(RwLock::new(MediaSnapshot {
            status: "Playing".into(),
            art: Some(art.clone()),
            ..Default::default()
        }));

        let config = MediaConfig {
            enabled: true,
            album_art_background: true,
            ..Default::default()
        };
        let effective = effective_background(&config, &snapshot, &Some(user));
        assert!(Arc::ptr_eq(effective.as_ref().unwrap(), &art));
    }

    #[tokio::test]
    async fn watcher_disabled_clears_snapshot_without_session_bus() {
        let snapshot = Arc::new(RwLock::new(MediaSnapshot {
            title: "Active".into(),
            status: "Playing".into(),
            ..Default::default()
        }));
        let (_tx, rx) = watch::channel(MediaConfig {
            enabled: false,
            ..Default::default()
        });
        let _handle = MediaWatcher::spawn(snapshot.clone(), rx);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let snap = snapshot.read().unwrap();
        assert_eq!(snap.title, "");
        assert_eq!(snap.status, "");
    }

    #[test]
    fn effective_background_disabled_media_restores_user() {
        let art_bytes = include_bytes!("../../assets/backgrounds/dark-solid.png");
        let art = Arc::new(BackgroundImage::decode(art_bytes).unwrap());
        let user = Arc::new(BackgroundImage::decode(art_bytes).unwrap());
        let snapshot = Arc::new(RwLock::new(MediaSnapshot {
            status: "Playing".into(),
            art: Some(art),
            ..Default::default()
        }));
        let config = MediaConfig {
            enabled: false,
            album_art_background: true,
            ..Default::default()
        };
        let effective = effective_background(&config, &snapshot, &Some(user.clone()));
        assert!(Arc::ptr_eq(effective.as_ref().unwrap(), &user));
    }

    #[test]
    fn effective_background_restores_user_when_stopped() {
        let art_bytes = include_bytes!("../../assets/backgrounds/dark-solid.png");
        let art = Arc::new(BackgroundImage::decode(art_bytes).unwrap());
        let user = Arc::new(BackgroundImage::decode(art_bytes).unwrap());

        let snapshot = Arc::new(RwLock::new(MediaSnapshot {
            status: "Stopped".into(),
            art: Some(art),
            ..Default::default()
        }));

        let config = MediaConfig {
            enabled: true,
            album_art_background: true,
            ..Default::default()
        };
        let effective = effective_background(&config, &snapshot, &Some(user.clone()));
        assert!(Arc::ptr_eq(effective.as_ref().unwrap(), &user));
    }

    #[test]
    fn track_progress_does_not_overflow() {
        assert_eq!(track_progress(i64::MAX, i64::MAX), 100);
    }

    #[test]
    fn art_url_to_path_percent_decodes_file_uri() {
        assert_eq!(
            art_url_to_path("file:///tmp/Album%20Art.jpg"),
            Some(PathBuf::from("/tmp/Album Art.jpg")),
        );
    }
}
