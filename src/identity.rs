//! Transport-safe semantic identities for runtime work.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub(crate) struct $name(Uuid);

        impl $name {
            pub(crate) fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

uuid_id!(OperationId);
uuid_id!(StepId);
uuid_id!(ToolInvocationId);
uuid_id!(SessionId);
uuid_id!(ThreadId);
uuid_id!(AgentId);
uuid_id!(ConversationEntryId);
uuid_id!(RecordId);
uuid_id!(ArtifactId);
uuid_id!(ContextId);
uuid_id!(ContextViewId);
uuid_id!(PrincipalId);
uuid_id!(ToolResultId);
uuid_id!(NamedValueId);
uuid_id!(OrchestrationPlanId);
uuid_id!(ProjectId);

impl Default for OrchestrationPlanId {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentId {
    /// Returns the stable root-agent identity owned by one durable session.
    ///
    /// Session ids are public semantic identifiers, not secrets. Deriving the
    /// root id keeps legacy sessions stable without mutating them during a
    /// read-only resume or inspection.
    pub(crate) fn for_session(session_id: SessionId) -> Self {
        Self(Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!("io.github.labcoder.xana/session/{session_id}/root-agent").as_bytes(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_as_json_strings() {
        let operation = OperationId::new();
        let json = serde_json::to_string(&operation).expect("serialize operation id");
        let decoded: OperationId = serde_json::from_str(&json).expect("deserialize operation id");

        assert_eq!(decoded, operation);
        assert!(json.starts_with('"'));
    }

    #[test]
    fn semantic_id_types_remain_distinct() {
        fn takes_operation(_id: OperationId) {}

        let operation = OperationId::new();
        let step = StepId::new();
        let invocation = ToolInvocationId::new();

        takes_operation(operation);
        assert_ne!(step.to_string(), invocation.to_string());
    }

    #[test]
    fn durable_session_has_one_stable_root_agent_identity() {
        let session = SessionId::new();

        assert_eq!(AgentId::for_session(session), AgentId::for_session(session));
        assert_ne!(
            AgentId::for_session(session),
            AgentId::for_session(SessionId::new())
        );
    }
}
