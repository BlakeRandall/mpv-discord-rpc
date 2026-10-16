use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};

use log::{debug, error, info, warn};

use std::time::{Duration, Instant};

const RETRY_INITIAL: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);

pub struct Discord {
    app_id: String,
    client: Option<DiscordIpcClient>,
    retry_delay: Duration,
    next_attempt: Instant,
}

impl Discord {
    pub fn new(app_id: String) -> Self {
        Self {
            app_id,
            client: None,
            retry_delay: RETRY_INITIAL,
            next_attempt: Instant::now(),
        }
    }

    pub fn connected(&mut self) -> bool {
        if self.client.is_some() || Instant::now() < self.next_attempt {
            return false;
        }

        let mut client = DiscordIpcClient::new(&self.app_id);
        match client.connect() {
            Ok(()) => {
                info!("Connected to Discord.");
                self.client = Some(client);
                self.retry_delay = RETRY_INITIAL;
                true
            }
            Err(e) => {
                debug!("Discord still unavailable ({e}); retrying in {:?}", self.retry_delay);
                self.schedule_retry();
                false
            }
        }
    }

    fn schedule_retry(&mut self) {
        self.next_attempt = Instant::now() + self.retry_delay;
        self.retry_delay = (self.retry_delay * 2).min(RETRY_MAX);
    }

    fn disconnect(&mut self, reason: &dyn std::fmt::Display) {
        warn!("Lost Discord connection: {reason}");
        if let Some(mut c) = self.client.take() {
            let _ = c.close();
        }
        self.retry_delay = RETRY_INITIAL;
        self.schedule_retry();
    }

    pub fn set_activity(&mut self, presence: activity::Activity) {
        let Some(client) = self.client.as_mut() else {
            return;
        };

        match client.set_activity(presence) {
            Ok(()) => {},
            Err(e) => {
                error!("Failed to update Discord activity: {e}");
                self.disconnect(&e);
            }
        }
    }

    pub fn clear_activity(&mut self) {
        let Some(client) = self.client.as_mut() else {
            return;
        };

        match client.clear_activity() {
            Ok(()) => {},
            Err(e) => self.disconnect(&e),
        }
    }

    pub fn shutdown(&mut self) {
        let Some(mut client) = self.client.take() else {
            return;
        };

        if let Err(error) = client.clear_activity() {
            debug!("Failed to clear activity: {error}");
        }

        if let Err(error) = client.close() {
            debug!("Failed to close Discord RPC: {error}");
        }
    }
}