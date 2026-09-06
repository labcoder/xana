//! Revocation/inspection bypasses the model loop but not the client controller.
//! The weak event sender cannot keep a stopped runtime's stream alive.
use super::{AgentEvent, RuntimeHandle, RuntimeUnavailable};
use crate::{
    browser::{BrowserControl, BrowserError, BrowserOwner, BrowserRequest},
    identity::OperationId,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(super) struct ControlState {
    slot: Arc<tokio::sync::Semaphore>,
    shutdown_ok: Arc<AtomicBool>,
}
impl Default for ControlState {
    fn default() -> Self {
        Self {
            slot: Arc::new(tokio::sync::Semaphore::new(1)),
            shutdown_ok: Arc::new(AtomicBool::new(true)),
        }
    }
}
impl ControlState {
    pub(super) fn shutdown_succeeded(&self) -> bool {
        self.shutdown_ok.load(Ordering::Acquire)
    }
}

impl RuntimeHandle {
    pub(crate) fn with_browser(mut self, browser: Option<BrowserOwner>) -> Self {
        if let Some(owner) = browser.clone() {
            let mut exit = self.exit.clone();
            let shutdown_ok = self.browser_controls.shutdown_ok.clone();
            shutdown_ok.store(false, Ordering::Release);
            // No strong event sender: browser cleanup cannot keep the dead
            // runtime stream alive. The owning shutdown joins this tracked task.
            self.owned_tasks.spawn(async move {
                while exit.borrow().is_none() {
                    if exit.changed().await.is_err() {
                        break;
                    }
                }
                shutdown_ok.store(owner.shutdown().await.is_ok(), Ordering::Release);
            });
        }
        self.browser = browser;
        self
    }

    pub(super) async fn browser_control(
        &self,
        action: BrowserControl,
    ) -> Result<(), RuntimeUnavailable> {
        if self.exit.borrow().is_some() {
            return Err(RuntimeUnavailable);
        }
        let events = self.control_events.upgrade().ok_or(RuntimeUnavailable)?;
        let Some(browser) = &self.browser else {
            let _ = events.send(AgentEvent::CommandRejected { reason: "This runtime has no qualified protected local browser; managed runtimes own their own tools.".into() });
            return Ok(());
        };
        let Ok(permit) = self.browser_controls.slot.clone().try_acquire_owned() else {
            let _ = events.send(AgentEvent::CommandRejected {
                reason: "A browser control is already pending; wait for its result.".into(),
            });
            return Ok(());
        };
        let browser = browser.clone();
        let events = self.control_events.clone();
        self.owned_tasks.spawn(async move {
            let _permit = permit;
            let event = control_result(&browser, action).await;
            if let Some(events) = events.upgrade() {
                let _ = events.send(event);
            }
        });
        Ok(())
    }
}

async fn control_result(browser: &BrowserOwner, action: BrowserControl) -> AgentEvent {
    let result = match action {
        BrowserControl::Status => Ok(()),
        BrowserControl::Close => browser.shutdown().await,
        BrowserControl::Resolve {
            receipt,
            revision,
            outcome,
        } => browser
            .resolve(receipt, revision, outcome)
            .await
            .map(|_| ()),
        BrowserControl::Takeover => match browser.plan(BrowserRequest::Takeover {}) {
            Ok(plan) => browser
                .execute(plan, OperationId::new())
                .await
                .and_then(|receipt| {
                    if receipt.acknowledged {
                        Ok(())
                    } else {
                        Err(BrowserError::Process)
                    }
                }),
            Err(error) => Err(error),
        },
    };
    match result {
        Ok(()) => match browser_status(browser).await {
            Ok(detail) if detail.len() <= 16 * 1024 => AgentEvent::BrowserStatus { detail },
            Ok(_) => AgentEvent::CommandRejected {
                reason: "Browser status exceeds its display bound".into(),
            },
            Err(error) => AgentEvent::CommandRejected {
                reason: format!("Could not read protected browser receipts: {error}"),
            },
        },
        Err(error) => AgentEvent::CommandRejected {
            reason: error.to_string(),
        },
    }
}

async fn browser_status(browser: &BrowserOwner) -> Result<String, BrowserError> {
    let pending_review = browser.pending_review().await?;
    let receipts = browser.receipts(8).await?;
    let recent: Vec<_> = receipts.into_iter().take(8).map(|receipt| serde_json::json!({
        "id":receipt.id,"task":receipt.task,"operation":receipt.operation,"outcome":receipt.outcome,"acknowledged":receipt.acknowledged
    })).collect();
    serde_json::to_string_pretty(
        &serde_json::json!({"browser":browser.snapshot(),"recent_receipts":recent,"pending_review":pending_review}),
    )
    .map_err(|_| BrowserError::Protocol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        identity::PrincipalId,
        paths::XanaPaths,
        session::ConversationPage,
        storage::{ProtectedStore, RecoveryIdentity, TestCustody},
    };
    use tokio::sync::{mpsc, watch};

    #[tokio::test]
    async fn browser_controls_share_owner_and_bypass_a_full_model_command_queue() {
        let root = tempfile::tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(root.path().as_os_str().to_owned())).unwrap();
        let store = ProtectedStore::initialize(
            paths.data_dir(),
            &RecoveryIdentity::generate(),
            &TestCustody::default(),
        )
        .unwrap();
        let browser = BrowserOwner::new(
            paths,
            store,
            PrincipalId::new(),
            crate::identity::SessionId::new(),
        );
        let (commands, mut blocked_model) = mpsc::channel(1);
        commands
            .try_send(super::super::RuntimeCommand::ClearConversation)
            .unwrap();
        let (events, receiver) = mpsc::unbounded_channel();
        let (_exit_sender, exit) = watch::channel(None);
        let mut runtime = RuntimeHandle {
            runtime_task: None,
            browser: Some(browser.clone()),
            browser_controls: ControlState::default(),
            control_events: events.downgrade(),
            owned_tasks: tokio_util::task::TaskTracker::new(),
            commands,
            events: receiver,
            initial_history: ConversationPage {
                messages: Vec::new(),
                start: 0,
                total: 0,
                has_older: false,
            },
            exit,
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            runtime.send(super::super::RuntimeCommand::BrowserControl {
                action: BrowserControl::Status,
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            matches!(runtime.next_event().await, Some(AgentEvent::BrowserStatus { detail }) if detail.len() <= 16*1024 && detail.contains("recent_receipts"))
        );
        let task = uuid::Uuid::new_v4();
        browser.simulate_cleanup_failure(task);
        let (ready, held) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let delayed = browser.clone();
        let delayed = tokio::spawn(async move {
            delayed.hold_control_fixture(ready, released).await;
        });
        held.await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            runtime.send(super::super::RuntimeCommand::BrowserControl {
                action: BrowserControl::Close,
            }),
        )
        .await
        .expect("browser cleanup cannot block input/render processing")
        .unwrap();
        runtime
            .send(super::super::RuntimeCommand::BrowserControl {
                action: BrowserControl::Status,
            })
            .await
            .unwrap();
        assert!(
            matches!(runtime.next_event().await, Some(AgentEvent::CommandRejected { reason }) if reason.contains("already pending"))
        );
        blocked_model.recv().await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            runtime.send(super::super::RuntimeCommand::ClearConversation),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(
            blocked_model.recv().await,
            Some(super::super::RuntimeCommand::ClearConversation)
        ));
        release.send(()).unwrap();
        delayed.await.unwrap();
        assert!(matches!(
            runtime.next_event().await,
            Some(AgentEvent::CommandRejected { .. })
        ));
        assert_eq!(browser.snapshot().task, Some(task));
        assert_eq!(browser.snapshot().state, "cleanup_failed");
        runtime.owned_tasks.close();
        runtime.owned_tasks.wait().await;
        drop(events);
        assert!(
            runtime.next_event().await.is_none(),
            "control tasks must not retain a dead runtime event stream"
        );
    }
}
