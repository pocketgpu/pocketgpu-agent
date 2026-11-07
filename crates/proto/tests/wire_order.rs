//! A phone and an agent of different versions understand each other only while every message
//! keeps its number: postcard sends an enum variant as its position. New variants go at the
//! end. A variant once put in the middle shifted the rest, and an older agent read a newer
//! phone's push ticket as `Unpair`.

use pocketgpu_proto::msg::control::{
    AlertKind, AlertRule, IdleAlert, NewAction, PowerKind, Range, Request, Response,
};
use pocketgpu_proto::{Error, ErrorCode};

fn tag<T: serde::Serialize>(v: &T) -> u8 {
    postcard::to_allocvec(v).unwrap()[0]
}

fn idle() -> IdleAlert {
    IdleAlert {
        enabled: true,
        minutes: 30,
        cents_per_hour: Some(190),
    }
}

#[test]
fn requests_keep_their_numbers() {
    let action = NewAction {
        name: "a".into(),
        command: "true".into(),
        icon: "zap".into(),
        risky: false,
        timeout_s: None,
    };
    let cases = [
        (
            Request::Hello {
                app_version: "1".into(),
                proto: 1,
            },
            0,
        ),
        (
            Request::GetHistory {
                range: Range::H1,
                gpu: 0,
            },
            1,
        ),
        (Request::ListProcesses, 2),
        (Request::KillProcess { pid: 1 }, 3),
        (Request::ListActions, 4),
        (Request::RunAction { id: 1 }, 5),
        (Request::CreateAction(action.clone()), 6),
        (Request::EditAction { id: 1, action }, 7),
        (Request::DeleteAction { id: 1 }, 8),
        (Request::GetAlertSettings, 9),
        (Request::SetAlertSettings { rules: vec![] }, 10),
        (Request::SetPushTicket { ticket: "t".into() }, 11),
        (Request::Unpair, 12),
        (Request::ListRuns { limit: 1 }, 13),
        (Request::ListGames, 14),
        (
            Request::Status {
                app_version: "1".into(),
                proto: 1,
            },
            15,
        ),
        (Request::GetRecent, 16),
        (Request::CheckUpdate, 17),
        (
            Request::InstallUpdate {
                version: "1".into(),
            },
            18,
        ),
        (Request::ListDevices, 19),
        (Request::RevokeDevice { device: "d".into() }, 20),
        (Request::GetPower, 21),
        (
            Request::PowerAction {
                action: PowerKind::Restart,
            },
            22,
        ),
        (Request::GetSystemHistory { range: Range::H1 }, 23),
        (Request::GetIdleAlert, 24),
        (Request::SetIdleAlert(idle()), 25),
    ];
    for (req, n) in cases {
        assert_eq!(tag(&req), n, "{req:?} moved");
    }
}

#[test]
fn responses_keep_their_numbers() {
    let cases = [
        (Response::Ok, 0),
        (Response::History(vec![]), 2),
        (Response::Processes(vec![]), 3),
        (Response::Actions(vec![]), 4),
        (
            Response::ActionResult {
                exit_code: None,
                output: String::new(),
                duration_ms: 0,
                timed_out: false,
            },
            6,
        ),
        (Response::Runs(vec![]), 7),
        (Response::AlertSettings(vec![]), 8),
        (Response::Error(Error::new(ErrorCode::Invalid, "x")), 9),
        (Response::Games(vec![]), 10),
        (Response::Recent(vec![]), 12),
        (
            Response::Installed {
                version: "1".into(),
                restarting: true,
            },
            14,
        ),
        (Response::Devices(vec![]), 15),
        (Response::Revoked { device: "d".into() }, 16),
        (Response::PowerScheduled { at_ms: 0 }, 18),
        (Response::SystemHistory(vec![]), 19),
        (Response::IdleAlert(idle()), 20),
    ];
    for (res, n) in cases {
        assert_eq!(tag(&res), n, "{res:?} moved");
    }
}

#[test]
fn alert_kinds_keep_their_numbers() {
    let kinds = [
        AlertKind::JobEnd,
        AlertKind::GpuTemp,
        AlertKind::Offline,
        AlertKind::OllamaDown,
        AlertKind::DiskFull,
        AlertKind::Unknown,
        AlertKind::GenericText,
    ];
    for (n, kind) in kinds.into_iter().enumerate() {
        let rule = AlertRule {
            kind,
            enabled: true,
            threshold: None,
        };
        assert_eq!(tag(&rule) as usize, n, "{kind:?} moved");
    }
}
