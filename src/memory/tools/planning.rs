//! Normalize bounded model arguments against a host-owned source before review.

use super::types::*;
use crate::{
    memory::{MemoryOwner, MemoryScope, validate_scope, validate_statement},
    permission::PermissionScope,
    tool::{OwnerTurnInput, PlannedToolInvocation},
};
use anyhow::{Context, Result, ensure};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn bounded_arguments<T: DeserializeOwned>(value: &Value) -> Result<T> {
    let fields = value
        .as_object()
        .context("Memory tool arguments must be an object")?;
    ensure!(
        fields.len() <= 8,
        "Memory tool arguments exceed their field bound"
    );
    let mut bytes = 0usize;
    for (key, value) in fields {
        ensure!(key.len() <= 64, "Memory tool field name exceeds its bound");
        bytes = bytes.saturating_add(key.len());
        match value {
            Value::String(text) => bytes = bytes.saturating_add(text.len()),
            Value::Null | Value::Bool(_) | Value::Number(_) => bytes = bytes.saturating_add(32),
            Value::Array(_) | Value::Object(_) => {
                anyhow::bail!("Memory tool fields must be scalar values")
            }
        }
        ensure!(
            bytes <= 16 * 1024,
            "Memory tool arguments exceed 16384 UTF-8 bytes"
        );
    }
    serde_json::from_value(value.clone()).context("Invalid memory tool arguments")
}

fn bind(owner: &MemoryOwner, turn: Option<&OwnerTurnInput>) -> Result<CommitGuard> {
    let turn = turn.context("Personal memory requires the current foreground owner's input")?;
    ensure!(
        !turn.source_id.is_nil() && !turn.operation_id.as_uuid().is_nil(),
        "Invalid memory owner source identity"
    );
    ensure!(
        !turn.cancellation.is_cancelled(),
        "Memory request was cancelled; nothing was changed"
    );
    ensure!(
        owner.context.conversation.is_some(),
        "Personal memory needs a current Conversation"
    );
    for scope in owner.context.scopes() {
        validate_scope(&scope)?;
    }
    Ok(CommitGuard {
        context: owner.context.clone(),
        operation_id: turn.operation_id,
        source_id: turn.source_id,
        source_digest: blake3::hash(turn.text.as_bytes()).to_hex().to_string(),
        generation: owner.store.privacy_generation()?,
        cancellation: turn.cancellation.clone(),
    })
}

pub(super) fn lookup(
    owner: Option<&MemoryOwner>,
    arguments: &Value,
    turn: Option<&OwnerTurnInput>,
) -> Result<PlannedToolInvocation> {
    let owner = owner.context(crate::memory::UNAVAILABLE_NOTICE)?;
    let args: LookupArgs = bounded_arguments(arguments)?;
    ensure!(
        args.query.len() <= 256 && !args.query.chars().any(char::is_control),
        "Memory query must be at most 256 UTF-8 bytes without controls"
    );
    ensure!(
        (1..=8).contains(&args.limit) && args.after <= i64::MAX as u64,
        "Memory lookup limit or cursor exceeds its bound"
    );
    let guard = bind(owner, turn)?;
    let scopes = match args.scope {
        Some(scope) => vec![scope.resolve(&guard.context)?],
        None => guard.context.scopes(),
    };
    let final_arguments = json!({
        "query":args.query,"scopes":scopes,"after":args.after,"limit":args.limit,
        "source_id":guard.source_id,
    });
    Ok(PlannedToolInvocation::new(
        final_arguments,
        PermissionScope::PersonalMemory {
            scope: format!(
                "conversation:{}",
                guard.context.conversation.expect("bound Conversation")
            ),
            review: false,
        },
        LookupPlan {
            guard,
            args,
            scopes,
        },
    ))
}

pub(super) fn update(
    owner: Option<&MemoryOwner>,
    arguments: &Value,
    turn: Option<&OwnerTurnInput>,
) -> Result<PlannedToolInvocation> {
    let owner = owner.context(crate::memory::UNAVAILABLE_NOTICE)?;
    let args: UpdateArgs = bounded_arguments(arguments)?;
    let guard = bind(owner, turn)?;
    let quote = args.quote.as_deref().unwrap_or("");
    ensure!(
        quote.len() <= 8192,
        "Owner memory quote exceeds 8192 UTF-8 bytes"
    );
    if args.action != UpdateAction::Forget {
        validate_statement(
            args.statement
                .as_deref()
                .context("Remember and correct require a statement")?,
        )?;
        ensure!(
            !quote.trim().is_empty(),
            "Quote the current owner's request and fact; earlier context requires the owner to restate it"
        );
    } else {
        ensure!(
            args.statement.is_none(),
            "Forget accepts an exact memory id and revision, not replacement text"
        );
    }
    if !quote.is_empty() {
        ensure!(
            turn.expect("bound turn").text.contains(quote),
            "Memory quote is not in the current owner message; ask the owner to restate the request and fact"
        );
    }
    let scope = if args.action == UpdateAction::Remember {
        ensure!(
            args.id.is_none() && args.revision.is_none(),
            "Remember creates a fact; use correct with id and revision to replace one"
        );
        args.scope
            .unwrap_or(ScopeSelector::Conversation)
            .resolve(&guard.context)?
    } else {
        let id = args
            .id
            .context("Correct and forget require an exact memory id")?;
        ensure!(
            !id.is_nil()
                && args
                    .revision
                    .is_some_and(|revision| revision > 0 && revision < i64::MAX as u64),
            "Correct and forget require a non-nil id and valid observed revision"
        );
        let record = owner.record(id)?;
        ensure!(
            guard.context.scopes().contains(&record.scope),
            "Memory is outside the current Conversation's visible scopes"
        );
        if let Some(scope) = args.scope {
            ensure!(
                scope.resolve(&guard.context)? == record.scope,
                "Correct and forget cannot move memory scope; use explicit owner memory controls"
            );
        }
        record.scope
    };
    let review = args.action != UpdateAction::Remember
        || args.risk != Risk::Ordinary
        || scope
            != MemoryScope::Conversation(guard.context.conversation.expect("bound Conversation"));
    let intent = UpdateIntent {
        action: args.action,
        scope,
        statement: args.statement,
        id: args.id,
        revision: args.revision,
    };
    let final_arguments = json!({
        "action":intent.action,"scope":intent.scope.to_string(),
        "statement":intent.statement,"id":intent.id,"revision":intent.revision,
        "quote":quote,"risk":args.risk,"source_id":guard.source_id,
    });
    Ok(PlannedToolInvocation::new(
        final_arguments,
        PermissionScope::PersonalMemory {
            scope: intent.scope.to_string(),
            review,
        },
        UpdatePlan { guard, intent },
    ))
}
