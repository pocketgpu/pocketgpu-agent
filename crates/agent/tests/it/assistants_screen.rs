//! F32 AI assistants screen (US3): every paired phone sees who is connected, only Full
//! control reads the settings and the activity, and hooks heard from show when they were
//! last active.

use pocketgpu_proto::Permission;
use pocketgpu_proto::assistants::{AssistantSettings, Kind};
use pocketgpu_proto::local::AssistantEventKind;

use crate::harness::Harness;

#[tokio::test(flavor = "multi_thread")]
async fn full_control_reads_settings_and_activity_others_only_the_list() {
    let h = Harness::start().await;
    let owner = h.phone(Some(Permission::Shell)).await;
    let m = h.connect(&owner).await.unwrap();
    let want = AssistantSettings {
        ask_first: vec![Kind::Delete],
        ..Default::default()
    };
    m.set_assistant_settings(want.clone()).await.unwrap();
    // Claude Code's plugin wrote nothing connect records; its hooks still count.
    pocketgpu::assistants::event(&h.agent, "claude", "s1", AssistantEventKind::Working).unwrap();

    let got = m.assistants().await.unwrap();
    assert_eq!(got.settings, Some(want));
    let claude = got.connected.iter().find(|a| a.id == "claude").unwrap();
    assert_eq!(claude.name, "Claude Code");
    assert!(claude.alerts && claude.last_seen_ms.is_some());

    let activity = m.assistant_activity(0).await.unwrap();
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0].event, "assistant_settings");
    assert!(activity.iter().all(|e| e.event.starts_with("assistant_")));

    for level in [Permission::View, Permission::Control] {
        let phone = h.phone(Some(level)).await;
        let other = h.connect(&phone).await.unwrap();
        let got = other.assistants().await.unwrap();
        assert_eq!(got.settings, None, "{level:?}");
        assert!(got.connected.iter().any(|a| a.id == "claude"));
        assert!(other.assistant_activity(0).await.is_err(), "{level:?}");
    }
}
