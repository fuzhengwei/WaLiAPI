# 免费渠道对接计划（2026-10-22）

> **进度（2026-10-22）：第一批、第二批均已完成** ——
> 第一批：Google AI Studio / 智谱 GLM-Flash 免费层标注（signup + 描述）、
> ModelScope 魔搭、GitHub Models 两个新 preset。
> 第二批：SiliconFlow 硅基流动、Hugging Face、SambaNova、Cohere 四个新 preset。
> 全部落地于 `channel_presets.rs` + `types/index.ts` + `constants.ts`，含契约测试，
> 全量 cargo test 1568 通过、tsc 通过、clippy 无新增告警。
> 实施备注：GitHub Models 走标准 Bearer Key（GitHub PAT，models:read），
> 不需要 OAuth auth 渠道。第三批（Together/UnoRouter/LongCat/Cloudflare）待后续实施。
> 模型静态建议核对日均为 2026-10-22，免费清单动态变化，可经「同步上游模型」覆盖。

> 背景：WaLiAPI 已内置 7 个免费渠道 preset（OpenCode 免费车道、Kilo 免费池、Groq、
> Cerebras、Mistral、NVIDIA NIM、OpenRouter :free）。本文档列出下一批待对接的
> 免费模型渠道、优先级与每家的具体接入参数，供实施时逐项核对。

## 一、待接入清单（按优先级分三批）

### 第一批：改动小、价值高（建议尽快接入）

| # | 渠道 | 类型 | 免费方式 | 地区分组 |
|:--|:---|:---|:---|:---|
| 1 | Google AI Studio | 改造现有 preset | Gemini Flash 系免费层，15 RPM / 100 万 token/天，免信用卡 | International |
| 2 | 智谱 GLM-Flash | 改造现有 preset | GLM Flash 系列免费（现有 preset 已含 glm-4.7-flash） | Domestic |
| 3 | ModelScope 魔搭 | 新增 preset | 注册（需实名）即送每日 2000 次调用，单模型 ≤500 次/天 | Domestic |
| 4 | GitHub Models | 新增 preset | GitHub 账号 + PAT（models:read）即可用，个人账户免费层 | International |

### 第二批：标准免费层补充

| # | 渠道 | 免费方式 | 地区分组 |
|:--|:---|:---|:---|
| 5 | SiliconFlow 硅基流动 | 注册送额度 + 多个永久免费小模型 | Domestic |
| 6 | Hugging Face | 免费账户每月送 Inference Provider 额度，1000 请求/天（serverless） | International |
| 7 | SambaNova | 免费层 20 RPM / 200 次/天，免信用卡 | International |
| 8 | Cohere | Trial key 每月 1000 次调用（仅评估用） | International |

### 第三批：可选（接入体验或稳定性存疑，缓做）

| # | 渠道 | 备注 |
|:--|:---|:---|
| 9 | Together AI | 仅部分精选模型 $0，无普遍免费层 |
| 10 | UnoRouter | 56 个 :free 路由，可作 OpenRouter 分流备份 |
| 11 | LongCat（美团） | 注册 + KYC 一次性送 1000 万 token，base_url 待官方文档核对 |
| 12 | Cloudflare Workers AI | 每天 1 万 Neurons，但 URL 需拼 account_id，配置体验差 |

**明确不接入**：Free.ai、ZeroLimitAI、Chat Oripe、Requesty 等小众「终身免费」网关
（稳定性与隐私合规风险不明）。

## 二、各渠道接入参数（2026-10-22 核对）

### 1. Google AI Studio（改造）

- 现有 preset：`openai:google`（`https://generativelanguage.googleapis.com/v1beta/openai`）
- 改动：补 `.signup("https://aistudio.google.com/apikey")`，描述注明
  「免费层 15 RPM / 100 万 token/天，免费层内容可能被 Google 用于改进产品」。
- 注意：免费层声明要写进 description，与 Groq preset 文案风格一致。

### 2. 智谱 GLM-Flash（改造）

> 核对说明（2026-10-22）：智谱**不是全平台免费**，仅 Flash 系列永久免费，
> 旗舰（glm-4.7 等）正常计费。免费清单（官方 docs.bigmodel.cn「免费模型」目录）：
>
> | 模型 | 上下文 | 速率限制 |
> |:---|:---|:---|
> | glm-4.7-flash | 200K | **仅 1 条并发请求** |
> | glm-4.6v-flash | 128K（文本+图片） | 仅 1 条并发请求 |
> | glm-4.5-flash | 128K | 仅 1 条并发请求（2026-01-30 起下线，请求自动路由至 glm-4.7-flash） |
>
> ⚠️ 关键限制：免费层**仅支持 1 条并发请求**——多用户同时经 WaLiAPI 转发时
> 会排队/失败，只适合单用户串行场景；调度层不要把免费模型放进高并发优先级组。

- 现有 preset：`openai:zhipu` / `anthropic:zhipu`
- 改动：openai 侧补 `.signup("https://open.bigmodel.cn/usercenter/apikeys")`
  （URL 以官方控制台实际路径核对为准），描述改为：
  「GLM Flash 系列永久免费（200K 上下文），但免费层仅 1 条并发请求，
  多用户并发会排队；旗舰模型计费。」
- 模型建议核对：现有 preset 已含 glm-4.7-flash（免费）；确认 glm-4.6v-flash
  是否值得加入（多模态免费）。

### 3. ModelScope 魔搭（新增）

```text
preset id        : openai:modelscope
provider         : ChannelProvider::ModelScope（serde rename "modelscope"）
native_base_url  : https://api-inference.modelscope.cn/v1
legacy_base_url  : 同上
legacy_type      : "openai"
auth             : Bearer（ModelScope SDK Token）
endpoints        : [ChatCompletions]，默认勾选同
模型建议（verified_at=2026-10-22）:
  - Qwen/Qwen3.5-35B-A3B
  - Qwen/Qwen3.5-27B
  - deepseek-ai/DeepSeek-V3（以官方「支持 API Inference 的模型列表」核对为准）
signup           : https://modelscope.cn/my/myaccesstoken
description 要点 : 每日 2000 次免费调用（单模型 ≤500 次/天，动态限速）；
                   需实名认证，仅限中国手机号注册；请勿用于高并发/SLA 场景。
source_url       : https://modelscope.cn/headlines/article/795（官方公告）
```

### 4. GitHub Models（新增）

```text
preset id        : openai:github_models
provider         : ChannelProvider::GithubModels（serde rename "github_models"）
native_base_url  : https://models.github.ai/inference
legacy_base_url  : 同上
legacy_type      : "openai"
auth             : Bearer（GitHub PAT，需 models:read 权限）
endpoints        : [ChatCompletions]
模型建议（verified_at=2026-10-22）:
  - openai/gpt-4.1
  - openai/gpt-4o-mini
  - deepseek/DeepSeek-R1（模型 id 以 https://models.github.ai/catalog 核对为准）
signup           : https://github.com/settings/tokens
description 要点 : GitHub 账号即用，免费层限速、上下文受限；PAT 需勾选 models:read；
                   模型 id 为 publisher/model 复合格式。
source_url       : https://docs.github.com/en/github-models（官方文档）
```

### 5. SiliconFlow 硅基流动（新增）

```text
preset id        : openai:siliconflow
native_base_url  : https://api.siliconflow.cn/v1
auth             : Bearer
endpoints        : [ChatCompletions]
模型建议         : 免费档小模型（Qwen 小尺寸 / GLM 小尺寸等），
                   以 https://docs.siliconflow.cn 模型价格页核对（标注 free 档）。
signup           : https://cloud.siliconflow.cn/account/ak
description 要点 : 注册送体验额度，部分小模型永久免费；国内访问友好。
```

### 6. Hugging Face（新增）

```text
preset id        : openai:huggingface
native_base_url  : https://router.huggingface.co/v1
auth             : Bearer
endpoints        : [ChatCompletions]
模型建议         : router 上的免费 Inference Provider 模型，以官方核对。
signup           : https://huggingface.co/settings/tokens
description 要点 : 免费账户每月赠送 Inference Provider 额度；token 需
                   "Make calls to Inference Providers" 权限。
```

### 7. SambaNova（新增）

```text
preset id        : openai:sambanova
native_base_url  : https://api.sambanova.ai/v1
auth             : Bearer
endpoints        : [ChatCompletions]
signup           : https://cloud.sambanova.ai/apis
description 要点 : 免费层 20 RPM / 200 次/天，免信用卡。
```

### 8. Cohere（新增）

```text
preset id        : openai:cohere
native_base_url  : https://api.cohere.ai/compatibility/v1   ← OpenAI 兼容端点
auth             : Bearer
endpoints        : [ChatCompletions]
signup           : https://dashboard.cohere.com/api-keys
description 要点 : Trial key 每月 1000 次调用，仅评估用途，非生产可用。
```

## 三、统一对接步骤（每个新增 preset 的标准改动点）

以 ModelScope 为例，共 **4 个文件、无数据库迁移**（preset registry 是纯内存只读数据，
不入库；已保存渠道不受 preset 更新影响）：

1. **`src-tauri/src/channel_presets.rs`**
   - `ChannelProvider` 枚举加 variant：`#[serde(rename = "modelscope")] ModelScope,`
   - `as_str()` 补对应分支
   - 新增 `const SRC_MODELSCOPE: &str = "...";`（官方模型/公告页）
   - `openai_presets()` 末尾（Ollama 之前）追加 `preset(...)`，免费层链式 `.signup(...)`
   - 统一参数：`AuthScheme::Bearer` + `[NativeEndpoint::ChatCompletions]` +
     `ModelEnumStrategy::StaticPlusSync` + `EndpointTestStrategy::ProbeFirstModel`
   - `PRESET_REVISION` 顺延为当日日期
2. **`src/types/index.ts`**：provider union 加 `"modelscope"`
3. **`src/lib/constants.ts`**：displayName 映射 + icon SVG（手写简化标识，参考
   nvidia_nim 的注释风格，勿复刻官方 logo）
4. **契约测试**：`channel_presets.rs` 内 `is_free_tier` 的 `matches!` 列表补上
   新 provider（保证「免费层必带 signup URL」断言覆盖）
5. **验证**：`cd src-tauri && cargo test`（全量）+ `pnpm build`（tsc 严格检查）

注意：不需要动 `endpoint_executor`（免 Key 的 `OptionalBearer` 判定仅 OpenCode/Kilo
需要，本批全部是 Bearer）。静态模型建议**禁止 `-latest` 浮动别名**，必须带
`verified_at` + 官方 `source_url`（基线测试强制）。

## 四、逐项核对清单（实施前逐条打勾）

- [ ] 每家 base_url 用 curl 实测 `POST {base}/chat/completions` 通
- [ ] 模型 id 从 `GET {base}/models` 或官方目录页核对（记录核对日期）
- [ ] signup URL 在浏览器实测可达且直达 Key 页面
- [ ] description 写明免费额度、限流（429）、隐私提示
- [ ] ModelScope/智谱归 Domestic，其余归 International
- [ ] `cargo fmt` + `cargo clippy` 零告警
