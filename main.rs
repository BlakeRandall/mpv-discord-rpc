mod parent_watch;
mod discord;
mod arr;

use discord::Discord;

use single_instance::SingleInstance;

use interprocess::local_socket::{prelude::*, Stream};

#[cfg(target_os = "windows")]
use interprocess::local_socket::GenericNamespaced;

#[cfg(target_os = "linux")]
use interprocess::local_socket::GenericFilePath;

use serde_json::{json, Value};

use discord_rich_presence::activity;

use log::{debug, info, warn};

use std::error::Error;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DISCORD_APP_ID: &str = env!("DISCORD_APP_ID");
const MPV_SOCKET: &str = env!("MPV_SOCKET");

fn load_env() -> Option<PathBuf> {
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        let path = dir.join(".env");
        if dotenvy::from_path(&path).is_ok() {
            return Some(path);
        }
    }

    dotenvy::dotenv().ok()
}

fn connect_to_mpv() -> Result<Stream, Box<dyn Error>> {
    let socket = std::env::var("MPV_SOCKET")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| MPV_SOCKET.to_string());

    #[cfg(target_os = "windows")]
    {
        let name = socket.to_ns_name::<GenericNamespaced>()?;
        Ok(Stream::connect(name)?)
    }

    #[cfg(target_os = "linux")]
    {
        let path = if socket.starts_with('/') {
            socket
        } else {
            format!("/tmp/{socket}")
        };

        let name = path.to_fs_name::<GenericFilePath>()?;
        Ok(Stream::connect(name)?)
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = socket;
        Err("Unsupported operating system".into())
    }
}

fn get_property(property: &str) -> Result<Value, Box<dyn Error>> {
    let stream = connect_to_mpv()?;
    let mut reader = BufReader::new(stream);

    let request = json!({
        "command": ["get_property", property],
        "request_id": 1
    });

    writeln!(reader.get_mut(), "{request}")?;

    let mut response = String::new();
    reader.read_line(&mut response)?;

    let response: Value = serde_json::from_str(&response)?;

    if response["error"] != "success" {
        return Err(format!("mpv returned an error: {}", response["error"]).into());
    }

    Ok(response["data"].clone())
}

fn build_lines(media_title: &str, known: Option<arr::Lookup>, paused: bool) -> (String, String) {
    let (details, label) = match known {
        Some(l) => (l.details, l.state),
        None => (media_title.to_string(), None),
    };

    let state = match (label, paused) {
        (Some(l), true) => format!("{l} (paused)"),
        (Some(l), false) => l,
        (None, true) => "Paused".to_string(),
        (None, false) => "Playing".to_string(),
    };

    (details, state)
}

fn main() -> Result<(), Box<dyn Error>> {
    let env_path = load_env();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    match &env_path {
        Some(path) => info!("Loaded settings from {}", path.display()),
        None => warn!("No .env file found; using environment variables only."),
    }

    let instance = SingleInstance::new("mpv-discord-rich-presence")?;

    if !instance.is_single() {
        info!("Rich Presence is already running. Exiting.");
        return Ok(());
    }

    let app_id = std::env::var("DISCORD_APP_ID")
        .ok()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| DISCORD_APP_ID.to_string());

    let mut arr = arr::Arr::from_env();

    let running = Arc::new(AtomicBool::new(true));
    let shutdown_flag = Arc::clone(&running);

    ctrlc::set_handler(move || {
        info!("Ctrl+C received. Shutting down...");
        shutdown_flag.store(false, Ordering::SeqCst);
    })?;

    parent_watch::exit_when_parent_exits(Arc::clone(&running));

    let mut discord = Discord::new(app_id);

    info!("Rich Presence started. Press Ctrl+C to exit.");

    let mut last_shown: Option<(String, String)> = None;

    while running.load(Ordering::SeqCst) {
        if discord.connected() {
            last_shown = None;
        }

        let media = get_property("media-title").ok().and_then(|v| v.as_str().map(str::to_owned));
        let position = get_property("playback-time").ok().and_then(|v| v.as_f64());
        let paused = get_property("pause").ok().and_then(|v| v.as_bool());
        let duration = get_property("duration").ok().and_then(|v| v.as_f64());
        let idle = get_property("idle-active")
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let (Some(media), Some(position), Some(paused), false) = (media, position, paused, idle)
        else {
            if last_shown.take().is_some() {
                info!("Playback stopped or mpv closed. Clearing presence.");
            }
            discord.clear_activity();
            thread::sleep(Duration::from_secs(1));
            continue;
        };

        let path = get_property("path")
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();

        let full_path = if path.is_empty() || path.contains("://") || Path::new(&path).is_absolute()
        {
            path
        } else {
            let cwd = get_property("working-directory")
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            format!("{cwd}/{path}")
        };

        let (details, state) = build_lines(&media, arr.lookup(&full_path), paused);

        debug!("path={full_path:?} media-title={media:?} -> details={details:?} state={state:?}");

        let shown = (details.clone(), state.clone());
        if last_shown.as_ref() != Some(&shown) {
            info!("Now showing: {details} | {state}");
            last_shown = Some(shown);
        }

        let assets = activity::Assets::new()
            .large_image("static")
            .large_text("MEDIA PLAYER");

        let mut presence = activity::Activity::new()
            .activity_type(activity::ActivityType::Watching)
            .status_display_type(activity::StatusDisplayType::Name)
            .assets(assets)
            .details(&details)
            .state(&state)
            .name("mpv");

        if let Some(duration) = duration.filter(|d| !paused && *d > 0.0) {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let start = now - position as i64;
            presence = presence.timestamps(
                activity::Timestamps::new()
                    .start(start)
                    .end(start + duration as i64),
            );
        }

        discord.set_activity(presence);

        thread::sleep(Duration::from_secs(1));
    }

    info!("Cleaning up Discord Rich Presence...");

    discord.shutdown();

    info!("Shutdown complete.");

    Ok(())
}