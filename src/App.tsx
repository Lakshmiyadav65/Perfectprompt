import { StatusIndicator } from "./components/StatusIndicator";
import { Settings } from "./components/Settings";
import { ClarifyPopup } from "./components/ClarifyPopup";
import { ProjectManager } from "./components/ProjectManager";
import { QuestionCard } from "./components/QuestionCard";

function App() {
  const hash = window.location.hash;
  if (hash === "#/status") return <StatusIndicator />;
  if (hash === "#/settings") return <Settings />;
  if (hash === "#/clarify") return <ClarifyPopup />;
  if (hash === "#/projects") return <ProjectManager />;
  if (hash === "#/question-card") return <QuestionCard />;

  return (
    <main style={{ padding: 16, fontFamily: "system-ui, sans-serif" }}>
      <h1>PromptForge</h1>
      <p>Use the tray icon to open Settings. Press your hotkey to enhance any selected text.</p>
    </main>
  );
}

export default App;
