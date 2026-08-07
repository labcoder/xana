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
}
