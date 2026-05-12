import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import "./ClarifyPopup.css";

interface Question {
  id: string;
  question: string;
  options: string[];
}

export function ClarifyPopup() {
  const [prompt, setPrompt] = useState<string>("");
  const [questions, setQuestions] = useState<Question[]>([]);
  const [answers, setAnswers] = useState<Record<string, { option: string; otherText: string }>>({});
  const [loading, setLoading] = useState<boolean>(true);
  const [enhancing, setEnhancing] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [debug, setDebug] = useState<string>("Mounting...");
  const appWindow = getCurrentWebviewWindow();

  useEffect(() => {
    // On mount, fetch the pending prompt from backend state
    async function fetchAndGenerate() {
      try {
        setDebug("Fetching prompt from backend state...");
        const pendingPrompt: string = await invoke("get_pending_prompt");
        console.log("Got pending prompt:", pendingPrompt);
        
        if (!pendingPrompt || pendingPrompt.trim() === "") {
          setDebug("No prompt found in state. Waiting...");
          setLoading(false);
          return;
        }

        setPrompt(pendingPrompt);
        setDebug(`Prompt received (${pendingPrompt.length} chars). Calling API...`);
        setLoading(true);
        setError(null);

        // Call backend to generate questions
        const generatedQuestions: Question[] = await invoke("generate_clarifying_questions", {
          prompt: pendingPrompt,
        });
        setQuestions(generatedQuestions);
        setDebug("Questions generated successfully!");

        // Initialize answers state
        const initialAnswers: Record<string, { option: string; otherText: string }> = {};
        for (const q of generatedQuestions) {
          initialAnswers[q.id] = { option: "", otherText: "" };
        }
        setAnswers(initialAnswers);
      } catch (err: any) {
        console.error("Failed to generate questions:", err);
        setDebug(`Error: ${err.toString()}`);
        setError(err.toString());
      } finally {
        setLoading(false);
      }
    }

    fetchAndGenerate();
  }, []);

  const handleOptionChange = (questionId: string, option: string) => {
    setAnswers((prev) => ({
      ...prev,
      [questionId]: { ...prev[questionId], option },
    }));
  };

  const handleOtherTextChange = (questionId: string, text: string) => {
    setAnswers((prev) => ({
      ...prev,
      [questionId]: { ...prev[questionId], otherText: text },
    }));
  };

  const handleCancel = async () => {
    // Hide window
    await appWindow.hide();
    // Reset state for next time
    setQuestions([]);
    setPrompt("");
    setLoading(true);
    setEnhancing(false);
  };

  const handleSubmit = async () => {
    if (enhancing) return;
    
    // Format answers
    const formattedAnswers = questions.map(q => {
      const answer = answers[q.id];
      const answerText = answer.option === "Other" ? answer.otherText : answer.option;
      return { question: q.question, answer: answerText || "Not specified" };
    });

    setEnhancing(true);
    setError(null);

    try {
      await invoke("submit_answers_and_enhance", {
        prompt,
        answers: formattedAnswers
      });
      // The backend handles hiding the window after pasting
      
      // Reset state for next time
      setTimeout(() => {
        setQuestions([]);
        setPrompt("");
        setLoading(true);
        setEnhancing(false);
      }, 500);
    } catch (err: any) {
      console.error("Enhancement failed:", err);
      setError(err.toString());
      setEnhancing(false);
    }
  };

  // Ensure all questions have an option selected, or if 'Other' is selected, text is provided.
  const isSubmitDisabled = questions.length === 0 || questions.some((q) => {
    const ans = answers[q.id];
    if (!ans || !ans.option) return true;
    if (ans.option === "Other" && !ans.otherText.trim()) return true;
    return false;
  });

  return (
    <div className="clarify-container">
      <div className="clarify-header">
        <h2 className="clarify-header-title">Enhance Prompt</h2>
      </div>

      <div className="clarify-content">
        {loading ? (
          <div className="loading-state">
            <div className="spinner"></div>
            <p>Analyzing prompt and generating clarifying questions...</p>
            <p style={{color: '#888', fontSize: '12px', marginTop: '10px'}}>{debug}</p>
          </div>
        ) : enhancing ? (
          <div className="loading-state">
            <div className="spinner"></div>
            <p>Applying enhancements and replacing text...</p>
          </div>
        ) : (
          <>
            <div style={{color: '#888', fontSize: '12px', marginBottom: '10px'}}>{debug}</div>
            {error && <div className="error-message">{error}</div>}
            {questions.map((q, index) => (
              <div key={q.id} className="question-block">
                <div className="question-text">
                  {index + 1}. {q.question}
                </div>
                <div className="options-list">
                  {q.options.map((opt) => (
                    <label key={opt} className="option-label">
                      <input
                        type="radio"
                        name={q.id}
                        value={opt}
                        checked={answers[q.id]?.option === opt}
                        onChange={() => handleOptionChange(q.id, opt)}
                        className="option-input"
                      />
                      {opt}
                    </label>
                  ))}
                  <label className="option-label">
                    <input
                      type="radio"
                      name={q.id}
                      value="Other"
                      checked={answers[q.id]?.option === "Other"}
                      onChange={() => handleOptionChange(q.id, "Other")}
                      className="option-input"
                    />
                    Other
                  </label>
                  {answers[q.id]?.option === "Other" && (
                    <input
                      type="text"
                      className="other-input"
                      placeholder="Please specify..."
                      value={answers[q.id]?.otherText || ""}
                      onChange={(e) => handleOtherTextChange(q.id, e.target.value)}
                      autoFocus
                    />
                  )}
                </div>
              </div>
            ))}
          </>
        )}
      </div>

      <div className="clarify-footer">
        <button className="btn btn-secondary" onClick={handleCancel} disabled={loading || enhancing}>
          Cancel
        </button>
        <button 
          className="btn btn-primary" 
          onClick={handleSubmit} 
          disabled={loading || enhancing || isSubmitDisabled}
        >
          {enhancing ? "Enhancing..." : "Next"}
        </button>
      </div>
    </div>
  );
}
