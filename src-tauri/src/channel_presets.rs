//! 渠道提供商模板 registry（唯一可信源）
//!
//! 本模块是前端、迁移、路由与草稿测试共用的 provider preset 单一数据来源
//! （设计 4.4）。它只定义纯类型与只读数据，不访问网络、不写入数据库。
//!
//! 设计文档：docs/channel-protocol-provider-refactor-design.md §2、§4.2、§4.4、§5.2
//! 任务规格：docs/channel-refactor-tasks/01-presets-and-domain-model.md
//!
//! 序列化稳定性：枚举字符串与设计 5.2 的 TS DTO 完全一致，不得随意改名。

use serde::{Deserialize, Serialize};

/// 协议：决定上游请求/响应格式、鉴权、测试方式与端点集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelProtocol {
    #[serde(rename = "openai")]
    OpenAI,
    Anthropic,
    Ollama,
}

impl ChannelProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChannelProtocol::OpenAI => "openai",
            ChannelProtocol::Anthropic => "anthropic",
            ChannelProtocol::Ollama => "ollama",
        }
    }
}

/// 渠道提供商：决定默认 Base URL、模型建议、地区分组与厂商提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelProvider {
    #[serde(rename = "openai")]
    OpenAI,
    Google,
    #[serde(rename = "deepseek")]
    DeepSeek,
    Qwen,
    Zhipu,
    Doubao,
    #[serde(rename = "doubao_coding_plan")]
    DoubaoCodingPlan,
    Moonshot,
    #[serde(rename = "stepfun")]
    StepFun,
    /// OpenCode 免费车道（opencode.ai 公开免密网关，v2 客户端指纹门禁）。
    #[serde(rename = "opencode")]
    Opencode,
    /// Kilo 免费池（api.kilo.ai 公共网关免费池，完全免 Key）。
    #[serde(rename = "kilo")]
    Kilo,
    /// Groq 免费层（超高速 LPU 推理，注册领 Key）。
    #[serde(rename = "groq")]
    Groq,
    /// Cerebras 免费层（注册即用的高速推理）。
    #[serde(rename = "cerebras")]
    Cerebras,
    /// Mistral 免费实验层（La Plateforme 免费额度）。
    #[serde(rename = "mistral")]
    Mistral,
    /// NVIDIA NIM 免费层（注册送 credits，聚合多厂商开源模型）。
    #[serde(rename = "nvidia_nim")]
    NvidiaNim,
    /// OpenRouter（聚合网关，:free 后缀模型免费调用）。
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// ModelScope 魔搭免费推理 API（注册实名送每日 2000 次调用）。
    #[serde(rename = "modelscope")]
    ModelScope,
    /// GitHub Models（GitHub PAT 即用的免费推理层）。
    #[serde(rename = "github_models")]
    GithubModels,
    /// SiliconFlow 硅基流动（注册送额度 + 多款永久免费小模型）。
    #[serde(rename = "siliconflow")]
    SiliconFlow,
    /// Hugging Face Inference Providers（免费账户月度额度）。
    #[serde(rename = "huggingface")]
    HuggingFace,
    /// SambaNova Cloud 免费层（20 RPM / 20 RPD，免信用卡）。
    #[serde(rename = "sambanova")]
    SambaNova,
    /// Cohere Trial Key（每月 1000 次评估调用）。
    #[serde(rename = "cohere")]
    Cohere,
    Anthropic,
    Ollama,
    Custom,
}

impl ChannelProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChannelProvider::OpenAI => "openai",
            ChannelProvider::Google => "google",
            ChannelProvider::DeepSeek => "deepseek",
            ChannelProvider::Qwen => "qwen",
            ChannelProvider::Zhipu => "zhipu",
            ChannelProvider::Doubao => "doubao",
            ChannelProvider::DoubaoCodingPlan => "doubao_coding_plan",
            ChannelProvider::Moonshot => "moonshot",
            ChannelProvider::StepFun => "stepfun",
            ChannelProvider::Opencode => "opencode",
            ChannelProvider::Kilo => "kilo",
            ChannelProvider::Groq => "groq",
            ChannelProvider::Cerebras => "cerebras",
            ChannelProvider::Mistral => "mistral",
            ChannelProvider::NvidiaNim => "nvidia_nim",
            ChannelProvider::OpenRouter => "openrouter",
            ChannelProvider::ModelScope => "modelscope",
            ChannelProvider::GithubModels => "github_models",
            ChannelProvider::SiliconFlow => "siliconflow",
            ChannelProvider::HuggingFace => "huggingface",
            ChannelProvider::SambaNova => "sambanova",
            ChannelProvider::Cohere => "cohere",
            ChannelProvider::Anthropic => "anthropic",
            ChannelProvider::Ollama => "ollama",
            ChannelProvider::Custom => "custom",
        }
    }
}

/// 原生端点：描述该渠道上游真实提供的端点（T00 决策 9）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeEndpoint {
    ChatCompletions,
    Responses,
    Messages,
    CountTokens,
    Embeddings,
    ApiChat,
}

impl NativeEndpoint {
    pub fn as_str(&self) -> &'static str {
        match self {
            NativeEndpoint::ChatCompletions => "chat_completions",
            NativeEndpoint::Responses => "responses",
            NativeEndpoint::Messages => "messages",
            NativeEndpoint::CountTokens => "count_tokens",
            NativeEndpoint::Embeddings => "embeddings",
            NativeEndpoint::ApiChat => "api_chat",
        }
    }
}

/// 鉴权方案：各厂商接受不同的凭据放置方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    /// `Authorization: Bearer <key>`
    Bearer,
    /// `x-api-key: <key>`
    XApiKey,
    /// URL query 参数携带 key（仅旧 Google 原生配置）
    QueryKey,
    /// Bearer 可选（Ollama 本地默认无 Key）
    OptionalBearer,
}

/// 地区分组：产品分组，非服务器部署地域判断（设计 4.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionGroup {
    /// 自定义配置，置顶、默认选中，不归入国际/国内/本地
    Custom,
    International,
    Domestic,
    Local,
}

/// 单个静态模型建议：必须可追溯到 `verified_at` + 官方 `source_url`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSuggestion {
    pub id: String,
    /// 复核日期，格式 `YYYY-MM-DD`（2026-08-04 基线）
    pub verified_at: String,
    /// 官方模型目录/文档地址
    pub source_url: String,
}

/// 端点测试策略：草稿测试如何验证该预设的真实端点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointTestStrategy {
    /// 用最小推理请求（stream:false + 最小输出上限）验证端点可用性
    ProbeFirstModel,
    /// 查询模型列表接口（OpenAI 兼容 `/models` / Ollama `/api/tags`）
    ListModels,
}

/// 模型枚举策略：新建引导时如何获得模型候选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelEnumStrategy {
    /// 仅用静态建议（Anthropic 不假设存在兼容模型列表）
    StaticOnly,
    /// 静态建议 + 允许上游同步（OpenAI 兼容 `/models`）
    StaticPlusSync,
    /// 仅从上游枚举（Ollama `/api/tags`）
    SyncOnly,
}

/// 渠道提供商模板。所有字段序列化稳定；URL 为完整规范值，不做运行时猜厂商。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelPreset {
    /// 稳定 ID，格式 `{protocol}:{provider}`；custom 为 `{protocol}:custom`
    pub id: String,
    pub protocol: ChannelProtocol,
    pub provider: ChannelProvider,
    /// 显示名称（例如“字节豆包（Coding Plan）”“Ollama（本地）”）
    pub display_name: String,
    pub region: RegionGroup,
    pub description: String,
    /// 前端图标 key（`"openai" | "google" | ...`）
    pub icon_key: String,
    /// 新协议规范根 URL（UI 显示/编辑）
    pub native_base_url: String,
    /// 旧代码兼容根 URL（迁移期写回 `channels.base_url`）
    pub legacy_base_url: String,
    /// 旧适配器 `type`（写回 `channels.type`）
    pub legacy_type: String,
    /// 上游原生端点能力
    pub native_endpoints: Vec<NativeEndpoint>,
    /// 新建时默认勾选的端点
    pub default_checked_endpoints: Vec<NativeEndpoint>,
    pub auth_scheme: AuthScheme,
    pub model_suggestions: Vec<ModelSuggestion>,
    pub model_enum_strategy: ModelEnumStrategy,
    pub endpoint_test_strategy: EndpointTestStrategy,
    /// preset revision；保存渠道时记录供追溯，模板更新不覆盖已保存渠道
    pub preset_revision: String,
    /// 免费层提供商的「获取 API Key」页面；前端在 Key 配置上方渲染跳转提示，
    /// 方便用户直接前往注册领 Key。非免费层为 None（序列化时省略）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_signup_url: Option<String>,
}

impl ChannelPreset {
    /// 附上「获取 API Key」跳转链接（链式，注册表内免费层提供商用）。
    fn signup(mut self, url: &str) -> Self {
        self.key_signup_url = Some(url.to_string());
        self
    }
}

/// 每个协议返回一组：`presets[0]` 恒为 custom option（置顶、默认选中）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolPresetGroup {
    pub protocol: ChannelProtocol,
    pub presets: Vec<ChannelPreset>,
}

/// 当前 registry revision（YYYY-MM-DD）。2026-08-06 建立基线，2026-10-22 加入
/// OpenCode 免费车道与 Kilo 免费池两个 preset，补齐免费层标注
/// （Google/智谱 signup）与 ModelScope 魔搭免费层 preset；同日移除
/// GitHub Models preset（该服务已于 2026-07-30 被 GitHub 全面下线）。
pub const PRESET_REVISION: &str = "2026-10-22";

/// 2026-08-06 基线时入库的模型建议复核日（与 registry revision 解耦，
/// 避免后加 preset 时把老模型的复核日期一起顺延）。
const MODEL_BASELINE_0806: &str = "2026-08-06";

const SRC_OPENAI: &str = "https://platform.openai.com/docs/api-reference/chat";
const SRC_GEMINI_MODELS: &str = "https://ai.google.dev/gemini-api/docs/models";
const SRC_DEEPSEEK_FUNCTION_CALLING: &str =
    "https://api-docs.deepseek.com/guides/function_calling/";
const SRC_DEEPSEEK_ANTHROPIC: &str = "https://api-docs.deepseek.com/guides/anthropic_api";
const SRC_QWEN_ANTHROPIC: &str = "https://help.aliyun.com/en/model-studio/more-tools";
const SRC_QWEN_RESPONSES: &str =
    "https://help.aliyun.com/en/model-studio/qwen-api-via-openai-responses";
const SRC_ZHIPU: &str = "https://open.bigmodel.cn/dev/api";
const SRC_DOUBAO: &str = "https://www.volcengine.com/docs/82379/";
const SRC_MOONSHOT: &str = "https://platform.moonshot.ai/docs/api/chat";
const SRC_STEPFUN: &str = "https://platform.stepfun.com/docs/zh/api-reference/models/list.md";
const SRC_ANTHROPIC: &str = "https://docs.anthropic.com/en/api/messages";
/// OpenCode 免费车道模型清单的核对出处（dsh-our-free-model 免费车道文档）。
const SRC_OPENCODE: &str =
    "https://github.com/Ebony-Vinyl/dsh-our-free-model#免费车道opencode-的免密网关";
/// Kilo 免费池模型清单的核对出处（Kilo 网关模型列表，isFree 切片）。
const SRC_KILO: &str = "https://kilo.ai";
/// 免费层 API Key 提供商的模型清单核对出处（官方模型目录）。
const SRC_GROQ: &str = "https://console.groq.com/docs/models";
const SRC_CEREBRAS: &str = "https://inference-docs.cerebras.ai/models";
const SRC_MISTRAL: &str = "https://docs.mistral.ai/getting-started/models";
const SRC_NVIDIA: &str = "https://build.nvidia.com/explore/reasoning";
const SRC_OPENROUTER: &str = "https://openrouter.ai/docs/models";
const SRC_MODELSCOPE: &str = "https://modelscope.cn/headlines/article/795";
const SRC_SILICONFLOW: &str = "https://siliconflow.cn/pricing";
const SRC_HUGGINGFACE: &str = "https://huggingface.co/docs/inference-providers/index";
const SRC_SAMBANOVA: &str = "https://docs.sambanova.ai/docs/en/models";
const SRC_COHERE: &str = "https://docs.cohere.com/docs/models";

/// 构建一个 preset 的便捷函数。
#[allow(clippy::too_many_arguments)]
fn preset(
    protocol: ChannelProtocol,
    provider: ChannelProvider,
    display_name: &str,
    region: RegionGroup,
    description: &str,
    icon_key: &str,
    native_base_url: &str,
    legacy_base_url: &str,
    legacy_type: &str,
    native_endpoints: Vec<NativeEndpoint>,
    default_checked_endpoints: Vec<NativeEndpoint>,
    auth_scheme: AuthScheme,
    model_suggestions: Vec<ModelSuggestion>,
    model_enum_strategy: ModelEnumStrategy,
    endpoint_test_strategy: EndpointTestStrategy,
) -> ChannelPreset {
    let id = format!("{}:{}", protocol.as_str(), provider.as_str());
    ChannelPreset {
        id,
        protocol,
        provider,
        display_name: display_name.to_string(),
        region,
        description: description.to_string(),
        icon_key: icon_key.to_string(),
        native_base_url: native_base_url.to_string(),
        legacy_base_url: legacy_base_url.to_string(),
        legacy_type: legacy_type.to_string(),
        native_endpoints,
        default_checked_endpoints,
        auth_scheme,
        model_suggestions,
        model_enum_strategy,
        endpoint_test_strategy,
        preset_revision: PRESET_REVISION.to_string(),
        key_signup_url: None,
    }
}

fn model(id: &str, verified_at: &str, source_url: &str) -> ModelSuggestion {
    ModelSuggestion {
        id: id.to_string(),
        verified_at: verified_at.to_string(),
        source_url: source_url.to_string(),
    }
}

/// custom option：不提供默认 URL、密钥或模型；协议决定其允许端点。
fn custom_preset(protocol: ChannelProtocol) -> ChannelPreset {
    let (native_endpoints, default_checked, auth, strategy) = match protocol {
        ChannelProtocol::OpenAI => (
            vec![
                NativeEndpoint::ChatCompletions,
                NativeEndpoint::Responses,
                NativeEndpoint::Embeddings,
            ],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            EndpointTestStrategy::ProbeFirstModel,
        ),
        ChannelProtocol::Anthropic => (
            vec![NativeEndpoint::Messages],
            vec![NativeEndpoint::Messages],
            AuthScheme::XApiKey,
            EndpointTestStrategy::ProbeFirstModel,
        ),
        ChannelProtocol::Ollama => (
            vec![NativeEndpoint::ApiChat],
            vec![NativeEndpoint::ApiChat],
            AuthScheme::OptionalBearer,
            EndpointTestStrategy::ProbeFirstModel,
        ),
    };
    preset(
        protocol,
        ChannelProvider::Custom,
        "自定义配置",
        RegionGroup::Custom,
        "手动配置协议与 Base URL，适用于私有网关或未内置厂商。",
        "custom",
        "",
        "",
        match protocol {
            ChannelProtocol::OpenAI | ChannelProtocol::Ollama => "openai",
            ChannelProtocol::Anthropic => "claude",
        },
        native_endpoints,
        default_checked,
        auth,
        vec![],
        ModelEnumStrategy::StaticOnly,
        strategy,
    )
}

fn openai_presets() -> Vec<ChannelPreset> {
    vec![
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::OpenAI,
            "OpenAI",
            RegionGroup::International,
            "OpenAI 官方 API（Chat Completions 与 Responses）。",
            "openai",
            "https://api.openai.com/v1",
            "https://api.openai.com/v1",
            "openai",
            vec![NativeEndpoint::ChatCompletions, NativeEndpoint::Responses],
            vec![NativeEndpoint::ChatCompletions, NativeEndpoint::Responses],
            AuthScheme::Bearer,
            vec![
                model("gpt-5.2", MODEL_BASELINE_0806, SRC_OPENAI),
                model("gpt-5-mini", MODEL_BASELINE_0806, SRC_OPENAI),
                model("gpt-5-nano", MODEL_BASELINE_0806, SRC_OPENAI),
                model("gpt-4.1", MODEL_BASELINE_0806, SRC_OPENAI),
                model("gpt-4.1-mini", MODEL_BASELINE_0806, SRC_OPENAI),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        ),
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::Google,
            "Google",
            RegionGroup::International,
            "Google Gemini 官方 OpenAI 接口。AI Studio 免费层：Flash 系列每日 100 万 token 免费额度（15 RPM），免费层内容可能被 Google 用于改进产品。",
            "google",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "openai",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                model("gemini-3.6-flash", MODEL_BASELINE_0806, SRC_GEMINI_MODELS),
                model("gemini-3.5-flash", MODEL_BASELINE_0806, SRC_GEMINI_MODELS),
                model("gemini-3.5-flash-lite", MODEL_BASELINE_0806, SRC_GEMINI_MODELS),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        )
        .signup("https://aistudio.google.com/apikey"),
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::DeepSeek,
            "DeepSeek",
            RegionGroup::Domestic,
            "DeepSeek 官方 OpenAI 接口。",
            "deepseek",
            "https://api.deepseek.com",
            "https://api.deepseek.com",
            "deepseek",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                model(
                    "deepseek-v4-pro",
                    MODEL_BASELINE_0806,
                    SRC_DEEPSEEK_FUNCTION_CALLING,
                ),
                model(
                    "deepseek-v4-flash",
                    MODEL_BASELINE_0806,
                    SRC_DEEPSEEK_FUNCTION_CALLING,
                ),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        ),
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::Qwen,
            "通义千问",
            RegionGroup::Domestic,
            "阿里云百炼 OpenAI 接口。",
            "qwen",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "qwen",
            vec![NativeEndpoint::ChatCompletions, NativeEndpoint::Responses],
            vec![NativeEndpoint::ChatCompletions, NativeEndpoint::Responses],
            AuthScheme::Bearer,
            vec![
                model("qwen3.7-plus", MODEL_BASELINE_0806, SRC_QWEN_RESPONSES),
                model("qwen3.7-max", MODEL_BASELINE_0806, SRC_QWEN_RESPONSES),
                model("qwen3-coder-next", MODEL_BASELINE_0806, SRC_QWEN_RESPONSES),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        ),
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::Zhipu,
            "智谱 GLM",
            RegionGroup::Domestic,
            "智谱 GLM OpenAI 接口（PAAS v4）。glm-4.7-flash 等 Flash 系列免费（免费层仅 1 条并发请求），旗舰模型计费。",
            "zhipu",
            "https://open.bigmodel.cn/api/paas/v4",
            "https://open.bigmodel.cn/api/paas/v4",
            "zhipu",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                model("glm-4.7", MODEL_BASELINE_0806, SRC_ZHIPU),
                model("glm-4.7-flash", MODEL_BASELINE_0806, SRC_ZHIPU),
                model("glm-4.6v", MODEL_BASELINE_0806, SRC_ZHIPU),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        )
        .signup("https://open.bigmodel.cn/usercenter/apikeys"),
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::Doubao,
            "字节豆包 (Coding Plan)",
            RegionGroup::Domestic,
            "字节豆包官方 OpenAI 接口。",
            "doubao",
            "https://ark.cn-beijing.volces.com/api/v3",
            "https://ark.cn-beijing.volces.com/api/v3",
            "doubao",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                model("doubao-seed-2-0-pro-260215", MODEL_BASELINE_0806, SRC_DOUBAO),
                model("doubao-seed-2-0-lite-260215", MODEL_BASELINE_0806, SRC_DOUBAO),
                model("doubao-seed-1-6", MODEL_BASELINE_0806, SRC_DOUBAO),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        ),
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::Moonshot,
            "Moonshot(Kimi)",
            RegionGroup::Domestic,
            "Moonshot Kimi OpenAI 接口。",
            "moonshot",
            "https://api.moonshot.ai/v1",
            "https://api.moonshot.ai/v1",
            "moonshot",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                model("kimi-k2.5", MODEL_BASELINE_0806, SRC_MOONSHOT),
                model("kimi-k2-thinking", MODEL_BASELINE_0806, SRC_MOONSHOT),
                model("kimi-k2-turbo-preview", MODEL_BASELINE_0806, SRC_MOONSHOT),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        ),
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::StepFun,
            "StepFun（阶跃星辰）",
            RegionGroup::Domestic,
            "阶跃星辰 StepFun 官方 OpenAI 接口。",
            "stepfun",
            "https://api.stepfun.com/v1",
            "https://api.stepfun.com/v1",
            "openai",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            // 5 条静态建议，顺序即预填/探测默认（旗舰在前）；verified_at 为调研
            // 抓取日（2026-09-20），source_url 指向官方模型清单文档。
            vec![
                model("step-5-preview", "2026-09-20", SRC_STEPFUN),
                model("step-3.7-flash", "2026-09-20", SRC_STEPFUN),
                model("step-3.5-flash", "2026-09-20", SRC_STEPFUN),
                model("step-3.5-flash-2603", "2026-09-20", SRC_STEPFUN),
                model("step-1o-turbo-vision", "2026-09-20", SRC_STEPFUN),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        ),
        // ModelScope 魔搭免费推理 API：注册并实名后每日 2000 次免费调用，
        // 单模型 ≤500 次/天（动态限速）。仅支持中国手机号注册，境外用户
        // 不可用；凭据为魔搭 SDK Token，Bearer 方式。
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::ModelScope,
            "ModelScope 魔搭（免费层）",
            RegionGroup::Domestic,
            "魔搭社区免费推理 API：注册并实名后每日 2000 次免费调用（单模型 ≤500 次/天，动态限速）。需中国手机号注册；请勿用于高并发或 SLA 保障场景。",
            "modelscope",
            "https://api-inference.modelscope.cn/v1",
            "https://api-inference.modelscope.cn/v1",
            "openai",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                model("Qwen/Qwen3.5-35B-A3B", "2026-10-22", SRC_MODELSCOPE),
                model("Qwen/Qwen3.5-27B", "2026-10-22", SRC_MODELSCOPE),
                model("deepseek-ai/DeepSeek-V3", "2026-10-22", SRC_MODELSCOPE),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        )
        .signup("https://modelscope.cn/my/myaccesstoken"),
        // SiliconFlow 硅基流动：注册（需实名）送体验额度，另有若干永久免费
        // 小模型（固定限速，超额 429）。免费清单动态变化，以官网价格页为准，
        // 可经「同步上游模型」覆盖。
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::SiliconFlow,
            "SiliconFlow 硅基流动（免费层）",
            RegionGroup::Domestic,
            "硅基流动 OpenAI 接口：注册并实名后部分小模型永久免费（固定限速，超额 429），注册另送体验额度（付费模型可用）。免费模型清单动态变化，以官网价格页为准。",
            "siliconflow",
            "https://api.siliconflow.cn/v1",
            "https://api.siliconflow.cn/v1",
            "openai",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                model("THUDM/GLM-4-9B-0414", "2026-10-22", SRC_SILICONFLOW),
                model("THUDM/GLM-Z1-9B-0414", "2026-10-22", SRC_SILICONFLOW),
                model("tencent/Hunyuan-MT-7B", "2026-10-22", SRC_SILICONFLOW),
                model("deepseek-ai/DeepSeek-R1-Distill-Qwen-7B", "2026-10-22", SRC_SILICONFLOW),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        )
        .signup("https://cloud.siliconflow.cn/account/ak"),
        // StepFun 模型建议的排除项（依据 2026-09-20 官方文档调研）：
        // - 音频族（stepaudio-* / step-1o-audio / step-audio-2 / step-audio-r1.5）：
        //   走 chat completions 需厂商私有 modalities/audio 参数，网关不透传；
        // - step-router-v1：仅 Step Plan 通道可用，标准通道 400；
        // - TTS/ASR/Realtime 与生图/改图（step-2x-large、step-image-edit-2、
        //   step-1x-edit，且后者 2026-10-10 下线）：非 chat completions 能力；
        // - 2026-07-08 已下线型号（step-1-8k/32k、step-1v-8k/32k、step-2-mini、
        //   step-1o-vision-32k、step-2-16k、step-3、step-1x-medium）。
        // 上述均不预填，用户可经「同步上游模型」拉取 GET /v1/models 覆盖。
        // OpenCode 免费车道：公开免密网关（凭据即字符串 "public"）。上游有
        // v2 客户端指纹门禁（固定 UA + 会话头 + 逐字工具指纹），由
        // endpoint_executor::opencode_lane 在转发层适配；本预设只声明
        // Chat Completions —— 上游 /responses 与 /messages 仅个别贡献者模型
        // 支持，预设级声明会让其它模型被原生路由击中 404。
        // 模型清单跟随上游刷新（插件与上游均不保存静态快照），静态建议取自
        // dsh-our-free-model 2026-10-22 源码/文档核对，可经「同步上游模型」覆盖。
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::Opencode,
            "OpenCode 免费车道",
            RegionGroup::International,
            "OpenCode 公开免密网关：无需账号与 Key，模型清单跟随上游，地区受限模型会被上游拒绝。免费额度按会话统计，上游可能限流。",
            "opencode",
            "https://opencode.ai/inference/v1",
            "https://opencode.ai/inference/v1",
            "openai",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                // 2026-10-22 对上游 GET /models 实测在列的免费模型（其后缀 -free）。
                model("deepseek-v4.1-flash-free", "2026-10-22", SRC_OPENCODE),
                model("glm-5.3-flash-free", "2026-10-22", SRC_OPENCODE),
                model("mimo-v2.6-flash-free", "2026-10-22", SRC_OPENCODE),
                model("nemotron-3.5-lightning-free", "2026-10-22", SRC_OPENCODE),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        ),
        // Kilo 免费池：api.kilo.ai 公共网关的免费切片，完全免 Key 免登录
        // （AuthScheme::OptionalBearer + 空 Key = 不发 Authorization 头，
        // 运行时经 endpoint_executor::auth_scheme_for 的 provider 判定）。
        // 仅 Chat Completions 一种线协议。上游模型卡声明 prompt 可能被记录
        // 用于改进服务——描述里如实提示，勿传敏感内容。
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::Kilo,
            "Kilo 免费池",
            RegionGroup::International,
            "Kilo AI 公共网关免费池：无需任何账号或 Key。上游声明 prompt 可能被记录用于改进服务，请勿发送敏感内容；免费池模型清单实时变化。",
            "kilo",
            "https://api.kilo.ai/api/gateway",
            "https://api.kilo.ai/api/gateway",
            "openai",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::OptionalBearer,
            vec![model("kilo-auto/free", "2026-10-22", SRC_KILO)],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        ),
        // 免费层 API Key 提供商：注册后在官方控制台领取 Key，免费额度各有
        // 限额与速率约束（超额 429）。端点均为 OpenAI Chat Completions 兼容，
        // 模型清单可经「同步上游模型」从 GET /models 拉取覆盖；静态建议取自
        // dsh-our-free-model 提供商注册表（2026-10-22 核对）。key_signup_url
        // 供前端在 Key 配置上方渲染「前往获取 API Key」跳转提示。
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::Groq,
            "Groq（免费层）",
            RegionGroup::International,
            "Groq 超高速 LPU 推理免费层：注册后领取 API Key 即用，速率与每日额度有限，超额返回 429。模型清单跟随上游。",
            "groq",
            "https://api.groq.com/openai/v1",
            "https://api.groq.com/openai/v1",
            "openai",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                model("llama-3.3-70b-versatile", "2026-10-22", SRC_GROQ),
                model("meta-llama/llama-4-maverick-17b-128e-instruct", "2026-10-22", SRC_GROQ),
                model("qwen/qwen3-32b", "2026-10-22", SRC_GROQ),
                model("openai/gpt-oss-120b", "2026-10-22", SRC_GROQ),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        )
        .signup("https://console.groq.com/keys"),
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::Cerebras,
            "Cerebras（免费层）",
            RegionGroup::International,
            "Cerebras 晶圆级芯片高速推理免费层：注册即送额度，速率有限，超额返回 429。",
            "cerebras",
            "https://api.cerebras.ai/v1",
            "https://api.cerebras.ai/v1",
            "openai",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                model("gpt-oss-120b", "2026-10-22", SRC_CEREBRAS),
                model("zai-glm-4.7", "2026-10-22", SRC_CEREBRAS),
                model("llama-3.3-70b", "2026-10-22", SRC_CEREBRAS),
                model("llama-4-scout-17b-16e-instruct", "2026-10-22", SRC_CEREBRAS),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        )
        .signup("https://cloud.cerebras.ai"),
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::Mistral,
            "Mistral（免费层）",
            RegionGroup::International,
            "Mistral La Plateforme 免费实验层：注册领取 API Key，免费额度按日/月限额，速率受限。",
            "mistral",
            "https://api.mistral.ai/v1",
            "https://api.mistral.ai/v1",
            "openai",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                // 固定版本号 id（-latest 浮动别名被预设规约禁止，见基线测试）；
                // pinned 与 latest 的对应关系经 Mistral 官方模型目录核对。
                model("mistral-large-2411", "2026-10-22", SRC_MISTRAL),
                model("mistral-medium-2505", "2026-10-22", SRC_MISTRAL),
                model("codestral-2508", "2026-10-22", SRC_MISTRAL),
                model("devstral-small-2507", "2026-10-22", SRC_MISTRAL),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        )
        .signup("https://console.mistral.ai/api-keys"),
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::NvidiaNim,
            "NVIDIA NIM（免费层）",
            RegionGroup::International,
            "NVIDIA NIM 聚合推理：注册送 1000 credits，可在 build.nvidia.com 调用多厂商开源模型（DeepSeek/Kimi/GLM/MiniMax 等）。",
            "nvidia_nim",
            "https://integrate.api.nvidia.com/v1",
            "https://integrate.api.nvidia.com/v1",
            "openai",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                model("deepseek-ai/deepseek-v4-pro", "2026-10-22", SRC_NVIDIA),
                model("z-ai/glm-5.2", "2026-10-22", SRC_NVIDIA),
                model("moonshotai/kimi-k2.6", "2026-10-22", SRC_NVIDIA),
                model("minimaxai/minimax-m2.7", "2026-10-22", SRC_NVIDIA),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        )
        .signup("https://build.nvidia.com/settings/api-keys"),
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::OpenRouter,
            "OpenRouter（免费模型）",
            RegionGroup::International,
            "OpenRouter 聚合网关：带 :free 后缀的模型免费调用（有每日请求上限）；openrouter/free 自动路由当前可用的免费池。同步上游模型后可浏览全部 :free 清单。",
            "openrouter",
            "https://openrouter.ai/api/v1",
            "https://openrouter.ai/api/v1",
            "openai",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![model("openrouter/free", "2026-10-22", SRC_OPENROUTER)],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        )
        .signup("https://openrouter.ai/keys"),
        // GitHub Models 已于 2026-07-30 被 GitHub 官方全面下线（playground /
        // 模型目录 / 推理 API / BYOK 全部关闭，见 GitHub changelog）。下线后
        // models.github.ai 对任意路径/方法/凭据统一返回 200 + 纯文本 `OK`，
        // 任何 Key 都无法使用，预设随之移除（ChannelProvider::GithubModels
        // 枚举保留，兼容已存储渠道的反序列化）。
        // SambaNova Cloud 免费层：无绑卡状态即 Free Tier（20 RPM / 20 RPD /
        // 200K tokens/天，超额 429）。模型清单可无凭据经 GET /models 拉取，
        // 可经「同步上游模型」覆盖；DeepSeek-V3.2 与 MiniMax-M3 为 Preview，
        // 上游可能随时下线。
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::SambaNova,
            "SambaNova（免费层）",
            RegionGroup::International,
            "SambaNova RDU 高速推理免费层：邮箱注册即用（无需信用卡），免费层 20 次/分钟、20 次/天、20 万 token/天，超额返回 429。DeepSeek-V3.2 / MiniMax-M3 为 Preview 型号，可能随时下线。",
            "sambanova",
            "https://api.sambanova.ai/v1",
            "https://api.sambanova.ai/v1",
            "openai",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                model("DeepSeek-V3.1", "2026-10-22", SRC_SAMBANOVA),
                model("Meta-Llama-3.3-70B-Instruct", "2026-10-22", SRC_SAMBANOVA),
                model("gpt-oss-120b", "2026-10-22", SRC_SAMBANOVA),
                model("MiniMax-M3", "2026-10-22", SRC_SAMBANOVA),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        )
        .signup("https://cloud.sambanova.ai/apis"),
        // Hugging Face Inference Providers：免费账户每月赠送 Inference
        // 额度，token 需「Make calls to Inference Providers」权限。router
        // 按 model id 自动路由到合作的推理 Provider，模型清单可经
        // 「同步上游模型」覆盖。
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::HuggingFace,
            "Hugging Face（免费层）",
            RegionGroup::International,
            "Hugging Face Inference Providers：免费账户每月赠送推理额度，请求自动路由到合作 Provider。token 需勾选 Inference Providers 调用权限；额度用尽后返回 402/429。",
            "huggingface",
            "https://router.huggingface.co/v1",
            "https://router.huggingface.co/v1",
            "openai",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                model("deepseek-ai/DeepSeek-V3-0324", "2026-10-22", SRC_HUGGINGFACE),
                model("Qwen/Qwen3-235B-A22B", "2026-10-22", SRC_HUGGINGFACE),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        )
        .signup("https://huggingface.co/settings/tokens"),
        // Cohere Trial Key：每月 1000 次调用（20 RPM），仅评估用途。
        // compatibility 端点提供 OpenAI Chat Completions 兼容层。
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::Cohere,
            "Cohere（Trial）",
            RegionGroup::International,
            "Cohere 官方 OpenAI 兼容端点：Trial Key 每月 1000 次调用（20 RPM），仅用于评估，不可用于生产。Aya 系列擅长多语言。",
            "cohere",
            "https://api.cohere.ai/compatibility/v1",
            "https://api.cohere.ai/compatibility/v1",
            "openai",
            vec![NativeEndpoint::ChatCompletions],
            vec![NativeEndpoint::ChatCompletions],
            AuthScheme::Bearer,
            vec![
                model("command-a-03-2025", "2026-10-22", SRC_COHERE),
                model("command-r-plus-08-2024", "2026-10-22", SRC_COHERE),
                model("command-r7b-12-2024", "2026-10-22", SRC_COHERE),
            ],
            ModelEnumStrategy::StaticPlusSync,
            EndpointTestStrategy::ProbeFirstModel,
        )
        .signup("https://dashboard.cohere.com/api-keys"),
        preset(
            ChannelProtocol::OpenAI,
            ChannelProvider::Ollama,
            "Ollama（本地）",
            RegionGroup::Local,
            "本机或远程 Ollama 的 OpenAI 接口。",
            "ollama",
            "http://localhost:11434/v1",
            "http://localhost:11434/v1",
            "openai",
            vec![NativeEndpoint::ChatCompletions, NativeEndpoint::Responses],
            vec![NativeEndpoint::ChatCompletions, NativeEndpoint::Responses],
            AuthScheme::OptionalBearer,
            vec![],
            ModelEnumStrategy::SyncOnly,
            EndpointTestStrategy::ProbeFirstModel,
        ),
    ]
}

fn anthropic_presets() -> Vec<ChannelPreset> {
    vec![
        preset(
            ChannelProtocol::Anthropic,
            ChannelProvider::Anthropic,
            "Anthropic",
            RegionGroup::International,
            "Anthropic Claude Code 官方 Messages API。",
            "claudecode",
            "https://api.anthropic.com/v1",
            "https://api.anthropic.com/v1",
            "claude",
            vec![NativeEndpoint::Messages],
            vec![NativeEndpoint::Messages],
            AuthScheme::XApiKey,
            vec![
                model("claude-opus-4-6", MODEL_BASELINE_0806, SRC_ANTHROPIC),
                model("claude-sonnet-4-6", MODEL_BASELINE_0806, SRC_ANTHROPIC),
                model("claude-haiku-4-5-20251001", MODEL_BASELINE_0806, SRC_ANTHROPIC),
            ],
            ModelEnumStrategy::StaticOnly,
            EndpointTestStrategy::ProbeFirstModel,
        ),
        preset(
            ChannelProtocol::Anthropic,
            ChannelProvider::DeepSeek,
            "DeepSeek",
            RegionGroup::Domestic,
            "DeepSeek 官方 Anthropic 接口。",
            "deepseek",
            "https://api.deepseek.com/anthropic/v1",
            "https://api.deepseek.com/anthropic/v1",
            "claude",
            vec![NativeEndpoint::Messages],
            vec![NativeEndpoint::Messages],
            AuthScheme::XApiKey,
            vec![
                model("deepseek-v4-pro", MODEL_BASELINE_0806, SRC_DEEPSEEK_ANTHROPIC),
                model("deepseek-v4-flash", MODEL_BASELINE_0806, SRC_DEEPSEEK_ANTHROPIC),
            ],
            ModelEnumStrategy::StaticOnly,
            EndpointTestStrategy::ProbeFirstModel,
        ),
        preset(
            ChannelProtocol::Anthropic,
            ChannelProvider::Qwen,
            "通义千问",
            RegionGroup::Domestic,
            "阿里云百炼 Anthropic 接口。",
            "qwen",
            "https://dashscope.aliyuncs.com/apps/anthropic/v1",
            "https://dashscope.aliyuncs.com/apps/anthropic/v1",
            "claude",
            vec![NativeEndpoint::Messages],
            vec![NativeEndpoint::Messages],
            AuthScheme::XApiKey,
            vec![
                model("qwen3.7-plus", MODEL_BASELINE_0806, SRC_QWEN_ANTHROPIC),
                model("qwen3-coder-next", MODEL_BASELINE_0806, SRC_QWEN_ANTHROPIC),
            ],
            ModelEnumStrategy::StaticOnly,
            EndpointTestStrategy::ProbeFirstModel,
        ),
        preset(
            ChannelProtocol::Anthropic,
            ChannelProvider::Zhipu,
            "智谱 GLM",
            RegionGroup::Domestic,
            "智谱 GLM 官方 Anthropic 接口。glm-4.7-flash 等 Flash 系列免费（免费层仅 1 条并发请求），旗舰模型计费。",
            "zhipu",
            "https://open.bigmodel.cn/api/anthropic/v1",
            "https://open.bigmodel.cn/api/anthropic/v1",
            "claude",
            vec![NativeEndpoint::Messages],
            vec![NativeEndpoint::Messages],
            AuthScheme::XApiKey,
            vec![
                model("glm-4.7", MODEL_BASELINE_0806, SRC_ZHIPU),
                model("glm-4.7-flash", MODEL_BASELINE_0806, SRC_ZHIPU),
            ],
            ModelEnumStrategy::StaticOnly,
            EndpointTestStrategy::ProbeFirstModel,
        )
        .signup("https://open.bigmodel.cn/usercenter/apikeys"),
        preset(
            ChannelProtocol::Anthropic,
            ChannelProvider::DoubaoCodingPlan,
            "字节豆包 (Coding Plan)",
            RegionGroup::Domestic,
            "字节豆包官方 Anthropic 接口。",
            "doubao_coding_plan",
            "https://ark.cn-beijing.volces.com/api/coding/v1",
            "https://ark.cn-beijing.volces.com/api/coding/v1",
            "claude",
            vec![NativeEndpoint::Messages],
            vec![NativeEndpoint::Messages],
            AuthScheme::XApiKey,
            // Coding Plan 当前开通模型：官方目录随接入点/区域变化（设计 4.2 备注）。
            // 2026-08-04 复核时未获得可追溯的官方型号清单，故不预置未经确认的 ID，
            // 由用户在保存前按官方 Coding Plan 控制台选择；避免「latest/preview」式猜测。
            vec![],
            ModelEnumStrategy::StaticOnly,
            EndpointTestStrategy::ProbeFirstModel,
        ),
        preset(
            ChannelProtocol::Anthropic,
            ChannelProvider::Ollama,
            "Ollama（本地）",
            RegionGroup::Local,
            "本机或远程 Ollama 的 Anthropic Messages 接口。",
            "ollama",
            "http://localhost:11434/v1",
            "http://localhost:11434/v1",
            "claude",
            vec![NativeEndpoint::Messages],
            vec![NativeEndpoint::Messages],
            AuthScheme::OptionalBearer,
            vec![],
            ModelEnumStrategy::SyncOnly,
            EndpointTestStrategy::ProbeFirstModel,
        ),
    ]
}

fn ollama_presets() -> Vec<ChannelPreset> {
    vec![preset(
        ChannelProtocol::Ollama,
        ChannelProvider::Ollama,
        "Ollama（本地）",
        RegionGroup::Local,
        "Ollama 原生 /api/chat 协议。",
        "ollama",
        "http://localhost:11434",
        "http://localhost:11434/v1",
        "openai",
        vec![NativeEndpoint::ApiChat],
        vec![NativeEndpoint::ApiChat],
        AuthScheme::OptionalBearer,
        vec![],
        ModelEnumStrategy::SyncOnly,
        EndpointTestStrategy::ProbeFirstModel,
    )]
}

/// 全部 preset，顺序为每个协议的 custom 置顶，其后 international → domestic → local。
pub fn all_channel_presets() -> Vec<ChannelPreset> {
    let mut all = Vec::new();
    all.extend(presets_for_protocol(ChannelProtocol::OpenAI));
    all.extend(presets_for_protocol(ChannelProtocol::Anthropic));
    all.extend(presets_for_protocol(ChannelProtocol::Ollama));
    all
}

/// 指定协议的全部 preset：custom 置顶，其后 international → domestic → local。
pub fn presets_for_protocol(protocol: ChannelProtocol) -> Vec<ChannelPreset> {
    let mut presets = vec![custom_preset(protocol)];
    let mut vendor: Vec<ChannelPreset> = match protocol {
        ChannelProtocol::OpenAI => openai_presets(),
        ChannelProtocol::Anthropic => anthropic_presets(),
        ChannelProtocol::Ollama => ollama_presets(),
    };
    vendor.sort_by_key(|p| region_order(p.region));
    presets.extend(vendor);
    presets
}

fn region_order(region: RegionGroup) -> u8 {
    match region {
        RegionGroup::Custom => 0,
        RegionGroup::International => 1,
        RegionGroup::Domestic => 2,
        RegionGroup::Local => 3,
    }
}

/// 按协议分组返回，供 `get_channel_presets()` 使用。
pub fn groups_for_protocols() -> Vec<ProtocolPresetGroup> {
    [
        ChannelProtocol::OpenAI,
        ChannelProtocol::Anthropic,
        ChannelProtocol::Ollama,
    ]
    .into_iter()
    .map(|protocol| ProtocolPresetGroup {
        protocol,
        presets: presets_for_protocol(protocol),
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_is_first_for_each_protocol() {
        for protocol in [
            ChannelProtocol::OpenAI,
            ChannelProtocol::Anthropic,
            ChannelProtocol::Ollama,
        ] {
            let presets = presets_for_protocol(protocol);
            assert_eq!(presets[0].provider, ChannelProvider::Custom, "{protocol:?}");
            assert_eq!(presets[0].region, RegionGroup::Custom, "{protocol:?}");
            assert!(presets[0].native_base_url.is_empty());
            assert!(presets[0].model_suggestions.is_empty());
        }
    }

    #[test]
    fn custom_default_checked_chat_for_openai() {
        let presets = presets_for_protocol(ChannelProtocol::OpenAI);
        assert_eq!(
            presets[0].default_checked_endpoints,
            vec![NativeEndpoint::ChatCompletions]
        );
    }

    #[test]
    fn preset_ids_are_unique_and_stable() {
        let all = all_channel_presets();
        let mut ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "duplicate preset ids");
        assert!(all
            .iter()
            .all(|p| p.id == format!("{}:{}", p.protocol.as_str(), p.provider.as_str())));
        assert!(all.iter().all(|p| p.preset_revision == PRESET_REVISION));
    }

    #[test]
    fn anthropic_excludes_moonshot() {
        let presets = presets_for_protocol(ChannelProtocol::Anthropic);
        assert!(
            presets
                .iter()
                .all(|p| p.provider != ChannelProvider::Moonshot),
            "Anthropic 不得包含 Moonshot"
        );
    }

    #[test]
    fn custom_openai_allows_embeddings_without_enabling_them_by_default() {
        let preset = custom_preset(ChannelProtocol::OpenAI);
        assert!(preset
            .native_endpoints
            .contains(&NativeEndpoint::Embeddings));
        assert_eq!(
            preset.default_checked_endpoints,
            vec![NativeEndpoint::ChatCompletions]
        );
    }

    #[test]
    fn deepseek_region_is_domestic() {
        for protocol in [ChannelProtocol::OpenAI, ChannelProtocol::Anthropic] {
            let p = presets_for_protocol(protocol)
                .into_iter()
                .find(|p| p.provider == ChannelProvider::DeepSeek)
                .expect("deepseek preset");
            assert_eq!(p.region, RegionGroup::Domestic);
        }
    }

    /// StepFun（阶跃星辰）预设契约：协议/base_url/legacy 双写/端点/鉴权/策略/文案
    /// 逐项锁定（spec.md「序列化契约」+ S01–S04 决议）。
    #[test]
    fn stepfun_openai_preset_matches_spec() {
        let p = presets_for_protocol(ChannelProtocol::OpenAI)
            .into_iter()
            .find(|p| p.provider == ChannelProvider::StepFun)
            .expect("openai stepfun preset");
        assert_eq!(p.region, RegionGroup::Domestic);
        assert_eq!(p.native_base_url, "https://api.stepfun.com/v1");
        assert_eq!(p.legacy_base_url, "https://api.stepfun.com/v1");
        // 旧适配器 type 必须是 openai：不得发明 "stepfun" 厂商名，否则旧二进制
        // 兜底进 CustomAdaptor 丢能力（S02 决策）。
        assert_eq!(p.legacy_type, "openai");
        // 端点仅 Chat Completions：上游无 /v1/embeddings；/v1/responses 仅
        // step-3.7-flash 支持，预设级声明会让其它模型被原生路由击中 400。
        assert_eq!(p.native_endpoints, vec![NativeEndpoint::ChatCompletions]);
        assert_eq!(
            p.default_checked_endpoints,
            vec![NativeEndpoint::ChatCompletions]
        );
        assert!(!p.native_endpoints.contains(&NativeEndpoint::Embeddings));
        assert!(!p.native_endpoints.contains(&NativeEndpoint::Responses));
        assert_eq!(p.auth_scheme, AuthScheme::Bearer);
        assert_eq!(p.model_enum_strategy, ModelEnumStrategy::StaticPlusSync);
        assert_eq!(
            p.endpoint_test_strategy,
            EndpointTestStrategy::ProbeFirstModel
        );
        assert_eq!(p.icon_key, "stepfun");
        assert_eq!(p.display_name, "StepFun（阶跃星辰）");
        assert_eq!(p.description, "阶跃星辰 StepFun 官方 OpenAI 接口。");
        // 预设自身 revision 仍由构造函数统一填常量，不 bump
        assert_eq!(p.preset_revision, PRESET_REVISION);
        // 序列化契约之一（Rust 侧）：as_str 与 serde 字符串恒为 "stepfun"
        assert_eq!(p.provider.as_str(), "stepfun");
        assert_eq!(
            serde_json::to_string(&p.provider).unwrap(),
            "\"stepfun\"".to_string()
        );
    }

    /// StepFun 模型建议：5 条、顺序即预填/探测默认（旗舰在前）、复核日期与溯源。
    #[test]
    fn stepfun_model_suggestions_match_spec() {
        let p = presets_for_protocol(ChannelProtocol::OpenAI)
            .into_iter()
            .find(|p| p.provider == ChannelProvider::StepFun)
            .expect("openai stepfun preset");
        let ids: Vec<&str> = p.model_suggestions.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "step-5-preview",
                "step-3.7-flash",
                "step-3.5-flash",
                "step-3.5-flash-2603",
                "step-1o-turbo-vision",
            ]
        );
        for m in &p.model_suggestions {
            // verified_at = 调研抓取日（2026-09-20），不是全局 PRESET_REVISION
            assert_eq!(m.verified_at, "2026-09-20", "{}", m.id);
            assert_eq!(m.source_url, SRC_STEPFUN, "{}", m.id);
        }
    }

    /// OpenCode 免费车道预设契约：URL/鉴权/端点/策略/文案逐项锁定。
    #[test]
    fn opencode_openai_preset_contract() {
        let p = presets_for_protocol(ChannelProtocol::OpenAI)
            .into_iter()
            .find(|p| p.provider == ChannelProvider::Opencode)
            .expect("openai opencode preset");
        assert_eq!(p.id, "openai:opencode");
        assert_eq!(p.region, RegionGroup::International);
        assert_eq!(p.native_base_url, "https://opencode.ai/inference/v1");
        assert_eq!(p.legacy_base_url, "https://opencode.ai/inference/v1");
        assert_eq!(p.legacy_type, "openai");
        assert_eq!(p.native_endpoints, vec![NativeEndpoint::ChatCompletions]);
        assert_eq!(
            p.default_checked_endpoints,
            vec![NativeEndpoint::ChatCompletions]
        );
        assert_eq!(p.auth_scheme, AuthScheme::Bearer);
        assert_eq!(p.model_enum_strategy, ModelEnumStrategy::StaticPlusSync);
        assert_eq!(
            p.endpoint_test_strategy,
            EndpointTestStrategy::ProbeFirstModel
        );
        assert_eq!(p.icon_key, "opencode");
        assert_eq!(p.provider.as_str(), "opencode");
        assert_eq!(
            serde_json::to_string(&p.provider).unwrap(),
            "\"opencode\"".to_string()
        );
        assert!(!p.model_suggestions.is_empty());
        assert!(p.model_suggestions.iter().all(|m| m.id.ends_with("-free")));
    }

    /// Kilo 免费池预设契约：免 Key（OptionalBearer）、仅 Chat Completions。
    #[test]
    fn kilo_openai_preset_contract() {
        let p = presets_for_protocol(ChannelProtocol::OpenAI)
            .into_iter()
            .find(|p| p.provider == ChannelProvider::Kilo)
            .expect("openai kilo preset");
        assert_eq!(p.id, "openai:kilo");
        assert_eq!(p.region, RegionGroup::International);
        assert_eq!(p.native_base_url, "https://api.kilo.ai/api/gateway");
        assert_eq!(p.legacy_base_url, "https://api.kilo.ai/api/gateway");
        assert_eq!(p.legacy_type, "openai");
        assert_eq!(p.native_endpoints, vec![NativeEndpoint::ChatCompletions]);
        assert_eq!(p.auth_scheme, AuthScheme::OptionalBearer);
        assert_eq!(p.model_enum_strategy, ModelEnumStrategy::StaticPlusSync);
        assert_eq!(p.icon_key, "kilo");
        assert_eq!(p.provider.as_str(), "kilo");
        assert_eq!(
            serde_json::to_string(&p.provider).unwrap(),
            "\"kilo\"".to_string()
        );
        // 免费池以自动路由模型为主：不预置具体的上游私有型号。
        assert_eq!(p.model_suggestions.len(), 1);
        assert_eq!(p.model_suggestions[0].id, "kilo-auto/free");
    }

    /// ModelScope 魔搭免费层预设契约：URL/鉴权/端点/策略/文案逐项锁定。
    #[test]
    fn modelscope_openai_preset_contract() {
        let p = presets_for_protocol(ChannelProtocol::OpenAI)
            .into_iter()
            .find(|p| p.provider == ChannelProvider::ModelScope)
            .expect("openai modelscope preset");
        assert_eq!(p.id, "openai:modelscope");
        assert_eq!(p.region, RegionGroup::Domestic);
        assert_eq!(p.native_base_url, "https://api-inference.modelscope.cn/v1");
        assert_eq!(p.legacy_base_url, "https://api-inference.modelscope.cn/v1");
        assert_eq!(p.legacy_type, "openai");
        assert_eq!(p.native_endpoints, vec![NativeEndpoint::ChatCompletions]);
        assert_eq!(
            p.default_checked_endpoints,
            vec![NativeEndpoint::ChatCompletions]
        );
        assert_eq!(p.auth_scheme, AuthScheme::Bearer);
        assert_eq!(p.model_enum_strategy, ModelEnumStrategy::StaticPlusSync);
        assert_eq!(
            p.endpoint_test_strategy,
            EndpointTestStrategy::ProbeFirstModel
        );
        assert_eq!(p.icon_key, "modelscope");
        assert_eq!(p.provider.as_str(), "modelscope");
        assert_eq!(
            serde_json::to_string(&p.provider).unwrap(),
            "\"modelscope\"".to_string()
        );
        // 凭据为魔搭 SDK Token，signup 直达访问令牌页。
        assert_eq!(
            p.key_signup_url.as_deref(),
            Some("https://modelscope.cn/my/myaccesstoken")
        );
        // 模型 id 为 ModelScope 的 owner/model 复合格式。
        assert!(p
            .model_suggestions
            .iter()
            .all(|m| m.id.contains('/') && m.source_url == SRC_MODELSCOPE));
        assert_eq!(m_verified_at(&p), "2026-10-22");
    }

    /// 新 preset 全部模型建议的复核日期（本批 2026-10-22 核对）。
    fn m_verified_at(p: &ChannelPreset) -> &str {
        let dates: std::collections::HashSet<&str> = p
            .model_suggestions
            .iter()
            .map(|m| m.verified_at.as_str())
            .collect();
        assert_eq!(dates.len(), 1, "{} 模型建议复核日期不一致", p.id);
        dates.into_iter().next().unwrap()
    }

    /// 第二批免费层预设的批量契约：URL/鉴权/端点/策略/signup 逐项锁定。
    #[test]
    fn batch2_free_tier_openai_preset_contracts() {
        let cases: &[(&str, &str, &str, &str)] = &[
            (
                "siliconflow",
                "https://api.siliconflow.cn/v1",
                "https://cloud.siliconflow.cn/account/ak",
                SRC_SILICONFLOW,
            ),
            (
                "huggingface",
                "https://router.huggingface.co/v1",
                "https://huggingface.co/settings/tokens",
                SRC_HUGGINGFACE,
            ),
            (
                "sambanova",
                "https://api.sambanova.ai/v1",
                "https://cloud.sambanova.ai/apis",
                SRC_SAMBANOVA,
            ),
            (
                "cohere",
                "https://api.cohere.ai/compatibility/v1",
                "https://dashboard.cohere.com/api-keys",
                SRC_COHERE,
            ),
        ];
        for (provider_str, base_url, signup_url, src) in cases {
            let p = presets_for_protocol(ChannelProtocol::OpenAI)
                .into_iter()
                .find(|p| p.provider.as_str() == *provider_str)
                .expect("openai preset");
            assert_eq!(p.id, format!("openai:{provider_str}"));
            assert_eq!(p.native_base_url, *base_url);
            assert_eq!(p.legacy_base_url, *base_url);
            assert_eq!(p.legacy_type, "openai");
            assert_eq!(p.native_endpoints, vec![NativeEndpoint::ChatCompletions]);
            assert_eq!(
                p.default_checked_endpoints,
                vec![NativeEndpoint::ChatCompletions]
            );
            assert_eq!(p.auth_scheme, AuthScheme::Bearer);
            assert_eq!(p.model_enum_strategy, ModelEnumStrategy::StaticPlusSync);
            assert_eq!(
                p.endpoint_test_strategy,
                EndpointTestStrategy::ProbeFirstModel
            );
            assert_eq!(p.icon_key, *provider_str);
            assert_eq!(p.key_signup_url.as_deref(), Some(*signup_url));
            assert!(!p.model_suggestions.is_empty());
            assert!(p
                .model_suggestions
                .iter()
                .all(|m| m.source_url == *src && m.verified_at == "2026-10-22"));
            assert_eq!(
                serde_json::to_string(&p.provider).unwrap(),
                format!("\"{provider_str}\"")
            );
        }
        // 地区分组：SiliconFlow 国内，其余国际。
        let region = |s: &str| {
            presets_for_protocol(ChannelProtocol::OpenAI)
                .into_iter()
                .find(|p| p.provider.as_str() == s)
                .unwrap()
                .region
        };
        assert_eq!(region("siliconflow"), RegionGroup::Domestic);
        for s in ["huggingface", "sambanova", "cohere"] {
            assert_eq!(region(s), RegionGroup::International, "{}", s);
        }
    }

    #[test]
    fn ordering_custom_then_international_domestic_local() {
        for protocol in [
            ChannelProtocol::OpenAI,
            ChannelProtocol::Anthropic,
            ChannelProtocol::Ollama,
        ] {
            let presets = presets_for_protocol(protocol);
            let order: Vec<u8> = presets.iter().map(|p| region_order(p.region)).collect();
            let mut sorted = order.clone();
            sorted.sort_unstable();
            assert_eq!(
                order, sorted,
                "preset 顺序必须为 custom→international→domestic→local"
            );
            assert_eq!(order[0], 0);
        }
    }

    #[test]
    fn per_protocol_membership_matches_spec() {
        fn providers(protocol: ChannelProtocol) -> Vec<ChannelProvider> {
            presets_for_protocol(protocol)
                .iter()
                .map(|p| p.provider)
                .collect()
        }
        assert_eq!(
            providers(ChannelProtocol::OpenAI),
            vec![
                ChannelProvider::Custom,
                ChannelProvider::OpenAI,
                ChannelProvider::Google,
                ChannelProvider::Opencode,
                ChannelProvider::Kilo,
                ChannelProvider::Groq,
                ChannelProvider::Cerebras,
                ChannelProvider::Mistral,
                ChannelProvider::NvidiaNim,
                ChannelProvider::OpenRouter,
                ChannelProvider::SambaNova,
                ChannelProvider::HuggingFace,
                ChannelProvider::Cohere,
                ChannelProvider::DeepSeek,
                ChannelProvider::Qwen,
                ChannelProvider::Zhipu,
                ChannelProvider::Doubao,
                ChannelProvider::Moonshot,
                ChannelProvider::StepFun,
                ChannelProvider::ModelScope,
                ChannelProvider::SiliconFlow,
                ChannelProvider::Ollama,
            ]
        );
        // 免费层 API Key 提供商必须带「获取 Key」跳转链接，且其余预设不带。
        // Google/智谱为「部分免费」（Flash/免费层系列），同样提供 signup 引导。
        for preset in all_channel_presets() {
            let has_signup = preset.key_signup_url.is_some();
            let is_free_tier = matches!(
                preset.provider,
                ChannelProvider::Google
                    | ChannelProvider::Zhipu
                    | ChannelProvider::ModelScope
                    | ChannelProvider::SiliconFlow
                    | ChannelProvider::HuggingFace
                    | ChannelProvider::SambaNova
                    | ChannelProvider::Cohere
                    | ChannelProvider::Groq
                    | ChannelProvider::Cerebras
                    | ChannelProvider::Mistral
                    | ChannelProvider::NvidiaNim
                    | ChannelProvider::OpenRouter
            );
            assert_eq!(has_signup, is_free_tier, "{}", preset.id);
            if is_free_tier {
                assert!(
                    preset
                        .key_signup_url
                        .as_deref()
                        .unwrap()
                        .starts_with("https://"),
                    "{} signup url must be https",
                    preset.id
                );
            }
        }
        assert_eq!(
            providers(ChannelProtocol::Anthropic),
            vec![
                ChannelProvider::Custom,
                ChannelProvider::Anthropic,
                ChannelProvider::DeepSeek,
                ChannelProvider::Qwen,
                ChannelProvider::Zhipu,
                ChannelProvider::DoubaoCodingPlan,
                ChannelProvider::Ollama,
            ]
        );
        assert_eq!(
            providers(ChannelProtocol::Ollama),
            vec![ChannelProvider::Custom, ChannelProvider::Ollama]
        );
    }

    #[test]
    fn non_custom_presets_have_full_fields() {
        for p in all_channel_presets()
            .into_iter()
            .filter(|p| p.provider != ChannelProvider::Custom)
        {
            assert!(!p.native_base_url.is_empty(), "{}", p.id);
            assert!(!p.legacy_base_url.is_empty(), "{}", p.id);
            assert!(!p.legacy_type.is_empty(), "{}", p.id);
            assert!(!p.native_endpoints.is_empty(), "{}", p.id);
            assert!(!p.default_checked_endpoints.is_empty(), "{}", p.id);
            assert!(!p.display_name.is_empty(), "{}", p.id);
        }
    }

    #[test]
    fn every_model_suggestion_has_verified_at_and_source() {
        for p in all_channel_presets() {
            for m in &p.model_suggestions {
                assert!(!m.verified_at.is_empty(), "{}", m.id);
                assert!(!m.source_url.is_empty(), "{}", m.id);
                // 禁止把浮动的 latest/preview 别名作为默认生产模型
                // （如 "gpt-latest" / "latest"）；官方已命名且带版本的含 preview 型号
                // （如 kimi-k2-turbo-preview，见设计 4.2 基线）不在此列。
                let lower = m.id.to_ascii_lowercase();
                assert!(
                    !(lower == "latest" || lower == "preview" || lower.ends_with("-latest")),
                    "不得把 latest/preview 的浮动别名作为默认生产模型: {}",
                    m.id
                );
            }
        }
    }

    #[test]
    fn anthropic_zhipu_native_root_is_bigmodel() {
        let p = presets_for_protocol(ChannelProtocol::Anthropic)
            .into_iter()
            .find(|p| p.provider == ChannelProvider::Zhipu)
            .unwrap();
        assert_eq!(
            p.native_base_url,
            "https://open.bigmodel.cn/api/anthropic/v1"
        );
        assert_eq!(
            p.legacy_base_url,
            "https://open.bigmodel.cn/api/anthropic/v1"
        );
    }

    #[test]
    fn doubao_coding_plan_display_name_fixed() {
        let p = presets_for_protocol(ChannelProtocol::Anthropic)
            .into_iter()
            .find(|p| p.provider == ChannelProvider::DoubaoCodingPlan)
            .unwrap();
        assert_eq!(p.display_name, "字节豆包 (Coding Plan)");
        assert_eq!(p.description, "字节豆包官方 Anthropic 接口。");
        assert_eq!(
            p.native_base_url,
            "https://ark.cn-beijing.volces.com/api/coding/v1"
        );
    }

    #[test]
    fn anthropic_icon_and_endpoints_matched_spec() {
        let p = presets_for_protocol(ChannelProtocol::Anthropic)
            .into_iter()
            .find(|p| p.provider == ChannelProvider::Anthropic)
            .unwrap();
        assert_eq!(p.icon_key, "claudecode");
        assert_eq!(p.description, "Anthropic Claude Code 官方 Messages API。");
        assert!(!p.native_endpoints.contains(&NativeEndpoint::CountTokens));
        assert_eq!(p.native_endpoints, vec![NativeEndpoint::Messages]);
        assert_eq!(p.default_checked_endpoints, vec![NativeEndpoint::Messages]);
    }

    #[test]
    fn moonshot_and_doubao_openai_names_match_spec() {
        let moonshot = presets_for_protocol(ChannelProtocol::OpenAI)
            .into_iter()
            .find(|p| p.provider == ChannelProvider::Moonshot)
            .unwrap();
        assert_eq!(moonshot.display_name, "Moonshot(Kimi)");
        assert_eq!(moonshot.description, "Moonshot Kimi OpenAI 接口。");

        let doubao = presets_for_protocol(ChannelProtocol::OpenAI)
            .into_iter()
            .find(|p| p.provider == ChannelProvider::Doubao)
            .unwrap();
        assert_eq!(doubao.display_name, "字节豆包 (Coding Plan)");
        assert_eq!(doubao.description, "字节豆包官方 OpenAI 接口。");
    }

    #[test]
    fn no_compatibility_jargon_in_any_preset() {
        for p in all_channel_presets() {
            for bad in ["兼容面", "兼容层", "兼容网关", "兼容根地址"] {
                assert!(!p.description.contains(bad), "{}: {}", p.id, p.description);
            }
        }
    }

    #[test]
    fn ollama_roots() {
        let p = presets_for_protocol(ChannelProtocol::OpenAI)
            .into_iter()
            .find(|p| p.provider == ChannelProvider::Ollama)
            .unwrap();
        assert_eq!(p.native_base_url, "http://localhost:11434/v1");
        assert_eq!(p.legacy_base_url, "http://localhost:11434/v1");
        assert_eq!(p.legacy_type, "openai");

        let p = presets_for_protocol(ChannelProtocol::Ollama)
            .into_iter()
            .find(|p| p.provider == ChannelProvider::Ollama)
            .unwrap();
        assert_eq!(p.native_base_url, "http://localhost:11434");
        assert_eq!(p.legacy_base_url, "http://localhost:11434/v1");
        assert_eq!(p.legacy_type, "openai");
    }

    #[test]
    fn url_fixtures_match_spec_table() {
        // T01 规格「URL 兼容 fixture 契约」硬性样例，逐条断言
        let f = |protocol: ChannelProtocol, provider: ChannelProvider| {
            all_channel_presets()
                .into_iter()
                .find(|p| p.protocol == protocol && p.provider == provider)
                .unwrap()
        };
        // OpenAI / OpenAI
        let p = f(ChannelProtocol::OpenAI, ChannelProvider::OpenAI);
        assert_eq!(p.native_base_url, "https://api.openai.com/v1");
        assert_eq!(
            join(&p.native_base_url, "chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            join(&p.native_base_url, "responses"),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(p.legacy_type, "openai");
        assert_eq!(
            join(&p.legacy_base_url, "chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        // OpenAI / StepFun（阶跃星辰）
        let p = f(ChannelProtocol::OpenAI, ChannelProvider::StepFun);
        assert_eq!(p.native_base_url, "https://api.stepfun.com/v1");
        assert_eq!(
            join(&p.native_base_url, "chat/completions"),
            "https://api.stepfun.com/v1/chat/completions"
        );
        assert_eq!(p.legacy_type, "openai");
        assert_eq!(p.legacy_base_url, "https://api.stepfun.com/v1");
        assert_eq!(
            join(&p.legacy_base_url, "chat/completions"),
            "https://api.stepfun.com/v1/chat/completions"
        );
        // Anthropic / Anthropic
        let p = f(ChannelProtocol::Anthropic, ChannelProvider::Anthropic);
        assert_eq!(p.native_base_url, "https://api.anthropic.com/v1");
        assert_eq!(
            join(&p.native_base_url, "messages"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(p.legacy_type, "claude");
        assert_eq!(p.legacy_base_url, "https://api.anthropic.com/v1");
        assert_eq!(
            join(&p.legacy_base_url, "messages"),
            "https://api.anthropic.com/v1/messages"
        );
        // Anthropic / DeepSeek
        let p = f(ChannelProtocol::Anthropic, ChannelProvider::DeepSeek);
        assert_eq!(p.native_base_url, "https://api.deepseek.com/anthropic/v1");
        assert_eq!(
            join(&p.native_base_url, "messages"),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
        assert_eq!(p.legacy_type, "claude");
        assert_eq!(p.legacy_base_url, "https://api.deepseek.com/anthropic/v1");
        assert_eq!(
            join(&p.legacy_base_url, "messages"),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
        // Anthropic / 智谱
        let p = f(ChannelProtocol::Anthropic, ChannelProvider::Zhipu);
        assert_eq!(
            p.native_base_url,
            "https://open.bigmodel.cn/api/anthropic/v1"
        );
        assert_eq!(
            join(&p.native_base_url, "messages"),
            "https://open.bigmodel.cn/api/anthropic/v1/messages"
        );
        assert_eq!(p.legacy_type, "claude");
        assert_eq!(
            p.legacy_base_url,
            "https://open.bigmodel.cn/api/anthropic/v1"
        );
        assert_eq!(
            join(&p.legacy_base_url, "messages"),
            "https://open.bigmodel.cn/api/anthropic/v1/messages"
        );
        // Anthropic / 豆包 Coding Plan
        let p = f(
            ChannelProtocol::Anthropic,
            ChannelProvider::DoubaoCodingPlan,
        );
        assert_eq!(
            p.native_base_url,
            "https://ark.cn-beijing.volces.com/api/coding/v1"
        );
        assert_eq!(
            join(&p.native_base_url, "messages"),
            "https://ark.cn-beijing.volces.com/api/coding/v1/messages"
        );
        assert_eq!(p.legacy_type, "claude");
        assert_eq!(
            p.legacy_base_url,
            "https://ark.cn-beijing.volces.com/api/coding/v1"
        );
        assert_eq!(
            join(&p.legacy_base_url, "messages"),
            "https://ark.cn-beijing.volces.com/api/coding/v1/messages"
        );
        // Anthropic / Ollama
        let p = f(ChannelProtocol::Anthropic, ChannelProvider::Ollama);
        assert_eq!(p.native_base_url, "http://localhost:11434/v1");
        assert_eq!(
            join(&p.native_base_url, "messages"),
            "http://localhost:11434/v1/messages"
        );
        assert_eq!(p.legacy_type, "claude");
        assert_eq!(p.legacy_base_url, "http://localhost:11434/v1");
        assert_eq!(
            join(&p.legacy_base_url, "messages"),
            "http://localhost:11434/v1/messages"
        );
        // Ollama / Ollama
        let p = f(ChannelProtocol::Ollama, ChannelProvider::Ollama);
        assert_eq!(p.native_base_url, "http://localhost:11434");
        assert_eq!(
            join(&p.native_base_url, "api/chat"),
            "http://localhost:11434/api/chat"
        );
        assert_eq!(p.legacy_type, "openai");
        assert_eq!(p.legacy_base_url, "http://localhost:11434/v1");
        assert_eq!(
            join(&p.legacy_base_url, "chat/completions"),
            "http://localhost:11434/v1/chat/completions"
        );
    }

    fn join(base: &str, path: &str) -> String {
        format!("{}/{}", base.trim_end_matches('/'), path)
    }

    #[test]
    fn qwen_anthropic_final_path_falls_into_fixture() {
        // 通义 Anthropic 的最终路径必须按其官方文档落入 fixture（规格第 59 行）
        let p = presets_for_protocol(ChannelProtocol::Anthropic)
            .into_iter()
            .find(|p| p.provider == ChannelProvider::Qwen)
            .unwrap();
        assert_eq!(
            p.native_base_url,
            "https://dashscope.aliyuncs.com/apps/anthropic/v1"
        );
        assert_eq!(
            join(&p.native_base_url, "messages"),
            "https://dashscope.aliyuncs.com/apps/anthropic/v1/messages"
        );
        // 模型建议必须带 verified_at/source_url
        assert!(p.model_suggestions.iter().all(|m| !m.source_url.is_empty()));
    }
}
