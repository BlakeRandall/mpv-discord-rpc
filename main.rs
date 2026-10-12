mod parent_watch;

use single_instance::SingleInstance;

use interprocess::local_socket::{
    prelude::*,
    Stream,
};

#[cfg(target_os = "windows")]
use interprocess::local_socket::GenericNamespaced;

#[cfg(target_os = "linux")]
use interprocess::local_socket::GenericFilePath;

use serde_json::{json, Value};

use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};

use std::error::Error;
use std::io::{BufRead, BufReader, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};


const DISCORD_APP_ID: &str = env!("DISCORD_APP_ID");
const MPV_SOCKET: &str = env!("MPV_SOCKET");

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
        return Err(format!(
            "mpv returned an error: {}",
            response["error"]
        ).into());
    }

    Ok(response["data"].clone())
}

fn main() -> Result<(), Box<dyn Error>> {
    let instance = SingleInstance::new("mpv-discord-rich-presence")?;

    if !instance.is_single() {
        println!("Rich Presence is already running. Exiting.");
        return Ok(());
    }

    // Load environment variables from .env if present.
    dotenvy::dotenv().ok();

    let app_id = std::env::var("DISCORD_APP_ID")
        .ok()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| DISCORD_APP_ID.to_string());

    // Shared shutdown flag.
    let running = Arc::new(AtomicBool::new(true));
    let shutdown_flag = Arc::clone(&running);

    ctrlc::set_handler(move || {
        println!("\nCtrl+C received. Shutting down...");
        shutdown_flag.store(false, Ordering::SeqCst);
    })?;

    parent_watch::exit_when_parent_exits(Arc::clone(&running));

    // Connect to Discord.
    let mut client = DiscordIpcClient::new(&app_id);
    client.connect()?;

    println!("Rich Presence active. Press Ctrl+C to exit.");

    while running.load(Ordering::SeqCst) {
        let media = get_property("media-title")
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned));

        let position = get_property("playback-time")
            .ok()
            .and_then(|v| v.as_f64());

        let paused = get_property("pause")
            .ok()
            .and_then(|v| v.as_bool());

        let duration = get_property("duration")
            .ok()
            .and_then(|v| v.as_f64());

        let idle = get_property("idle-active")
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let idle = idle
            || media.is_none()
            || position.is_none()
            || paused.is_none();

        if idle {
            if let Err(error) = client.clear_activity() {
                eprintln!("Failed to clear activity: {error}");
            }

            thread::sleep(Duration::from_seconds(1));
            continue;
        }

        let (details, state, timestamps) =
            match (media, position, paused, duration) {
                (Some(title), Some(position), Some(paused), Some(duration))
                    if duration > 0.0 =>
                {
                    let state = if paused {
                        "Paused"
                    } else {
                        "Playing"
                    };

                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)?
                        .as_secs() as i64;

                    // Calculate the start and end timestamps.
                    let start = now - position as i64;
                    let end = start + duration as i64;

                    (
                        title,
                        state,
                        Some(activity::Timestamps::new()
                            .start(start)
                            .end(end)),
                    )
                }

                (Some(title), Some(_), Some(paused), _) => {
                    let state = if paused { "Paused" } else { "Playing" };

                    (title, state, None)
                }

                _ => (
                    "Waiting for mpv...".to_string(),
                    "Waiting for mpv...",
                    None,
                ),
            };

        let assets = activity::Assets::new()
            .large_image("static")
            .large_text("MEDIA PLAYER");

        let mut presence = activity::Activity::new()
            .activity_type(activity::ActivityType::Watching)
            .status_display_type(activity::StatusDisplayType::Name)
            .assets(assets)
            .details(&details)
            .state(state)
            .name("mpv");

        if let Some(timestamps) = timestamps {
            presence = presence.timestamps(timestamps);
        }

        // Refresh every iteration so timestamps stay synchronized.
        if let Err(error) = client.set_activity(presence) {
            eprintln!("Failed to update Discord activity: {error}");
        }

        thread::sleep(Duration::from_seconds(1));
    }

    println!("Cleaning up Discord Rich Presence...");

    if let Err(error) = client.clear_activity() {
        eprintln!("Failed to clear activity: {error}");
    }

    if let Err(error) = client.close() {
        eprintln!("Failed to close Discord RPC: {error}");
    }

    println!("Shutdown complete.");

    Ok(())
}