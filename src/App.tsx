import { invoke } from "@tauri-apps/api/core";
import { createSignal, onMount } from "solid-js";
import logo from "./assets/logo.svg";
import "./App.css";

function App() {
  const [greetMsg, setGreetMsg] = createSignal("");
  const [name, setName] = createSignal("");
  // M1 PoC：嵌入式 bun 运行时（libpi-bun / libskal）冒烟结果
  const [bunResult, setBunResult] = createSignal("(not run)");

  async function greet() {
    // Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
    setGreetMsg(await invoke("greet", { name: name() }));
  }

  async function runBunSmoke() {
    setBunResult("running…");
    try {
      setBunResult(await invoke<string>("pi_bun_smoke"));
    } catch (e) {
      setBunResult(`ERROR: ${e}`);
    }
  }

  onMount(() => {
    runBunSmoke();
  });

  return (
    <main class="container">
      <h1>Welcome to Tauri + Solid</h1>

      <div class="row">
        <a href="https://vite.dev" target="_blank" rel="noopener">
          <img src="/vite.svg" class="logo vite" alt="Vite logo" />
        </a>
        <a href="https://tauri.app" target="_blank" rel="noopener">
          <img src="/tauri.svg" class="logo tauri" alt="Tauri logo" />
        </a>
        <a href="https://solidjs.com" target="_blank" rel="noopener">
          <img src={logo} class="logo solid" alt="Solid logo" />
        </a>
      </div>
      <p>Click on the Tauri, Vite, and Solid logos to learn more.</p>

      <form
        class="row"
        onSubmit={(e) => {
          e.preventDefault();
          greet();
        }}
      >
        <input
          id="greet-input"
          onChange={(e) => setName(e.currentTarget.value)}
          placeholder="Enter a name..."
        />
        <button type="submit">Greet</button>
      </form>
      <p>{greetMsg()}</p>

      <h2>libpi-bun PoC (M1)</h2>
      <p>
        Embedded bun+JSC runtime smoke test. Result also lands in logcat (tag:
        pi-bun).
      </p>
      <div class="row">
        <button type="button" onClick={() => runBunSmoke()}>
          Run bun smoke
        </button>
      </div>
      <pre
        style={{
          "text-align": "left",
          "white-space": "pre-wrap",
          "font-size": "0.8rem",
        }}
      >
        {bunResult()}
      </pre>
    </main>
  );
}

export default App;
