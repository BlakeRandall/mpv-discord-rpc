use log::{debug, info, warn};
use percent_encoding::percent_decode_str;
use serde_json::Value;
use std::collections::HashMap;
use std::error::Error;
use std::time::{Duration, Instant};

const RETRY_AFTER: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct Lookup {
    pub details: String,
    pub state: Option<String>,
}

struct Service {
    base: String,
    key: String,
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl Service {
    fn from_env(name: &str, url_var: &str, key_var: &str) -> Option<Self> {
        let base = env_nonempty(url_var)?;
        let key = env_nonempty(key_var)?;
        info!("{name} lookup enabled ({base}).");
        Some(Service { base: base.trim_end_matches('/').to_string(), key })
    }

    fn get(&self, endpoint: &str) -> Result<Value, Box<dyn Error>> {
        let url = format!("{}/api/v3/{}", self.base, endpoint);
        debug!("GET {url}");

        let mut response = ureq::get(&url)
            .header("X-Api-Key", &self.key)
            .config()
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .call()?;
        Ok(response.body_mut().read_json::<Value>()?)
    }
}

pub struct Arr {
    sonarr: Option<Service>,
    radarr: Option<Service>,
    cache: HashMap<String, (Instant, Option<Lookup>)>,
}

impl Arr {
    pub fn from_env() -> Self {
        let arr = Arr {
            sonarr: Service::from_env("Sonarr", "SONARR_URL", "SONARR_API_KEY"),
            radarr: Service::from_env("Radarr", "RADARR_URL", "RADARR_API_KEY"),
            cache: HashMap::new(),
        };

        if arr.sonarr.is_none() && arr.radarr.is_none() {
            warn!("Neither Sonarr nor Radarr is configured; using mpv's media-title only.");
        }

        arr
    }

    pub fn lookup(&mut self, mpv_path: &str) -> Option<Lookup> {
        if let Some((at, found)) = self.cache.get(mpv_path) {
            if found.is_some() || at.elapsed() < RETRY_AFTER {
                return found.clone();
            }
        }

        debug!("Looking up: {mpv_path}");

        let result = match self.resolve(mpv_path) {
            Ok(Some(found)) => {
                info!(
                    "Matched: {} {}",
                    found.details,
                    found.state.as_deref().unwrap_or("")
                );
                Some(found)
            }
            Ok(None) => {
                warn!("No Sonarr/Radarr match for: {mpv_path}");
                None
            }
            Err(error) => {
                warn!("Sonarr/Radarr lookup failed: {error}");
                None
            }
        };

        self.cache.insert(mpv_path.to_string(), (Instant::now(), result.clone()));
        result
    }

    fn resolve(&self, mpv_path: &str) -> Result<Option<Lookup>, Box<dyn Error>> {
        if self.sonarr.is_none() && self.radarr.is_none() {
            return Ok(None);
        }
        if mpv_path.is_empty()
            || mpv_path.starts_with("http://")
            || mpv_path.starts_with("https://")
        {
            debug!("Skipping lookup for stream/empty path: {mpv_path:?}");
            return Ok(None);
        }

        let path = if mpv_path.contains("://") {
            percent_decode_str(mpv_path).decode_utf8_lossy().into_owned()
        } else {
            mpv_path.to_string()
        };
        let comps = components(&path);

        if let Some(sonarr) = &self.sonarr {
            if let Some(found) = find_episode(sonarr, &comps)? {
                return Ok(Some(found));
            }
        }
        if let Some(radarr) = &self.radarr {
            if let Some(found) = find_movie(radarr, &comps)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
}

fn components(path: &str) -> Vec<String> {
    path.replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn ends_with_tail(mpv_comps: &[String], folder: &str, relative: &str) -> bool {
    let mut tail = vec![folder.to_string()];
    tail.extend(components(relative));
    mpv_comps.ends_with(&tail)
}

fn find_episode(sonarr: &Service, comps: &[String]) -> Result<Option<Lookup>, Box<dyn Error>> {
    let series_list = sonarr.get("series")?;

    for series in series_list.as_array().into_iter().flatten() {
        let (Some(id), Some(title), Some(path)) =
            (series["id"].as_u64(), series["title"].as_str(), series["path"].as_str())
        else {
            continue;
        };

        let Some(folder) = components(path).pop() else { continue };
        if !comps.contains(&folder) {
            continue;
        }

        debug!("Sonarr candidate series: {title} (folder {folder:?})");

        let files = sonarr.get(&format!("episodefile?seriesId={id}"))?;
        for file in files.as_array().into_iter().flatten() {
            let (Some(file_id), Some(relative)) =
                (file["id"].as_u64(), file["relativePath"].as_str())
            else {
                continue;
            };

            if ends_with_tail(comps, &folder, relative) {
                debug!("Sonarr file match: {relative}");

                let episodes =
                    sonarr.get(&format!("episode?seriesId={id}&episodeFileId={file_id}"))?;
                let state = episodes
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|e| e["episodeFileId"].as_u64() == Some(file_id))
                    .min_by_key(|e| {
                        (
                            e["seasonNumber"].as_u64().unwrap_or(0),
                            e["episodeNumber"].as_u64().unwrap_or(0),
                        )
                    })
                    .and_then(|e| {
                    let season = e["seasonNumber"].as_u64()?;
                    let number = e["episodeNumber"].as_u64()?;
                    let label = format!("S{season:02}E{number:02}");

                    Some(match e["title"].as_str().filter(|t| !t.is_empty()) {
                        Some(t) => format!("{label} · {t}"),
                        None => label,
                    })
                });

                return Ok(Some(Lookup {
                    details: title.to_string(),
                    state,
                }));
            }
        }

        debug!("Series {title} found, but no episode file matched the path.");
    }

    Ok(None)
}

fn find_movie(radarr: &Service, comps: &[String]) -> Result<Option<Lookup>, Box<dyn Error>> {
    let movies = radarr.get("movie")?;

    for movie in movies.as_array().into_iter().flatten() {
        let (Some(title), Some(path), Some(relative)) = (
            movie["title"].as_str(),
            movie["path"].as_str(),
            movie["movieFile"]["relativePath"].as_str(),
        ) else {
            continue;
        };

        let Some(folder) = components(path).pop() else { continue };

        if ends_with_tail(comps, &folder, relative) {
            debug!("Radarr file match: {title}");

            let details = match movie["year"].as_u64().filter(|y| *y > 0) {
                Some(year) => format!("{title} ({year})"),
                None => title.to_string(),
            };
            return Ok(Some(Lookup { details, state: None }));
        }
    }

    Ok(None)
}