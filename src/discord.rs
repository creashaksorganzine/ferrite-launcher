use discord_rich_presence::{DiscordIpc, DiscordIpcClient, activity};

pub struct DiscordPresence {
    client: DiscordIpcClient,
}

impl DiscordPresence {
    pub fn new() -> Option<Self> {
        let mut client = DiscordIpcClient::new("1548778248069054584");

        if let Err(err) = client.connect() {
            eprintln!("Could not connect to Discord: {err}");
            return None;
        }

        let activity = activity::Activity::new()
            .state("FERRITE LAUNCHER OMG!!!")
            .details("OMG THIS GUY IS USING FERRITE LAUNCHER")
            .assets(
                activity::Assets::new()
                    .large_image("pixil-frame-0_4_")
                    .large_text("Ferrite Launcher"),
            );

        if let Err(err) = client.set_activity(activity) {
            eprintln!("Could not set Discord activity: {err}");
        }

        Some(Self { client })
    }

    pub fn set_activity(&mut self, state: &str, details: &str) {
        let activity = activity::Activity::new()
            .state(state)
            .details(details)
            .assets(
                activity::Assets::new()
                    .large_image("pixil-frame-0_4_")
                    .large_text("Ferrite Launcher"),
            );

        if let Err(err) = self.client.set_activity(activity) {
            eprintln!("Could not update Discord activity: {err}");
        }
    }

    pub fn clear(&mut self) {
        let _ = self.client.clear_activity();
    }
}

impl Drop for DiscordPresence {
    fn drop(&mut self) {
        let _ = self.client.clear_activity();
        let _ = self.client.close();
    }
}
