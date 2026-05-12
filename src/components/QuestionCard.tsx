import { useEffect, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import "./QuestionCard.css";

type QuestionType = "single_select" | "multi_select" | "free_text" | "chips";

type ImpactDimension =
  | "tone"
  | "audience"
  | "goal"
  | "constraints"
  | "format"
  | "length"
  | "domain"
  | "other";

interface GeneratedQuestion {
  id: string;
  question: string;
  type: QuestionType;
  options: string[];
  placeholder: string | null;
  impact_dimension: ImpactDimension;
  required: boolean;
}

interface QuestionSession {
  original_input: string;
  questions: GeneratedQuestion[];
  answers: unknown[];
  remembered_values: Record<string, string>;
}

interface QuestionAnswer {
  question_id: string;
  impact_dimension: ImpactDimension;
  value: string;
}

type CardState = "loading" | "loaded" | "submitting" | "error";

type AnswerMap = Record<string, string | string[]>;

export function QuestionCard() {
  const [questions, setQuestions] = useState<GeneratedQuestion[]>([]);
  const [answers, setAnswers] = useState<AnswerMap>({});
  const [rememberedIds, setRememberedIds] = useState<Set<string>>(new Set());
  const [originalInput, setOriginalInput] = useState<string>("");
  const [state, setState] = useState<CardState>("loading");
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [copyState, setCopyState] = useState<"idle" | "copied">("idle");
  const appWindow = getCurrentWebviewWindow();

  // Scope the transparent body background to the question-card route only.
  // Without this guard, QuestionCard.css's global body rule would leak into
  // Settings / Projects / Clarify and break their dark theme + scrolling.
  useEffect(() => {
    document.body.classList.add("qc-route");
    return () => {
      document.body.classList.remove("qc-route");
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    async function load() {
      try {
        const session = await invoke<QuestionSession>("fetch_question_card_session");
        if (cancelled) return;
        const remembered = session.remembered_values ?? {};
        const { initial, prefilled } = prefillFromMemory(session.questions, remembered);
        setQuestions(session.questions);
        setAnswers(initial);
        setRememberedIds(prefilled);
        setOriginalInput(session.original_input ?? "");
        setState("loaded");
      } catch (err) {
        if (cancelled) return;
        setErrorMsg(String(err));
        setState("error");
      }
    }
    load();
    return () => {
      cancelled = true;
    };
  }, []);

  const handleSkipAll = useCallback(async () => {
    try {
      await invoke("cancel_question_card");
    } catch (err) {
      console.error("cancel failed", err);
    }
    await appWindow.hide();
  }, [appWindow]);

  useEffect(() => {
    const onKeydown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        void handleSkipAll();
      }
    };
    window.addEventListener("keydown", onKeydown);
    return () => window.removeEventListener("keydown", onKeydown);
  }, [handleSkipAll]);

  const updateAnswer = (questionId: string, next: string | string[]) => {
    setAnswers((prev) => ({ ...prev, [questionId]: next }));
    // Manual edits override the "remembered" provenance.
    setRememberedIds((prev) => {
      if (!prev.has(questionId)) return prev;
      const copy = new Set(prev);
      copy.delete(questionId);
      return copy;
    });
  };

  const handleSubmit = async () => {
    if (state === "submitting" || state === "loading") return;
    setState("submitting");
    setErrorMsg(null);

    const payload: QuestionAnswer[] = questions
      .map((q) => {
        const raw = answers[q.id];
        if (raw === undefined || raw === "" || (Array.isArray(raw) && raw.length === 0)) {
          return null;
        }
        const value = Array.isArray(raw) ? raw.join(", ") : raw;
        return {
          question_id: q.id,
          impact_dimension: q.impact_dimension,
          value,
        };
      })
      .filter((a): a is QuestionAnswer => a !== null);

    try {
      await invoke("submit_question_card_answers", { answers: payload });
      await appWindow.hide();
    } catch (err) {
      setErrorMsg(String(err));
      setState("error");
    }
  };

  const handleCopyRaw = async () => {
    try {
      await navigator.clipboard.writeText(originalInput);
      setCopyState("copied");
      setTimeout(() => setCopyState("idle"), 1500);
    } catch (err) {
      console.error("copy failed", err);
    }
  };

  const handleEnterSubmit = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void handleSubmit();
    }
  };

  const questionCount = questions.length;
  const title = `Quick context — ${questionCount} question${questionCount === 1 ? "" : "s"}`;

  return (
    <div className="qc-container" onKeyDown={handleEnterSubmit}>
      <header className="qc-header">
        <div>
          <div className="qc-title">{title}</div>
          <div className="qc-subtitle">helps get better output</div>
        </div>
        <button
          className="qc-close"
          aria-label="Close"
          onClick={() => void handleSkipAll()}
        >
          ×
        </button>
      </header>

      <div className="qc-body">
        {state === "loading" && <SkeletonBlock />}
        {state === "error" && (
          <div className="qc-error">
            <p>Couldn't enhance your prompt.</p>
            <p className="qc-error-detail">{errorMsg}</p>
            <button
              type="button"
              className="qc-copy-raw"
              onClick={() => void handleCopyRaw()}
              disabled={!originalInput}
            >
              {copyState === "copied" ? "Copied!" : "Copy raw input"}
            </button>
          </div>
        )}
        {(state === "loaded" || state === "submitting") &&
          questions.map((q) => (
            <QuestionRow
              key={q.id}
              question={q}
              value={answers[q.id]}
              remembered={rememberedIds.has(q.id)}
              disabled={state === "submitting"}
              onChange={(v) => updateAnswer(q.id, v)}
            />
          ))}
      </div>

      <footer className="qc-footer">
        <button
          className="qc-skip"
          onClick={() => void handleSkipAll()}
          disabled={state === "submitting"}
        >
          Skip all
        </button>
        <button
          className="qc-primary"
          onClick={() => void handleSubmit()}
          disabled={state === "loading" || state === "submitting"}
        >
          {state === "submitting"
            ? "Enhancing…"
            : state === "error"
              ? "Retry"
              : "Enhance Now →"}
        </button>
      </footer>
    </div>
  );
}

function prefillFromMemory(
  questions: GeneratedQuestion[],
  remembered: Record<string, string>,
): { initial: AnswerMap; prefilled: Set<string> } {
  const initial: AnswerMap = {};
  const prefilled = new Set<string>();

  for (const q of questions) {
    const memVal = remembered[q.impact_dimension];
    if (memVal === undefined || memVal.trim() === "") {
      initial[q.id] = q.type === "multi_select" ? [] : "";
      continue;
    }

    switch (q.type) {
      case "chips":
      case "single_select": {
        const match = q.options.find(
          (opt) => opt.toLowerCase() === memVal.toLowerCase(),
        );
        if (match) {
          initial[q.id] = match;
          prefilled.add(q.id);
        } else {
          initial[q.id] = "";
        }
        break;
      }
      case "multi_select": {
        const parts = memVal
          .split(",")
          .map((s) => s.trim())
          .filter((s) => s.length > 0);
        const matches = q.options.filter((opt) =>
          parts.some((p) => p.toLowerCase() === opt.toLowerCase()),
        );
        if (matches.length > 0) {
          initial[q.id] = matches;
          prefilled.add(q.id);
        } else {
          initial[q.id] = [];
        }
        break;
      }
      case "free_text": {
        initial[q.id] = memVal;
        prefilled.add(q.id);
        break;
      }
    }
  }

  return { initial, prefilled };
}

interface QuestionRowProps {
  question: GeneratedQuestion;
  value: string | string[] | undefined;
  remembered: boolean;
  disabled: boolean;
  onChange: (next: string | string[]) => void;
}

function QuestionRow({
  question,
  value,
  remembered,
  disabled,
  onChange,
}: QuestionRowProps) {
  return (
    <div className="qc-row">
      <div className="qc-question">
        {question.question}
        {remembered && <span className="qc-remembered">remembered</span>}
      </div>
      {renderWidget(question, value, disabled, onChange)}
    </div>
  );
}

// Arrow-key / Home / End navigation within a chip row (PRD §12 keyboard-first).
// Moves focus among sibling chips; the user still uses Enter/Space (browser
// default for <button>) to actually select.
function handleChipKeyDown(e: React.KeyboardEvent<HTMLButtonElement>) {
  const parent = e.currentTarget.parentElement;
  if (!parent) return;
  const chips = Array.from(
    parent.querySelectorAll<HTMLButtonElement>("button.qc-chip"),
  );
  const idx = chips.indexOf(e.currentTarget);
  if (idx === -1) return;
  let next: number | null = null;
  switch (e.key) {
    case "ArrowLeft":
    case "ArrowUp":
      next = idx === 0 ? chips.length - 1 : idx - 1;
      break;
    case "ArrowRight":
    case "ArrowDown":
      next = idx === chips.length - 1 ? 0 : idx + 1;
      break;
    case "Home":
      next = 0;
      break;
    case "End":
      next = chips.length - 1;
      break;
  }
  if (next !== null) {
    e.preventDefault();
    e.stopPropagation();
    chips[next].focus();
  }
}

function renderWidget(
  question: GeneratedQuestion,
  value: string | string[] | undefined,
  disabled: boolean,
  onChange: (next: string | string[]) => void,
) {
  switch (question.type) {
    case "chips":
    case "single_select": {
      const current = typeof value === "string" ? value : "";
      return (
        <div className="qc-chip-row" role="radiogroup">
          {question.options.map((opt) => (
            <button
              key={opt}
              type="button"
              role="radio"
              aria-checked={current === opt}
              disabled={disabled}
              className={`qc-chip ${current === opt ? "selected" : ""}`}
              onClick={() => onChange(current === opt ? "" : opt)}
              onKeyDown={handleChipKeyDown}
            >
              {opt}
            </button>
          ))}
        </div>
      );
    }
    case "multi_select": {
      const current = Array.isArray(value) ? value : [];
      const toggle = (opt: string) => {
        const next = current.includes(opt)
          ? current.filter((o) => o !== opt)
          : [...current, opt];
        onChange(next);
      };
      return (
        <div className="qc-chip-row" role="group">
          {question.options.map((opt) => (
            <button
              key={opt}
              type="button"
              role="checkbox"
              aria-checked={current.includes(opt)}
              disabled={disabled}
              className={`qc-chip ${current.includes(opt) ? "selected" : ""}`}
              onClick={() => toggle(opt)}
              onKeyDown={handleChipKeyDown}
            >
              {opt}
            </button>
          ))}
        </div>
      );
    }
    case "free_text": {
      const current = typeof value === "string" ? value : "";
      return (
        <input
          type="text"
          className="qc-input"
          placeholder={question.placeholder ?? ""}
          value={current}
          disabled={disabled}
          onChange={(e) => onChange(e.target.value)}
        />
      );
    }
  }
}

function SkeletonBlock() {
  return (
    <div className="qc-skeleton">
      <div className="qc-skeleton-line" />
      <div className="qc-skeleton-chips">
        <div className="qc-skeleton-chip" />
        <div className="qc-skeleton-chip" />
        <div className="qc-skeleton-chip" />
      </div>
      <div className="qc-skeleton-line short" />
      <div className="qc-skeleton-chips">
        <div className="qc-skeleton-chip" />
        <div className="qc-skeleton-chip" />
      </div>
    </div>
  );
}
