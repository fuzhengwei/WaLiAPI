import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

const source = await readFile(new URL("./auditLog.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.ESNext },
});
const { requestLogSource, responseLogSource, toolArgumentsSource, contentToString } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);

test("请求源数据保留空白、CRLF、转义和字段顺序，不重新序列化", () => {
  const body = ' {\r\n  "input" : "C:\\\\new\\\\test \\n \\t", "model":"test"\r\n}\r\n';
  assert.equal(requestLogSource(body).text, body);
  assert.notEqual(JSON.stringify(JSON.parse(body), null, 2), body);
  assert.match(requestLogSource(body).description, /脱敏/);
  assert.match(requestLogSource(body).description, /裁剪/);
});

test("空、无记录和无法解析的请求源数据不伪造内容", () => {
  assert.equal(requestLogSource(null).text, null);
  assert.equal(requestLogSource("").text, "");
  assert.equal(requestLogSource(" {broken\\njson").text, " {broken\\njson");
});

test("响应源数据优先使用 SSE，按 seq 拼接且不更改帧和原数组", () => {
  const first = 'event: response.output_text.delta\r\ndata: {"delta":"C:\\\\new\\\\test\\n"}\r\n\r\n';
  const second = 'data: [DONE]\n\n';
  const segments = Object.freeze([Object.freeze({ seq: 2, content: second }), Object.freeze({ seq: 1, content: first })]);
  const result = responseLogSource('[{"message":{"content":"summary"}}]', segments);
  assert.equal(result.text, first + second);
  assert.equal(result.title, "保存的下游 SSE");
  assert.equal(segments[0].seq, 2);
});

test("任意边界切分和中断的 SSE 不插入换行、不解析 JSON", () => {
  const frame = 'data: {"delta":"你好\\nC:\\\\new"}\r\n\r\ndata: {"incomplete":';
  const segments = Array.from(frame, (content, seq) => ({ seq, content }));
  assert.equal(responseLogSource(null, segments).text, frame);
});

test("无 SSE 时只展示保存的摘要，不把摘要称为原始响应", () => {
  const summary = '[ {"message": {"content": "literal\\\\n"}} ]\r\n';
  const result = responseLogSource(summary, []);
  assert.equal(result.text, summary);
  assert.equal(result.title, "保存的响应摘要 JSON");
  assert.match(result.description, /未保存下游 SSE/);
  assert.equal(responseLogSource(null, []).text, null);
});

test("正文、推理字段保留路径、正则、字面转义和真实行尾", () => {
  const content = String.raw`C:\new\test /\n+/g literal\n\r\t` + "\r\n\t真实换行\r";
  assert.equal(contentToString(content), content);
  assert.equal(contentToString(JSON.parse(JSON.stringify(content))), content);
  assert.equal(contentToString(null), "");
});

test("内容块只在可读视图提取，字符串本身不反转义", () => {
  const content = [{ type: "input_text", text: String.raw`C:\new\test` }, { type: "output_text", text: String.raw`literal\n` }, { type: "image" }];
  const snapshot = JSON.stringify(content);
  assert.equal(contentToString(content), String.raw`C:\new\test` + "\n" + String.raw`literal\n` + "\n[image]");
  assert.equal(JSON.stringify(content), snapshot);
});

test("参数原文复制保留重复编码、脚本和转义，空参数不变成空对象", () => {
  const script = 'text(await tools.apply_patch("*** Begin Patch\\n+ C:\\\\new\\\\test\\n*** End Patch"));\r\n';
  assert.equal(toolArgumentsSource(script), script);
  assert.equal(toolArgumentsSource(JSON.stringify(script)), JSON.stringify(script));
  assert.equal(toolArgumentsSource(""), "");
  assert.equal(toolArgumentsSource(undefined), "");
  assert.equal(toolArgumentsSource({ command: script }), JSON.stringify({ command: script }));
});
