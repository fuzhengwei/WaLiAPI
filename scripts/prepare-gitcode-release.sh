#!/usr/bin/env bash
#
# 生成 GitCode 版 latest.json + 上传指引
#
# 用法：
#   ./scripts/prepare-gitcode-release.sh <tag>
#
# 示例：
#   ./scripts/prepare-gitcode-release.sh waliapi-v0.1.2
#
# 做了什么：
#   1. 从 GitHub Release 下载 latest.json
#   2. 把 url 字段的 github.com 替换为 gitcode.com
#   3. 输出到 dist/gitcode/latest.json
#   4. 打印手动上传指引
#
set -euo pipefail

if [ $# -lt 1 ]; then
  echo "用法: $0 <tag>"
  echo "示例: $0 waliapi-v0.1.2"
  exit 1
fi

TAG="$1"
REPO="fuzhengwei/WaLiAPI"
OUT_DIR="dist/gitcode"

echo "=== 准备 GitCode Release 产物 ==="
echo "Tag: $TAG"
echo ""

# 1. 下载 GitHub 的 latest.json
TMP_DIR=$(mktemp -d)
TMP_JSON="$TMP_DIR/latest.json"
trap "rm -rf $TMP_DIR" EXIT

echo "[1/3] 下载 GitHub latest.json..."
if ! gh release download "$TAG" --repo "$REPO" --dir "$TMP_DIR" --pattern "latest.json" --clobber 2>/dev/null; then
  echo "✗ 无法下载 latest.json，请确认 tag $TAG 的 Release 存在"
  exit 1
fi

# 2. 替换 url 域名
echo "[2/3] 生成 GitCode 版 latest.json..."
mkdir -p "$OUT_DIR"
GITCODE_JSON="$OUT_DIR/latest.json"

# 用 python3 确保 JSON 正确处理
python3 -c "
import json, sys

with open('$TMP_JSON') as f:
    data = json.load(f)

for platform in data.get('platforms', {}):
    url = data['platforms'][platform]['url']
    url = url.replace('https://github.com', 'https://gitcode.com')
    data['platforms'][platform]['url'] = url

with open('$GITCODE_JSON', 'w') as f:
    json.dump(data, f, indent=2, ensure_ascii=False)
    f.write('\n')

print(f'  → {sys.argv[1]}' if len(sys.argv) > 1 else '')
" "$GITCODE_JSON"

# 显示 diff
echo ""
echo "--- GitHub 版 url ---"
python3 -c "import json; d=json.load(open('$TMP_JSON')); [print(f'  {p}: {d[\"platforms\"][p][\"url\"]}') for p in d.get('platforms',{})]"
echo ""
echo "--- GitCode 版 url ---"
python3 -c "import json; d=json.load(open('$GITCODE_JSON')); [print(f'  {p}: {d[\"platforms\"][p][\"url\"]}') for p in d.get('platforms',{})]"
echo ""

# 3. 下载其他产物（原样上传）
echo "[3/3] 下载其他产物到 $OUT_DIR/ ..."
for f in "WaLiAPI_aarch64.app.tar.gz" "WaLiAPI_aarch64.app.tar.gz.sig" "WaLiAPI_0.1.2_aarch64.dmg"; do
  VERSION=$(echo "$TAG" | sed 's/waliapi-v//')
  FILENAME=$(echo "$f" | sed "s/0.1.2/$VERSION/g")
  if gh release download "$TAG" --repo "$REPO" --dir "$OUT_DIR" --pattern "$FILENAME" --clobber 2>/dev/null; then
    echo "  ✓ $FILENAME"
  else
    echo "  - $FILENAME (跳过，可能不存在)"
  fi
done

echo ""
echo "=== 完成 ==="
echo ""
echo "文件列表："
ls -lh "$OUT_DIR/"
echo ""
echo "=== 手动上传步骤 ==="
echo "1. 打开 https://gitcode.com/fuzhengwei/WaLiAPI/releases"
echo "2. 创建/编辑 tag $TAG 的 Release"
echo "3. 上传 $OUT_DIR/ 下的所有文件（注意 latest.json 是改过 url 的版本）"
echo "4. 确保文件名与 GitHub Release 一致"
echo ""
echo "⚠️  关键：GitCode 上的 latest.json 里的 url 必须指向 gitcode.com，不是 github.com"
