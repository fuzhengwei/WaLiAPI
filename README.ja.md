<div align="center">

<img src="public/logo.png" width="104" alt="WaLiAPI"/>

# WaLiAPI

### ローカル完結型 LLM API ゲートウェイ · マルチプロトコル · ナレッジベース RAG · MCP ツール

**OpenAI、Claude、DeepSeek、Gemini などの上流モデルを、ひとつのローカルゲートウェイに統合。**<br>
**ひとつのキー、ひとつのプロトコル、完全な監査ログで、Claude Code・Codex CLI・Gemini CLI・WaLiCode などの AI コーディングツールに接続。**

[![Version](https://img.shields.io/badge/version-0.4.5-blue.svg)](./src-tauri/tauri.conf.json)
[![License](https://img.shields.io/badge/license-MIT-green.svg)](./LICENSE)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-lightgrey.svg)](#はじめに)
[![Built with Tauri](https://img.shields.io/badge/built%20with-Tauri%202-orange.svg)](https://tauri.app)

**[ダウンロード](#はじめに)** &nbsp;·&nbsp; **[主な機能](#主な機能)** &nbsp;·&nbsp; **[仕組み](#仕組み)** &nbsp;·&nbsp; **[Docker デプロイ](#方法-2docker-デプロイチーム・サーバー向け)**

[简体中文](README.md) &nbsp;|&nbsp; [English](README.en.md) &nbsp;|&nbsp; **日本語**

</div>

> 🎁 **無料チャネルをすぐに使える**：初回起動時にキー不要の **OpenCode 無料レーン** と **Kilo 無料プール** の 2 チャネルを自動登録。設定ゼロでそのまま呼び出せます。さらに **Groq / Cerebras / Mistral / NVIDIA NIM / OpenRouter** の 5 つの無料枠チャネルプリセットも用意されており、登録してキーを取得するだけで使えます。詳しくは[無料チャネル](#-無料チャネルをすぐに使える)へ。

---

## 🧭 30 秒でわかる WaLiAPI

次の図は WaLiAPI の全体的な動作フローです。左側がリクエストを送る AI アプリ、中央が WaLiAPI ゲートウェイの処理チェーン（内蔵の RAG ナレッジベースと LLM-Wiki を含む）、右側が 3 種類の上流チャネル——**API チャネル**（キー接続）、**無料チャネル**（キー不要 / 無料枠）、**アカウントチャネル**（OAuth ログインの Auth アカウント）です。

```mermaid
flowchart LR
    subgraph Clients["💬 下流 AI アプリ"]
        direction TB
        A1["Claude Code"]
        A2["Codex CLI"]
        A3["Gemini CLI"]
        A4["WaLiCode"]
        A5["OpenClaw"]
        A6["ChatBox / NextChat"]
    end

    subgraph Gateway["🛡️ WaLiAPI ローカルゲートウェイ  127.0.0.1:8777"]
        direction TB
        B["① マルチプロトコル受付<br/>OpenAI Chat · Responses · Anthropic"]
        C["② セキュリティ監査<br/>機密情報スキャン · マスキング / ブロック"]
        D["③ スマートルーティング<br/>優先度 + 重み · マルチキー · フェイルオーバー"]
        E["④ プロトコル変換 · 上流アダプタ<br/>OpenAI · Claude · Gemini · DeepSeek · カスタム"]

        B --> C --> D --> E

        subgraph Knowledge["📚 内蔵ナレッジサービス（同じルーティングと監査を利用）"]
            direction LR
            R["RAG ナレッジベース<br/>解析 → チャンク → ベクトル化<br/>HNSW + FTS5 ハイブリッド検索"]
            W["LLM-Wiki<br/>Markdown ページ · ナレッジグラフ<br/>MCP ツールとして公開"]
        end
    end

    subgraph Channels["☁️ 上流チャネル（3 種類）"]
        direction TB
        F1["🔗 API チャネル（キー接続）<br/>OpenAI · Claude · DeepSeek · Gemini<br/>智谱 · 通義 · Moonshot · 豆包 · StepFun"]
        F2["🎁 無料チャネル<br/>OpenCode レーン / Kilo プール（キー不要 · 初回起動時に登録）<br/>Groq · Cerebras · Mistral · NVIDIA NIM · OpenRouter"]
        F3["👤 アカウントチャネル（OAuth ログイン）<br/>Codex · Kimi · Antigravity<br/>Qoder · WorkBuddy · Grok"]
    end

    Clients -- "統一キー<br/>sk-waliapi-*" --> B
    E -- "HTTPS 転送" --> Channels
    Clients -. "MCP /mcp · REST /api/kb<br/>RAG 検索 · Wiki Q&A" .-> Knowledge
    Knowledge -. "Embedding / 回答モデル<br/>ゲートウェイルーティングを利用" .-> D

    style Clients fill:#f0f7ff,stroke:#4a90d9,color:#1a3a5c
    style Gateway fill:#f6f0ff,stroke:#7c5cd6,color:#3a2a5c
    style Knowledge fill:#fffbea,stroke:#d9a514,color:#5c4a1a
    style Channels fill:#f0fdf4,stroke:#34a853,color:#1a4a2c
```

**ひとことで**: すべての下流 AI アプリは WaLiAPI に向ければ OK —— プロトコル変換・セキュリティ監査・負荷分散・監査ログを担い、設定した API チャネル・無料チャネル・アカウントチャネルへ転送します。チャネルのクォータ枯渇や障害時は自動で次の候補へフェイルオーバーし、AI ツール側は一切感知しません。内蔵の RAG ナレッジベースと LLM-Wiki により、エージェントは MCP / REST でプライベートナレッジを直接検索でき、そのモデル呼び出しも同じチャネルルーティングと監査を通ります。

## 🎁 無料チャネルをすぐに使える

費用ゼロで始められます。WaLiAPI には 2 種類の無料チャネルがあります。

**1. キー不要の内蔵チャネル（設定ゼロ）**

初回起動時に「チャネル管理」へ自動登録されます。登録もキーも不要：

| チャネル | 説明 |
|:---|:---|
| **OpenCode 無料レーン** | 公開プール型クレデンシャル。ゲートウェイが上流 v2 フィンガープリントゲート対策（セッション鋳造・ツール宣言）を自動整形 |
| **Kilo 無料プール** | 完全キー不要でそのまま呼び出し |

シードチャネルは 1 度だけ追加され、削除しても再登録されません。通常のチャネルと同じく編集・有効化・スケジューリングできます。

**2. 無料枠チャネルプリセット（登録してキー取得）**

「新規チャネル」で以下のプリセットを選び、画面の「API Key を取得」リンクから登録するだけ。モデル一覧は「上流モデル同期」で更新できます：

| プリセット | 無料枠 |
|:---|:---|
| **Groq** | 登録でキー取得。高速 LPU 推理、レート・日次制限あり |
| **Cerebras** | 登録でクレジット進呈。ウェハレベルの高速推論 |
| **Mistral** | La Plateforme 無料実験枠。日次 / 月次制限あり |
| **NVIDIA NIM** | 登録で 1000 クレジット。DeepSeek / Kimi / GLM / MiniMax などを集約 |
| **OpenRouter** | `:free` 接尾辞モデルは無料。`openrouter/free` で現在使える無料プールへ自動ルーティング |

> 💡 無料枠にはそれぞれレート・クォータ制限があり、超過時は上流が 429 を返します。ゲートウェイは自動で次の利用可能チャネルへフェイルオーバーします。また「Auth アカウント」で OAuth ログインしたアカウントチャネル（Antigravity・Codex・Kimi・Qoder など）でも、アカウント自身のクォータを利用できます。

## 🌟 WaLiAPI が選ばれる理由

- **🔌 ひとつの入口、複数のプロトコル** —— 下流は OpenAI Chat Completions・OpenAI Responses・Anthropic Messages の 3 プロトコルで接続でき、ゲートウェイ内で双方向変換。上流チャネルは何も変わりません。Claude Code で DeepSeek を、Codex で Kimi を動かす —— すべて自由に組み合わせられます。
- **⚖️ クォータを無駄にしない** —— チャネルは優先度 + 重みで負荷分散し、1 チャネルあたり複数 API Key をキー単位の重み付きで運用。クォータ枯渇や障害時は自動フェイルオーバー。残量もひと目で確認できます（例：Antigravity のモデル別残クォータとリセット時刻）。
- **🛡️ AI が何を話しているか一目瞭然** —— 全リクエスト/レスポンスを完全に記録し、キーワード・キー・チャネル・モデル・Trace ID で検索可能。内蔵の 25+ ルールで機密情報リーク・パス露出・Unicode ステガノグラフィを検出し、監査のみ / 警告 / マスキング / ブロックの 4 ポリシーに対応。
- **📚 ゲートウェイにとどまらない** —— ナレッジベース RAG エンジン（tree-sitter によるシンボル認識チャンキング、HNSW + FTS5 ハイブリッド検索）、Wiki ナレッジエンジン、29 ツールの MCP Server を内蔵し、AI エージェントがプライベートナレッジを直接検索できます。
- **🏠 データはローカルに** —— デスクトップ版はローカル完結・SQLite 保存。Docker / ヘッドレスサーバー版でチーム・プライベートデプロイにも対応。

- GitHub: [https://github.com/fuzhengwei/WaLiAPI](https://github.com/fuzhengwei/WaLiAPI)
- Gitcode: [https://gitcode.com/fuzhengwei/WaLiAPI](https://gitcode.com/fuzhengwei/WaLiAPI)

---

## 📑 目次

- [はじめに](#はじめに)
- [無料チャネルをすぐに使える](#-無料チャネルをすぐに使える)
- [仕組み](#仕組み)
- [主な機能](#主な機能)
- [マルチプロトコル接続](#マルチプロトコル接続)
- [技術スタック](#技術スタック)
- [セキュリティ境界と脅威モデル](#セキュリティ境界と脅威モデル)
- [バージョン履歴](#バージョン履歴)
- [コントリビューター](#コントリビューター)
- [ライセンス](#ライセンス)

---

## 🚀 はじめに

WaLiAPI には 2 つのデプロイ方式があります：**デスクトップアプリ**（個人利用向け）と **Docker デプロイ**（チーム・クラウドサーバー向け）。

### 方法 1：デスクトップアプリ（個人利用・推奨）

#### 1. ダウンロード

GitHub Releases から各プラットフォームのインストーラをダウンロード：

- GitHub: [https://github.com/fuzhengwei/WaLiAPI/releases](https://github.com/fuzhengwei/WaLiAPI/releases)

| プラットフォーム | 形式 | アーキテクチャ |
|:---|:---|:---|
| macOS | `.dmg` | ARM64 (Apple Silicon) |
| Windows | `.msi` / `.exe` | x64 |
| Linux | `.deb` / `.AppImage` | x64 |

#### 2. チャネルを追加

WaLiAPI を開く → **チャネル管理** → **新規チャネル** → 名称・Base URL・API Key・対応モデルを入力 → 保存。`curl` コマンドを貼り付けて自動入力したり、**Auth アカウント**から Codex・Kimi・Grok・Antigravity・WorkBuddy の OAuth ログインも可能です。

#### 3. キーを作成

**API キー** → **新規作成** → `sk-waliapi-*` 形式のローカルアクセストークンを生成。

#### 4. 下流アプリを接続

ChatBox / NextChat / OpenAI SDK / WaLiCode などで設定：

- **Base URL**: `http://127.0.0.1:8777/v1`
- **API Key**: 作成した `sk-waliapi-...`

#### 5. ワンクリック設定（任意）

**アプリ設定**ページでインストール済みの AI コーディングツールを選び、ゲートウェイアドレスとキーをワンクリックで書き込み —— Claude Code・Codex CLI・Gemini CLI・Claude Desktop・OpenCode・OpenClaw・Hermes Agent・WaLiCode の 8 ツールに対応。

### 方法 2：Docker デプロイ（チーム・サーバー向け）

WaLIAPI は Linux サーバーでの長期運行向け Docker イメージを提供しています。マルチステージビルド（Node/pnpm でフロントエンド、Rust でサーバーバイナリをコンパイル）、非 root 実行、SQLite は `/data` に永続化。

```bash
# イメージを取得
docker pull fuzhengwei/waliapi:latest

# 管理トークンを生成
export WALIAPI_ADMIN_TOKEN="$(openssl rand -hex 32)"
export WALIAPI_MCP_TOKEN="$(openssl rand -hex 32)"

# コンテナを起動
docker run -d --name waliapi \
  -p 127.0.0.1:8777:8777 \
  -v waliapi-data:/data \
  -e WALIAPI_ADMIN_TOKEN \
  -e WALIAPI_MCP_TOKEN \
  fuzhengwei/waliapi:latest

# 動作確認
curl http://127.0.0.1:8777/health
```

ソースからビルドする場合（Docker Compose）：

```bash
git clone https://github.com/fuzhengwei/WaLiAPI.git
cd WaLiAPI
export WALIAPI_ADMIN_TOKEN="$(openssl rand -hex 32)"
export WALIAPI_MCP_TOKEN="$(openssl rand -hex 32)"
docker compose up -d --build
curl http://127.0.0.1:8777/health
```

| 環境変数 | 説明 | デフォルト |
|:---|:---|:---|
| `WALIAPI_SERVER_HOST` | リッスンアドレス | `0.0.0.0` (Docker) / `127.0.0.1` (デスクトップ) |
| `WALIAPI_SERVER_PORT` | リッスンポート | `8777` |
| `WALIAPI_DATA_DIR` | データディレクトリ（SQLite + KB インデックス） | `/data` |
| `WALIAPI_ADMIN_TOKEN` | KB/Wiki REST 管理用トークン（≥32 文字、Bearer） | — |
| `WALIAPI_MCP_TOKEN` | MCP エンドポイント用トークン（≥32 文字、管理用と別の値） | — |
| `WALIAPI_PUBLIC_URL` | 公開 URL（クライアント設定生成に使用） | — |

Compose はデフォルトで `127.0.0.1` のみにバインドします。公開する場合は Caddy / Nginx で TLS を終端し `127.0.0.1:8777` へリバースプロキシしてください（例：[`deploy/caddy/Caddyfile.example`](deploy/caddy/Caddyfile.example)）。systemd デプロイの手順は[中国語 README](README.md) を参照してください。

---

## 🔄 仕組み

WaLiAPI は下流 AI アプリと上流モデルプロバイダの間で、プロトコル変換・負荷分散・セキュリティ監査・リクエストログ記録を行います。さらにナレッジベースエンジン・Wiki エンジン・MCP Server を内蔵し、AI エージェントがプライベートナレッジを直接検索できるようにします。

### リクエストフロー

```mermaid
graph TD
    subgraph Downstream[下流 AI アプリ]
        A1[WaLiCode]
        A2[Claude Code]
        A3[Codex CLI]
        A4[Gemini CLI]
        A5[OpenClaw]
        A6[ChatBox / NextChat]
    end

    Downstream -->|"OpenAI / Anthropic / Responses プロトコル<br/>Authorization: Bearer sk-waliapi-*"| Gateway

    subgraph Gateway[WaLiAPI ローカルゲートウェイ]
        B[プロトコル変換層<br/>OpenAI Chat · Responses · Anthropic<br/>双方向変換]
        C[セキュリティ監査エンジン<br/>リスクスキャン · マスキング/ブロック · ルール]
        D[チャネルディスパッチャ<br/>優先度+重み · フェイルオーバー · モデルマッピング]
        E[アダプタ層<br/>OpenAI · Claude · DeepSeek<br/>Gemini · カスタム]
        F[監査ログ記録<br/>要求/応答ボディ · トークン統計 · Trace ID]

        B --> C --> D --> E
        C --> F
    end

    E -->|HTTPS| Upstream

    subgraph Upstream[上流モデルプロバイダ]
        U1[OpenAI]
        U2[Claude]
        U3[DeepSeek]
        U4[Gemini]
        U5[智谱 · 通義 · Moonshot · StepFun · 豆包 · Ollama]
    end
```

<details>
<summary>📁 ナレッジベース RAG パイプライン</summary>

```mermaid
flowchart TD
    A[ドキュメントをアップロード] --> B[ドキュメントパーサ<br/>Markdown / Code / PDF / JSON / YAML]
    B --> C[tree-sitter シンボル抽出<br/>関数 / クラス / 構造体 / インターフェース]
    C --> D[スマートチャンカー<br/>スライディングウィンドウ + オーバーラップ · シンボル認識]
    D --> E[埋め込みエンジン<br/>WaLiAPI チャネルルーティングを再利用]
    E --> F

    subgraph F[ストレージ + インデックス]
        F1[(SQLite<br/>chunks + FTS5)]
        F2[(HNSW ベクトルインデックス)]
    end

    F --> G[検索<br/>HNSW ベクトル + FTS5 全文検索<br/>→ 加重ハイブリッドランキング]
    G --> H[RAG 生成<br/>Top-K チャンク + 対話履歴<br/>→ ゲートウェイ経由で LLM へ<br/>→ 出典付き回答を生成]
```

</details>

<details>
<summary>🔗 MCP ツールサービス</summary>

WaLiAPI は `/mcp` エンドポイントで **29 個のツール**（ナレッジベース 13 + Wiki 16）を公開する MCP (Model Context Protocol) Server を内蔵しています。Streamable HTTP + SSE に対応し、MCP 対応のあらゆる AI エージェントから利用できます。

KB ツール（13）：セマンティック検索、RAG Q&A、ドキュメント読取、KB CRUD、ドキュメントアップロード/削除、インデックス管理、マルチソースインポート。
Wiki ツール（16）：プロジェクト CRUD、ページ CRUD、検索、Q&A、タグ、ナレッジグラフ、ソース管理、取り込み。

</details>

---

## 🎯 主な機能

### 🔌 マルチチャネル管理

- **11 種類のチャネル**：OpenAI、DeepSeek、Claude、Gemini、智谱、通義、Moonshot、StepFun、豆包、Ollama、カスタム
- 優先度 + 重みの負荷分散と自動フェイルオーバー
- **マルチキー負荷分散**：チャネルごとに複数 API Key、キー単位の重み設定
- チャネル複製、接続テスト（レイテンシ/エラー表示）、チャネルレベルのモデルマッピング、統計（呼び出し数・トークン・成功率・平均レイテンシ）

### 🔑 キー管理

- 下流アプリ向け `sk-waliapi-*` ローカルアクセスキー
- クォータ制限、有効/無効、キー単位の統計

### 📊 ダッシュボード

- 6 つのコア指標：本日のリクエスト/トークン、累計リクエスト/トークン、アクティブチャネル、平均レイテンシ
- カラー分けされたサービス可用率バッジ、動的な運用アドバイス

### 📝 監査ログ

- リクエスト/レスポンス本文、モデルパラメータ、ツール呼び出し、トークン消費、ステータスコードを完全記録
- キーワード・キー・チャネル・モデル・日付範囲・Trace ID で検索
- ページ表示中は 5 秒ごとに自動更新

### 🛡️ セキュリティ監査センター

- **リスク検出エンジン**：機密情報リーク（API キー・秘密鍵・JWT・Cookie・Bearer トークン）、機密パス（`~/.ssh`、`.env`、クラウド認証情報）、Unicode ステガノグラフィ、不審なツール呼び出し、ネットワークリスク、トラッキングピクセル
- **リスクレベル**：clean / info / low / medium / high / critical、0–100 の総合スコア
- **ポリシー**：監査のみ / 警告 / マスキング / ブロック（デフォルトは監査のみ）
- **ルール管理**：25+ の内蔵ルール + カスタム black/white リスト

### 📚 ナレッジベースエンジン

- **解析**：Markdown、コードファイル（20+ 言語）、PDF（VLM OCR によるスキャン PDF 対応）、JSON/YAML/CSV
- **シンボル認識チャンキング**：tree-sitter で関数/クラス境界を保持
- **ハイブリッド検索**：HNSW ベクトル検索 + SQLite FTS5 全文検索の加重融合
- **RAG Q&A**：Top-K 検索 + 対話履歴 → 出典付き回答
- **マルチソースインポート**：Git リポジトリ、URL 一括、ローカルディレクトリ

### 📓 Wiki ナレッジエンジン

- Markdown + frontmatter による構造化プロジェクト/ページ、ディレクトリ階層
- タグと `[[wikilinks]]` を自動抽出する取り込みパイプライン
- ページ検索、出典付き Wiki Q&A、ナレッジグラフ可視化、タグ体系

### 🔗 MCP Server

- `/mcp` で 29 ツール（KB 13 + Wiki 16）、Streamable HTTP + SSE
- Claude Desktop・OpenClaw など MCP 対応エージェントと互換

### ⚙️ その他

- **設定**：テーマ（ダーク/ライト/システム）、トレイ、自動起動、リトライポリシー
- **アプリ設定**：8 種類の AI コーディングツールへワンクリック設定
- **インポート/エクスポート**：チャネル JSON バックアップ、WaLiCode バックアップ取り込み
- **ストリーミング**：完全な SSE 転送と使用量解析

---

## 🔗 マルチプロトコル接続

WaLiAPI はゲートウェイ層でプロトコルを翻訳します：入口はマルチプロトコル、出口は統一 —— 上流チャネルは一切感知しません。

| プロトコル | エンドポイント | 認証 | 備考 |
|:---|:---|:---|:---|
| **OpenAI Chat Completions** | `POST /v1/chat/completions` | `Authorization: Bearer sk-waliapi-*` | 標準互換、ストリーミング対応 |
| **OpenAI Responses** | `POST /v1/responses` | `Authorization: Bearer sk-waliapi-*` | Responses 双方向変換 |
| **Anthropic Messages** | `POST /v1/messages` | `x-api-key: sk-waliapi-*` | Anthropic プロトコル、ヘッダ自動変換 |
| **OpenAI Embeddings** | `POST /v1/embeddings` | `Authorization: Bearer sk-waliapi-*` | 埋め込み、ナレッジベースで再利用 |
| **モデル一覧** | `GET /v1/models` | `Authorization: Bearer sk-waliapi-*` | チャネル + Auth アカウントを集約 |
| **ヘルスチェック** | `GET /health` | なし | 生存プローブ |
| **MCP** | `POST /mcp` / `GET /mcp` | — | MCP Streamable HTTP + SSE |
| **ナレッジベース API** | `/api/kb/*` | — | KB CRUD、検索、RAG |

curl での動作確認：

```bash
curl http://127.0.0.1:8777/v1/chat/completions \
  -H "Authorization: Bearer sk-waliapi-xxxx" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4o",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
```

---

## 🏗️ 技術スタック

| レイヤ | 技術 |
|:---|:---|
| フロントエンド | React 19 + TypeScript + Vite 7 + Tailwind CSS 4 + React Router 7 |
| バックエンド | Rust (edition 2021) + Tauri 2 + Axum 0.8 + sqlx (SQLite) + reqwest + tokio |
| ナレッジベース | tree-sitter (7 言語) + HNSW + FTS5 + bincode |
| Wiki | Markdown + frontmatter 解析 + wikilinks グラフ + SQLite |
| パッケージング | Tauri bundler (.dmg / .msi / .deb / .AppImage)、Docker マルチステージビルド |

---

## 🛡️ セキュリティ境界と脅威モデル

WaLiAPI は**ローカル / イントラネット優先**で設計されています。公開環境にデプロイする前に必ずお読みください。

**認証情報と権限**：

| 用途 | 認証情報 |
|:---|:---|
| データプレーン `/v1/*` | `sk-waliapi-*` ゲートウェイキー |
| Web 管理パネル | 管理者ログインセッション（初回起動時に初期パスワードを生成、`INITIAL_PASSWORD` ファイルに保存し初回ログイン後に削除） |
| KB/Wiki 管理 REST | `WALIAPI_ADMIN_TOKEN`（≥32 文字） |
| RAG REST / MCP 検索 | ナレッジベースへのアクセスを許可された API Key |
| MCP 管理ツール / Wiki / SSE | `WALIAPI_MCP_TOKEN`（≥32 文字、管理トークンと別の値） |

**平文でのキー保存（開示）**：上流チャネルキーとゲートウェイ API キーはローカル SQLite データディレクトリに**平文**で保存されます —— ファイルシステムの権限がセキュリティ境界であり、静的暗号化は提供していません。チャネルのエクスポートファイルには平文キーが含まれます（UI で明示的に警告）。

**セキュリティスキャンの境界（ベストエフォート DLP）**：内蔵リスクスキャナはヒューリスティック検出であり、完全なコンテンツセーフティソリューションではありません。レスポンス側スキャンは fail-open、スキャン予算超過時は fail-closed でリクエストを拒否します。

**CORS スコープ**：寛容な CORS（`Access-Control-Allow-Origin: *`）はデータプレーンのゲートウェイルート（API キー認証の `/v1/*`）に**のみ**適用されます。管理画面・KB/Wiki REST・MCP エンドポイントには寛容な CORS を付与せず、管理画面には CSRF 保護があります。

**公開デプロイの推奨**：Docker はデフォルトで `127.0.0.1` のみにバインド。Caddy/Nginx で TLS を終端し、強力な `WALIAPI_ADMIN_TOKEN` / `WALIAPI_MCP_TOKEN` を注入し、プロキシで認証ヘッダを剥がさず、単一インスタンスで運用してください（SQLite はマルチライター非対応）。

---

## 📌 バージョン履歴

完全なリリース履歴（中国語）は[メイン README](README.md#-版本历史) を参照してください。最新リリース：

### v0.4.5 (2026-10-22)

- ✨ **無料チャネル対応**（@fuzhengwei）：初回起動時に OpenCode 無料レーンと Kilo 無料プールをキー不要のチャネルとして自動登録（設定ゼロ・1 度のみ・削除は尊重）。上流 v2 フィンガープリントゲートを通過させる専用 OpenCode レーン実行器を追加（公開クレデンシャル `public`、固定ヘッダ、セッション ID 鋳造とセッションプール、`read` / `shell` ツール宣言）。無料枠チャネルプリセット 5 種を追加 —— Groq・Cerebras・Mistral・NVIDIA NIM・OpenRouter —— キー入力欄の上に「API Key を取得」リンク（`key_signup_url`）を表示
- ✨ **Wiki 再構築と UX**（PR #166、@freakojc）：プログラマティックなインデックス再構築（`services/wiki/rebuild.rs`）——LLM 意味グルーピングとログ整合性バックフィル。ページタブ UX 改善（スクロール位置・ドロップダウン統一・ドキュメント検索）
- 🐛 **WorkBuddy レスポンス互換**（PR #168、@mw）：上流レスポンス形式の差異に対するレスポンス解析と SSE ストリーミング処理の互換性強化
- 🔧 バージョンを 0.4.5 に更新（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

### v0.4.4 (2026-10-18)

- ✨ **Wiki プロジェクトのエクスポート / インポート（zip バックアップ）**（PR #163、@freakojc）：新規 `services/wiki/backup.rs` がページ Markdown・ソースファイル・画像アセット（トークン含む）・コアデータを zip にパッキング。エクスポート / インポートコマンドと Admin ルートを追加し、UI にエクスポートダイアログを提供
- 🐛 **ask_wiki 全経路修正**（PR #164、@freakojc）：中国語句読点のバイトスライス panic・画像 URL のネスト解析・推論レベル監査の実際の API Key への帰属・MCP ツール説明の `http://` 二重プレフィックス・チャネルインポート / エクスポートのアイコン向きを修正
- 🐛 **Responses→Chat 変換の省略時に max_tokens=4096 を注入しない**（PR #165、@ymsy2021）：Codex など Responses クライアントは `max_output_tokens` を送信しないが、旧変換は `max_tokens=4096` をハードコーディングし、長い回答が黙って打ち切られていた（`finish_reason=length`、予算には reasoning トークンも含まれる）。明示的に指定がある場合のみマッピングするよう修正（#162 を修正）
- 🎨 **推論レベルドロップダウンの統一**（@fuzhengwei）：ナレッジベース設定・Wiki 新規作成ダイアログ・Wiki プロジェクト設定の 3 ヶ所の「推論レベル」ネイティブ `<select>` を共通カスタム Select コンポーネントに置き換え、UI 言語を統一
- 🔧 バージョンを 0.4.4 に更新（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

### v0.4.3 (2026-10-17)

- ✨ **ソフトウェア連携（Link）**：MCP 機能ドメインを `services/software`（ソフトウェア連携）へ発展。サイドバーとナレッジベースページに Link 入口を追加（テスト機能・開発中）。コマンド層・MCP handlers・サーバー / Admin ルート・ナレッジアクセス制御も同期して対応
- ✨ **OpenAPI / Swagger ワンクリック取り込み**：OpenAPI 3.x / Swagger 2.0（JSON/YAML）ドキュメントを貼り付けるだけで generic_http コネクタ設定に変換——各 operation が 1 つの MCP ツールになり、ツール名・パス / クエリパラメータ・リクエストボディ・リスクレベルは自動マッピング
- ✨ **MCP による対話式インスタンス管理**：外部 AI エージェントが `/mcp` 経由でコネクタの発見・OpenAPI の取り込み・ソフトウェアインスタンスの作成 / 更新 / 削除が可能。削除には明示的な `confirm: true` が必要で、インスタンス単位のツールは承認ポリシーで制御
- ✨ **CLI-Hub 連携**：CLI-Anything コミュニティカタログ（103+ ソフトウェア）からワンクリックでインストール + インスタンス作成。`help` / `run` の 2 ツールを持つ `ClihubConnector` を追加。hub 未インストール時は内蔵セレクトカタログへフォールバック
- 🎨 **ソフトウェアページ刷新**：SoftwarePage を大幅に作り直し、インスタンス管理とオンラインデバッグの操作性を改善。ナレッジベースページに「利用可能ツール」テーブルとキー試用入口を追加
- 🐛 **デスクトップ /mcp オンライン呼び出しプロキシ**：新規 `mcp_online_call` コマンドが Rust 側からローカル `/mcp` へのリクエストを代理送信し、WKWebView のループバックアドレスに対する混在コンテンツ / CORS 制限（TypeError: Load failed）を回避
- 🐛 **Qoder ストリーミングデコード修復**：内側の Chat チャンクが裸 JSON として SSE パーサに渡され黙って破棄され、ストリーミングリクエストが「no message_start emitted」で失敗していた（非ストリーミングは影響なし）。SSE レコードとして再フレーミングし、下流プロトコルに応じて内側デコーダを選択するように修正。Responses / Chat / Messages の 3 下流でストリーミング全経路を検証済み

<details>
<summary><b>v0.4.2 (2026-10-16)</b></summary>

- ✨ **Qoder Auth Provider**：`auth_provider/qoder_backend.rs`・`qoder_cosy.rs`・`qoder_login.rs` を追加。Auth アカウント体系が Qoder OAuth ログイン・トークン更新・モデル発見に対応し、`protocol/codec/qoder/` コーデックをレジストリに登録
- ✨ **Wiki PDF 取り込みの全面修復**（PR #159、@freakojc）：`services/wiki/ingest.rs` を書き直し、OCR レンダリング強化。API Key ごとにナレッジベース / Wiki へのアクセス許可を個別設定可能に、RAG・Wiki Q&A の推論温度を個別設定可能に、取り込み進捗をモジュールレベル状態に変更
- ✨ **可観測性とダッシュボード**：リクエスト意図分類（`prompt_classify.rs`）、ダッシュボードに `RouteMapCard` ルーティングマップと `TokenHeatmap` を追加
- ✨ **API ログのクライアント識別**：リクエスト特徴から下流クライアント（Claude Code / Codex / ChatBox など）を識別して記録し、ログページでフィルタ可能
- 🐛 **旧 WebKit 向けグラデーション互換修復（ADR 0002）**：`bg-gradient-to-*` が出力する `in oklab` 補間構文を旧 WKWebView が黙って破棄し、グラデーション背景が消失（例：ルートマップカードの WaLiAPI アイコンが空白化）。`App.css` で一括修復
- 📝 **README 多言語化**：「30 秒でわかる WaLiAPI」Mermaid 図、英語版・日本語版と言語切替を追加
- 🔧 **バージョンを 0.4.2 に更新**（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

</details>

---

## 👥 コントリビューター

> WaLiAPI は情熱的なオープンソースコミュニティによって構築されています。詳細なコントリビューター表（中国語）は[メイン README](README.md#-贡献者) を参照してください。

<a href="https://github.com/fuzhengwei/WaLiAPI/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=fuzhengwei/WaLiAPI"  alt="Contributors"/>
</a>

---

## 📄 ライセンス

[MIT](./LICENSE)

原著者の著作権表示とライセンス表示を保持する限り、商用利用・個人利用・改変・再配布は自由です。詳細な権利表は[中国語 README](README.md#-license) を参照してください。

---

<div align="center">
  <sub>Built with ❤️ by the WaLiAPI community</sub>
</div>
