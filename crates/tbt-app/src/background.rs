pub const DAEMON_COMMAND: &str = "tbt-daemon";

pub fn available() -> bool {
    tbt_core::sandbox::in_flatpak()
}

#[cfg(target_os = "linux")]
pub async fn request_autostart(enable: bool) -> anyhow::Result<bool> {
    use ashpd::desktop::background::Background;

    let reason = if enable {
        "Collect token usage while TokenBurnTrack is closed"
    } else {
        "Stop collecting token usage while TokenBurnTrack is closed"
    };
    let response = Background::request()
        .reason(reason)
        .auto_start(enable)
        .command([DAEMON_COMMAND])
        .dbus_activatable(false)
        .send()
        .await?
        .response()?;
    Ok(response.auto_start())
}

#[cfg(not(target_os = "linux"))]
pub async fn request_autostart(_enable: bool) -> anyhow::Result<bool> {
    anyhow::bail!("background collection through the portal is only available on Linux")
}
