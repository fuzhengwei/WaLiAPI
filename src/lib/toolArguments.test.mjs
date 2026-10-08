import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

const source = await readFile(new URL("./toolArguments.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.ESNext },
});
const { formatToolArguments } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);

test("按行展示 exec 中的多个补丁，保留实际换行和缩进", () => {
  const patch = '*** Begin Patch\n*** Update File: E:/project/Gateway.java\n@@\n+    product.put("currency", "USD");\n*** End Patch';
  const secondPatch = '*** Begin Patch\n*** Update File: E:/project/Config.java\n@@\n+    enabled = true;\n*** End Patch';
  const script = `text(await tools.apply_patch(${JSON.stringify(patch)}));\ntext(await tools.apply_patch(${JSON.stringify(secondPatch)}));\n`;
  const rendered = formatToolArguments(script);
  assert.ok(rendered.includes(patch));
  assert.ok(rendered.includes(secondPatch));
  assert.equal(rendered.split("\n").length, 11);
  assert.ok(script.includes("\\n"));
  assert.equal(formatToolArguments(JSON.stringify(script)), rendered);
});

test("兼容 JSON 参数和多行 command 字段", () => {
  const command = 'echo "你好"\n  echo done';
  const rendered = formatToolArguments(JSON.stringify({ command, cwd: "E:/project" }));
  assert.ok(rendered.includes(command));
  assert.ok(rendered.includes('  "cwd": "E:/project"'));
  assert.equal(formatToolArguments('{"city":"Paris","limit":2}'), '{\n  "city": "Paris",\n  "limit": 2\n}');
});

test("保留 Windows 路径、正则和普通字面转义", () => {
  const script = String.raw`const path = "C:\\new\\test"; const pattern = "\\n+"; const regex = /\n/g;`;
  assert.equal(formatToolArguments(script), script);
  const argumentsValue = { path: String.raw`C:\new\test`, pattern: String.raw`\n+` };
  assert.equal(formatToolArguments(JSON.stringify(argumentsValue)), JSON.stringify(argumentsValue, null, 2));
});

test("不解析单引号和模板字符串内的双引号，也不执行脚本", () => {
  const script = "const quoted = '\"a\\nb\"'; const template = `\"c\\nd\"`; globalThis.auditFormatterExecuted = true;";
  assert.equal(formatToolArguments(script), script);
  assert.equal(globalThis.auditFormatterExecuted, undefined);
});

test("缺失参数、对象参数和非标准脚本保持可展示", () => {
  assert.equal(formatToolArguments(undefined), "{}");
  assert.equal(formatToolArguments(""), "{}");
  assert.equal(formatToolArguments({ city: "Paris" }), '{\n  "city": "Paris"\n}');
  const invalid = String.raw`text("broken\x20\ncontent");`;
  assert.equal(formatToolArguments(invalid), invalid);
  assert.equal(formatToolArguments("unquoted\\ntext"), "unquoted\\ntext");
});

test("多行字符串统一 CRLF，并保留制表符和 Unicode", () => {
  const script = `apply_patch(${JSON.stringify("第一行\r\n\t第二行\r第三行")})`;
  assert.equal(formatToolArguments(script), 'apply_patch("第一行\n\t第二行\n第三行")');
});
