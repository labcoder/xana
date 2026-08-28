//! Validate the effective thread policy before returning a usable handle.
//!
//! Vendor configuration and organization requirements remain Codex-owned. A
//! different policy is an incompatibility, not permission to silently widen
//! Xana's requested execution scope or replace its approval controller.

use super::{CodexError, ManagedThreadPolicy};
use serde_json::Value;
use std::path::{Component, Path, Prefix};

pub(super) fn checked_thread_id(
    result: &Value,
    method: &str,
    workspace: &Path,
    policy: ManagedThreadPolicy,
    resumed_id: Option<&str>,
) -> Result<String, CodexError> {
    let incompatible = |field| {
        CodexError::Protocol(format!(
            "{method} returned missing or incompatible {field}; no turn was started; check the Codex CLI and its user/organization configuration"
        ))
    };
    let id = result
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 4096)
        .ok_or_else(|| incompatible("thread id"))?;
    if resumed_id.is_some_and(|expected| expected != id) {
        return Err(incompatible("resumed thread id"));
    }
    if result.get("approvalPolicy").and_then(Value::as_str) != Some(policy.approval.wire()) {
        return Err(incompatible("approvalPolicy"));
    }
    if result.get("approvalsReviewer").and_then(Value::as_str) != Some("user") {
        return Err(incompatible("approvalsReviewer"));
    }
    if result.pointer("/sandbox/type").and_then(Value::as_str) != Some("workspaceWrite") {
        return Err(incompatible("sandbox"));
    }
    // These are the stable workspace-write defaults. A named preset alone
    // does not rule out broader settings inherited from the vendor home.
    if let Some(network) = result.pointer("/sandbox/networkAccess")
        && network.as_bool() != Some(false)
    {
        return Err(incompatible("sandbox.networkAccess"));
    }
    if let Some(roots) = result.pointer("/sandbox/writableRoots") {
        let roots = roots
            .as_array()
            .ok_or_else(|| incompatible("sandbox.writableRoots"))?;
        if !roots.iter().all(|root| {
            root.as_str()
                .is_some_and(|root| same_workspace(workspace, Path::new(root)))
        }) {
            return Err(incompatible("sandbox.writableRoots"));
        }
    }
    let cwd = result
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| cwd.len() <= 4096)
        .map(Path::new)
        .ok_or_else(|| incompatible("cwd"))?;
    if !same_workspace(workspace, cwd) {
        return Err(incompatible("cwd"));
    }
    Ok(id.to_owned())
}

fn same_workspace(expected: &Path, actual: &Path) -> bool {
    if !expected.is_absolute() || !actual.is_absolute() {
        return false;
    }
    // Codex may omit Windows' verbatim prefix from a canonical path. Compare
    // path components without following a vendor-supplied path or probing an
    // unrelated filesystem/network location. Do not assume case insensitivity.
    fn conventional_prefix(prefix: Prefix<'_>) -> Prefix<'_> {
        match prefix {
            Prefix::VerbatimDisk(drive) => Prefix::Disk(drive),
            Prefix::VerbatimUNC(server, share) => Prefix::UNC(server, share),
            prefix => prefix,
        }
    }
    let mut expected = expected.components();
    let mut actual = actual.components();
    let same_root = match (expected.next(), actual.next()) {
        (Some(Component::Prefix(left)), Some(Component::Prefix(right))) => {
            conventional_prefix(left.kind()) == conventional_prefix(right.kind())
        }
        (left, right) => left == right,
    };
    same_root && expected.eq(actual)
}
