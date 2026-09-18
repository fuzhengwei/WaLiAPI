//! Directed codec strategy registry.

use super::direction::CodecDirection;
use super::error::{CodecError, DecodeError, FeatureKind, PrepareError, UnsupportedFeatures};
use super::identity::{CHAT_IDENTITY, MESSAGES_IDENTITY, RESPONSES_IDENTITY};
use super::ports::{
    DecodedResponse, NonStreamDecoder as FactoryNonStreamDecoder,
    StreamDecoder as FactoryStreamDecoder,
};
use super::report::{ConversionContext, ConversionReport};
use super::types::{CodecId, PreparedCodec, PreparedConversion, Protocol};
use super::{chat, directions, gemini, messages, responses_codec};
use serde_json::Value;

type EncodeFn = fn(&Value, &str) -> Result<(Value, ConversionContext), PrepareError>;
type NonStreamDecoderFn = fn(&ConversionContext) -> Box<dyn FactoryNonStreamDecoder + Send + Sync>;
type StreamDecoderFn = fn(&ConversionContext) -> Box<dyn FactoryStreamDecoder + Send + Sync>;

struct FnDirection {
    id: CodecId,
    downstream: Protocol,
    upstream: Protocol,
    encode: EncodeFn,
    non_stream: NonStreamDecoderFn,
    streaming: StreamDecoderFn,
}

impl CodecDirection for FnDirection {
    fn id(&self) -> CodecId {
        self.id
    }
    fn downstream(&self) -> Protocol {
        self.downstream
    }
    fn upstream(&self) -> Protocol {
        self.upstream
    }

    fn encode_request(
        &self,
        request: &Value,
        mapped_model: &str,
    ) -> Result<(Value, super::report::ConversionContext), PrepareError> {
        (self.encode)(request, mapped_model)
    }

    fn new_response_decoder(
        &self,
        context: &super::report::ConversionContext,
    ) -> Box<dyn FactoryNonStreamDecoder + Send + Sync> {
        (self.non_stream)(context)
    }

    fn new_stream_response_decoder(
        &self,
        context: &super::report::ConversionContext,
    ) -> Box<dyn FactoryStreamDecoder + Send + Sync> {
        (self.streaming)(context)
    }
}

static CHAT_TO_MESSAGES: FnDirection = FnDirection {
    id: CodecId::ChatToMessagesV1,
    downstream: Protocol::Chat,
    upstream: Protocol::Messages,
    encode: chat::encode_chat_to_messages,
    // Upstream is Messages, so responses travel Messages -> Chat.
    non_stream: messages::NonStreamResponseDecoder::boxed,
    streaming: messages::MessagesStreamDecoder::boxed,
};
static MESSAGES_TO_CHAT: FnDirection = FnDirection {
    id: CodecId::MessagesToChatV1,
    downstream: Protocol::Messages,
    upstream: Protocol::Chat,
    encode: messages::encode_messages_to_chat,
    // Upstream is Chat, so responses travel Chat -> Messages.
    non_stream: chat::NonStreamResponseDecoder::boxed,
    streaming: chat::ChatStreamDecoder::boxed,
};
static CHAT_TO_RESPONSES: FnDirection = FnDirection {
    id: CodecId::ChatToResponsesV1,
    downstream: Protocol::Chat,
    upstream: Protocol::Responses,
    encode: responses_codec::encode_chat_to_responses,
    non_stream: responses_codec::ResponsesNonStreamDecoder::boxed,
    streaming: responses_codec::ResponsesStreamDecoder::boxed,
};
static RESPONSES_TO_CHAT: FnDirection = FnDirection {
    id: CodecId::ResponsesToChatV1,
    downstream: Protocol::Responses,
    upstream: Protocol::Chat,
    encode: encode_responses_to_chat,
    non_stream: ResponsesToChatNonStreamDecoder::boxed,
    streaming: responses_codec::ChatToResponsesStreamDecoder::boxed,
};

/// Transitional single-hop Responses -> Chat request wrapper. The upstream
/// response returns through the complementary Chat -> Responses decoders.
fn encode_responses_to_chat(
    request: &Value,
    model: &str,
) -> Result<(Value, super::report::ConversionContext), PrepareError> {
    let mut encoded = crate::protocol::responses_to_openai(request)?;
    encoded
        .as_object_mut()
        .ok_or_else(|| {
            UnsupportedFeatures::single(
                FeatureKind::UnsupportedField,
                "/",
                "Responses to Chat encoder produced a non-object request",
            )
        })?
        .insert("model".to_owned(), Value::String(model.to_owned()));
    let request_id = request
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("resp_{}", uuid::Uuid::new_v4().simple()));
    let stream = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut context = super::report::ConversionContext::new(request_id, model, stream);
    for field in [
        "parallel_tool_calls",
        "store",
        "include",
        "prompt_cache_key",
        "prompt_cache_options",
        "client_metadata",
    ] {
        if request.get(field).is_some() {
            context.normalized.push(format!("/{field}"));
        }
    }
    Ok((encoded, context))
}

struct ResponsesToChatNonStreamDecoder {
    context: super::report::ConversionContext,
}

impl ResponsesToChatNonStreamDecoder {
    fn boxed(
        context: &super::report::ConversionContext,
    ) -> Box<dyn FactoryNonStreamDecoder + Send + Sync> {
        Box::new(Self {
            context: context.clone(),
        })
    }
}

impl FactoryNonStreamDecoder for ResponsesToChatNonStreamDecoder {
    fn decode(&self, body: &Value) -> Result<DecodedResponse, DecodeError> {
        // `openai_to_responses` is the established response representation
        // helper. Validate the provider's Chat completion envelope first so a
        // malformed upstream body cannot become a synthetic Responses success.
        if body.pointer("/choices/0/message").is_none() {
            return Err(DecodeError::new(
                "/choices/0/message",
                "Chat response missing choices[0].message",
            ));
        }
        Ok(DecodedResponse {
            body: crate::protocol::openai_to_responses(body, &self.context.upstream_model),
            usage: super::identity::parse_usage(Protocol::Chat, body),
        })
    }
}

/// Lookup and prepare codec strategies. Registered strategies are stateless;
/// the returned plan owns only request context and creates fresh decoder state.
pub struct CodecRegistry;

impl CodecRegistry {
    fn direction(
        downstream: Protocol,
        upstream: Protocol,
    ) -> Result<&'static dyn CodecDirection, CodecError> {
        let strategy: &'static dyn CodecDirection = match (downstream, upstream) {
            (Protocol::Chat, Protocol::Chat) => &CHAT_IDENTITY,
            (Protocol::Messages, Protocol::Messages) => &MESSAGES_IDENTITY,
            (Protocol::Responses, Protocol::Responses) => &RESPONSES_IDENTITY,
            (Protocol::Chat, Protocol::Messages) => &CHAT_TO_MESSAGES,
            (Protocol::Messages, Protocol::Chat) => &MESSAGES_TO_CHAT,
            (Protocol::Chat, Protocol::Responses) => &CHAT_TO_RESPONSES,
            (Protocol::Responses, Protocol::Chat) => &RESPONSES_TO_CHAT,
            (Protocol::Messages, Protocol::Responses) => &directions::MESSAGES_TO_RESPONSES_V2,
            (Protocol::Responses, Protocol::Messages) => &directions::RESPONSES_TO_MESSAGES_V2,
            (Protocol::Chat, Protocol::Gemini) => &gemini::CHAT_TO_GEMINI,
            (Protocol::Messages, Protocol::Gemini) => &gemini::MESSAGES_TO_GEMINI,
            (Protocol::Responses, Protocol::Gemini) => &gemini::RESPONSES_TO_GEMINI,
            (Protocol::Gemini, _) => {
                return Err(CodecError::new(
                    "Gemini is an upstream-only protocol and cannot be used downstream",
                ))
            }
        };
        debug_assert_eq!(strategy.downstream(), downstream);
        debug_assert_eq!(strategy.upstream(), upstream);
        Ok(strategy)
    }

    /// Prepare a protocol-pair conversion using the current matrix strategy.
    ///
    /// This factory-native API makes both protocol sides explicit. The
    /// five-argument [`Self::prepare`] remains available for legacy callers.
    pub fn prepare_pair(
        downstream: Protocol,
        upstream: Protocol,
        model: &str,
        request: &Value,
    ) -> Result<PreparedConversion, PrepareError> {
        let strategy = Self::direction(downstream, upstream).map_err(|error| {
            UnsupportedFeatures::single(FeatureKind::UnsupportedField, "/", error.to_string())
        })?;
        let (encoded_request, context) = strategy.encode_request(request, model)?;
        let codec = PreparedCodec::new(strategy, context.clone());
        let report = ConversionReport::for_codec(codec.id(), vec![], context.normalized.clone());
        Ok(PreparedConversion {
            encoded_request,
            report,
            codec,
        })
    }

    pub fn chat_to_messages(
        model: &str,
        request: &Value,
    ) -> Result<PreparedConversion, PrepareError> {
        Self::prepare_pair(Protocol::Chat, Protocol::Messages, model, request)
    }

    pub fn messages_to_chat(
        model: &str,
        request: &Value,
    ) -> Result<PreparedConversion, PrepareError> {
        Self::prepare_pair(Protocol::Messages, Protocol::Chat, model, request)
    }

    pub fn chat_to_responses(
        model: &str,
        request: &Value,
    ) -> Result<PreparedConversion, PrepareError> {
        Self::prepare_pair(Protocol::Chat, Protocol::Responses, model, request)
    }

    pub fn messages_to_responses(
        model: &str,
        request: &Value,
    ) -> Result<PreparedConversion, PrepareError> {
        Self::prepare_pair(Protocol::Messages, Protocol::Responses, model, request)
    }

    pub fn responses_to_messages(
        model: &str,
        request: &Value,
    ) -> Result<PreparedConversion, PrepareError> {
        Self::prepare_pair(Protocol::Responses, Protocol::Messages, model, request)
    }
}
