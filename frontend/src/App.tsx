import { StatusIndicator } from "./components/StatusIndicator";
import { ClarifyPopup } from "./components/ClarifyPopup";
import { QuestionCard } from "./components/QuestionCard";
import { AnnotateOverlay } from "./components/AnnotateOverlay";
import { CommandBar } from "./components/CommandBar";
import { PasswordRecovery } from "./components/PasswordRecovery";
import { PostAuthSetup } from "./components/PostAuthSetup";
import { Shell } from "./components/Shell";
import { SignupGate } from "./components/SignupGate";
import { Toast } from "./components/Toast";
import { useAuth } from "./hooks/useAuth";
import "./components/SignupGate.css";

function App() {
  const hash = window.location.hash;

  // Floating popups stay independent — they live in their own borderless,
  // transparent windows with their own bespoke styling. They never block
  // on auth: the hotkey flow needs to fire regardless of sign-in state.
  if (hash === "#/status") return <StatusIndicator />;
  if (hash === "#/clarify") return <ClarifyPopup />;
  if (hash === "#/question-card") return <QuestionCard />;
  if (hash === "#/annotate") return <AnnotateOverlay />;
  if (hash === "#/command-bar") return <CommandBar />;
  if (hash === "#/toast") return <Toast />;

  return <MainAppGated hash={hash} />;
}

function MainAppGated({ hash }: { hash: string }) {
  const auth = useAuth();

  // Don't lock dev / unconfigured installs out — if Supabase env vars
  // are missing, fall straight through to the shell.
  const gateActive = auth.configured && !auth.loading && !auth.user;

  if (auth.configured && auth.loading) {
    // Initial getSession() in flight — show a small branded loading
    // state instead of flashing the gate at users who already have a
    // persisted session.
    return <AuthLoading />;
  }

  // Recovery preempts both the gate and the shell: the user has a
  // valid session from a reset email but their password is still the
  // old (forgotten) one. Force them through the new-password form
  // before any main-app surface renders.
  if (auth.configured && auth.user && auth.recoveryMode) {
    return <PasswordRecovery />;
  }

  // Just-signed-in interstitial. Sits between auth completion and the
  // main app — confirms the sign-in and prompts the user to set up
  // their Groq API key. Returning users with a key already configured
  // get bounced through automatically inside PostAuthSetup itself.
  if (auth.configured && auth.user && auth.justSignedIn) {
    return <PostAuthSetup />;
  }

  if (gateActive) return <SignupGate />;

  if (hash === "#/projects") return <Shell initial="projects" />;
  if (hash === "#/settings") return <Shell initial="settings" />;
  return <Shell initial="home" />;
}

function AuthLoading() {
  return (
    <div className="pf-gate-loading">
      <div className="pf-gate-loading-mark" aria-hidden="true" />
      <span>Checking your session…</span>
    </div>
  );
}

export default App;
