//! Crash reports at the machine (board 25): off until turned on, and kept in the settings.

mod harness;

use harness::Harness;
use pocketgpu::store::config::Config;
use pocketgpu_proto::local::{Request, Response};

#[tokio::test]
async fn crash_reports_are_off_until_turned_on() {
    let h = Harness::start().await;
    let ask = |on| pocketgpu::ipc::handle(&h.agent, Request::CrashReports { on });
    assert_eq!(ask(None), Response::CrashReports { on: false });
    assert_eq!(ask(Some(true)), Response::CrashReports { on: true });
    assert!(
        Config::load(&h.agent.paths.config_file())
            .unwrap()
            .crash_reports
    );
    assert_eq!(ask(Some(false)), Response::CrashReports { on: false });
    assert_eq!(ask(None), Response::CrashReports { on: false });
}
