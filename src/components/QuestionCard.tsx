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
  const [state, setState] = useState<CardState>("loading");
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const appWindow = getCurrentWebviewWindow();

  useEffect(() => {
    let cancelled = false;
    async function load() {
      try {
        const session = await invoke<QuestionSession>("fetch_question_card_session");
        if (cancelled) return;
        setQuestions(session.questions);
        setAnswers(initialiseAnswers(session.questions));
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
            <p>Couldn't load questions.</p>
            <p className="qc-error-detail">{errorMsg}</p>
          </div>
        )}
        {(state === "loaded" || state === "submitting") &&
          questions.map((q) => (
            <QuestionRow
              key={q.id}
              question={q}
              value={answers[q.id]}
              disabled={state === "submitting"}
              onChange={(v) => setAnswers((prev) => ({ ...prev, [q.id]: v }))}
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

function initialiseAnswers(questions: GeneratedQuestion[]): AnswerMap {
  const out: AnswerMap = {};
  for (const q of questions) {
    out[q.id] = q.type === "multi_select" ? [] : "";
  }
  return out;
}

interface QuestionRowProps {
  question: GeneratedQuestion;
  value: string | string[] | undefined;
  disabled: boolean;
  onChange: (next: string | string[]) => void;
}

function QuestionRow({ question, value, disabled, onChange }: QuestionRowProps) {
  return (
    <div className="qc-row">
      <div className="qc-question">{question.question}</div>
      {renderWidget(question, value, disabled, onChange)}
    </div>
  );
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
        <div className="qc-chip-row">
          {question.options.map((opt) => (
            <button
              key={opt}
              type="button"
              disabled={disabled}
              className={`qc-chip ${current === opt ? "selected" : ""}`}
              onClick={() => onChange(current === opt ? "" : opt)}
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
        <div className="qc-chip-row">
          {question.options.map((opt) => (
            <button
              key={opt}
              type="button"
              disabled={disabled}
              className={`qc-chip ${current.includes(opt) ? "selected" : ""}`}
              onClick={() => toggle(opt)}
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
