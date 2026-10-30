//! Alert settings from a phone: View reads them, Control changes them, and they're saved.

mod harness;

use harness::Harness;
use pocketgpu_proto::msg::control::{AlertKind, AlertRule, Request, Response};
use pocketgpu_proto::{ErrorCode, Permission};

fn set(kind: AlertKind, threshold: f32) -> Request {
    Request::SetAlertSettings {
        rules: vec![AlertRule {
            kind,
            enabled: true,
            threshold: Some(threshold),
        }],
    }
}

#[tokio::test]
async fn phones_read_and_change_alert_settings() {
    let h = Harness::start().await;
    let viewer = h.phone(Some(Permission::View)).await;
    let v = h.connect(&viewer).await.unwrap();
    let Response::AlertSettings(rules) = v.control(Request::GetAlertSettings).await.unwrap() else {
        panic!("expected AlertSettings");
    };
    assert_eq!(rules.len(), 6, "five alerts and the generic text setting");
    match v.control(set(AlertKind::GpuTemp, 80.0)).await {
        Err(pocketgpu_client::ClientError::Remote(e)) => assert_eq!(e.code, ErrorCode::Forbidden),
        other => panic!("view must not change alerts: {other:?}"),
    }

    let controller = h.phone(Some(Permission::Control)).await;
    let c = h.connect(&controller).await.unwrap();
    let Response::AlertSettings(rules) = c.control(set(AlertKind::GpuTemp, 80.0)).await.unwrap()
    else {
        panic!("expected AlertSettings");
    };
    let temp = rules.iter().find(|r| r.kind == AlertKind::GpuTemp).unwrap();
    assert_eq!((temp.enabled, temp.threshold), (true, Some(80.0)));
    // Saved for the next start.
    let saved = pocketgpu::store::config::Config::load(&h.agent.paths.config_file()).unwrap();
    assert_eq!(saved.alerts.gpu_temp_c, Some(80.0));
    assert_eq!(h.audit_events("alert_settings"), 1);

    match c.control(set(AlertKind::DiskFull, 10.0)).await {
        Err(pocketgpu_client::ClientError::Remote(e)) => assert_eq!(e.code, ErrorCode::Invalid),
        other => panic!("10% is out of range: {other:?}"),
    }
}

#[tokio::test]
async fn the_tray_pauses_and_resumes_alerts() {
    use pocketgpu::ipc::handle;
    use pocketgpu_proto::local;
    let h = Harness::start().await;
    let paused = |minutes| match handle(&h.agent, local::Request::PauseAlerts { minutes }) {
        local::Response::Status(s) => s.alerts_paused_until_ms,
        other => panic!("{other:?}"),
    };
    let until = paused(60).expect("paused");
    let left = until - pocketgpu::store::db::now_ms();
    assert!((59 * 60_000..=60 * 60_000).contains(&left), "{left}");
    assert!(pocketgpu::alerts::paused_until(&h.agent).is_some());
    assert_eq!(paused(0), None);
    assert!(pocketgpu::alerts::paused_until(&h.agent).is_none());
}
