import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useProjectWaitlist } from "../hooks/useProjectWaitlist";
import "./ProjectManager.css";

/// Project Knowledge digest — mirrors the Rust `RepoDigest` struct.
/// The `source` discriminator is `tag = "kind"` on the Rust side, so
/// the TS shape uses the same `{ kind, value }` envelope.
interface RepoDigest {
  digest_text: string;
  token_count_estimate: number;
  file_count: number;
  elided_count: number;
  source:
    | { kind: "local"; value: { path: string } }
    | {
        kind: "github";
        value: { owner: string; repo: string; branch: string; html_url: string };
      };
  fetched_at: string;
  sha256: string;
}

/// Project Knowledge rethink — the curated PROJECT.md attached to a
/// project. Generated once at digest-build time, editable by the
/// user. The shape mirrors the Rust `ProjectSummary` struct in
/// `src-tauri/src/project_summary.rs` exactly.
interface ProjectSummary {
  markdown: string;
  generated_at: string;
  user_edited: boolean;
  generator_model: string;
}

interface Project {
  id: string;
  name: string;
  description: string;
  links: string[];
  path: string | null;
  digest?: RepoDigest | null;
  project_summary?: ProjectSummary | null;
  created_at: string;
  updated_at: string;
}

interface ProjectStore {
  active_project_id: string | null;
  projects: Project[];
}

type FormMode = { type: "closed" } | { type: "add" } | { type: "edit"; project: Project };

export function ProjectManager() {
  const [store, setStore] = useState<ProjectStore>({ active_project_id: null, projects: [] });
  const [formMode, setFormMode] = useState<FormMode>({ type: "closed" });
  const [formName, setFormName] = useState("");
  const [formDesc, setFormDesc] = useState("");
  const [formLinks, setFormLinks] = useState<string[]>([]);
  const [formPath, setFormPath] = useState<string>("");
  const [formRepoUrl, setFormRepoUrl] = useState<string>("");
  const [analyzing, setAnalyzing] = useState(false);
  const [newLink, setNewLink] = useState("");
  const [uploadedFiles, setUploadedFiles] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  /// Unix-epoch-seconds string (e.g. "1700000000s") from the cached
  /// GitHub fetch for the project being edited. Null when no cache
  /// exists. Phase 2 Step 9.
  const [lastFetched, setLastFetched] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  // Project Knowledge — repository digest state. Lives on the project
  // object itself once saved, but we mirror it locally during the edit
  // session so the panel re-renders the moment a digest is added /
  // refreshed / cleared without waiting for the next refresh().
  const [digestState, setDigestState] = useState<RepoDigest | null>(null);
  const [digestBusy, setDigestBusy] = useState(false);
  const [digestError, setDigestError] = useState<string | null>(null);
  // Layer 1 observability — when set, render the DigestViewerModal over
  // the edit form. Holds the SAME object reference as digestState so any
  // refresh that mutates digestState also updates the modal's view.
  const [digestViewerOpen, setDigestViewerOpen] = useState(false);
  // Project Knowledge rethink — curated PROJECT.md state. Mirrors the
  // backend ProjectSummary so the panel re-renders the moment Add /
  // Refresh / Regenerate / Save completes.
  const [summaryState, setSummaryState] = useState<ProjectSummary | null>(null);
  const [summaryEditorOpen, setSummaryEditorOpen] = useState(false);
  const [summaryBusy, setSummaryBusy] = useState(false);
  const [summaryError, setSummaryError] = useState<string | null>(null);

  useEffect(() => {
    refresh();
  }, []);

  async function refresh() {
    try {
      const data = await invoke<ProjectStore>("list_projects");
      setStore(data);
    } catch (e) {
      console.error("Failed to list projects:", e);
    }
  }

  function openAddForm() {
    setFormName("");
    setFormDesc("");
    setFormLinks([]);
    setFormPath("");
    setFormRepoUrl("");
    setNewLink("");
    setUploadedFiles([]);
    setLastFetched(null);
    setMsg(null);
    setDigestState(null);
    setDigestError(null);
    setDigestBusy(false);
    setSummaryState(null);
    setSummaryError(null);
    setSummaryBusy(false);
    setSummaryEditorOpen(false);
    setFormMode({ type: "add" });
  }

  // ── Project Knowledge rethink — PROJECT.md handlers ─────────────
  //
  // The backend produces a six-section PROJECT.md once when a digest
  // is built (or when the user clicks Regenerate). The frontend
  // surfaces three actions:
  //   - View / Edit summary: opens the modal editor (read + edit)
  //   - Regenerate: re-runs the LLM generator against the current
  //                 digest, warns first if user_edited is true
  //   - Save (inside the modal): persists user edits

  async function handleRegenerateSummary() {
    if (formMode.type !== "edit") return;
    if (summaryState?.user_edited) {
      const ok = confirm(
        "This summary has been edited by you. Regenerating will overwrite your edits. Continue?",
      );
      if (!ok) return;
    }
    setSummaryError(null);
    setSummaryBusy(true);
    try {
      const next = await invoke<ProjectSummary>("regenerate_project_summary", {
        projectId: formMode.project.id,
      });
      setSummaryState(next);
      await refresh();
    } catch (e) {
      console.error("[project-summary] regenerate failed:", e);
      setSummaryError(String(e));
    } finally {
      setSummaryBusy(false);
    }
  }

  async function handleSaveSummary(markdown: string) {
    if (formMode.type !== "edit") return;
    setSummaryError(null);
    setSummaryBusy(true);
    try {
      await invoke("update_project_summary", {
        projectId: formMode.project.id,
        markdown,
      });
      const updated: ProjectSummary = {
        markdown,
        generated_at: new Date().toISOString(),
        user_edited: true,
        generator_model: "user-edit",
      };
      setSummaryState(updated);
      setSummaryEditorOpen(false);
      await refresh();
    } catch (e) {
      console.error("[project-summary] save failed:", e);
      setSummaryError(String(e));
    } finally {
      setSummaryBusy(false);
    }
  }

  function formatSummaryFetchedAt(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  }

  function openEditForm(project: Project) {
    setFormName(project.name);
    setFormDesc(project.description);
    setFormLinks(project.links || []);
    setFormPath(project.path || "");
    // Pre-fill the repo-url field if any saved link is a github URL —
    // saves the user from re-typing it when editing.
    const githubLink = (project.links || []).find((l) =>
      /^(https?:\/\/(www\.)?)?github\.com\//i.test(l.trim()),
    );
    setFormRepoUrl(githubLink || "");
    setNewLink("");
    setUploadedFiles([]);
    setLastFetched(null);
    setMsg(null);
    // Mirror the saved digest into local state so the Project Context
    // panel renders immediately on open.
    setDigestState(project.digest ?? null);
    setDigestError(null);
    setDigestBusy(false);
    // Same for the curated PROJECT.md — local state lets the editor
    // surface manual edits without waiting for refresh() to round-trip
    // through projects.json.
    setSummaryState(project.project_summary ?? null);
    setSummaryError(null);
    setSummaryBusy(false);
    setSummaryEditorOpen(false);
    setFormMode({ type: "edit", project });
    // Load the cached GitHub fetch timestamp (if any) for this project
    // so the "Last fetched:" line renders without a refresh round-trip.
    void invoke<string | null>("get_cached_context_timestamp", { id: project.id })
      .then((ts) => setLastFetched(ts ?? null))
      .catch((e) => {
        console.error("get_cached_context_timestamp failed:", e);
        setLastFetched(null);
      });
  }

  /// Manual refresh of the cached GitHub fetch. Only meaningful when
  /// the project is already saved (edit mode) and has at least one
  /// github.com link. Surfaces backend errors verbatim into the form's
  /// message area so the user can see why a refresh failed.
  async function handleRefreshContext() {
    if (formMode.type !== "edit") return;
    setRefreshing(true);
    setMsg(null);
    try {
      const ts = await invoke<string>("refresh_project_context", {
        id: formMode.project.id,
      });
      setLastFetched(ts);
      setMsg({ ok: true, text: "Project context refreshed." });
    } catch (e) {
      setMsg({ ok: false, text: `Refresh failed: ${e}` });
    } finally {
      setRefreshing(false);
    }
  }

  /// Format the backend's `"{seconds}s"` timestamp into a locale string.
  /// Returns `"never"` for `null` so callers don't need to branch.
  function formatLastFetched(raw: string | null): string {
    if (!raw) return "never";
    const match = /^(\d+)s$/.exec(raw);
    if (!match) return raw;
    const ms = Number(match[1]) * 1000;
    if (!Number.isFinite(ms) || ms <= 0) return raw;
    return new Date(ms).toLocaleString();
  }

  async function handleAnalyzeRepo() {
    const url = formRepoUrl.trim();
    if (!url) return;
    setAnalyzing(true);
    setMsg(null);
    try {
      const analyzed = await invoke<{
        name: string;
        description: string;
        default_branch: string;
        html_url: string;
      }>("analyze_github_repo", { url });
      // Always overwrite name; analyzed value wins. The user can still
      // edit it before saving.
      setFormName(analyzed.name);
      // Description: replace only if empty, otherwise append. Avoids
      // silently destroying notes the user already wrote.
      setFormDesc((prev) => (prev.trim() ? `${prev}\n\n${analyzed.description}` : analyzed.description));
      // Add the canonical html_url to links if not already present.
      setFormLinks((prev) =>
        prev.some((l) => l.trim() === analyzed.html_url) ? prev : [...prev, analyzed.html_url],
      );
      setMsg({ ok: true, text: `Analyzed ${analyzed.name}. Review and edit before saving.` });
    } catch (e) {
      setMsg({ ok: false, text: String(e) });
    } finally {
      setAnalyzing(false);
    }
  }

  async function handleBrowseDirectory() {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (typeof selected === "string") {
        setFormPath(selected);
      }
    } catch (e) {
      console.error("Directory picker failed:", e);
    }
  }

  // ── Project Knowledge: digest handlers ──────────────────────────
  //
  // All four mutate the saved Project on disk via the matching Tauri
  // command, then mirror the result into local state so the panel
  // re-renders immediately. `refresh()` after each so the project
  // list / active-banner pick up the new updated_at.

  async function handleAddLocalDigest() {
    if (formMode.type !== "edit") return;
    setDigestError(null);
    let folderPath: string | string[] | null;
    try {
      folderPath = await open({
        directory: true,
        multiple: false,
        title: "Pick the project folder to digest",
      });
    } catch (e) {
      console.error("Folder picker failed:", e);
      setDigestError(`Folder picker failed: ${e}`);
      return;
    }
    if (!folderPath || typeof folderPath !== "string") return;

    setDigestBusy(true);
    try {
      const digest = await invoke<RepoDigest>("digest_local_project", {
        projectId: formMode.project.id,
        folderPath,
      });
      setDigestState(digest);
      await refresh();
      // The backend also generates a PROJECT.md after the digest
      // finishes (best-effort — failures are logged, not thrown).
      // Pull whatever it produced from the freshly-refreshed store
      // so the summary status row updates immediately.
      await syncSummaryFromStore(formMode.project.id);
    } catch (e) {
      setDigestError(String(e));
    } finally {
      setDigestBusy(false);
    }
  }

  /// Look up the named project in the freshly-fetched store and copy
  /// its `project_summary` into local state. Idempotent — safe to call
  /// after any backend mutation that might have touched the summary.
  async function syncSummaryFromStore(projectId: string) {
    try {
      const data = await invoke<ProjectStore>("list_projects");
      const p = data.projects.find((x) => x.id === projectId);
      setSummaryState(p?.project_summary ?? null);
    } catch (e) {
      console.error("[project-summary] store sync failed:", e);
    }
  }

  async function handleAddGithubDigest() {
    if (formMode.type !== "edit") return;
    const url = formRepoUrl.trim();
    if (!url) {
      setDigestError("Paste a GitHub URL in the Repo URL field first.");
      return;
    }
    setDigestError(null);
    setDigestBusy(true);
    console.info(
      "[digest-github] starting project_id=%s url=%s",
      formMode.project.id,
      url,
    );
    try {
      const digest = await invoke<RepoDigest>("digest_github_project", {
        projectId: formMode.project.id,
        githubUrl: url,
      });
      console.info(
        "[digest-github] done project_id=%s files=%d elided=%d tokens=%d",
        formMode.project.id,
        digest.file_count,
        digest.elided_count,
        digest.token_count_estimate,
      );
      setDigestState(digest);
      await refresh();
      // Project Knowledge rethink — the digest_github_project step=7
      // generates PROJECT.md after the digest. Pull whatever landed.
      await syncSummaryFromStore(formMode.project.id);
    } catch (e) {
      // Log AND surface — previous symptom (UI "did nothing") was an
      // unhandled rejection being swallowed. The eprintln chain in
      // projects.rs::digest_github_project tags the exact failing step;
      // this log preserves the same message in the browser devtools so
      // a Windows user without an attached terminal still sees it.
      console.error("[digest-github] failed:", e);
      setDigestError(String(e));
    } finally {
      setDigestBusy(false);
    }
  }

  async function handleRefreshDigest() {
    if (formMode.type !== "edit" || !digestState) return;
    setDigestError(null);
    setDigestBusy(true);
    try {
      const projectId = formMode.project.id;
      const digest =
        digestState.source.kind === "local"
          ? await invoke<RepoDigest>("digest_local_project", {
              projectId,
              folderPath: digestState.source.value.path,
            })
          : await invoke<RepoDigest>("digest_github_project", {
              projectId,
              githubUrl: digestState.source.value.html_url,
            });
      setDigestState(digest);
      await refresh();
      await syncSummaryFromStore(projectId);
    } catch (e) {
      console.error("[digest-refresh] failed:", e);
      setDigestError(String(e));
    } finally {
      setDigestBusy(false);
    }
  }

  async function handleClearDigest() {
    if (formMode.type !== "edit") return;
    setDigestError(null);
    setDigestBusy(true);
    try {
      await invoke("clear_project_digest", { projectId: formMode.project.id });
      setDigestState(null);
      await refresh();
    } catch (e) {
      setDigestError(String(e));
    } finally {
      setDigestBusy(false);
    }
  }

  /// Format an ISO-8601 timestamp as a short relative-ish string for
  /// the "Last refreshed" line. Falls back to the raw string if the
  /// date doesn't parse.
  function formatDigestFetchedAt(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  }

  function describeDigestSource(d: RepoDigest): string {
    return d.source.kind === "local"
      ? d.source.value.path
      : `${d.source.value.owner}/${d.source.value.repo}@${d.source.value.branch}`;
  }

  function closeForm() {
    setFormMode({ type: "closed" });
    setMsg(null);
  }

  // ---- File Upload ----
  async function handleFileUpload() {
    try {
      const selected = await open({
        multiple: true,
        filters: [
          {
            name: "Documents",
            extensions: ["txt", "md", "json", "csv", "html", "xml", "yaml", "yml", "toml", "ini", "log", "cfg", "rs", "py", "js", "ts", "tsx", "jsx", "css", "scss", "java", "cpp", "c", "h", "go", "rb", "php", "sql", "sh", "bat", "ps1"],
          },
        ],
      });

      if (!selected) return;

      const paths = Array.isArray(selected) ? selected : [selected];
      
      for (const filePath of paths) {
        try {
          const content = await invoke<string>("read_file_content", { path: filePath });
          const fileName = filePath.split(/[/\\]/).pop() || filePath;
          
          // Append file content to description with a header
          const fileBlock = `\n\n--- File: ${fileName} ---\n${content}`;
          setFormDesc((prev) => prev + fileBlock);
          setUploadedFiles((prev) => [...prev, fileName]);
        } catch (e) {
          setMsg({ ok: false, text: `Failed to read ${filePath}: ${e}` });
        }
      }
    } catch (e) {
      console.error("File dialog error:", e);
    }
  }

  // ---- Links ----
  function addLink() {
    const trimmed = newLink.trim();
    if (!trimmed) return;
    setFormLinks((prev) => [...prev, trimmed]);
    setNewLink("");
  }

  function removeLink(index: number) {
    setFormLinks((prev) => prev.filter((_, i) => i !== index));
  }

  function handleLinkKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      addLink();
    }
  }

  // ---- Save ----
  async function handleSave() {
    if (!formName.trim()) return;
    setBusy(true);
    setMsg(null);
    try {
      const path = formPath.trim() || null;
      if (formMode.type === "add") {
        await invoke("add_project", { name: formName, description: formDesc, path });
        // If links were added, immediately update the project with links
        const data = await invoke<ProjectStore>("list_projects");
        const newProject = data.projects[data.projects.length - 1];
        if (newProject && formLinks.length > 0) {
          await invoke("update_project", {
            id: newProject.id,
            name: formName,
            description: formDesc,
            links: formLinks,
            path,
          });
        }
        setMsg({ ok: true, text: "Project added!" });
      } else if (formMode.type === "edit") {
        await invoke("update_project", {
          id: formMode.project.id,
          name: formName,
          description: formDesc,
          links: formLinks,
          path,
        });
        setMsg({ ok: true, text: "Project updated!" });
      }
      await refresh();
      setTimeout(() => closeForm(), 600);
    } catch (e) {
      setMsg({ ok: false, text: String(e) });
    } finally {
      setBusy(false);
    }
  }

  async function handleDelete(id: string) {
    if (!confirm("Delete this project?")) return;
    try {
      await invoke("delete_project", { id });
      await refresh();
    } catch (e) {
      console.error("Delete failed:", e);
    }
  }

  async function handleSetActive(id: string) {
    try {
      await invoke("set_active_project", { id });
      await refresh();
    } catch (e) {
      console.error("Set active failed:", e);
    }
  }

  const activeProject = store.projects.find((p) => p.id === store.active_project_id);

  return (
    <div className="pm-container">
      {/* Beta notice + waitlist — sets expectations that Projects is
          still being finished and offers a one-click way to be told
          when it ships. */}
      <BetaWaitlistBanner />

      {/* Header */}
      <div className="pm-header">
        <div>
          <h1 className="pm-title">
            Project Context
            <span className="pm-title-beta" aria-label="Beta feature">Beta</span>
          </h1>
          <p className="pm-subtitle">
            Add project descriptions to make prompt enhancements context-aware
          </p>
        </div>
        <button className="pm-add-btn" onClick={openAddForm}>
          + Add Project
        </button>
      </div>

      {/* Active project banner */}
      {activeProject && (
        <div className="pm-active-banner">
          <div className="pm-active-dot" />
          <span className="pm-active-label">
            Active: <span className="pm-active-name">{activeProject.name}</span>
          </span>
        </div>
      )}

      {/* Project list or empty state */}
      {store.projects.length === 0 ? (
        <div className="pm-empty">
          <div className="pm-empty-icon">📁</div>
          <h3>No projects yet</h3>
          <p>
            Add a project description so PerfectPrompt knows what you're building.
            The AI will generate smarter questions and more relevant prompts.
          </p>
        </div>
      ) : (
        <div className="pm-list">
          {store.projects.map((p) => (
            <div
              key={p.id}
              className={`pm-card ${p.id === store.active_project_id ? "active" : ""}`}
            >
              <div className="pm-card-header">
                <div className="pm-card-name">
                  {p.name}
                  {p.id === store.active_project_id && (
                    <span className="pm-card-badge">Active</span>
                  )}
                </div>
                <div className="pm-card-actions">
                  {p.id !== store.active_project_id && (
                    <button
                      className="pm-activate-btn"
                      onClick={() => handleSetActive(p.id)}
                    >
                      Set Active
                    </button>
                  )}
                  <button onClick={() => openEditForm(p)}>Edit</button>
                  <button
                    className="pm-delete-btn"
                    onClick={() => handleDelete(p.id)}
                  >
                    Delete
                  </button>
                </div>
              </div>
              <div className="pm-card-desc">
                {p.description || "No description provided."}
              </div>
              {p.links && p.links.length > 0 && (
                <div className="pm-card-links">
                  🔗 {p.links.length} link{p.links.length > 1 ? "s" : ""} attached
                </div>
              )}
              {/* Project Knowledge digest indicator — surfaces at-a-glance
                  whether the project has a real source-of-truth digest
                  attached vs just lightweight metadata. Clicking through
                  to Edit reveals the actual Add / Refresh controls. */}
              {p.digest ? (
                <div className="pm-card-digest pm-card-digest-on">
                  📚 {p.digest.file_count} files indexed
                  {p.digest.elided_count > 0
                    ? ` (${p.digest.elided_count} elided)`
                    : ""}
                  {" · "}~{p.digest.token_count_estimate.toLocaleString()} tokens
                </div>
              ) : (
                <div className="pm-card-digest pm-card-digest-off">
                  No project knowledge attached — open Edit → Project
                  Knowledge to add a local folder or GitHub repo.
                </div>
              )}
            </div>
          ))}
        </div>
      )}

      {/* Add/Edit Form Modal */}
      {formMode.type !== "closed" && (
        <div className="pm-form-overlay" onClick={closeForm}>
          <div className="pm-form" onClick={(e) => e.stopPropagation()}>
            <h3>{formMode.type === "add" ? "Add New Project" : "Edit Project"}</h3>

            {msg && (
              <div className={`pm-msg ${msg.ok ? "pm-msg-ok" : "pm-msg-err"}`}>
                {msg.text}
              </div>
            )}

            <label>Repo URL <span className="pm-optional">(paste & analyze to auto-fill)</span></label>
            <div className="pm-path-row">
              <input
                type="text"
                placeholder="https://github.com/owner/repo"
                value={formRepoUrl}
                onChange={(e) => setFormRepoUrl(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    void handleAnalyzeRepo();
                  }
                }}
                disabled={busy || analyzing}
                className="pm-path-input"
                autoFocus
              />
              <button
                className="pm-link-add-btn"
                onClick={() => void handleAnalyzeRepo()}
                disabled={busy || analyzing || !formRepoUrl.trim()}
                type="button"
              >
                {analyzing ? "Analyzing…" : "Analyze"}
              </button>
            </div>

            {formMode.type === "edit" && (
              <div className="pm-refresh-row">
                <span className="pm-refresh-label">
                  Project context cache —{" "}
                  <span className="pm-refresh-timestamp">
                    Last fetched: {formatLastFetched(lastFetched)}
                  </span>
                </span>
                <button
                  className="pm-link-add-btn"
                  onClick={() => void handleRefreshContext()}
                  disabled={
                    busy ||
                    refreshing ||
                    !formLinks.some((l) =>
                      /^(https?:\/\/(www\.)?)?github\.com\//i.test(l.trim()) ||
                      /^git@github\.com:/i.test(l.trim()),
                    )
                  }
                  type="button"
                  title={
                    formLinks.some((l) =>
                      /^(https?:\/\/(www\.)?)?github\.com\//i.test(l.trim()) ||
                      /^git@github\.com:/i.test(l.trim()),
                    )
                      ? "Re-fetch and overwrite the cached GitHub context"
                      : "Add a github.com link to enable refresh"
                  }
                >
                  {refreshing ? "Refreshing…" : "Refresh project context"}
                </button>
              </div>
            )}

            {/* ── Project Knowledge — repository digest panel ──
                Edit mode only because the backend commands need a
                stored project_id to attach the digest to. */}
            {formMode.type === "edit" && (
              <div className="pm-context-section">
                <div className="pm-context-header">
                  <span className="pm-context-title">Project Knowledge</span>
                  <span className="pm-context-sub">
                    Full README + manifests + key source files,
                    injected into every enhance/polish call.
                  </span>
                </div>

                {digestState ? (
                  <div className="pm-context-status">
                    <div className="pm-context-status-row">
                      <span className="pm-context-status-key">Source</span>
                      <span className="pm-context-source-label">
                        {describeDigestSource(digestState)}
                      </span>
                    </div>
                    <div className="pm-context-status-row">
                      <span className="pm-context-status-key">Indexed</span>
                      <span>
                        {digestState.file_count} files
                        {digestState.elided_count > 0
                          ? ` (${digestState.elided_count} elided)`
                          : ""}
                      </span>
                    </div>
                    <div className="pm-context-status-row">
                      <span className="pm-context-status-key">Size</span>
                      <span>
                        ~{digestState.token_count_estimate.toLocaleString()} tokens
                      </span>
                    </div>
                    <div className="pm-context-status-row">
                      <span className="pm-context-status-key">Last refreshed</span>
                      <span>{formatDigestFetchedAt(digestState.fetched_at)}</span>
                    </div>

                    {/* Project Knowledge rethink — PROJECT.md status.
                        Two states: still being generated (digest
                        present, summary absent) vs ready (summary
                        present). */}
                    {summaryState ? (
                      <div className="pm-summary-status">
                        <div className="pm-context-status-row">
                          <span className="pm-context-status-key">Summary</span>
                          <span>
                            {summaryState.user_edited ? "edited by you" : "auto-generated"}
                            {" · "}
                            {summaryState.markdown.length.toLocaleString()} chars
                            {" · "}
                            generated {formatSummaryFetchedAt(summaryState.generated_at)}
                          </span>
                        </div>
                        <div className="pm-summary-actions">
                          <button
                            type="button"
                            className="pm-link-add-btn"
                            disabled={summaryBusy || digestBusy}
                            onClick={() => setSummaryEditorOpen(true)}
                          >
                            View / Edit summary
                          </button>
                          <button
                            type="button"
                            className="pm-link-add-btn"
                            disabled={summaryBusy || digestBusy}
                            onClick={() => void handleRegenerateSummary()}
                          >
                            {summaryBusy ? "Regenerating…" : "Regenerate"}
                          </button>
                        </div>
                      </div>
                    ) : (
                      <div className="pm-summary-banner-warning">
                        ⚠ Project summary is being generated — enhanced
                        prompts use a fallback path until it's ready.
                        If this persists, click Refresh to retry.
                      </div>
                    )}

                    {summaryError && (
                      <div className="pm-context-error" role="alert">
                        {summaryError}
                      </div>
                    )}

                    <div className="pm-context-actions">
                      <button
                        type="button"
                        className="pm-link-add-btn"
                        disabled={digestBusy}
                        onClick={() => void handleRefreshDigest()}
                      >
                        {digestBusy ? "Working…" : "Refresh"}
                      </button>
                      <button
                        type="button"
                        className="pm-link-add-btn"
                        disabled={digestBusy}
                        onClick={() => setDigestViewerOpen(true)}
                        title="See exactly what's being sent to the LLM"
                      >
                        View digest
                      </button>
                      <button
                        type="button"
                        className="pm-btn-cancel"
                        disabled={digestBusy}
                        onClick={() => void handleClearDigest()}
                      >
                        Remove
                      </button>
                    </div>
                  </div>
                ) : (
                  <div className="pm-context-empty">
                    <div className="pm-context-actions">
                      <button
                        type="button"
                        className="pm-link-add-btn"
                        disabled={digestBusy}
                        onClick={() => void handleAddLocalDigest()}
                      >
                        {digestBusy ? "Working…" : "Add local folder"}
                      </button>
                      <button
                        type="button"
                        className="pm-link-add-btn"
                        disabled={digestBusy || !formRepoUrl.trim()}
                        title={
                          formRepoUrl.trim()
                            ? "Download + digest the GitHub repo from the Repo URL field above"
                            : "Paste a GitHub URL in the Repo URL field above first"
                        }
                        onClick={() => void handleAddGithubDigest()}
                      >
                        {digestBusy ? "Working…" : "Add GitHub repo"}
                      </button>
                    </div>
                  </div>
                )}

                {digestError && (
                  <div className="pm-context-error" role="alert">
                    {digestError}
                  </div>
                )}
              </div>
            )}

            <label>Project Name</label>
            <input
              type="text"
              placeholder="e.g. PerfectPrompt, MyShopApp, PortfolioSite..."
              value={formName}
              onChange={(e) => setFormName(e.target.value)}
              disabled={busy}
            />

            <label>Project Directory <span className="pm-optional">(optional)</span></label>
            <div className="pm-path-row">
              <input
                type="text"
                placeholder="C:\path\to\project — enables file-aware enhancement"
                value={formPath}
                onChange={(e) => setFormPath(e.target.value)}
                disabled={busy}
                className="pm-path-input"
              />
              <button
                className="pm-link-add-btn"
                onClick={() => void handleBrowseDirectory()}
                disabled={busy}
                type="button"
              >
                Browse…
              </button>
            </div>

            <label>Project Description</label>
            <textarea
              placeholder={`Describe your project in detail. Include:\n\n• Tech stack (e.g. React, Tauri, Node.js)\n• What the project does\n• Key features and architecture\n• File structure overview\n• Any conventions or patterns used\n\nThe more detail, the better the AI understands your project.`}
              value={formDesc}
              onChange={(e) => setFormDesc(e.target.value)}
              disabled={busy}
            />

            {/* File Upload */}
            <div className="pm-upload-section">
              <button className="pm-upload-btn" onClick={handleFileUpload} disabled={busy} type="button">
                📄 Upload Files
              </button>
              <span className="pm-upload-hint">
                .txt, .md, .json, .csv, .html, code files, and more
              </span>
            </div>
            {uploadedFiles.length > 0 && (
              <div className="pm-uploaded-list">
                {uploadedFiles.map((f, i) => (
                  <span key={i} className="pm-uploaded-file">✓ {f}</span>
                ))}
              </div>
            )}

            {/* Links */}
            <label style={{ marginTop: 16 }}>Project Links</label>
            <div className="pm-links-input-row">
              <input
                type="text"
                placeholder="https://github.com/your-project or any relevant URL..."
                value={newLink}
                onChange={(e) => setNewLink(e.target.value)}
                onKeyDown={handleLinkKeyDown}
                disabled={busy}
                className="pm-link-input"
              />
              <button
                className="pm-link-add-btn"
                onClick={addLink}
                disabled={busy || !newLink.trim()}
                type="button"
              >
                Add
              </button>
            </div>
            {formLinks.length > 0 && (
              <div className="pm-links-list">
                {formLinks.map((link, i) => (
                  <div key={i} className="pm-link-item">
                    <span className="pm-link-text">🔗 {link}</span>
                    <button
                      className="pm-link-remove"
                      onClick={() => removeLink(i)}
                      type="button"
                    >
                      ×
                    </button>
                  </div>
                ))}
              </div>
            )}

            <p className="pm-form-hint">
              💡 Tip: Upload README, docs, or code files. Add links to repos, designs, or docs.
            </p>

            <div className="pm-form-footer">
              <button className="pm-btn-cancel" onClick={closeForm} disabled={busy}>
                Cancel
              </button>
              <button
                className="pm-btn-save"
                onClick={handleSave}
                disabled={busy || !formName.trim()}
              >
                {busy ? "Saving..." : formMode.type === "add" ? "Add Project" : "Save Changes"}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Project Knowledge revamp — Layer 1 observability surface.
          Shows the user EXACTLY what's being sent to the LLM. If
          lib/option.js (or whatever file they're debugging against)
          isn't in this textarea, no amount of prompt tuning will help
          — the digest itself is wrong, and the prioritization layer
          needs further tuning. */}
      {digestViewerOpen && digestState && (
        <DigestViewerModal
          digest={digestState}
          onClose={() => setDigestViewerOpen(false)}
        />
      )}

      {/* Project Knowledge rethink — PROJECT.md editor. Surfaces the
          six-section structured editor over the form. Only mounted
          when both the edit form is open AND a summary exists,
          because there's nothing to edit otherwise. */}
      {summaryEditorOpen && summaryState && formMode.type === "edit" && (
        <ProjectSummaryEditor
          projectName={formMode.project.name}
          summary={summaryState}
          busy={summaryBusy}
          onSave={(md) => void handleSaveSummary(md)}
          onRegenerate={() => void handleRegenerateSummary()}
          onClose={() => setSummaryEditorOpen(false)}
        />
      )}
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────
// BetaWaitlistBanner — "this feature is in beta, join the waitlist"
// ─────────────────────────────────────────────────────────────────────
//
// Prominent, persistent notice pinned to the top of the Projects
// screen. It sets the expectation that Projects is still being
// finished and offers a one-click waitlist signup (backed by the
// project_waitlist table, migration 0005). Once the user has joined,
// the banner flips to a confirmed state on this and every future visit.

function BetaWaitlistBanner() {
  const { status, error, join } = useProjectWaitlist();
  const joined = status === "joined";
  const pending = status === "loading" || status === "joining";

  return (
    <div
      className={`pm-beta-banner ${joined ? "is-joined" : ""}`}
      role="status"
      aria-live="polite"
    >
      <div className="pm-beta-icon" aria-hidden="true">
        {joined ? "✓" : "✦"}
      </div>

      <div className="pm-beta-copy">
        <div className="pm-beta-title">
          {joined ? "You're on the waitlist" : "Projects is in Beta"}
        </div>
        <p className="pm-beta-text">
          {joined
            ? "Thanks for signing up — we'll email you the moment Projects is ready to launch."
            : "We're still putting the finishing touches on this feature. Join the waitlist and we'll let you know the moment it launches."}
        </p>
        {error && (
          <p className="pm-beta-error" role="alert">
            {error}
          </p>
        )}
      </div>

      {!joined && (
        <button
          type="button"
          className="pm-beta-cta"
          onClick={() => void join()}
          disabled={pending}
        >
          {status === "joining" ? "Joining…" : "Join the waitlist"}
        </button>
      )}
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────
// DigestViewerModal — Layer 1 observability
// ─────────────────────────────────────────────────────────────────────

interface DigestViewerModalProps {
  digest: RepoDigest;
  onClose: () => void;
}

function DigestViewerModal({ digest, onClose }: DigestViewerModalProps) {
  const [copied, setCopied] = useState(false);

  // Esc-to-close. The edit form's overlay-click-to-close pattern is
  // less appropriate here because the digest text in the textarea is
  // selectable — an accidental click in the textarea margin shouldn't
  // dismiss the modal.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(digest.digest_text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch (e) {
      console.error("[digest-viewer] clipboard write failed:", e);
    }
  }

  const sourceLabel =
    digest.source.kind === "local"
      ? digest.source.value.path
      : `${digest.source.value.owner}/${digest.source.value.repo}@${digest.source.value.branch}`;

  const fetchedAt = (() => {
    const d = new Date(digest.fetched_at);
    return Number.isNaN(d.getTime()) ? digest.fetched_at : d.toLocaleString();
  })();

  return (
    <div className="pm-digest-viewer-overlay" onClick={onClose}>
      <div
        className="pm-digest-viewer"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="View repository digest"
      >
        <header className="pm-digest-viewer-head">
          <h3 className="pm-digest-viewer-title">Repository digest</h3>
          <button
            type="button"
            className="pm-digest-viewer-close"
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </header>

        <div className="pm-digest-viewer-meta">
          <div className="pm-digest-viewer-meta-row">
            <span className="pm-digest-viewer-meta-key">Source</span>
            <span className="pm-context-source-label">{sourceLabel}</span>
          </div>
          <div className="pm-digest-viewer-meta-row">
            <span className="pm-digest-viewer-meta-key">Fetched</span>
            <span>{fetchedAt}</span>
          </div>
          <div className="pm-digest-viewer-meta-row">
            <span className="pm-digest-viewer-meta-key">Files indexed</span>
            <span>
              {digest.file_count}
              {digest.elided_count > 0
                ? ` (${digest.elided_count} elided)`
                : ""}
            </span>
          </div>
          <div className="pm-digest-viewer-meta-row">
            <span className="pm-digest-viewer-meta-key">Token estimate</span>
            <span>~{digest.token_count_estimate.toLocaleString()}</span>
          </div>
          <div className="pm-digest-viewer-meta-row">
            <span className="pm-digest-viewer-meta-key">Size on the wire</span>
            <span>{digest.digest_text.length.toLocaleString()} bytes</span>
          </div>
        </div>

        <div className="pm-digest-viewer-actions">
          <button
            type="button"
            className="pm-link-add-btn"
            onClick={() => void handleCopy()}
          >
            {copied ? "Copied" : "Copy to clipboard"}
          </button>
          <span className="pm-digest-viewer-hint">
            This is the literal text that gets injected into the LLM's
            context block on every enhance/polish call.
          </span>
        </div>

        <textarea
          className="pm-digest-viewer-text"
          value={digest.digest_text}
          readOnly
          spellCheck={false}
        />
      </div>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────
// ProjectSummaryEditor — Project Knowledge rethink (Step 16)
// ─────────────────────────────────────────────────────────────────────
//
// Structured editor for the six-section PROJECT.md. The schema is
// fixed: the user can edit the body of each section but never the
// section headers themselves. This guarantees the rejoined markdown
// passes the backend's `validate_project_md` check.

const SUMMARY_SECTIONS: readonly string[] = [
  "## What this is",
  "## Stack",
  "## Architecture",
  "## Existing capabilities",
  "## Conventions",
  "## Gotchas",
] as const;

const SUMMARY_MAX_BYTES = 4_000;
const SUMMARY_WARN_BYTES = 3_500;

interface ParsedSummary {
  /// The `# Project Name` line (H1). We render it as a read-only
  /// title; the user can't edit project name through this modal.
  h1: string;
  /// Body text per section header, in declared order. Missing
  /// sections come back as empty strings; the schema validator
  /// rejects empty rejoined output, so the UI never lets the user
  /// save a summary with zero body content.
  bodies: Record<string, string>;
}

/// Split the markdown into the H1 title plus a body-per-section
/// map. Tolerates blank lines / extra whitespace between sections.
function parseSummaryMarkdown(md: string): ParsedSummary {
  const lines = md.split(/\r?\n/);
  const bodies: Record<string, string> = {};
  for (const header of SUMMARY_SECTIONS) bodies[header] = "";
  let h1 = "";
  let currentHeader: string | null = null;
  let currentLines: string[] = [];

  const flush = () => {
    if (currentHeader) {
      bodies[currentHeader] = currentLines.join("\n").trim();
    }
    currentLines = [];
  };

  for (const line of lines) {
    if (line.startsWith("# ") && !h1) {
      h1 = line.trim();
      continue;
    }
    const matched = SUMMARY_SECTIONS.find((h) => line.trim() === h);
    if (matched) {
      flush();
      currentHeader = matched;
      continue;
    }
    if (currentHeader) {
      currentLines.push(line);
    }
  }
  flush();
  return { h1, bodies };
}

/// Re-join an edited ParsedSummary back into a single markdown
/// document. The output always emits headers in the canonical order,
/// so even if the user-supplied input had a slightly different order
/// the saved file is well-formed.
function rejoinSummaryMarkdown(parsed: ParsedSummary): string {
  const h1 = parsed.h1.trim().length > 0 ? parsed.h1 : "# Project";
  const parts: string[] = [h1];
  for (const header of SUMMARY_SECTIONS) {
    const body = (parsed.bodies[header] ?? "").trim();
    parts.push(`${header}\n${body}`);
  }
  return parts.join("\n\n");
}

interface ProjectSummaryEditorProps {
  projectName: string;
  summary: ProjectSummary;
  busy: boolean;
  onSave: (markdown: string) => void;
  onRegenerate: () => void;
  onClose: () => void;
}

function ProjectSummaryEditor({
  projectName,
  summary,
  busy,
  onSave,
  onRegenerate,
  onClose,
}: ProjectSummaryEditorProps) {
  const initial = parseSummaryMarkdown(summary.markdown);
  // Each section has its own piece of state so a textarea change
  // doesn't re-parse and reflow the other five. Headers stay fixed.
  const [h1, setH1] = useState(initial.h1);
  const [bodies, setBodies] = useState<Record<string, string>>(initial.bodies);

  // Esc to close.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  const rejoined = rejoinSummaryMarkdown({ h1, bodies });
  const totalChars = rejoined.length;
  const overBudget = totalChars > SUMMARY_MAX_BYTES;
  const warn = totalChars > SUMMARY_WARN_BYTES && !overBudget;

  function updateBody(header: string, next: string) {
    setBodies((prev) => ({ ...prev, [header]: next }));
  }

  return (
    <div className="pm-summary-editor-overlay" onClick={onClose}>
      <div
        className="pm-summary-editor"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="Edit project summary"
      >
        <header className="pm-summary-editor-head">
          <h3 className="pm-summary-editor-title">
            {projectName} — Project Summary
          </h3>
          <button
            type="button"
            className="pm-digest-viewer-close"
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </header>

        <div className="pm-summary-editor-meta">
          {summary.user_edited
            ? "Edited by you. Saving overwrites the auto-generated version."
            : `Auto-generated by ${summary.generator_model}. Edit any section below.`}
        </div>

        <input
          className="pm-summary-h1"
          value={h1}
          onChange={(e) => setH1(e.target.value)}
          placeholder="# Project Name"
          disabled={busy}
        />

        {SUMMARY_SECTIONS.map((header) => (
          <div key={header} className="pm-summary-section">
            <div className="pm-summary-section-header">{header}</div>
            <textarea
              className="pm-summary-section-textarea"
              value={bodies[header] ?? ""}
              onChange={(e) => updateBody(header, e.target.value)}
              disabled={busy}
              spellCheck={false}
              rows={header === "## What this is" || header === "## Stack" ? 3 : 6}
            />
          </div>
        ))}

        <div className="pm-summary-editor-footer">
          <span
            className={`pm-summary-char-count ${
              overBudget ? "error" : warn ? "warn" : ""
            }`}
          >
            {totalChars.toLocaleString()} / {SUMMARY_MAX_BYTES.toLocaleString()} chars
            {overBudget && " — over hard limit, will be truncated server-side"}
          </span>
          <div className="pm-summary-editor-actions">
            <button
              type="button"
              className="pm-btn-cancel"
              onClick={onClose}
              disabled={busy}
            >
              Cancel
            </button>
            <button
              type="button"
              className="pm-link-add-btn"
              onClick={onRegenerate}
              disabled={busy}
              title="Re-run the LLM generator against the current digest"
            >
              Reset to auto-generated
            </button>
            <button
              type="button"
              className="pm-btn-save"
              onClick={() => onSave(rejoined)}
              disabled={busy || overBudget}
            >
              {busy ? "Saving…" : "Save"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
