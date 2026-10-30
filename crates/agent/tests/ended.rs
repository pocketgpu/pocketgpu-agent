//! A clean stop tells connected phones the machine ended, rather than leaving them to guess.

mod harness;

use std::time::Duration;

use harness::Harness;
use pocketgpu_proto::Permission;

#[tokio::test]
async fn phones_hear_that_the_machine_ended() {
    let h = Harness::start().await;
    let phone = h.phone(Some(Permission::View)).await;
    let machine = h.connect(&phone).await.unwrap();
    machine.hello("test").await.unwrap();
    assert!(!machine.ended());

    h.agent.close_all(pocketgpu_proto::ENDED_REASON);
    for _ in 0..50 {
        if machine.is_closed() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(machine.is_closed());
    assert!(machine.ended());
}
