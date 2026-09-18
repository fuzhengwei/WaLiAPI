//! Public value objects for selecting and preparing protocol codecs.

use super::direction::CodecDirection;
use super::ports::{NonStreamDecoder, StreamDecoder};
use super::report::{ConversionContext, ConversionReport};
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use serde_json::Value;
use std::fmt;

/// Application protocol families in the codec matrix.
///
/// `Gemini` is an **upstream-only** wire format (Code Assist generateContent).
/// Downstream HTTP still exposes only Chat / Messages / Responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Chat,
    Messages,
    Responses,
    Gemini,
}

/// Stable identifier recorded in logs and conversion reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodecId {
    Native,
    ChatToMessagesV1,
    MessagesToChatV1,
    ChatToResponsesV1,
    ResponsesToChatV1,
    /// Legacy composition, retained only for log/fixture compatibility.
    MessagesToResponsesV1,
    /// Legacy composition, retained only for log/fixture compatibility.
    ResponsesToMessagesV1,
    MessagesToResponsesV2,
    ResponsesToMessagesV2,
    ChatToGeminiV1,
    MessagesToGeminiV1,
    ResponsesToGeminiV1,
}

impl CodecId {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::ChatToMessagesV1 => "chat_to_messages_v1",
            Self::MessagesToChatV1 => "messages_to_chat_v1",
            Self::ChatToResponsesV1 => "chat_to_responses_v1",
            Self::ResponsesToChatV1 => "responses_to_chat_v1",
            Self::MessagesToResponsesV1 => "messages_to_responses_v1",
            Self::ResponsesToMessagesV1 => "responses_to_messages_v1",
            Self::MessagesToResponsesV2 => "messages_to_responses_v2",
            Self::ResponsesToMessagesV2 => "responses_to_messages_v2",
            Self::ChatToGeminiV1 => "chat_to_gemini_v1",
            Self::MessagesToGeminiV1 => "messages_to_gemini_v1",
            Self::ResponsesToGeminiV1 => "responses_to_gemini_v1",
        }
    }
}

/// Immutable plan for a prepared codec request. It can be cloned safely; each
/// decoder factory invocation returns a fresh stateful decoder.
#[derive(Clone)]
pub struct PreparedCodec {
    id: CodecId,
    downstream: Protocol,
    upstream: Protocol,
    context: ConversionContext,
    strategy: &'static dyn CodecDirection,
}

impl PreparedCodec {
    pub(crate) fn new(strategy: &'static dyn CodecDirection, context: ConversionContext) -> Self {
        Self {
            id: strategy.id(),
            downstream: strategy.downstream(),
            upstream: strategy.upstream(),
            context,
            strategy,
        }
    }

    pub fn id(&self) -> CodecId {
        self.id
    }

    pub fn label(&self) -> &'static str {
        self.id.label()
    }

    pub fn downstream(&self) -> Protocol {
        self.downstream
    }

    pub fn upstream(&self) -> Protocol {
        self.upstream
    }

    pub fn context(&self) -> &ConversionContext {
        &self.context
    }

    pub fn is_identity(&self) -> bool {
        self.id == CodecId::Native
    }

    pub fn new_non_stream_decoder(&self) -> Box<dyn NonStreamDecoder + Send + Sync> {
        self.strategy.new_response_decoder(&self.context)
    }

    pub fn new_stream_decoder(&self) -> Box<dyn StreamDecoder + Send + Sync> {
        self.strategy.new_stream_response_decoder(&self.context)
    }
}

impl fmt::Debug for PreparedCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedCodec")
            .field("id", &self.id)
            .field("downstream", &self.downstream)
            .field("upstream", &self.upstream)
            .field("context", &self.context)
            .finish()
    }
}

impl Serialize for PreparedCodec {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("PreparedCodec", 5)?;
        state.serialize_field("id", &self.id)?;
        state.serialize_field("label", self.label())?;
        state.serialize_field("downstream", &self.downstream)?;
        state.serialize_field("upstream", &self.upstream)?;
        state.serialize_field("context", &self.context)?;
        state.end()
    }
}

/// The result of request preparation.
pub struct PreparedConversion {
    pub encoded_request: Value,
    pub report: ConversionReport,
    pub codec: PreparedCodec,
}

impl fmt::Debug for PreparedConversion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedConversion")
            .field("encoded_request", &self.encoded_request)
            .field("report", &self.report)
            .field("codec", &self.codec)
            .finish()
    }
}
