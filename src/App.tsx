import { useState } from "react";

function App() {
  const [greetMsg, setGreetMsg] = useState("");
  const [name, setName] = useState("");

  async function greet() {
    const { invoke } = await import("@tauri-apps/api/core");
    setGreetMsg(await invoke<string>("greet", { name }));
  }

  return (
    <main className="container">
      <h1>WaLiAPI</h1>
      <p>本地 LLM API 网关 — 工程初始化完成</p>
      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          greet();
        }}
      >
        <input
          id="greet-input"
          onChange={(e) => setName(e.currentTarget.value)}
          placeholder="输入名称测试 Tauri 命令调用..."
        />
        <button type="submit">Greet</button>
      </form>
      <p>{greetMsg}</p>
    </main>
  );
}

export default App;
