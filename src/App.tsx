import { StatusIndicator } from "./components/StatusIndicator";
import { ClarifyPopup } from "./components/ClarifyPopup";
import { QuestionCard } from "./components/QuestionCard";
import { CommandBar } from "./components/CommandBar";
import { Shell } from "./components/Shell";
import { Toast } from "./components/Toast";

function App() {
  const hash = window.location.hash;
  // Floating popups stay independent — they live in their own borderless,
  // transparent windows with their own bespoke styling.
  if (hash === "#/status") return <StatusIndicator />;
  if (hash === "#/clarify") return <ClarifyPopup />;
  if (hash === "#/question-card") return <QuestionCard />;
  if (hash === "#/command-bar") return <CommandBar />;
  if (hash === "#/toast") return <Toast />;

  // Anything else (main / home / settings / projects, plus the legacy
  // "/settings" or "/projects" deep-links from the tray) renders the
  // shell with the requested initial route. The sidebar lets the user
  // navigate between Home, Projects, and Settings without spawning new
  // windows.
  if (hash === "#/projects") return <Shell initial="projects" />;
  if (hash === "#/settings") return <Shell initial="settings" />;
  return <Shell initial="home" />;
}

export default App;
