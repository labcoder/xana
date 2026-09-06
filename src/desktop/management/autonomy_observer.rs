//! Passive protected-home polling; no controller lease, provider, or mutation.
use super::{DesktopControlPlane, DesktopError, control_error};
use crate::{
    app::autonomy_commands,
    autonomy::supervision::attention::AttentionObserver,
    host_lifecycle::{ClientFocus, NotificationCandidate, NotificationPlanner, NotificationPolicy},
};

pub use crate::autonomy::supervision::attention::{
    BackgroundAttention as DesktopBackgroundAttention,
    BackgroundAttentionKind as DesktopBackgroundAttentionKind,
};

#[derive(Debug)]
pub struct DesktopBackgroundUpdate {
    pub attention: Vec<DesktopBackgroundAttention>,
    pub notifications: Vec<NotificationCandidate>,
}
#[derive(Default)]
pub struct DesktopAutonomyObserver {
    observer: AttentionObserver,
    notifications: NotificationPlanner,
}
impl DesktopAutonomyObserver {
    /// Poll at a modest cadence (for example 2s) on the background executor.
    /// No decrypted store handle is retained between polls or after locking.
    pub fn poll(
        &mut self,
        control: &DesktopControlPlane,
        settings: &NotificationPolicy,
        focus: ClientFocus,
    ) -> Result<DesktopBackgroundUpdate, DesktopError> {
        let store = autonomy_commands::store(&control.paths).map_err(control_error)?;
        let attention = self.observer.poll(&store).map_err(control_error)?;
        let notifications = attention
            .iter()
            .filter_map(|note| self.notifications.plan(settings, focus, &note.signal()))
            .collect();
        Ok(DesktopBackgroundUpdate {
            attention,
            notifications,
        })
    }
}
