//! Exact protected receipt lookup; an unfinished dispatch is never replay authority.

use super::*;

pub(super) fn load(
    config: &Configuration,
    operation: OperationId,
) -> Result<VisionReceipt, DesktopVisionError> {
    let store = config
        .artifacts
        .protected_home()
        .ok_or(DesktopVisionError::Unsupported)?;
    let receipt = store
        .history_vision_receipt(config.conversation, operation)
        .map_err(|_| DesktopVisionError::StorageUnavailable)?;
    let mut receipt = receipt.ok_or(DesktopVisionError::Unavailable)?;
    if receipt.status == VisionStatus::Dispatching {
        receipt.status = VisionStatus::Unknown;
    }
    Ok(receipt)
}
