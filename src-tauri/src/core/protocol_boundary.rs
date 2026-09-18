//! Adapter between route planning types and the typed codec protocol matrix.
//!
//! The route planner intentionally models an upstream transport as
//! [`UpstreamProtocol`], while the codec registry models only the three
//! application protocols.  This module is the single anti-corruption boundary
//! between those representations.  In particular, an OpenAI transport is not
//! enough to select a codec protocol: it can carry either Chat Completions or
//! Responses depending on the selected upstream endpoint.
//!
//! `None` has two deliberately distinct, caller-visible meanings:
//! - `CountTokens` and `Embeddings` are explicit native-executor bypasses;
//! - an unsupported, unknown, or inconsistent upstream pair is not part of
//!   the three-protocol codec matrix and must be rejected by the caller for a
//!   Chat/Messages/Responses request.

use crate::core::route_plan::{EndpointKind, UpstreamProtocol};
use crate::protocol::codec::Protocol;

/// Map a downstream route endpoint to its codec protocol.
///
/// Token counting and embeddings are intentionally outside the Chat /
/// Messages / Responses codec matrix.  Returning `None` for them is an
/// explicit native-passthrough signal, not a missing mapping.
pub fn downstream_protocol(endpoint: EndpointKind) -> Option<Protocol> {
    match endpoint {
        EndpointKind::ChatCompletions => Some(Protocol::Chat),
        EndpointKind::Messages => Some(Protocol::Messages),
        EndpointKind::Responses => Some(Protocol::Responses),
        EndpointKind::CountTokens | EndpointKind::Embeddings => None,
    }
}

/// Map an upstream transport protocol and its selected endpoint to a codec
/// protocol.
///
/// This is intentionally a pair match rather than a match on
/// [`UpstreamProtocol`] alone.  `UpstreamProtocol::OpenAI` supports both
/// `chat_completions` and `responses`; treating it as only one of those would
/// silently select the wrong request/response codecs.
///
/// Only exact route-plan endpoint names are accepted.  Ollama and native
/// `count_tokens` / `embeddings` endpoints do not participate in this matrix.
pub fn upstream_protocol(protocol: UpstreamProtocol, endpoint: &str) -> Option<Protocol> {
    match (protocol, endpoint) {
        (UpstreamProtocol::OpenAI, "chat_completions") => Some(Protocol::Chat),
        (UpstreamProtocol::OpenAI, "responses") => Some(Protocol::Responses),
        (UpstreamProtocol::Anthropic, "messages") => Some(Protocol::Messages),
        // Kimi Messages beta reuses the standard Messages codec; the fixed
        // `/v1/messages?beta=true` transport is a Kimi provider concern, not a
        // new codec.
        (UpstreamProtocol::Anthropic, "messages_beta") => Some(Protocol::Messages),
        (UpstreamProtocol::Responses, "responses") => Some(Protocol::Responses),
        (UpstreamProtocol::Gemini, "generate_content") => Some(Protocol::Gemini),
        // Ollama's `api_chat`, CountTokens / Embeddings, unknown endpoint
        // strings, and mismatched transport-endpoint pairs are all outside
        // the typed codec matrix.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downstream_maps_only_the_three_codec_protocols() {
        assert_eq!(
            downstream_protocol(EndpointKind::ChatCompletions),
            Some(Protocol::Chat)
        );
        assert_eq!(
            downstream_protocol(EndpointKind::Messages),
            Some(Protocol::Messages)
        );
        assert_eq!(
            downstream_protocol(EndpointKind::Responses),
            Some(Protocol::Responses)
        );
        assert_eq!(downstream_protocol(EndpointKind::CountTokens), None);
        assert_eq!(downstream_protocol(EndpointKind::Embeddings), None);
    }

    #[test]
    fn openai_upstream_endpoint_selects_chat_or_responses() {
        assert_eq!(
            upstream_protocol(UpstreamProtocol::OpenAI, "chat_completions"),
            Some(Protocol::Chat)
        );
        assert_eq!(
            upstream_protocol(UpstreamProtocol::OpenAI, "responses"),
            Some(Protocol::Responses)
        );
    }

    #[test]
    fn maps_only_consistent_upstream_pairs() {
        assert_eq!(
            upstream_protocol(UpstreamProtocol::Anthropic, "messages"),
            Some(Protocol::Messages)
        );
        assert_eq!(
            upstream_protocol(UpstreamProtocol::Responses, "responses"),
            Some(Protocol::Responses)
        );
        assert_eq!(
            upstream_protocol(UpstreamProtocol::Gemini, "generate_content"),
            Some(Protocol::Gemini)
        );

        for (protocol, endpoint) in [
            (UpstreamProtocol::Ollama, "api_chat"),
            (UpstreamProtocol::Anthropic, "count_tokens"),
            (UpstreamProtocol::OpenAI, "embeddings"),
            (UpstreamProtocol::OpenAI, "messages"),
            (UpstreamProtocol::Anthropic, "chat_completions"),
            (UpstreamProtocol::Responses, "chat_completions"),
            (UpstreamProtocol::Ollama, "responses"),
            (UpstreamProtocol::OpenAI, "/v1/chat/completions"),
            (UpstreamProtocol::OpenAI, "unknown"),
        ] {
            assert_eq!(
                upstream_protocol(protocol, endpoint),
                None,
                "{protocol:?}/{endpoint} must not enter the codec matrix"
            );
        }
    }
}
