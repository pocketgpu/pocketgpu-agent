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

#[test]
fn local_requests_keep_their_numbers() {
    use pocketgpu_proto::local::Request as L;
    let cases = [
        (L::Status, 0),
        (L::GetDocker, 29),
        (L::CancelPower, 40),
        (L::GetFiles, 41),
        (L::SetFiles { on: true }, 42),
        (L::ShareFolder { path: "/a".into() }, 43),
        (L::UnshareFolder { path: "/a".into() }, 44),
        (L::SetInbox { path: "/a".into() }, 45),
        (L::Inbox, 46),
        (
            L::Offer {
                path: "/a".into(),
                to: None,
            },
            47,
        ),
        (L::GetClip, 48),
        (L::SetClip { on: true }, 49),
        (L::ClipGet, 50),
        (
            L::ClipPut {
                text: "t".into(),
                once: false,
            },
            51,
        ),
        (L::ClipClear, 52),
        (L::GetApps, 53),
        (L::SetApps { on: true }, 54),
        (
            L::AddApp {
                port: 7000,
                name: None,
            },
            55,
        ),
        (L::RemoveApp { port: 7000 }, 56),
        (
            L::SetAppOff {
                port: 7000,
                off: true,
            },
            57,
        ),
    ];
    for (req, n) in cases {
        assert_eq!(tag(&req), n, "{req:?} moved");
    }
}

#[test]
fn local_responses_keep_their_numbers() {
    use pocketgpu_proto::clip::Place;
    use pocketgpu_proto::local::{ClipStatus, FilesStatus, NoticeLevel, Response as L};
    let cases = [
        (L::Ok, 19),
        (L::Notices(NoticeLevel::All), 26),
        (
            L::Files(FilesStatus {
                enabled: true,
                shared: vec![],
                inbox: String::new(),
                inbox_files: 0,
                offers: 0,
            }),
            28,
        ),
        (L::InboxList(vec![]), 29),
        (
            L::Offered {
                name: "a".into(),
                size: 1,
                to: vec![],
                expires_ms: 0,
                pushed: vec![],
            },
            30,
        ),
        (
            L::Clip(ClipStatus {
                enabled: true,
                place: Place::Memory,
                has_text: false,
                clears_in_s: None,
            }),
            31,
        ),
        (L::ClipText(None), 32),
        (
            L::Apps(pocketgpu_proto::apps::Apps {
                enabled: false,
                apps: vec![],
            }),
            33,
        ),
    ];
    for (res, n) in cases {
        assert_eq!(tag(&res), n, "{res:?} moved");
    }
}

#[test]
fn files_messages_keep_their_numbers() {
    use pocketgpu_proto::files::{Kind, Request, Response, Root, Upload};
    let roots = [
        Root::Shared(0),
        Root::Inbox,
        Root::JobLogs,
        Root::Offers,
        Root::Home,
    ];
    for (n, root) in roots.into_iter().enumerate() {
        assert_eq!(tag(&root) as usize, n, "{root:?} moved");
    }
    let requests = [
        (Request::Overview, 0),
        (
            Request::List {
                root: Root::Inbox,
                path: String::new(),
                offset: 0,
            },
            1,
        ),
        (
            Request::Get {
                root: Root::Inbox,
                path: "a".into(),
                from: 0,
            },
            2,
        ),
        (
            Request::Put {
                name: "a".into(),
                size: 1,
                transfer: [0; 16],
            },
            3,
        ),
    ];
    for (req, n) in requests {
        assert_eq!(tag(&req), n, "{req:?} moved");
    }
    assert_eq!(tag(&Upload::Chunk(vec![1])), 0);
    assert_eq!(tag(&Upload::Done { blake3: [0; 32] }), 1);
    let responses = [
        (
            Response::Entries {
                entries: vec![],
                next: None,
            },
            1,
        ),
        (
            Response::Head {
                size: 0,
                modified_ms: 0,
            },
            2,
        ),
        (Response::Chunk(vec![]), 3),
        (Response::Done { blake3: [0; 32] }, 4),
        (Response::Ready { have: 0 }, 5),
        (
            Response::Saved {
                name: "a".into(),
                blake3: [0; 32],
            },
            6,
        ),
        (Response::Error(Error::new(ErrorCode::Invalid, "x")), 7),
    ];
    for (res, n) in responses {
        assert_eq!(tag(&res), n, "{res:?} moved");
    }
    assert_eq!(tag(&Kind::File), 0);
    assert_eq!(tag(&Kind::Folder), 1);
}

#[test]
fn clip_messages_keep_their_numbers() {
    use pocketgpu_proto::clip::{Info, Place, Request, Response};
    assert_eq!(tag(&Request::Info), 0);
    let put = Request::Put {
        text: "t".into(),
        secret: false,
        once: false,
    };
    assert_eq!(tag(&put), 1);
    assert_eq!(tag(&Request::Get), 2);
    assert_eq!(tag(&Place::System), 0);
    assert_eq!(tag(&Place::Memory), 1);
    let responses = [
        (
            Response::Info(Info {
                enabled: true,
                place: Place::System,
                clears_after_s: None,
            }),
            0,
        ),
        (Response::Ok, 1),
        (
            Response::Text {
                text: "t".into(),
                age_ms: None,
            },
            2,
        ),
        (Response::Empty, 3),
        (Response::Secret, 4),
        (Response::Error(Error::new(ErrorCode::Invalid, "x")), 5),
    ];
    for (res, n) in responses {
        assert_eq!(tag(&res), n, "{res:?} moved");
    }
}

#[test]
fn new_protocols_come_last() {
    use pocketgpu_proto::alpn;
    assert_eq!(alpn::ALL.len(), 11);
    assert_eq!(alpn::ALL[8], alpn::FILES);
    assert_eq!(alpn::ALL[9], alpn::CLIP);
    assert_eq!(alpn::ALL[10], alpn::APPS);
    assert_eq!(alpn::FILES, b"pgpu/files/1");
    assert_eq!(alpn::CLIP, b"pgpu/clip/1");
    assert_eq!(alpn::APPS, b"pgpu/apps/1");
}

#[test]
fn apps_messages_keep_their_numbers() {
    use pocketgpu_proto::apps::{App, Apps, Kind, Request, Response};
    assert_eq!(tag(&Request::List), 0);
    assert_eq!(tag(&Request::Open { port: 1 }), 1);
    assert_eq!(tag(&Request::Start { port: 1 }), 2);
    let kinds = [
        Kind::Jupyter,
        Kind::TensorBoard,
        Kind::ComfyUI,
        Kind::Gradio,
        Kind::OpenWebUI,
        Kind::Streamlit,
        Kind::Ollama,
        Kind::LmStudio,
        Kind::Other,
    ];
    for (n, k) in kinds.iter().enumerate() {
        assert_eq!(tag(k), n as u8, "{k:?} moved");
    }
    let app = App {
        port: 1,
        name: "a".into(),
        kind: Kind::Other,
        added: false,
        off: false,
    };
    let list = Response::Apps(Apps {
        enabled: true,
        apps: vec![app],
    });
    assert_eq!(tag(&list), 0);
    assert_eq!(tag(&Response::Opened), 1);
    assert_eq!(
        tag(&Response::Error(Error::new(ErrorCode::Invalid, "x"))),
        2
    );
    assert_eq!(tag(&Response::Start { path: "/".into() }), 3);
}

#[test]
fn guest_messages_keep_their_numbers() {
    use pocketgpu_proto::msg::control::{Request, Response};
    let invite = Request::CreateInvite {
        permission: pocketgpu_proto::Permission::View,
        minutes: 60,
    };
    assert_eq!(tag(&invite), 26);
    assert_eq!(tag(&Request::ListGuests), 27);
    let extend = Request::ExtendGuest {
        device: "d".into(),
        minutes: 60,
    };
    assert_eq!(tag(&extend), 28);
    let r = Response::Invite {
        uri: "u".into(),
        ends_ms: 1,
    };
    assert_eq!(tag(&r), 21);
    assert_eq!(tag(&Response::Guests(vec![])), 22);
}
