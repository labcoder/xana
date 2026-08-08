//! Headless Xana contracts.
//!
//! This crate deliberately has no filesystem, terminal, HTTP, or process
//! dependencies. Runtime adapters compose these contracts into an immutable
//! agent snapshot at the application edge.

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeSet, fmt, sync::Arc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolResultStatus {
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub output: String,
    pub status: ToolResultStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContentBlock {
    Text(String),
    Image(ImageRef),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapabilityId(String);

impl CapabilityId {
    pub fn parse(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.split('.').all(|part| {
                !part.is_empty()
                    && part.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                    })
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(IdError(value))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogicalToolId(String);

impl LogicalToolId {
    pub fn parse(value: impl Into<String>) -> Result<Self, IdError> {
        CapabilityId::parse(value.into()).map(|id| Self(id.0))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LogicalToolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdError(pub String);

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid stable id {:?}", self.0)
    }
}
impl std::error::Error for IdError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectClass {
    Read,
    Write,
    Execute,
    Network,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplaySafety {
    Safe,
    Never,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: LogicalToolId,
    pub contract_version: u32,
    pub description: String,
    pub parameters: Value,
    pub effect_class: EffectClass,
    pub replay_safety: ReplaySafety,
}

pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    fn invoke<'a>(&'a self, arguments: &'a Value) -> BoxFuture<'a, Result<String, String>>;
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new(mut tools: Vec<Arc<dyn Tool>>) -> Result<Self, String> {
        tools.sort_by_key(|tool| tool.definition().name);
        for pair in tools.windows(2) {
            if pair[0].definition().name == pair[1].definition().name {
                return Err(format!("duplicate tool {}", pair[0].definition().name));
            }
        }
        Ok(Self { tools })
    }
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }
    pub fn contains(&self, id: &LogicalToolId) -> bool {
        self.tools.iter().any(|tool| &tool.definition().name == id)
    }
    pub fn tools(&self) -> &[Arc<dyn Tool>] {
        &self.tools
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageRef {
    pub artifact_id: String,
    pub media_type: String,
    pub byte_len: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub input_modalities: BTreeSet<String>,
    pub max_images: usize,
    pub max_image_bytes: usize,
    pub max_context_tokens: usize,
    pub tools: bool,
}

impl ModelCapabilities {
    pub fn text_only() -> Self {
        Self {
            input_modalities: ["text".to_owned()].into_iter().collect(),
            max_images: 0,
            max_image_bytes: 0,
            max_context_tokens: 128_000,
            tools: true,
        }
    }
    pub fn supports_images(&self) -> bool {
        self.input_modalities.contains("image") && self.max_images > 0
    }
    pub fn validate_images(&self, images: &[ImageRef]) -> Result<(), CapabilityError> {
        if images.is_empty() {
            return Ok(());
        }
        if !self.supports_images() {
            return Err(CapabilityError::UnsupportedModality("image".to_owned()));
        }
        if images.len() > self.max_images {
            return Err(CapabilityError::TooManyImages {
                max: self.max_images,
                found: images.len(),
            });
        }
        let bytes = images
            .iter()
            .map(|image| image.byte_len as usize)
            .sum::<usize>();
        if bytes > self.max_image_bytes {
            return Err(CapabilityError::ImageBytesExceeded {
                max: self.max_image_bytes,
                found: bytes,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityError {
    UnsupportedModality(String),
    TooManyImages { max: usize, found: usize },
    ImageBytesExceeded { max: usize, found: usize },
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedModality(m) => write!(f, "model does not support {m} input"),
            Self::TooManyImages { max, found } => {
                write!(f, "model accepts at most {max} images, received {found}")
            }
            Self::ImageBytesExceeded { max, found } => {
                write!(f, "image budget is {max} bytes, received {found}")
            }
        }
    }
}
impl std::error::Error for CapabilityError {}

#[derive(Clone)]
pub struct AgentCapabilitySnapshot {
    capabilities: BTreeSet<CapabilityId>,
    tools: ToolRegistry,
}

impl AgentCapabilitySnapshot {
    pub fn new(capabilities: BTreeSet<CapabilityId>, tools: ToolRegistry) -> Self {
        Self {
            capabilities,
            tools,
        }
    }
    pub fn contains(&self, id: &CapabilityId) -> bool {
        self.capabilities.contains(id)
    }
    pub fn capabilities(&self) -> &BTreeSet<CapabilityId> {
        &self.capabilities
    }
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools.definitions()
    }
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }
}

pub trait ContextService: Send + Sync {
    fn read_range<'a>(
        &'a self,
        request: ReadRange,
    ) -> BoxFuture<'a, Result<BoundedContext, ContextServiceError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextId(pub String);
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextViewRef(pub String);
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRange {
    pub context: ContextId,
    pub start: usize,
    pub max_bytes: usize,
    pub max_estimated_tokens: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedContext {
    pub text: String,
    pub actual_bytes: usize,
    pub estimated_tokens: usize,
    pub truncated: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextServiceError {
    NotFound,
    InvalidRequest(String),
    LimitExceeded,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(bytes: u64) -> ImageRef {
        ImageRef {
            artifact_id: "artifact-1".into(),
            media_type: "image/png".into(),
            byte_len: bytes,
            width: Some(2),
            height: Some(2),
        }
    }

    #[test]
    fn model_capabilities_reject_image_before_transport() {
        let text_only = ModelCapabilities::text_only();
        assert_eq!(
            text_only.validate_images(&[image(4)]),
            Err(CapabilityError::UnsupportedModality("image".into()))
        );
        let vision = ModelCapabilities {
            input_modalities: ["text".into(), "image".into()].into_iter().collect(),
            max_images: 1,
            max_image_bytes: 8,
            max_context_tokens: 128,
            tools: true,
        };
        assert!(vision.validate_images(&[image(4)]).is_ok());
        assert!(matches!(
            vision.validate_images(&[image(4), image(4)]),
            Err(CapabilityError::TooManyImages { .. })
        ));
    }
}
