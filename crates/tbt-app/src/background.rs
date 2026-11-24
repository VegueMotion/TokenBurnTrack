pub const DAEMON_COMMAND: &str = "tbt-daemon";

#[cfg(target_os = "linux")]
const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    Running,
    AlreadyRunning,
    Failed,
}

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

#[cfg(target_os = "linux")]
pub async fn spawn_daemon() -> anyhow::Result<Started> {
    use ashpd::flatpak::{Flatpak, SpawnFlags, SpawnOptions};
    use futures_lite::{StreamExt, future};
    use std::collections::HashMap;
    use std::os::fd::BorrowedFd;

    let portal = Flatpak::new().await?;
    let mut exits = portal.receive_spawn_exited().await?;
    let pid = portal
        .spawn(
            "/",
            &[DAEMON_COMMAND],
            HashMap::<u32, BorrowedFd<'_>>::new(),
            HashMap::new(),
            SpawnFlags::NoNetwork | SpawnFlags::LatestVersion,
            SpawnOptions::default(),
        )
        .await?;
    let exited = async {
        while let Some((exited_pid, status)) = exits.next().await {
            if exited_pid == pid {
                return Some(status);
            }
        }
        None
    };
    let grace = async {
        async_io::Timer::after(STARTUP_GRACE).await;
        None
    };
    Ok(match future::or(exited, grace).await {
        None => Started::Running,
        Some(0) => Started::AlreadyRunning,
        Some(_) => Started::Failed,
    })
}

#[cfg(not(target_os = "linux"))]
pub async fn spawn_daemon() -> anyhow::Result<Started> {
    anyhow::bail!("starting tbt-daemon through the portal is only available on Linux")
}

#[cfg(not(target_os = "linux"))]
pub async fn request_autostart(_enable: bool) -> anyhow::Result<bool> {
    anyhow::bail!("background collection through the portal is only available on Linux")
}
